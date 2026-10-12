mod audio;
mod export;
mod fonts;
mod image;
mod preview;
mod probe;
mod voicevox;

pub use audio::AudioPreview;
pub use export::{
    AudioCodec, EncoderPreset, ExportError, ExportOptions, VideoEncoder, export_frame_png,
    export_project_with_options,
};
pub use fonts::{
    fallback_font_families, list_font_families, preview_font_key, resolve_font_file,
};
pub use image::{
    IMAGE_DEFAULT_DURATION_SECONDS, IMAGE_EXTENSIONS, ensure_decodable_image,
    image_loop_input_args, is_container_image_path, is_image_path,
};
pub use preview::{PowerProfile, PreviewFrame, PreviewRequest, PreviewWorker};
pub use probe::{MediaError, probe_media};
pub use voicevox::{VoicevoxError, synthesize, voicevox_cache_dir};

use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct HardwareCapabilities {
    pub qsv_available: bool,
    pub qsv_h264_encode: bool,
    pub vaapi_h264_encode: bool,
    pub nvenc_h264_encode: bool,
    pub vulkan_available: bool,
    /// Every video encoder name advertised by `ffmpeg -encoders`, used to gray out
    /// codecs the local FFmpeg build cannot provide.
    #[serde(default)]
    pub video_encoders: Vec<String>,
}

impl HardwareCapabilities {
    pub fn supports_video_encoder(&self, name: &str) -> bool {
        self.video_encoders.iter().any(|encoder| encoder == name)
    }
}

pub fn detect_hardware_capabilities() -> HardwareCapabilities {
    let encoders = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let hwaccels = Command::new("ffmpeg")
        .args(["-hide_banner", "-hwaccels"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();

    let video_encoders = encoders
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let flags = columns.next()?;
            // The first flag column is `V` for video, `A` for audio, `S` for subtitles.
            if !flags.starts_with('V') {
                return None;
            }
            columns
                .next()
                .filter(|name| {
                    name.chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
                })
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();

    // Only probe QSV when the build actually advertises a QSV encoder; if it does, run the
    // real device-init probe so machines without working QSV hardware still report unavailable.
    let qsv_listed = video_encoders.iter().any(|name| name.ends_with("_qsv"));
    let qsv_available = qsv_listed
        && Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-init_hw_device",
                "qsv=hw",
                "-filter_hw_device",
                "hw",
                "-f",
                "lavfi",
                "-i",
                "color=size=64x64:duration=0.01",
                "-frames:v",
                "1",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());

    HardwareCapabilities {
        qsv_available,
        qsv_h264_encode: qsv_available && encoders.contains("h264_qsv"),
        vaapi_h264_encode: encoders.contains("h264_vaapi"),
        nvenc_h264_encode: encoders.contains("h264_nvenc"),
        vulkan_available: hwaccels.contains("vulkan"),
        video_encoders,
    }
}
