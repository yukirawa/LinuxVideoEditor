//! The analysis pipeline and its background worker.
//!
//! `run_analysis` is a plain function so it can be unit-tested and so a future CLI or batch
//! mode can reuse it; `AnalysisWorker` wraps it in a thread with cancellation and progress
//! reporting so the UI never blocks.

use super::cache::{self, AnalysisOptions};
use super::transcript::TranscriptSegment;
use super::{frames, ocr, scene, transcript};
use crate::domain::{AssetId, MediaAsset, ProjectId, TimelineTime};
use crate::protocol::{
    VIR_ANALYSIS_VERSION, VIR_SCHEMA_VERSION, VirDocument, VirFrame, VirMedia, VirProvenance,
    VirScene, VirShot, VirTranscriptSegment,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error("外部ツールを起動できませんでした: {0}")]
    Io(#[from] std::io::Error),
    #[error("解析キャッシュを読み書きできませんでした: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("{stage}に失敗しました: {message}")]
    Command {
        stage: &'static str,
        message: String,
    },
    #[error("解析はキャンセルされました")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnalysisStage {
    #[default]
    Metadata,
    Scenes,
    Keyframes,
    Ocr,
    Transcript,
    Saving,
    Cached,
}

impl AnalysisStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Metadata => "メタデータ",
            Self::Scenes => "シーン検出",
            Self::Keyframes => "キーフレーム抽出",
            Self::Ocr => "OCR",
            Self::Transcript => "文字起こし",
            Self::Saving => "保存",
            Self::Cached => "キャッシュ",
        }
    }

    /// Cumulative fraction of the work completed before this stage starts, and the share this
    /// stage contributes. Used to render a single progress bar across all stages.
    fn span(self) -> (f32, f32) {
        match self {
            Self::Metadata => (0.0, 0.05),
            Self::Scenes => (0.05, 0.30),
            Self::Keyframes => (0.35, 0.35),
            Self::Ocr => (0.70, 0.20),
            Self::Transcript => (0.90, 0.08),
            Self::Saving => (0.98, 0.02),
            Self::Cached => (1.0, 0.0),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AnalysisProgress {
    pub stage: AnalysisStage,
    pub completed: usize,
    pub total: usize,
    pub detail: String,
}

impl AnalysisProgress {
    fn new(
        stage: AnalysisStage,
        completed: usize,
        total: usize,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            stage,
            completed,
            total,
            detail: detail.into(),
        }
    }

    pub fn fraction(&self) -> f32 {
        let (base, span) = self.stage.span();
        let ratio = if self.total == 0 {
            0.0
        } else {
            (self.completed as f32 / self.total as f32).clamp(0.0, 1.0)
        };
        (base + span * ratio).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug)]
pub struct AnalysisRequest {
    pub asset: MediaAsset,
    pub project_id: ProjectId,
    pub options: AnalysisOptions,
    pub cache_root: PathBuf,
    /// Skip the cache and analyze from scratch.
    pub force: bool,
}

#[derive(Clone, Debug)]
pub struct AnalysisResult {
    pub vir: VirDocument,
    pub media: VirMedia,
    pub media_key: String,
    /// The media's analysis cache directory: thumbnails, VIR and embeddings live here.
    pub cache_dir: PathBuf,
    /// Signature of the options/pipeline that produced this VIR. Embeddings record it so a
    /// re-analysis invalidates them.
    pub signature: String,
    pub from_cache: bool,
    pub warnings: Vec<String>,
}

pub enum AnalysisEvent {
    Progress(AnalysisProgress),
    /// Boxed because an `AnalysisResult` carries a whole VIR, which is much larger than the
    /// per-stage progress events sharing this channel.
    Finished(Box<Result<AnalysisResult, AnalysisError>>),
}

/// Runs the full pipeline. Optional stages (OCR, transcript) degrade to a warning instead of
/// failing the whole analysis; only FFmpeg being unusable is fatal.
pub fn run_analysis(
    request: &AnalysisRequest,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(AnalysisProgress),
) -> Result<AnalysisResult, AnalysisError> {
    let mut options = request.options.clone();
    options.clamp();
    let key = cache::media_key(&request.asset.path);
    let signature = options.signature();
    let directory = cache::media_dir(&request.cache_root, &key);

    if !request.force
        && let Some(mut vir) = cache::load(&request.cache_root, &key, &signature)
    {
        rebind(&mut vir, request.project_id, request.asset.id);
        report(AnalysisProgress::new(
            AnalysisStage::Cached,
            1,
            1,
            "解析キャッシュを利用",
        ));
        let media = vir
            .media
            .first()
            .cloned()
            .unwrap_or_else(|| VirMedia::empty(request.asset.id));
        return Ok(AnalysisResult {
            vir,
            media,
            media_key: key,
            cache_dir: directory,
            signature,
            from_cache: true,
            warnings: Vec::new(),
        });
    }

    let mut warnings: Vec<String> = Vec::new();
    let asset = &request.asset;
    let duration = asset.metadata.duration;
    let has_video = asset.metadata.has_video;
    // A still image is a visual but has no motion: it becomes a single scene whose thumbnail is
    // the image itself, so cut detection is skipped and only OCR applies.
    let is_image = asset.metadata.is_image;
    let detect_motion = has_video && !is_image;

    // Stage 1: metadata is already known from the import-time probe.
    report(AnalysisProgress::new(
        AnalysisStage::Metadata,
        0,
        1,
        "メディア情報を収集",
    ));
    check_cancel(cancel)?;
    std::fs::create_dir_all(&directory)?;
    // Invalidate the previous entry before touching its artifacts: if this run fails partway,
    // the meta file is gone, so the cache reads as a miss instead of pointing at deleted
    // thumbnails. The new meta is written only after the VIR is stored.
    let _ = std::fs::remove_file(directory.join("meta.json"));
    frames::clear_thumbnails(&directory)?;

    // Stage 2: cut detection and shot/scene grouping.
    report(AnalysisProgress::new(
        AnalysisStage::Scenes,
        0,
        1,
        "カットを検出",
    ));
    let cuts = if detect_motion {
        scene::detect_cuts(
            &asset.path,
            options.cut_threshold,
            options.threads,
            &directory.join("scene_cuts.txt"),
        )?
    } else {
        Vec::new()
    };
    check_cancel(cancel)?;
    let grouping = scene::SceneGrouping {
        scene_threshold: options.scene_threshold,
        min_shot_seconds: options.min_shot_seconds,
        max_scene_seconds: options.max_scene_seconds,
    };
    let spans = scene::group_scenes(&cuts, duration, &grouping);

    // Stage 3: one representative keyframe (thumbnail) per scene.
    let total = spans.len();
    let mut stills: Vec<Option<(String, TimelineTime)>> = Vec::with_capacity(total);
    for (index, span) in spans.iter().enumerate() {
        check_cancel(cancel)?;
        report(AnalysisProgress::new(
            AnalysisStage::Keyframes,
            index,
            total,
            format!("シーン {}/{}", index + 1, total),
        ));
        let still = if has_video {
            // A still image only exists at t=0; seeking to the scene midpoint would find no
            // frame. Real footage is sampled at the midpoint as before.
            let sample_at = if is_image {
                TimelineTime::ZERO
            } else {
                TimelineTime::from_micros((span.start.as_micros() + span.end.as_micros()) / 2)
            };
            let file_name = format!("thumb_{index:04}.jpg");
            match frames::extract_thumbnail(
                &asset.path,
                sample_at,
                options.thumbnail_width,
                &directory.join(&file_name),
                options.threads,
            ) {
                Ok(()) => Some((file_name, sample_at)),
                Err(error) => {
                    warnings.push(error.to_string());
                    None
                }
            }
        } else {
            None
        };
        stills.push(still);
    }
    check_cancel(cancel)?;

    // Stage 4: OCR over the extracted stills (optional).
    let mut ocr_by_scene: Vec<Vec<String>> = vec![Vec::new(); total];
    let mut ocr_model = None;
    if options.ocr
        && has_video
        && let Some(languages) = resolve_ocr_languages(&options)
    {
        report(AnalysisProgress::new(
            AnalysisStage::Ocr,
            0,
            total,
            format!("言語: {languages}"),
        ));
        for (index, still) in stills.iter().enumerate() {
            check_cancel(cancel)?;
            report(AnalysisProgress::new(
                AnalysisStage::Ocr,
                index,
                total,
                format!("シーン {}/{}", index + 1, total),
            ));
            let Some((file_name, _)) = still else {
                continue;
            };
            match ocr::recognize_text(&directory.join(file_name), &languages) {
                Ok(lines) => ocr_by_scene[index] = lines,
                Err(error) => warnings.push(error.to_string()),
            }
        }
        ocr_model = Some(format!("tesseract:{languages}"));
    }
    check_cancel(cancel)?;

    // Stage 5: transcript (optional).
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    let mut asr_model = None;
    if options.transcript && asset.metadata.has_audio {
        report(AnalysisProgress::new(
            AnalysisStage::Transcript,
            0,
            1,
            "音声を抽出",
        ));
        if let Some((model_label, parsed)) =
            prepare_transcript(&options, asset, &directory, &mut warnings)
        {
            segments = parsed;
            asr_model = Some(model_label);
        }
    }
    check_cancel(cancel)?;

    // Stage 6: assemble and persist the VIR.
    report(AnalysisProgress::new(
        AnalysisStage::Saving,
        0,
        1,
        "VIRを書き出し",
    ));
    let media = build_media(
        request,
        &key,
        &spans,
        &stills,
        &ocr_by_scene,
        &segments,
        has_video,
    );
    let provenance = VirProvenance {
        scene_detector: detect_motion.then(|| "ffmpeg:scene".to_owned()),
        vision_model: None,
        asr_model,
        ocr_model,
        embedding_model: None,
    };
    let vir = VirDocument {
        schema_version: VIR_SCHEMA_VERSION.to_owned(),
        analysis_version: VIR_ANALYSIS_VERSION.to_owned(),
        provenance,
        project_id: request.project_id,
        media: vec![media.clone()],
    };
    cache::save(&request.cache_root, &key, &signature, &vir)?;
    report(AnalysisProgress::new(AnalysisStage::Saving, 1, 1, "完了"));

    Ok(AnalysisResult {
        vir,
        media,
        media_key: key,
        cache_dir: directory,
        signature,
        from_cache: false,
        warnings,
    })
}

/// Spawns the analysis on a worker thread with cancellation and progress events.
pub struct AnalysisWorker {
    cancel: Arc<AtomicBool>,
    events: Receiver<AnalysisEvent>,
}

impl AnalysisWorker {
    pub fn spawn(request: AnalysisRequest, repaint: impl Fn() + Send + 'static) -> Self {
        let (sender, events) = channel::<AnalysisEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = Arc::clone(&cancel);
        thread::Builder::new()
            .name("media-analysis".into())
            .spawn(move || {
                let mut report = |progress: AnalysisProgress| {
                    let _ = sender.send(AnalysisEvent::Progress(progress));
                    repaint();
                };
                let outcome = run_analysis(&request, &job_cancel, &mut report);
                let _ = sender.send(AnalysisEvent::Finished(Box::new(outcome)));
                repaint();
            })
            .expect("failed to start analysis worker");
        Self { cancel, events }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn try_event(&self) -> Option<AnalysisEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for AnalysisWorker {
    fn drop(&mut self) {
        // Ask the worker to stop; the thread is intentionally detached so that dropping the
        // handle never freezes the UI on a running FFmpeg child.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), AnalysisError> {
    if cancel.load(Ordering::Relaxed) {
        Err(AnalysisError::Cancelled)
    } else {
        Ok(())
    }
}

fn resolve_ocr_languages(options: &AnalysisOptions) -> Option<String> {
    let configured = options.ocr_languages.trim();
    if !configured.is_empty() {
        return Some(configured.to_owned());
    }
    let available = ocr::available_ocr_languages();
    if available.iter().any(|language| language == "jpn")
        && available.iter().any(|language| language == "eng")
    {
        return Some("jpn+eng".to_owned());
    }
    available.first().cloned()
}

/// Extracts audio and runs the ASR backend. Returns `None` (with a warning) when the backend
/// or model is unavailable, because transcription must never fail the whole analysis.
fn prepare_transcript(
    options: &AnalysisOptions,
    asset: &MediaAsset,
    directory: &Path,
    warnings: &mut Vec<String>,
) -> Option<(String, Vec<TranscriptSegment>)> {
    let model_path = options.asr_model_path.trim();
    if model_path.is_empty() {
        warnings.push("文字起こしにはwhisper.cppのモデルファイル指定が必要です".to_owned());
        return None;
    }
    let model = PathBuf::from(model_path);
    if !model.is_file() {
        warnings.push(format!("ASRモデルが見つかりません: {model_path}"));
        return None;
    }
    let Some(backend) = transcript::AsrBackend::detect() else {
        warnings.push("whisper-cli / whisper-cpp が見つかりません".to_owned());
        return None;
    };

    let audio = directory.join("audio_16k.wav");
    if let Err(error) = transcript::extract_audio(&asset.path, &audio, options.threads) {
        warnings.push(error.to_string());
        return None;
    }
    let outcome = transcript::transcribe(
        &backend,
        &model,
        &audio,
        &directory.join("transcript"),
        options.threads,
    );
    // The extracted audio is a throwaway intermediate; the transcript itself is already in the
    // VIR. Removing it keeps the cache small after transcribing a long recording.
    let _ = std::fs::remove_file(&audio);
    match outcome {
        Ok(segments) => {
            let label = format!(
                "{}:{}",
                backend.name(),
                model
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            );
            Some((label, segments))
        }
        Err(error) => {
            warnings.push(error.to_string());
            None
        }
    }
}

fn build_media(
    request: &AnalysisRequest,
    key: &str,
    spans: &[scene::SceneSpan],
    stills: &[Option<(String, TimelineTime)>],
    ocr_by_scene: &[Vec<String>],
    segments: &[TranscriptSegment],
    has_video: bool,
) -> VirMedia {
    let asset = &request.asset;
    let mut media = VirMedia {
        media_id: asset.id,
        fingerprint: key.to_owned(),
        path: asset.path.to_string_lossy().into_owned(),
        duration: asset.metadata.duration,
        width: asset.metadata.width,
        height: asset.metadata.height,
        fps: asset.metadata.frame_rate,
        has_video,
        has_audio: asset.metadata.has_audio,
        scenes: Vec::with_capacity(spans.len()),
        transcript: segments
            .iter()
            .map(|segment| VirTranscriptSegment {
                start: segment.start,
                end: segment.end,
                text: segment.text.clone(),
            })
            .collect(),
    };

    for (index, span) in spans.iter().enumerate() {
        let shots = span
            .shots
            .iter()
            .enumerate()
            .map(|(shot_index, shot)| VirShot {
                shot_id: super::deterministic_id(&format!("{key}/shot/{index}/{shot_index}")),
                start: shot.start,
                end: shot.end,
                scene_score: shot.score,
                visual_energy: None,
            })
            .collect();

        let scene_ocr = ocr_by_scene.get(index).cloned().unwrap_or_default();
        let text = transcript_for(segments, span.start, span.end);
        let keyframes = stills
            .get(index)
            .and_then(Option::as_ref)
            .map(|(file_name, timestamp)| {
                vec![VirFrame {
                    frame_id: super::deterministic_id(&format!("{key}/frame/{index}/0")),
                    timestamp: *timestamp,
                    thumbnail: Some(file_name.clone()),
                    caption: None,
                    ocr: scene_ocr.clone(),
                    quality_score: None,
                }]
            })
            .unwrap_or_default();

        let has_text = text.is_some() || !scene_ocr.is_empty();
        let summary = summarize(&text, &scene_ocr, span.end - span.start);
        media.scenes.push(VirScene {
            scene_id: super::deterministic_id(&format!("{key}/scene/{index}")),
            start: span.start,
            end: span.end,
            summary,
            transcript: text,
            ocr: scene_ocr,
            objects: Vec::new(),
            events: Vec::new(),
            mood: Vec::new(),
            importance: Some(importance(span.score, span.end - span.start, has_text)),
            shots,
            keyframes,
        });
    }
    media
}

fn transcript_for(
    segments: &[TranscriptSegment],
    start: TimelineTime,
    end: TimelineTime,
) -> Option<String> {
    let parts: Vec<&str> = segments
        .iter()
        .filter(|segment| segment.end > start && segment.start < end)
        .map(|segment| segment.text.as_str())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn summarize(transcript: &Option<String>, ocr: &[String], duration: TimelineTime) -> String {
    if let Some(text) = transcript {
        let shortened = truncate(text, 60);
        if !shortened.is_empty() {
            return shortened;
        }
    }
    if let Some(first) = ocr.first() {
        return truncate(first, 60);
    }
    format!("{:.1}秒のシーン", duration.as_seconds())
}

/// Editorial estimate (VIR spec section 8): a heuristic mix of visual change and duration,
/// later refined by transcript/OCR presence. Deterministic and explicitly not ground truth.
fn importance(score: f32, duration: TimelineTime, has_text: bool) -> f32 {
    let duration_factor = (duration.as_seconds() / 8.0).clamp(0.0, 1.0) as f32;
    let text_factor = if has_text { 0.15 } else { 0.0 };
    (0.05 + score.clamp(0.0, 1.0) * 0.55 + duration_factor * 0.25 + text_factor).clamp(0.0, 1.0)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let shortened: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

fn rebind(document: &mut VirDocument, project_id: ProjectId, media_id: AssetId) {
    document.project_id = project_id;
    for media in &mut document.media {
        media.media_id = media_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_fraction_is_monotonic_per_stage() {
        let early = AnalysisProgress::new(AnalysisStage::Scenes, 0, 0, "");
        let late = AnalysisProgress::new(AnalysisStage::Saving, 1, 1, "");
        assert!(early.fraction() < late.fraction());
        assert_eq!(late.fraction(), 1.0);
    }

    #[test]
    fn transcript_is_sliced_by_scene() {
        let segments = vec![
            TranscriptSegment {
                start: TimelineTime::from_seconds(0.0),
                end: TimelineTime::from_seconds(2.0),
                text: "まもなく".to_owned(),
            },
            TranscriptSegment {
                start: TimelineTime::from_seconds(2.0),
                end: TimelineTime::from_seconds(4.0),
                text: "発車します".to_owned(),
            },
        ];
        let text = transcript_for(
            &segments,
            TimelineTime::ZERO,
            TimelineTime::from_seconds(2.5),
        );
        assert_eq!(text.as_deref(), Some("まもなく 発車します"));
        assert!(
            transcript_for(
                &segments,
                TimelineTime::from_seconds(5.0),
                TimelineTime::from_seconds(6.0)
            )
            .is_none()
        );
    }

    #[test]
    fn importance_stays_in_range() {
        assert!((0.0..=1.0).contains(&importance(0.8, TimelineTime::from_seconds(4.0), true)));
        assert!((0.0..=1.0).contains(&importance(0.0, TimelineTime::ZERO, false)));
    }
}
