use super::HardwareCapabilities;
use super::audio::atempo_chain_filter;
use super::fonts::resolve_font_file_ref;
use super::image::{ensure_decodable_image, image_loop_input_args};
use crate::domain::{
    AssetId, Clip, Layer, MediaAsset, Project, SequenceSettings, TextAlign, TextItem, TimelineItem,
    TimelineTime,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Video encoders the editor can request. Software H.264/H.265 are the compatibility
/// baseline; AV1 and VP9 offer the best efficiency-per-bit, and the QSV/NVENC variants
/// offload to dedicated hardware. Selection is validated against `ffmpeg -encoders`,
/// and any failure falls back to libx264 so an export never just stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEncoder {
    Libx264,
    Libx265,
    Av1Svt,
    Av1Aom,
    Vp9,
    H264Qsv,
    HevcQsv,
    Av1Qsv,
    H264Nvenc,
    HevcNvenc,
    Av1Nvenc,
}

impl VideoEncoder {
    pub const ALL: [VideoEncoder; 11] = [
        VideoEncoder::Libx264,
        VideoEncoder::Libx265,
        VideoEncoder::Av1Svt,
        VideoEncoder::Av1Aom,
        VideoEncoder::Vp9,
        VideoEncoder::H264Qsv,
        VideoEncoder::HevcQsv,
        VideoEncoder::Av1Qsv,
        VideoEncoder::H264Nvenc,
        VideoEncoder::HevcNvenc,
        VideoEncoder::Av1Nvenc,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Libx264 => "libx264 (H.264)",
            Self::Libx265 => "libx265 (H.265/HEVC)",
            Self::Av1Svt => "libsvtav1 (AV1・高速)",
            Self::Av1Aom => "libaom-av1 (AV1・高圧縮)",
            Self::Vp9 => "libvpx-vp9 (VP9)",
            Self::H264Qsv => "h264_qsv (Intel QSV)",
            Self::HevcQsv => "hevc_qsv (Intel QSV)",
            Self::Av1Qsv => "av1_qsv (Intel QSV)",
            Self::H264Nvenc => "h264_nvenc (NVIDIA)",
            Self::HevcNvenc => "hevc_nvenc (NVIDIA)",
            Self::Av1Nvenc => "av1_nvenc (NVIDIA)",
        }
    }

    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Libx264 => "libx264",
            Self::Libx265 => "libx265",
            Self::Av1Svt => "libsvtav1",
            Self::Av1Aom => "libaom-av1",
            Self::Vp9 => "libvpx-vp9",
            Self::H264Qsv => "h264_qsv",
            Self::HevcQsv => "hevc_qsv",
            Self::Av1Qsv => "av1_qsv",
            Self::H264Nvenc => "h264_nvenc",
            Self::HevcNvenc => "hevc_nvenc",
            Self::Av1Nvenc => "av1_nvenc",
        }
    }

    pub const fn is_qsv(self) -> bool {
        matches!(self, Self::H264Qsv | Self::HevcQsv | Self::Av1Qsv)
    }

    pub const fn is_hardware(self) -> bool {
        self.is_qsv() || matches!(self, Self::H264Nvenc | Self::HevcNvenc | Self::Av1Nvenc)
    }

    /// Inclusive CRF/quality range accepted by the encoder (AV1 and VP9 use a wider scale).
    pub const fn quality_range(self) -> (u8, u8) {
        match self {
            Self::Av1Svt | Self::Av1Aom | Self::Vp9 => (0, 63),
            _ => (0, 51),
        }
    }

    /// Whether `ffmpeg` on this machine advertises the encoder (and, for QSV, can init it).
    pub fn is_available(self, hardware: &HardwareCapabilities) -> bool {
        if self.is_qsv() && !hardware.qsv_available {
            return false;
        }
        hardware.supports_video_encoder(self.ffmpeg_name())
    }

    /// Codec-level output flags: chroma format, MP4 sample-entry tag, and rate-control helpers
    /// (constant-quality AV1/VP9 needs `-b:v 0`).
    const fn extra_args(self) -> &'static [&'static str] {
        match self {
            Self::Libx265 | Self::HevcQsv | Self::HevcNvenc => {
                &["-pix_fmt", "yuv420p", "-tag:v", "hvc1"]
            }
            Self::Av1Svt | Self::Av1Qsv | Self::Av1Nvenc => {
                &["-pix_fmt", "yuv420p", "-tag:v", "av01"]
            }
            Self::Av1Aom => &[
                "-pix_fmt", "yuv420p", "-tag:v", "av01", "-b:v", "0", "-row-mt", "1",
            ],
            Self::Vp9 => &[
                "-pix_fmt",
                "yuv420p",
                "-tag:v",
                "vp09",
                "-b:v",
                "0",
                "-deadline",
                "good",
            ],
            Self::Libx264 | Self::H264Qsv | Self::H264Nvenc => &["-pix_fmt", "yuv420p"],
        }
    }

    /// Constant-quality argument pair. QSV uses `-global_quality`, NVENC uses `-cq`, and the
    /// software encoders use CRF.
    fn quality_args(self, crf: u8) -> [String; 2] {
        let flag = match self {
            Self::H264Qsv | Self::HevcQsv | Self::Av1Qsv => "-global_quality",
            Self::H264Nvenc | Self::HevcNvenc | Self::Av1Nvenc => "-cq",
            _ => "-crf",
        };
        [flag.to_owned(), crf.to_string()]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderPreset {
    Ultrafast,
    Superfast,
    Veryfast,
    Faster,
    Fast,
    Medium,
    Slow,
}

impl EncoderPreset {
    pub const ALL: [EncoderPreset; 7] = [
        EncoderPreset::Ultrafast,
        EncoderPreset::Superfast,
        EncoderPreset::Veryfast,
        EncoderPreset::Faster,
        EncoderPreset::Fast,
        EncoderPreset::Medium,
        EncoderPreset::Slow,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ultrafast => "最速 (ultrafast)",
            Self::Superfast => "超高速 (superfast)",
            Self::Veryfast => "高速 (veryfast)",
            Self::Faster => "やや高速 (faster)",
            Self::Fast => "普通 (fast)",
            Self::Medium => "標準 (medium)",
            Self::Slow => "高圧縮 (slow)",
        }
    }

    const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
            Self::Faster => "faster",
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
        }
    }

    /// Encoder-specific preset arguments. Hardware encoders do not accept the x264 names, SVT-AV1
    /// uses numeric presets, and the libaom/libvpx software encoders use `-cpu-used`.
    fn preset_args(self, encoder: VideoEncoder) -> (&'static str, &'static str) {
        match encoder {
            VideoEncoder::H264Nvenc | VideoEncoder::HevcNvenc | VideoEncoder::Av1Nvenc => (
                "-preset",
                match self {
                    Self::Ultrafast => "p1",
                    Self::Superfast => "p2",
                    Self::Veryfast => "p3",
                    Self::Faster => "p4",
                    Self::Fast => "p5",
                    Self::Medium => "p6",
                    Self::Slow => "p7",
                },
            ),
            VideoEncoder::Av1Svt => (
                "-preset",
                match self {
                    Self::Ultrafast => "12",
                    Self::Superfast => "10",
                    Self::Veryfast => "9",
                    Self::Faster => "8",
                    Self::Fast => "7",
                    Self::Medium => "6",
                    Self::Slow => "4",
                },
            ),
            VideoEncoder::Av1Aom => (
                "-cpu-used",
                match self {
                    Self::Ultrafast => "8",
                    Self::Superfast => "7",
                    Self::Veryfast => "6",
                    Self::Faster => "5",
                    Self::Fast => "4",
                    Self::Medium => "3",
                    Self::Slow => "2",
                },
            ),
            VideoEncoder::Vp9 => (
                "-cpu-used",
                match self {
                    Self::Ultrafast => "8",
                    Self::Superfast => "7",
                    Self::Veryfast => "6",
                    Self::Faster => "5",
                    Self::Fast => "4",
                    Self::Medium => "2",
                    Self::Slow => "1",
                },
            ),
            VideoEncoder::H264Qsv | VideoEncoder::HevcQsv | VideoEncoder::Av1Qsv => (
                "-preset",
                match self {
                    Self::Ultrafast | Self::Superfast | Self::Veryfast => "veryfast",
                    Self::Faster | Self::Fast => "fast",
                    Self::Medium => "medium",
                    Self::Slow => "slow",
                },
            ),
            VideoEncoder::Libx264 | VideoEncoder::Libx265 => ("-preset", self.ffmpeg_name()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Mp3,
    Opus,
    Flac,
    None,
}

impl AudioCodec {
    pub const ALL: [AudioCodec; 5] = [
        AudioCodec::Aac,
        AudioCodec::Mp3,
        AudioCodec::Opus,
        AudioCodec::Flac,
        AudioCodec::None,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Opus => "Opus",
            Self::Flac => "FLAC",
            Self::None => "音声なし",
        }
    }

    const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "libmp3lame",
            Self::Opus => "libopus",
            Self::Flac => "flac",
            Self::None => "",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExportOptions {
    pub threads: u8,
    pub use_qsv: bool,
    pub video_encoder: VideoEncoder,
    pub crf: u8,
    pub preset: EncoderPreset,
    pub audio_codec: AudioCodec,
    pub audio_bitrate_kbps: u32,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            threads: 2,
            use_qsv: false,
            video_encoder: VideoEncoder::Libx264,
            crf: 20,
            preset: EncoderPreset::Veryfast,
            audio_codec: AudioCodec::Aac,
            audio_bitrate_kbps: 192,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("timeline is empty")]
    EmptyTimeline,
    #[error("could not start FFmpeg: {0}")]
    Start(#[from] std::io::Error),
    #[error("FFmpeg export failed: {0}")]
    Ffmpeg(String),
    #[error("Japanese-capable system font was not found")]
    MissingFont,
}

pub fn export_project_with_options(
    project: &Project,
    destination: &Path,
    options: ExportOptions,
) -> Result<(), ExportError> {
    match render(project, destination, options) {
        // Any encoder that is unavailable or unsupported at runtime falls back to the
        // universally available software H.264 path, resetting codec-specific options.
        Err(ExportError::Ffmpeg(_)) if options.video_encoder != VideoEncoder::Libx264 => {
            let (_, max_quality) = VideoEncoder::Libx264.quality_range();
            render(
                project,
                destination,
                ExportOptions {
                    use_qsv: false,
                    video_encoder: VideoEncoder::Libx264,
                    preset: EncoderPreset::Veryfast,
                    crf: options.crf.clamp(0, max_quality),
                    ..options
                },
            )
        }
        result => result,
    }
}

/// Appends the `drawtext` filter for one text overlay (a plain text item or a speech subtitle)
/// and returns the new filter-graph label. Shared so both item kinds render identically.
fn push_text_filters(
    filters: &mut Vec<String>,
    previous_video: &str,
    stage: usize,
    font_cache: &mut HashMap<(Option<String>, bool, bool), Option<String>>,
    text: &TextItem,
) -> Result<String, ExportError> {
    let key = (text.font_family.clone(), text.bold, text.italic);
    let font_path = font_cache
        .entry(key)
        .or_insert_with(|| {
            resolve_font_file_ref(text.font_family.as_deref(), text.bold, text.italic)
                .map(|path| escape_filter_value(&path.to_string_lossy()))
        })
        .clone();
    let Some(font_path) = font_path else {
        return Err(ExportError::MissingFont);
    };

    let start = text.timeline_start.as_seconds();
    let end = text.timeline_end().as_seconds();
    let color = rgba_to_ffmpeg_color(text.color_rgba);
    let (anchor_x, anchor_y) = text.transform.anchor.fraction();
    let x_expr = format!("{}-text_w*{anchor_x}", text.transform.x);
    let y_expr = format!("{}-text_h*{anchor_y}", text.transform.y);
    let align = match text.align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
    };

    let mut options_string = format!(
        "fontfile='{font_path}':text='{}':fontsize={}:fontcolor={color}:x='{x_expr}':y='{y_expr}':text_align={align}:line_spacing={}",
        escape_filter_value(&text.text),
        (text.font_size * text.transform.scale / 100.0)
            .round()
            .max(1.0) as i32,
        text.line_spacing.round() as i32,
    );
    if text.outline_enabled && text.outline_width > 0.0 {
        options_string.push_str(&format!(
            ":borderw={}:bordercolor={}",
            text.outline_width.round().max(1.0) as i32,
            rgba_to_ffmpeg_color(text.outline_color_rgba)
        ));
    }
    if text.shadow_enabled {
        options_string.push_str(&format!(
            ":shadowx={}:shadowy={}:shadowcolor={}",
            text.shadow_offset_x.round() as i32,
            text.shadow_offset_y.round() as i32,
            rgba_to_ffmpeg_color(text.shadow_color_rgba)
        ));
    }
    if text.background_enabled {
        options_string.push_str(&format!(
            ":box=1:boxcolor={}:boxborderw={}",
            rgba_to_ffmpeg_color(text.background_color_rgba),
            text.background_padding.round().max(0.0) as i32
        ));
    }
    if text.fade_in > TimelineTime::ZERO || text.fade_out > TimelineTime::ZERO {
        let fade_in = text.fade_in.as_seconds().max(0.001);
        let fade_out = text.fade_out.as_seconds().max(0.001);
        let alpha_expr = format!(
            "if(lt(t,{start}+{fade_in}),(t-{start})/{fade_in},if(gt(t,{end}-{fade_out}),({end}-t)/{fade_out},1))"
        );
        options_string.push_str(&format!(":alpha='{alpha_expr}'"));
    }
    options_string.push_str(&format!(":enable='between(t,{start},{end})'"));

    let next = format!("text{stage}");
    filters.push(format!(
        "[{previous_video}]drawtext={options_string}[{next}]"
    ));
    Ok(next)
}

/// Piecewise-linear expression of output time `t` over absolute-timeline keyframes
/// `(time_seconds, value)`. Falls back to `default` outside the key span.
fn piecewise_expr(keys: &[(f64, f32)], default: f32) -> String {
    if keys.is_empty() {
        return format!("{default:.6}");
    }
    if keys.len() == 1 {
        return format!("{:.6}", keys[0].1);
    }
    // Work on a sorted copy so callers do not have to.
    let mut sorted: Vec<(f64, f32)> = keys.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let first = sorted[0];
    let last = *sorted.last().expect("non-empty keys");
    let mut expr = format!("{:.6}", last.1);
    // Nested if() built from the right so the first matching interval wins.
    for pair in sorted.windows(2).rev() {
        let (t0, v0) = (pair[0].0, pair[0].1);
        let (t1, v1) = (pair[1].0, pair[1].1);
        let slope = if (t1 - t0).abs() < 1e-9 {
            0.0
        } else {
            (v1 - v0) as f64 / (t1 - t0)
        };
        let linear = format!("{:.6}+(t-{:.6})*{:.6}", v0, t0, slope);
        expr = format!("if(lt(t,{t1:.6}),{linear},{expr})");
    }
    // Before the first key, hold the first value.
    expr = format!("if(lt(t,{:.6}),{:.6},{expr})", first.0, first.1);
    expr
}

/// Absolute-timeline keyframe samples `(time_seconds, value)` for one clip property.
fn anim_samples(clip: &Clip, property: crate::domain::AnimProperty) -> Vec<(f64, f32)> {
    let Some(track) = clip.track(property) else {
        return Vec::new();
    };
    track
        .keys
        .iter()
        .map(|key| {
            (
                clip.timeline_start.as_seconds() + key.offset.as_seconds(),
                key.value,
            )
        })
        .collect()
}

/// True when the property has keyframes (even one, which overrides the static field).
fn is_animated(clip: &Clip, property: crate::domain::AnimProperty) -> bool {
    clip.track(property)
        .map(|track| !track.keys.is_empty())
        .unwrap_or(false)
}

/// Builds the per-clip video chain: trim → speed → scale → rotate → effects → alpha.
fn push_clip_video_filters(
    filters: &mut Vec<String>,
    previous_video: &str,
    clip: &Clip,
    index: usize,
    stage: usize,
    settings: &SequenceSettings,
) -> String {
    let source_in = clip.source_in.as_seconds();
    let source_out = clip.source_out.as_seconds();
    let start = clip.timeline_start.as_seconds();
    let speed = clip.speed();
    let video = format!("v{stage}");

    let transform = clip.transform;
    let scale_pct = transform.scale;
    let rotation_deg = transform.rotation_degrees;

    let animated_scale = is_animated(clip, crate::domain::AnimProperty::Scale);
    let animated_rotation = is_animated(clip, crate::domain::AnimProperty::Rotation);
    let animated_x = is_animated(clip, crate::domain::AnimProperty::X);
    let animated_y = is_animated(clip, crate::domain::AnimProperty::Y);
    let animated_opacity = is_animated(clip, crate::domain::AnimProperty::Opacity);

    let base_w = settings.width as f32;
    let base_h = settings.height as f32;

    let (scale_w_expr, scale_h_expr) = if animated_scale {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Scale);
        let expr = piecewise_expr(&keys, scale_pct);
        (
            format!("({base_w}*({expr})/100)"),
            format!("({base_h}*({expr})/100)"),
        )
    } else {
        let box_w = (base_w * scale_pct / 100.0).round().max(2.0);
        let box_h = (base_h * scale_pct / 100.0).round().max(2.0);
        (format!("{box_w}"), format!("{box_h}"))
    };

    // setpts maps source time → timeline time under playback speed.
    let mut chain = format!(
        "[{index}:v:0]trim=start={source_in}:end={source_out},setpts=(PTS-STARTPTS)/{speed:.6}+{start}/TB"
    );

    if animated_scale {
        chain.push_str(&format!(
            ",scale=w={scale_w_expr}:h={scale_h_expr}:force_original_aspect_ratio=decrease:eval=frame"
        ));
    } else {
        chain.push_str(&format!(
            ",scale=w={scale_w_expr}:h={scale_h_expr}:force_original_aspect_ratio=decrease"
        ));
    }

    let needs_rotate = animated_rotation || rotation_deg.abs() > 1e-3;
    if needs_rotate {
        let rad_expr = if animated_rotation {
            let keys = anim_samples(clip, crate::domain::AnimProperty::Rotation);
            format!("(({piecewise}) * PI / 180)", piecewise = piecewise_expr(&keys, rotation_deg))
        } else {
            format!("({rotation_deg:.6}*PI/180)")
        };
        chain.push_str(&format!(
            ",rotate=a={rad_expr}:ow=rotw({rad_expr}):oh=roth({rad_expr}):c=none"
        ));
    }

    let mut effects = clip.effects;
    let animated_brightness = is_animated(clip, crate::domain::AnimProperty::Brightness);
    let animated_contrast = is_animated(clip, crate::domain::AnimProperty::Contrast);
    let animated_saturation = is_animated(clip, crate::domain::AnimProperty::Saturation);
    let animated_hue = is_animated(clip, crate::domain::AnimProperty::Hue);
    let animated_blur = is_animated(clip, crate::domain::AnimProperty::Blur);
    if animated_brightness {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Brightness);
        effects.brightness = keys
            .last()
            .map(|(_, v)| *v)
            .unwrap_or(effects.brightness);
    }
    if animated_contrast {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Contrast);
        effects.contrast = keys.last().map(|(_, v)| *v).unwrap_or(effects.contrast);
    }
    if animated_saturation {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Saturation);
        effects.saturation = keys.last().map(|(_, v)| *v).unwrap_or(effects.saturation);
    }
    if animated_hue {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Hue);
        effects.hue_degrees = keys.last().map(|(_, v)| *v).unwrap_or(effects.hue_degrees);
    }
    if animated_blur {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Blur);
        effects.blur_radius = keys.last().map(|(_, v)| *v).unwrap_or(effects.blur_radius);
    }

    // Color / blur effects (animated parameters use expressions evaluated per frame).
    let mut effect_parts: Vec<String> = Vec::new();
    let brightness = if animated_brightness {
        piecewise_expr(&anim_samples(clip, crate::domain::AnimProperty::Brightness), 0.0)
    } else {
        format!("{:.4}", effects.brightness)
    };
    let contrast = if animated_contrast {
        piecewise_expr(&anim_samples(clip, crate::domain::AnimProperty::Contrast), 1.0)
    } else {
        format!("{:.4}", effects.contrast)
    };
    let saturation = if animated_saturation {
        piecewise_expr(&anim_samples(clip, crate::domain::AnimProperty::Saturation), 1.0)
    } else {
        format!("{:.4}", effects.saturation)
    };
    let color_touched = animated_brightness
        || animated_contrast
        || animated_saturation
        || effects.brightness.abs() > 1e-4
        || (effects.contrast - 1.0).abs() > 1e-4
        || (effects.saturation - 1.0).abs() > 1e-4;
    if color_touched {
        effect_parts.push(format!(
            "eq=brightness={brightness}:contrast={contrast}:saturation={saturation}"
        ));
    }
    if !effects.grayscale {
        let hue = if animated_hue {
            piecewise_expr(&anim_samples(clip, crate::domain::AnimProperty::Hue), 0.0)
        } else {
            format!("{:.4}", effects.hue_degrees)
        };
        if animated_hue || effects.hue_degrees.abs() > 1e-3 {
            effect_parts.push(format!("hue=h={hue}"));
        }
    }
    let blur = if animated_blur {
        piecewise_expr(&anim_samples(clip, crate::domain::AnimProperty::Blur), 0.0)
    } else {
        format!("{:.4}", effects.blur_radius)
    };
    if animated_blur || effects.blur_radius > 0.01 {
        effect_parts.push(format!("gblur=sigma={blur}"));
    }
    if effects.grayscale {
        effect_parts.push("hue=s=0".to_owned());
    }
    if effects.invert {
        effect_parts.push("negate".to_owned());
    }
    for part in effect_parts {
        chain.push(',');
        chain.push_str(&part);
    }

    chain.push_str(",format=yuva420p");

    let opacity = clip.opacity;
    if animated_opacity {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Opacity);
        let alpha_expr = piecewise_expr(&keys, opacity / 100.0);
        // colorchannelmixer does not accept expressions; modulate alpha via geq instead.
        chain.push_str(&format!(
            ",geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='alpha(X,Y)*({alpha_expr})'"
        ));
    } else if opacity < 99.999 {
        let alpha = (opacity / 100.0).clamp(0.0, 1.0);
        chain.push_str(&format!(",colorchannelmixer=aa={alpha:.4}"));
    }

    chain.push_str(&format!("[{video}]"));
    filters.push(chain);

    let (anchor_x, anchor_y) = transform.anchor.fraction();
    let x_expr = if animated_x {
        let keys = anim_samples(clip, crate::domain::AnimProperty::X);
        format!("({})-overlay_w*{anchor_x}", piecewise_expr(&keys, transform.x))
    } else {
        format!("{}-overlay_w*{anchor_x}", transform.x)
    };
    let y_expr = if animated_y {
        let keys = anim_samples(clip, crate::domain::AnimProperty::Y);
        format!("({})-overlay_h*{anchor_y}", piecewise_expr(&keys, transform.y))
    } else {
        format!("{}-overlay_h*{anchor_y}", transform.y)
    };
    let composed = format!("vo{stage}");
    if animated_x || animated_y || animated_scale {
        filters.push(format!(
            "[{previous_video}][{video}]overlay=x='{x_expr}':y='{y_expr}':eval=frame:eof_action=pass:shortest=0[{composed}]"
        ));
    } else {
        filters.push(format!(
            "[{previous_video}][{video}]overlay=x='{x_expr}':y='{y_expr}':eof_action=pass:shortest=0[{composed}]"
        ));
    }
    composed
}

/// FFmpeg inputs and the video filter graph, shared by the MP4 render and the single-frame
/// PNG export so both composite clips, text and shapes identically.
struct Composition<'a> {
    /// Every clip on the timeline, including hidden layers (their audio may still be mixed).
    clip_entries: Vec<(&'a Layer, &'a Clip)>,
    /// One FFmpeg input per distinct asset, in stable layer/item order.
    inputs: Vec<&'a MediaAsset>,
    input_index: HashMap<AssetId, usize>,
    /// `filter_complex` fragments; [`Self::final_video`] is the last video label in the chain.
    filters: Vec<String>,
    final_video: String,
    duration: f64,
}

fn build_composition(project: &Project) -> Result<Composition<'_>, ExportError> {
    let settings = &project.sequence.settings;
    let duration = project.duration().as_seconds().max(0.001);

    // Every clip needs an `-i`, even ones living on a hidden layer, since their audio
    // may still be part of the mix. Index is assigned in stable layer/item order. A speech
    // contributes its synthesized audio here (so it is mixed), but is not composited as video.
    let clip_entries: Vec<(&Layer, &Clip)> = project
        .sequence
        .layers
        .iter()
        .flat_map(|layer| {
            layer
                .items
                .iter()
                .filter_map(move |item| item.audio_clip().map(|clip| (layer, clip)))
        })
        .collect();
    // A project with only text/shapes (no clips) is still renderable: the base black
    // canvas plus the overlays produces a valid video with a silent audio track.
    if clip_entries.is_empty()
        && !project
            .sequence
            .layers
            .iter()
            .any(|layer| !layer.items.is_empty())
    {
        return Err(ExportError::EmptyTimeline);
    }
    // One FFmpeg input per distinct asset rather than per clip: a still reused as a logo, or a
    // video placed twice, would otherwise open the same file several times (for images that means
    // several endless looped streams decoding the same frame). Index follows stable layer/item
    // order so the filter graph is deterministic.
    let mut inputs: Vec<&MediaAsset> = Vec::new();
    let mut input_index: HashMap<AssetId, usize> = HashMap::new();
    for (_, clip) in &clip_entries {
        let asset = project.asset(clip.asset_id).expect("validated asset");
        if let std::collections::hash_map::Entry::Vacant(entry) = input_index.entry(clip.asset_id) {
            entry.insert(inputs.len());
            inputs.push(asset);
        }
    }

    let mut filters = Vec::new();
    filters.push(format!(
        "color=c=black:s={}x{}:r={}:d={duration}[base]",
        settings.width, settings.height, settings.frame_rate
    ));
    let mut previous_video = "base".to_owned();
    let mut font_cache: HashMap<(Option<String>, bool, bool), Option<String>> = HashMap::new();
    let mut stage = 0usize;

    // Composite bottom-to-top so that layer 0 (front-most in the layer list) ends up
    // drawn last, i.e. visually on top. Clips and text are interleaved in this same
    // pass so cross-layer z-order between video and text is respected.
    for layer in project.sequence.layers.iter().rev() {
        if layer.hidden {
            continue;
        }
        for item in &layer.items {
            match item {
                TimelineItem::Clip(clip) => {
                    let index = input_index[&clip.asset_id];
                    let asset = project.asset(clip.asset_id).expect("validated asset");
                    if asset.metadata.has_video {
                        previous_video = push_clip_video_filters(
                            &mut filters,
                            &previous_video,
                            clip,
                            index,
                            stage,
                            settings,
                        );
                    }
                }
                TimelineItem::Text(text) => {
                    previous_video = push_text_filters(
                        &mut filters,
                        &previous_video,
                        stage,
                        &mut font_cache,
                        text,
                    )?;
                }
                // A speech draws its subtitle exactly like a text overlay; its audio is mixed
                // through `clip_entries`.
                TimelineItem::Speech(speech) => {
                    previous_video = push_text_filters(
                        &mut filters,
                        &previous_video,
                        stage,
                        &mut font_cache,
                        &speech.subtitle,
                    )?;
                }
                TimelineItem::Shape(shape) => {
                    let width = (shape.width * shape.transform.scale / 100.0)
                        .round()
                        .max(2.0);
                    let height = (shape.height * shape.transform.scale / 100.0)
                        .round()
                        .max(2.0);
                    let (anchor_x, anchor_y) = shape.transform.anchor.fraction();
                    let x = (shape.transform.x - width * anchor_x).round();
                    let y = (shape.transform.y - height * anchor_y).round();
                    let start = shape.timeline_start.as_seconds();
                    let end = shape.timeline_end().as_seconds();
                    let [r, g, b, a] = shape.color_rgba;
                    let alpha = (shape.opacity / 100.0).clamp(0.0, 1.0) * (a as f32 / 255.0);
                    let alpha_byte = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
                    let next = format!("shape{stage}");
                    let rotation_deg = shape.transform.rotation_degrees;
                    let needs_rotate = rotation_deg.abs() > 1e-3;
                    let rad = (rotation_deg as f64).to_radians();
                    match shape.kind {
                        crate::domain::ShapeKind::Rectangle => {
                            if needs_rotate {
                                filters.push(format!(
                                    "color=c=0x{r:02x}{g:02x}{b:02x}@{alpha:.3}:s={width:.0}x{height:.0}:r={}:d={}[shpcolor{stage}]",
                                    settings.frame_rate,
                                    (end - start).max(0.04)
                                ));
                                filters.push(format!(
                                    "[shpcolor{stage}]rotate=a={rad:.8}:ow=rotw({rad:.8}):oh=roth({rad:.8}):c=none,setpts=PTS-STARTPTS+{start}/TB[shprot{stage}]"
                                ));
                                filters.push(format!(
                                    "[{previous_video}][shprot{stage}]overlay=x={x:.0}:y={y:.0}:eof_action=pass:shortest=0[{next}]"
                                ));
                            } else {
                                filters.push(format!(
                                    "[{previous_video}]drawbox=x={x:.0}:y={y:.0}:w={width:.0}:h={height:.0}:color=0x{r:02x}{g:02x}{b:02x}@{alpha:.3}:t=fill:enable='between(t,{start},{end})'[{next}]"
                                ));
                            }
                        }
                        crate::domain::ShapeKind::Ellipse => {
                            filters.push(format!(
                                "color=c=0x{r:02x}{g:02x}{b:02x}@0.0:s={width:.0}x{height:.0}:r={}:d={}[shapecolor{stage}]",
                                settings.frame_rate,
                                shape.duration.as_seconds()
                            ));
                            filters.push(format!(
                                "[shapecolor{stage}]format=rgba,geq=r='{r}':g='{g}':b='{b}':a='if(lte(pow((X-(W/2))/(W/2),2)+pow((Y-(H/2))/(H/2),2),1),{alpha_byte},0)'[shapepad{stage}]"
                            ));
                            if needs_rotate {
                                filters.push(format!(
                                    "[shapepad{stage}]rotate=a={rad:.8}:ow=rotw({rad:.8}):oh=roth({rad:.8}):c=none[shprot{stage}]"
                                ));
                                filters.push(format!(
                                    "[shprot{stage}]setpts=PTS-STARTPTS+{start}/TB[shape{stage}]"
                                ));
                            } else {
                                filters.push(format!(
                                    "[shapepad{stage}]setpts=PTS-STARTPTS+{start}/TB[shape{stage}]"
                                ));
                            }
                            filters.push(format!(
                                "[{previous_video}][shape{stage}]overlay=x={x:.0}:y={y:.0}:eof_action=pass:shortest=0[{next}]"
                            ));
                        }
                    }
                    previous_video = next;
                }
            }
            stage += 1;
        }
    }

    Ok(Composition {
        clip_entries,
        inputs,
        input_index,
        filters,
        final_video: previous_video,
        duration,
    })
}

/// Appends one FFmpeg `-i` per distinct asset. Stills are looped so any source range works.
fn push_input_args(command: &mut Command, inputs: &[&MediaAsset], settings: &SequenceSettings) {
    for asset in inputs {
        // A still image supplies a single frame; looping it turns it into a continuous stream so
        // the clip's `trim` can pick any source range. The right loop flag depends on the format
        // (see `media::image`), and a container this FFmpeg cannot decode is converted first.
        let path = if asset.metadata.is_image {
            ensure_decodable_image(&asset.path)
        } else {
            asset.path.clone()
        };
        if asset.metadata.is_image {
            command.args(image_loop_input_args(&path, settings.frame_rate));
        }
        command.arg("-i").arg(&path);
    }
}

fn render(
    project: &Project,
    destination: &Path,
    options: ExportOptions,
) -> Result<(), ExportError> {
    let Composition {
        clip_entries,
        inputs,
        input_index,
        mut filters,
        final_video,
        duration,
    } = build_composition(project)?;
    let settings = &project.sequence.settings;

    let mut command = Command::new("nice");
    command.args(["-n", "5", "ffmpeg", "-hide_banner", "-y", "-nostdin"]);
    push_input_args(&mut command, &inputs, settings);

    // Only build the audio graph when audio will actually be muxed; otherwise FFmpeg rejects
    // the unconnected `[aout]` output of the mixing filter.
    if options.audio_codec != AudioCodec::None {
        let mut audio_labels = vec!["asilence".to_owned()];
        filters.push(format!(
            "anullsrc=r={}:cl=stereo:d={duration}[asilence]",
            settings.sample_rate
        ));
        for (layer, clip) in &clip_entries {
            let asset = project.asset(clip.asset_id).expect("validated asset");
            if asset.metadata.has_audio && !layer.muted && !clip.audio.muted {
                let index = input_index[&clip.asset_id];
                let source_in = clip.source_in.as_seconds();
                let source_out = clip.source_out.as_seconds();
                let start = clip.timeline_start.as_seconds();
                let audio = format!("a{index}_{}", audio_labels.len());
                let speed = clip.speed();
                let tempo = atempo_chain_filter(speed);
                let tempo_part = if tempo.is_empty() {
                    String::new()
                } else {
                    format!(",{tempo}")
                };
                let animated_volume = is_animated(clip, crate::domain::AnimProperty::Volume);
                let volume_part = if animated_volume {
                    let keys = anim_samples(clip, crate::domain::AnimProperty::Volume);
                    // Keys carry gain in dB; convert to linear gain in the expression.
                    let linear: Vec<(f64, f32)> = keys
                        .iter()
                        .map(|(t, db)| (*t, 10_f32.powf(*db / 20.0)))
                        .collect();
                    let expr = piecewise_expr(&linear, 10_f32.powf(clip.audio.gain_db / 20.0));
                    format!(",volume=volume='{expr}':eval=frame")
                } else {
                    let gain = 10_f32.powf(clip.audio.gain_db / 20.0);
                    format!(",volume={gain}")
                };
                let delay_ms = (start * 1_000.0).round() as u64;
                filters.push(format!(
                    "[{index}:a:0]atrim=start={source_in}:end={source_out},asetpts=PTS-STARTPTS{tempo_part}{volume_part},adelay={delay_ms}:all=1[{audio}]"
                ));
                audio_labels.push(audio);
            }
        }

        let audio_inputs = audio_labels
            .iter()
            .map(|label| format!("[{label}]"))
            .collect::<String>();
        filters.push(format!(
            "{audio_inputs}amix=inputs={}:duration=first:normalize=0[aout]",
            audio_labels.len()
        ));
    }
    filters.push(format!("[{final_video}]trim=duration={duration}[vout]"));

    let temporary = temporary_path(destination);
    command.args(["-filter_complex", &filters.join(";")]);
    command.args(["-map", "[vout]"]);
    if options.audio_codec != AudioCodec::None {
        command.args(["-map", "[aout]"]);
    }

    let encoder_name = options.video_encoder.ffmpeg_name();
    command.arg("-c:v").arg(encoder_name);
    let (preset_flag, preset_value) = options.preset.preset_args(options.video_encoder);
    command.args([preset_flag, preset_value]);
    for arg in options.video_encoder.quality_args(options.crf) {
        command.arg(arg);
    }
    for arg in options.video_encoder.extra_args() {
        command.arg(arg);
    }

    if options.audio_codec != AudioCodec::None {
        command.args(["-c:a", options.audio_codec.ffmpeg_name()]);
        command.args(["-b:a", &format!("{}k", options.audio_bitrate_kbps)]);
    }
    command
        .args(["-r"])
        .arg(settings.frame_rate.to_string())
        .arg("-threads")
        .arg(options.threads.max(1).to_string())
        .args(["-movflags", "+faststart"])
        .arg(&temporary);

    let output = command.output()?;
    if !output.status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(ExportError::Ffmpeg(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    fs::rename(&temporary, destination)?;
    Ok(())
}

/// Renders the composited frame at `time_sec` (clips + text + shapes) to `destination` as a
/// single PNG. Uses the same filter graph as [`render`], so the thumbnail matches the MP4.
/// No audio. Overwrites `destination` in place.
pub fn export_frame_png(
    project: &Project,
    time_sec: f64,
    destination: &Path,
) -> Result<(), ExportError> {
    let Composition {
        inputs,
        mut filters,
        final_video,
        duration,
        ..
    } = build_composition(project)?;
    let settings = &project.sequence.settings;

    let mut command = Command::new("nice");
    command.args(["-n", "5", "ffmpeg", "-hide_banner", "-y", "-nostdin"]);
    push_input_args(&mut command, &inputs, settings);

    // Keep the single output frame that covers the requested time. The one-frame window is
    // clamped inside the timeline so a playhead parked at the very end still resolves.
    let frame_dur = 1.0 / settings.frame_rate.max(1.0);
    let t = time_sec.clamp(0.0, (duration - frame_dur).max(0.0));
    filters.push(format!(
        "[{final_video}]trim=start={t:.6}:end={:.6},setpts=PTS-STARTPTS[vout]",
        t + frame_dur
    ));

    let mut temporary_os = destination.as_os_str().to_os_string();
    temporary_os.push(".part.png");
    let temporary = PathBuf::from(temporary_os);

    command.args(["-filter_complex", &filters.join(";")]);
    command.args(["-map", "[vout]", "-frames:v", "1", "-an"]);
    command.args(["-c:v", "png", "-f", "image2"]);
    command.arg(&temporary);

    let output = command.output()?;
    if !output.status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(ExportError::Ffmpeg(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    fs::rename(&temporary, destination)?;
    Ok(())
}

fn rgba_to_ffmpeg_color(rgba: [u8; 4]) -> String {
    format!(
        "#{:02x}{:02x}{:02x}@{:.3}",
        rgba[0],
        rgba[1],
        rgba[2],
        rgba[3] as f32 / 255.0
    )
}

fn escape_filter_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
        .replace(',', "\\,")
        .replace('%', "\\%")
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".part.mp4");
    PathBuf::from(value)
}
