use super::probe::MediaError;
use crate::domain::{MediaAsset, VoiceSettings};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;
use uuid::Uuid;

/// VOICEVOX is not bundled with the editor: the user points the app at a running engine
/// (the standard HTTP API, defaulting to `http://127.0.0.1:50021`) and this module performs
/// the two-step `audio_query` → `synthesis` request using the system `curl` binary, matching
/// the project's preference for shelling out to well-known system tools (FFmpeg, fc-match,
/// ...) instead of pulling in a network client dependency.
#[derive(Debug, Error)]
pub enum VoicevoxError {
    #[error("VOICEVOXのURLが設定されていません")]
    EmptyUrl,
    #[error("curlを起動できませんでした: {0}")]
    Start(#[from] std::io::Error),
    #[error("audio_queryに失敗しました: {0}")]
    AudioQuery(String),
    #[error("synthesisに失敗しました: {0}")]
    Synthesis(String),
    #[error("生成された音声を読み込めませんでした: {0}")]
    Probe(#[from] MediaError),
}

/// Stable, per-user cache directory for generated voice lines. These files are derived
/// data, so losing them only requires regenerating the line (the project references them
/// by path just like any other imported media).
pub fn voicevox_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("yuki-video-editor").join("voicevox")
}

/// Synthesizes `text` with the given speaker and voice tuning and returns a `MediaAsset`
/// pointing at the generated WAV (saved under `output_dir`).
pub fn synthesize(
    engine_url: &str,
    speaker_id: u32,
    voice: &VoiceSettings,
    text: &str,
    output_dir: &Path,
) -> Result<MediaAsset, VoicevoxError> {
    let base = engine_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(VoicevoxError::EmptyUrl);
    }
    std::fs::create_dir_all(output_dir)?;

    let query_url = format!(
        "{base}/audio_query?text={}&speaker={speaker_id}",
        url_encode(text)
    );
    // Timeouts matter twice over: a hung engine must not pin this worker thread forever, and a
    // blocked agent speech call keeps the whole chat marked "waiting".
    let query = Command::new("curl")
        .args([
            "-sS",
            "--fail",
            "--connect-timeout",
            "5",
            "--max-time",
            "120",
            "-X",
            "POST",
            &query_url,
        ])
        .output()?;
    if !query.status.success() {
        return Err(VoicevoxError::AudioQuery(command_error(&query)));
    }
    let query_json = String::from_utf8_lossy(&query.stdout).into_owned();
    // The character's voice tuning is applied to the query the engine just produced, which is
    // where VOICEVOX expects per-line adjustments (they are not query-string parameters).
    let query_json = apply_voice_settings(&query_json, voice);

    let wav_path = output_dir.join(format!("voicevox-{}.wav", Uuid::new_v4()));
    let synthesis_url = format!("{base}/synthesis?speaker={speaker_id}");
    let synthesis = Command::new("curl")
        .args([
            "-sS",
            "--fail",
            "--connect-timeout",
            "5",
            "--max-time",
            "120",
            "-X",
            "POST",
            &synthesis_url,
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &query_json,
            "--output",
        ])
        .arg(&wav_path)
        .output()?;
    if !synthesis.status.success() {
        let _ = std::fs::remove_file(&wav_path);
        return Err(VoicevoxError::Synthesis(command_error(&synthesis)));
    }

    let mut asset = super::probe_media(&wav_path)?;
    asset.name = short_label(text);
    Ok(asset)
}

/// Overrides the adjustable fields of a VOICEVOX `audio_query` response with `voice`.
/// Best-effort: if the engine returns something that is not a JSON object (a version difference,
/// a proxy error body, ...), the original body is passed through unchanged so synthesis can still
/// succeed rather than failing on our own rewrite.
fn apply_voice_settings(query_json: &str, voice: &VoiceSettings) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(query_json) else {
        return query_json.to_owned();
    };
    let Some(object) = value.as_object_mut() else {
        return query_json.to_owned();
    };
    object.insert("speedScale".to_owned(), voice.speed.into());
    object.insert("pitchScale".to_owned(), voice.pitch.into());
    object.insert("intonationScale".to_owned(), voice.intonation.into());
    object.insert("volumeScale".to_owned(), voice.volume.into());
    object.insert("prePhonemeLength".to_owned(), voice.pre_silence.into());
    object.insert("postPhonemeLength".to_owned(), voice.post_silence.into());
    value.to_string()
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn short_label(text: &str) -> String {
    let mut chars = text.chars();
    let label: String = chars.by_ref().take(30).collect();
    if chars.next().is_some() {
        format!("{label}…")
    } else {
        label
    }
}

/// Percent-encodes a string for use as a URL query value. Only RFC 3986 unreserved
/// characters are kept verbatim; space is encoded as `%20` (not `+`) to be unambiguous
/// across the various servers people might point the editor at.
fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char);
            }
            other => {
                encoded.push('%');
                encoded.push_str(&format!("{other:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{apply_voice_settings, url_encode};
    use crate::domain::VoiceSettings;

    #[test]
    fn encodes_only_reserved_characters() {
        assert_eq!(url_encode("abcABC123-._~"), "abcABC123-._~");
        assert_eq!(
            url_encode("こんにちは"),
            "%E3%81%93%E3%82%93%E3%81%AB%E3%81%A1%E3%81%AF"
        );
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a+b&c=d"), "a%2Bb%26c%3Dd");
    }

    #[test]
    fn voice_settings_override_the_audio_query_fields() {
        let voice = VoiceSettings {
            speed: 1.5,
            pitch: -0.2,
            intonation: 0.8,
            volume: 2.0,
            pre_silence: 0.3,
            post_silence: 0.4,
        };
        let rewritten = apply_voice_settings(
            r#"{"speedScale":1.0,"accent_phrases":[],"outputStereo":false}"#,
            &voice,
        );
        let parsed: serde_json::Value = serde_json::from_str(&rewritten).unwrap();
        let number = |key: &str| parsed[key].as_f64().unwrap();
        // f32 is widened to f64 on serialization, so compare with a tolerance.
        assert!((number("speedScale") - 1.5).abs() < 1e-6);
        assert!((number("pitchScale") - -0.2).abs() < 1e-6);
        assert!((number("intonationScale") - 0.8).abs() < 1e-6);
        assert!((number("volumeScale") - 2.0).abs() < 1e-6);
        assert!((number("prePhonemeLength") - 0.3).abs() < 1e-6);
        assert!((number("postPhonemeLength") - 0.4).abs() < 1e-6);
        // Unrelated fields the engine produced are preserved.
        assert_eq!(parsed["outputStereo"], false);
    }

    #[test]
    fn non_object_audio_query_is_passed_through() {
        let voice = VoiceSettings::default();
        assert_eq!(apply_voice_settings("not json", &voice), "not json");
        assert_eq!(apply_voice_settings("[1,2,3]", &voice), "[1,2,3]");
    }
}
