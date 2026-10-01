use crate::domain::{ClipId, TimelineTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerProfile {
    Eco,
    #[default]
    Balanced,
    Performance,
}

impl PowerProfile {
    pub const fn preview_fps(self) -> f64 {
        match self {
            Self::Eco => 15.0,
            Self::Balanced => 30.0,
            Self::Performance => 60.0,
        }
    }

    pub const fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Eco => (640, 360),
            Self::Balanced => (960, 540),
            Self::Performance => (1920, 1080),
        }
    }

    pub const fn decoder_threads(self) -> u8 {
        match self {
            Self::Eco => 1,
            Self::Balanced => 2,
            Self::Performance => 4,
        }
    }
}

/// How long a live FFmpeg preview decoder is kept warm after the last frame request. Long
/// enough to cover scrubbing pauses and playback frame gaps, short enough that a stopped
/// preview does not leave a decode process running in the background.
const DECODER_IDLE_PARK: Duration = Duration::from_millis(2_500);

#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub generation: u64,
    /// The timeline clip this frame is for. One decoder is kept per visible clip so several
    /// layers can composite at once.
    pub clip_id: ClipId,
    pub path: PathBuf,
    pub source_time: TimelineTime,
    pub profile: PowerProfile,
    pub use_qsv: bool,
    /// A still image has no timeline to seek in: it is looped and its (constant) frame is shown.
    pub is_image: bool,
}

impl PreviewRequest {
    /// Whether hardware (QSV) decoding should actually be attempted. Still images are always
    /// decoded in software: forcing QSV output on a software image decoder makes FFmpeg's filter
    /// graph fail, and for HEIC it hangs without ever producing a frame.
    fn hardware_decode(&self) -> bool {
        self.use_qsv && !self.is_image
    }
}

#[derive(Debug)]
pub struct PreviewFrame {
    pub generation: u64,
    pub clip_id: ClipId,
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    pub qsv_active: bool,
    pub error: Option<String>,
}

pub struct PreviewWorker {
    requests: Sender<Vec<PreviewRequest>>,
    frames: Receiver<PreviewFrame>,
    /// Decoded RGB buffers returned by the UI thread, so the decoder can reuse their
    /// allocations instead of allocating a fresh multi-megabyte buffer for every frame.
    recycle: Sender<Vec<u8>>,
}

impl PreviewWorker {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Self {
        let (request_tx, request_rx) = channel::<Vec<PreviewRequest>>();
        let (frame_tx, frame_rx) = sync_channel::<PreviewFrame>(16);
        let (recycle_tx, recycle_rx) = channel::<Vec<u8>>();
        thread::Builder::new()
            .name("preview-decoder".into())
            .spawn(move || {
                let mut decoders: HashMap<ClipId, ContinuousDecoder> = HashMap::new();
                let mut last_seen: HashMap<ClipId, Instant> = HashMap::new();
                let mut buffer_pool: Vec<Vec<u8>> = Vec::new();
                loop {
                    // While decoders are alive, idle just long enough to reuse them for
                    // scrubbing or playback, then tear them down so FFmpeg does not sit in
                    // the background after the preview goes quiet. With no decoder, block
                    // until work arrives.
                    let mut batch = if decoders.is_empty() {
                        match request_rx.recv() {
                            Ok(batch) => batch,
                            Err(_) => break,
                        }
                    } else {
                        match request_rx.recv_timeout(DECODER_IDLE_PARK) {
                            Ok(batch) => batch,
                            Err(RecvTimeoutError::Timeout) => {
                                decoders.clear();
                                continue;
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    };
                    while let Ok(newer) = request_rx.try_recv() {
                        batch = newer;
                    }
                    while buffer_pool.len() < 4 {
                        match recycle_rx.try_recv() {
                            Ok(buffer) => buffer_pool.push(buffer),
                            Err(_) => break,
                        }
                    }

                    let now = Instant::now();
                    for request in &batch {
                        last_seen.insert(request.clip_id, now);
                    }
                    decoders
                        .retain(|clip_id, _| last_seen.get(clip_id).is_some_and(|t| now.duration_since(*t) < DECODER_IDLE_PARK));
                    last_seen.retain(|clip_id, _| decoders.contains_key(clip_id));

                    for request in batch {
                        let hardware = request.hardware_decode();
                        let needs_restart = decoders
                            .get(&request.clip_id)
                            .is_none_or(|active| !active.can_continue(&request));
                        if needs_restart {
                            let spawned = ContinuousDecoder::spawn(&request, hardware)
                                .or_else(|_| ContinuousDecoder::spawn(&request, false))
                                .ok();
                            if let Some(spawned) = spawned {
                                decoders.insert(request.clip_id, spawned);
                            } else {
                                decoders.remove(&request.clip_id);
                            }
                        }

                        let read = decoders
                            .get_mut(&request.clip_id)
                            .map(|active| active.read_frame_at(request.source_time, &mut buffer_pool));
                        let frame = match read {
                            Some(Ok(frame)) => frame,
                            Some(Err(_)) if hardware => {
                                // The hardware decoder failed after starting; retry in software.
                                let software = ContinuousDecoder::spawn(&request, false).ok();
                                let retry = match software {
                                    Some(mut active) => {
                                        let frame =
                                            active.read_frame_at(request.source_time, &mut buffer_pool);
                                        decoders.insert(request.clip_id, active);
                                        Some(frame)
                                    }
                                    None => {
                                        decoders.remove(&request.clip_id);
                                        None
                                    }
                                };
                                match retry {
                                    Some(Ok(frame)) => frame,
                                    _ => PreviewFrame::error(
                                        &request,
                                        failure_message(decoders.get_mut(&request.clip_id)),
                                    ),
                                }
                            }
                            _ => PreviewFrame::error(
                                &request,
                                failure_message(decoders.get_mut(&request.clip_id)),
                            ),
                        };
                        let _ = frame_tx.try_send(frame);
                    }
                    repaint();
                }
            })
            .expect("failed to start preview decoder");
        Self {
            requests: request_tx,
            frames: frame_rx,
            recycle: recycle_tx,
        }
    }

    /// Queues one frame request per visible clip. Only the newest batch matters, so older
    /// pending batches are superseded by the decoder thread.
    pub fn request_batch(&self, batch: Vec<PreviewRequest>) {
        let _ = self.requests.send(batch);
    }

    /// Returns every decoded frame ready since the last call, handing any frames the UI
    /// cannot use (wrong generation) buffers back via [`Self::recycle`].
    pub fn try_frames(&self) -> Vec<PreviewFrame> {
        let mut frames = Vec::new();
        while let Ok(frame) = self.frames.try_recv() {
            frames.push(frame);
        }
        frames
    }

    /// Give a consumed frame's RGB buffer back to the decoder. The UI thread calls this once it
    /// has copied the pixels into a texture.
    pub fn recycle(&self, buffer: Vec<u8>) {
        let _ = self.recycle.send(buffer);
    }
}

impl PreviewFrame {
    fn error(request: &PreviewRequest, message: impl Into<String>) -> Self {
        let (width, height) = request.profile.dimensions();
        Self {
            generation: request.generation,
            clip_id: request.clip_id,
            width: width as usize,
            height: height as usize,
            rgb: Vec::new(),
            qsv_active: false,
            error: Some(message.into()),
        }
    }
}

/// Builds the error text for a failed read, asking the decoder to replay FFmpeg once so the real
/// reason (unsupported codec, missing decoder, ...) reaches the status bar instead of a generic
/// message that hides the actual cause.
fn failure_message(decoder: Option<&mut ContinuousDecoder>) -> String {
    match decoder {
        Some(active) => format!(
            "映像をデコードできません（{}）",
            active.diagnostic_message()
        ),
        None => "映像をデコードできません（FFmpegを起動できませんでした）".to_owned(),
    }
}

struct ContinuousDecoder {
    child: Child,
    stdout: BufReader<ChildStdout>,
    /// The timeline clip these frames belong to; echoed on every [`PreviewFrame`].
    clip_id: ClipId,
    path: PathBuf,
    generation: u64,
    profile: PowerProfile,
    next_source_time: TimelineTime,
    qsv_active: bool,
    is_image: bool,
    /// The one decoded frame of a still image, served for every later request instead of
    /// re-reading the process pipe.
    still: Option<Vec<u8>>,
    /// Cached reason for a failed read (see [`failure_message`]).
    diagnostic: Option<String>,
    width: usize,
    height: usize,
}

impl ContinuousDecoder {
    fn spawn(request: &PreviewRequest, use_qsv: bool) -> std::io::Result<Self> {
        // Belt-and-braces: even if a caller asks for QSV on a still image, software is the only
        // decode path that works (see `PreviewRequest::hardware_decode`).
        let use_qsv = use_qsv && !request.is_image;
        let (width, height) = request.profile.dimensions();
        let fps = request.profile.preview_fps();
        let scale_pad = format!(
            "scale=w={width}:h={height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black"
        );
        let software_filter = format!("{scale_pad},fps={fps}");
        let qsv_filter = format!("hwdownload,format=nv12,{scale_pad},format=rgb24,fps={fps}");
        let mut command = Command::new("nice");
        command.args([
            "-n",
            "5",
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-threads",
        ]);
        command.arg(request.profile.decoder_threads().to_string());
        if use_qsv {
            command.args(["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"]);
        }
        if request.is_image {
            // A still has no timeline: decode exactly one frame (no seek, no loop) and cache it.
            // This is the most compatible path — it avoids the container-specific loop flags that
            // `export` needs, and a container FFmpeg cannot decode is converted beforehand.
            let decodable = super::image::ensure_decodable_image(&request.path);
            command.arg("-i").arg(&decodable);
        } else {
            command
                .args([
                    "-ss",
                    &format!("{:.6}", request.source_time.as_seconds()),
                    "-i",
                ])
                .arg(&request.path);
        }
        command.args(["-an", "-vf"]);
        if request.is_image {
            command.arg(&scale_pad);
        } else if use_qsv {
            command.arg(&qsv_filter);
        } else {
            command.arg(&software_filter);
        }
        if request.is_image {
            command.args(["-frames:v", "1"]);
        } else {
            command.args(["-fps_mode", "cfr"]);
        }
        command
            .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("missing FFmpeg stdout"))?;
        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
            clip_id: request.clip_id,
            path: request.path.clone(),
            generation: request.generation,
            profile: request.profile,
            next_source_time: request.source_time,
            qsv_active: use_qsv,
            is_image: request.is_image,
            still: None,
            diagnostic: None,
            width: width as usize,
            height: height as usize,
        })
    }

    /// Replays FFmpeg once to capture the reason a frame could not be read, so the UI can show the
    /// real error instead of a generic message. The result is cached: a failing decode should not
    /// keep spawning extra FFmpeg processes.
    fn diagnostic_message(&mut self) -> String {
        if let Some(message) = &self.diagnostic {
            return message.clone();
        }
        let mut command = Command::new("ffmpeg");
        command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        if self.is_image {
            let decodable = super::image::ensure_decodable_image(&self.path);
            command.arg("-i").arg(&decodable);
        } else {
            command
                .args([
                    "-ss",
                    &format!("{:.3}", self.next_source_time.as_seconds()),
                    "-i",
                ])
                .arg(&self.path);
        }
        command.args(["-frames:v", "1", "-f", "null", "-"]);
        let message = match command.output() {
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let tail: Vec<&str> = stderr
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect();
                let joined = tail
                    .iter()
                    .rev()
                    .take(3)
                    .rev()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" / ");
                if joined.is_empty() {
                    "FFmpegが映像ストリームを返しませんでした".to_owned()
                } else {
                    joined
                }
            }
            Err(error) => format!("FFmpegを起動できません: {error}"),
        };
        self.diagnostic = Some(message.clone());
        message
    }

    fn can_continue(&self, request: &PreviewRequest) -> bool {
        let same_source = self.path == request.path
            && self.generation == request.generation
            && self.profile == request.profile;
        if !same_source {
            return false;
        }
        // A still image has no time axis: every request shows the same frame, so the decoder must
        // never be torn down and restarted just because the playhead moved.
        if request.is_image {
            return true;
        }
        let drift = request.source_time.as_seconds() - self.next_source_time.as_seconds();
        (-0.10..=1.0).contains(&drift)
    }

    fn read_frame_at(
        &mut self,
        requested_time: TimelineTime,
        pool: &mut Vec<Vec<u8>>,
    ) -> std::io::Result<PreviewFrame> {
        let frame_size = self.width * self.height * 3;
        // Reuse a recycled buffer when possible; `read_exact`/`copy_from_slice` fill it fully.
        let mut rgb = pool.pop().unwrap_or_default();
        rgb.resize(frame_size, 0);
        if self.is_image {
            // The still is decoded once; every later request is served from the cached frame, so a
            // long clip costs one decode no matter how the playhead moves.
            if let Some(still) = &self.still {
                rgb.copy_from_slice(still);
            } else {
                self.stdout.read_exact(&mut rgb)?;
                self.still = Some(rgb.clone());
                // The image path runs FFmpeg with `-frames:v 1`, so it exits after this frame.
                // Reap it immediately instead of leaving a zombie until Drop.
                let _ = self.child.wait();
            }
        } else {
            let frame_seconds = 1.0 / self.profile.preview_fps();
            let frames_to_skip = ((requested_time.as_seconds()
                - self.next_source_time.as_seconds())
                / frame_seconds)
                .floor()
                .clamp(0.0, 8.0) as usize;
            for _ in 0..frames_to_skip {
                self.stdout.read_exact(&mut rgb)?;
                self.next_source_time =
                    self.next_source_time + TimelineTime::from_seconds(frame_seconds);
            }
            self.stdout.read_exact(&mut rgb)?;
            self.next_source_time =
                self.next_source_time + TimelineTime::from_seconds(frame_seconds);
        }
        Ok(PreviewFrame {
            generation: self.generation,
            clip_id: self.clip_id,
            width: self.width,
            height: self.height,
            rgb,
            qsv_active: self.qsv_active,
            error: None,
        })
    }
}

impl Drop for ContinuousDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
