//! Speech-to-text backend.
//!
//! Phase 2 ships one adapter: whisper.cpp's CLI (`whisper-cli` / `whisper-cpp`). The engine
//! only looks for those unambiguous program names — the Python `whisper` package has a
//! different command line — and skips transcription silently when neither is installed.
//! Provider-neutral adapters for cloud ASR arrive in Phase 6.

use super::AnalysisError;
use crate::domain::TimelineTime;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct AsrBackend {
    program: PathBuf,
}

impl AsrBackend {
    /// Finds a whisper.cpp binary on `PATH` without executing it (running another tool's
    /// `--help` can be slow or interactive).
    pub fn detect() -> Option<Self> {
        ["whisper-cli", "whisper-cpp"]
            .into_iter()
            .find_map(find_in_path)
            .map(|program| Self { program })
    }

    pub fn name(&self) -> &str {
        self.program
            .file_name()
            .map(|name| name.to_str().unwrap_or("whisper"))
            .unwrap_or("whisper")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptSegment {
    pub start: TimelineTime,
    pub end: TimelineTime,
    pub text: String,
}

/// Extracts 16 kHz mono PCM audio, which is what whisper.cpp expects.
pub fn extract_audio(source: &Path, output: &Path, threads: u8) -> Result<(), AnalysisError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-threads", &threads.max(1).to_string()])
        .arg("-i")
        .arg(source)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(output)
        .output()?;
    if !result.status.success() || !output.exists() {
        return Err(AnalysisError::Command {
            stage: "音声抽出",
            message: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

/// Runs the backend and parses its JSON transcript. `output_prefix` is passed to `-of`; the
/// backend appends `.json`.
pub fn transcribe(
    backend: &AsrBackend,
    model: &Path,
    audio: &Path,
    output_prefix: &Path,
    threads: u8,
) -> Result<Vec<TranscriptSegment>, AnalysisError> {
    if let Some(parent) = output_prefix.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json_path = output_prefix.with_extension("json");
    let _ = std::fs::remove_file(&json_path);

    let result = Command::new(&backend.program)
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(audio)
        .args(["-oj"])
        .arg("-of")
        .arg(output_prefix)
        .args(["-t", &threads.max(1).to_string()])
        .output()?;
    if !result.status.success() {
        return Err(AnalysisError::Command {
            stage: "文字起こし",
            message: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        });
    }

    let json = std::fs::read_to_string(&json_path)?;
    Ok(parse_whisper_json(&json))
}

/// Parses whisper.cpp's `-oj` output. Offsets are milliseconds:
///
/// ```json
/// {"transcription":[{"offsets":{"from":0,"to":2000},"text":" こんにちは"}]}
/// ```
pub fn parse_whisper_json(text: &str) -> Vec<TranscriptSegment> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let Some(entries) = value.get("transcription").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut segments = Vec::new();
    for entry in entries {
        let Some(body) = entry.get("text").and_then(Value::as_str) else {
            continue;
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let offsets = entry.get("offsets");
        let start_ms = offsets
            .and_then(|value| value.get("from"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let end_ms = offsets
            .and_then(|value| value.get("to"))
            .and_then(Value::as_i64)
            .unwrap_or(start_ms)
            .max(start_ms);
        segments.push(TranscriptSegment {
            start: TimelineTime::from_micros(start_ms.saturating_mul(1_000)),
            end: TimelineTime::from_micros(end_ms.saturating_mul(1_000)),
            text: body.to_owned(),
        });
    }
    segments
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whisper_cpp_json() {
        let json = r#"{
            "systeminfo": "whisper.cpp",
            "transcription": [
                {"offsets": {"from": 0, "to": 2000}, "text": " まもなく列車が"},
                {"offsets": {"from": 2000, "to": 4200}, "text": " 発車します"},
                {"offsets": {"from": 4200, "to": 4300}, "text": "   "}
            ]
        }"#;
        let segments = parse_whisper_json(json);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].text, "まもなく列車が");
        assert_eq!(segments[1].start, TimelineTime::from_seconds(2.0));
        assert_eq!(segments[1].end, TimelineTime::from_seconds(4.2));
    }

    #[test]
    fn invalid_json_yields_no_segments() {
        assert!(parse_whisper_json("not json").is_empty());
        assert!(parse_whisper_json("{}").is_empty());
    }
}
