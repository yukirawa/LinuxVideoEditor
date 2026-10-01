//! Keyframe extraction — the thumbnail system's producer.
//!
//! One representative still is decoded per scene and stored as a JPEG inside the media's
//! analysis cache directory. The VIR references it by file name; the UI decodes it on demand.

use super::AnalysisError;
use crate::domain::TimelineTime;
use std::path::Path;
use std::process::Command;

/// Extracts a single frame at `at` and writes it to `output` as a JPEG, scaled to `width`
/// (height follows the source aspect ratio). `-ss` before `-i` keeps this a fast seek; the
/// small timing imprecision is irrelevant for a representative thumbnail.
pub fn extract_thumbnail(
    source: &Path,
    at: TimelineTime,
    width: u32,
    output: &Path,
    threads: u8,
) -> Result<(), AnalysisError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let filter = format!("scale={}:-2", width.max(16));
    let result = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-threads", &threads.max(1).to_string()])
        .args(["-ss", &format!("{:.3}", at.as_seconds())])
        .arg("-i")
        .arg(source)
        .args(["-frames:v", "1", "-vf"])
        .arg(&filter)
        .args(["-q:v", "3"])
        .arg(output)
        .output()?;
    if !result.status.success() || !output.exists() {
        return Err(AnalysisError::Command {
            stage: "キーフレーム抽出",
            message: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

/// Removes previously generated thumbnails so a re-analysis cannot leave orphans behind.
/// Only files the engine itself writes (`thumb_*.jpg`) are touched.
pub fn clear_thumbnails(directory: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("thumb_") && name.to_string_lossy().ends_with(".jpg")
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn clear_thumbnails_removes_only_generated_files() {
        let directory = std::env::temp_dir().join(format!("yuki-thumb-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("thumb_0000_00.jpg"), b"x").unwrap();
        fs::write(directory.join("vir.json"), b"{}").unwrap();

        clear_thumbnails(&directory).unwrap();

        assert!(!directory.join("thumb_0000_00.jpg").exists());
        assert!(directory.join("vir.json").exists());
        let _ = fs::remove_dir_all(&directory);
    }
}
