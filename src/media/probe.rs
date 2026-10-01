use crate::domain::{MediaAsset, MediaMetadata, TimelineTime};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use thiserror::Error;
use uuid::Uuid;

use super::image::{IMAGE_DEFAULT_DURATION_SECONDS, is_image_path};

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("ffprobe could not be started: {0}")]
    Start(#[from] std::io::Error),
    #[error("ffprobe failed: {0}")]
    ProbeFailed(String),
    #[error("ffprobe returned invalid JSON: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    #[error(
        "media duration is missing or invalid (for a still image, the format must be one this build supports: PNG/JPEG/WebP/BMP/TIFF/AVIF/HEIC/HEIF/JXL)"
    )]
    InvalidDuration,
    #[error("the selected file has neither a video nor an audio stream")]
    NoStreams,
}

#[derive(Deserialize)]
struct ProbeOutput {
    format: ProbeFormat,
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    duration: Option<String>,
}

pub fn probe_media(path: &Path) -> Result<MediaAsset, MediaError> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(path)
        .output()?;

    if !output.status.success() {
        return Err(MediaError::ProbeFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    let probe: ProbeOutput = serde_json::from_slice(&output.stdout)?;
    // A still image must be recognized before the duration is parsed: FFmpeg reports it as a
    // single video frame with a near-zero duration, which would otherwise import as a blip.
    let is_image = is_image_path(path);
    let video = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"));
    let audio = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("audio"));
    if video.is_none() && audio.is_none() {
        return Err(MediaError::NoStreams);
    }
    let duration = if is_image {
        TimelineTime::from_seconds(IMAGE_DEFAULT_DURATION_SECONDS)
    } else {
        let seconds = parse_number(probe.format.duration.as_deref())
            .or_else(|| video.and_then(|stream| parse_number(stream.duration.as_deref())))
            .or_else(|| audio.and_then(|stream| parse_number(stream.duration.as_deref())))
            .filter(|duration| duration.is_finite() && *duration > 0.0)
            .ok_or(MediaError::InvalidDuration)?;
        TimelineTime::from_seconds(seconds)
    };

    Ok(MediaAsset {
        id: Uuid::new_v4(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Media".to_owned()),
        path: path.to_path_buf(),
        metadata: MediaMetadata {
            duration,
            width: video.and_then(|stream| stream.width).unwrap_or(0),
            height: video.and_then(|stream| stream.height).unwrap_or(0),
            frame_rate: if is_image {
                0.0
            } else {
                video
                    .and_then(|stream| parse_rational(stream.avg_frame_rate.as_deref()))
                    .unwrap_or(0.0)
            },
            has_video: video.is_some(),
            has_audio: audio.is_some() && !is_image,
            video_codec: video.and_then(|stream| stream.codec_name.clone()),
            audio_codec: if is_image {
                None
            } else {
                audio.and_then(|stream| stream.codec_name.clone())
            },
            is_image,
        },
    })
}

fn parse_number(value: Option<&str>) -> Option<f64> {
    value?.parse().ok()
}

fn parse_rational(value: Option<&str>) -> Option<f64> {
    let (numerator, denominator) = value?.split_once('/')?;
    let denominator: f64 = denominator.parse().ok()?;
    if denominator == 0.0 {
        return None;
    }
    Some(numerator.parse::<f64>().ok()? / denominator)
}
