use crate::domain::{ClipId, TimelineTime};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Hard cap on concurrent preview players. Each audible clip is one ffplay process; the
/// cap keeps a dense mix from turning into a process storm.
pub const MAX_CONCURRENT_PLAYERS: usize = 8;

/// Lightweight audio-only preview backed by one FFplay child per audible clip.
/// Concurrent children mix at the audio device, matching export's `amix` behaviour.
pub struct AudioPreview {
    players: HashMap<ClipId, Child>,
}

/// FFmpeg `atempo` chain covering arbitrary playback speeds (each stage is limited to 0.5..=2.0).
/// Shared by preview and export so both produce the same pitch-preserving stretch.
pub(crate) fn atempo_chain_filter(speed: f64) -> String {
    let mut parts = Vec::new();
    let mut rate = speed.clamp(0.1, 8.0);
    while rate > 2.0 {
        parts.push("atempo=2.0".to_owned());
        rate /= 2.0;
    }
    while rate < 0.5 {
        parts.push("atempo=0.5".to_owned());
        rate /= 0.5;
    }
    if (rate - 1.0).abs() > 1e-4 {
        parts.push(format!("atempo={rate:.6}"));
    }
    parts.join(",")
}

impl AudioPreview {
    pub fn new() -> Self {
        Self {
            players: HashMap::new(),
        }
    }

    /// Starts (or keeps) playback of one clip. A clip that is already playing is never
    /// restarted: re-spawning ffplay would snap the audio back to the clip start every frame.
    pub fn play(
        &mut self,
        clip_id: ClipId,
        path: &Path,
        source_time: TimelineTime,
        remaining: TimelineTime,
        gain_db: f32,
        speed: f64,
    ) -> std::io::Result<()> {
        self.reap_finished();
        if self.players.contains_key(&clip_id) {
            return Ok(());
        }
        if self.players.len() >= MAX_CONCURRENT_PLAYERS {
            return Ok(());
        }
        let gain = 10_f32.powf(gain_db / 20.0);
        let tempo = atempo_chain_filter(speed);
        let mut af = format!("volume={gain}");
        if !tempo.is_empty() {
            af.push(',');
            af.push_str(&tempo);
        }
        let child = Command::new("nice")
            .args([
                "-n",
                "10",
                "ffplay",
                "-nodisp",
                "-autoexit",
                "-loglevel",
                "error",
                "-vn",
                "-ss",
            ])
            .arg(format!("{:.6}", source_time.as_seconds()))
            .args(["-t", &format!("{:.6}", remaining.as_seconds()), "-af"])
            .arg(af)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        self.players.insert(clip_id, child);
        Ok(())
    }

    /// Whether `clip_id` currently has a live player. Also reaps clips that finished.
    pub fn is_playing_clip(&mut self, clip_id: ClipId) -> bool {
        self.reap_finished();
        self.players.contains_key(&clip_id)
    }

    /// Ids of all live players (finished ones reaped first).
    pub fn playing_clips(&mut self) -> Vec<ClipId> {
        self.reap_finished();
        self.players.keys().copied().collect()
    }

    pub fn stop_clip(&mut self, clip_id: ClipId) {
        if let Some(mut child) = self.players.remove(&clip_id) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Stops every player. Called on pause/seek so playback halt silences the whole mix.
    pub fn stop(&mut self) {
        for (_, mut child) in self.players.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn reap_finished(&mut self) {
        self.players.retain(|_, child| match child.try_wait() {
            Ok(Some(_)) => false,
            _ => true,
        });
    }
}

impl Default for AudioPreview {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioPreview {
    fn drop(&mut self) {
        self.stop();
    }
}
