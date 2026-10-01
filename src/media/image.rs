//! Still-image facts shared by the import probe, the preview decoder and the exporter.
//!
//! An image has no intrinsic duration and cannot be seeked in, so it is treated as a clip whose
//! source is an endless loop of one frame. Two different FFmpeg mechanisms are needed to build
//! that loop, and picking the wrong one is not merely inefficient:
//!
//! * `image2`-readable stills (PNG/JPEG/WebP/TIFF/BMP) loop with `-f image2 -loop 1`.
//! * ISO-BMFF container stills (HEIC/HEIF/AVIF) are decoded through the MOV demuxer;
//!   `image2 -loop 1` cannot read them, and they must use `-stream_loop -1` instead.
//!
//! `-stream_loop -1` is deliberately not used for the `image2` formats: on this FFmpeg it hangs
//! on a single JPEG, so the two cases have to stay separate.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Timeline length given to a still image on import. An image has no intrinsic duration, so a
/// sensible default keeps a single dropped image from being a zero-length clip; the clip can be
/// stretched afterwards (see `is_image` in `MediaMetadata`).
pub const IMAGE_DEFAULT_DURATION_SECONDS: f64 = 5.0;

/// Still-image extensions imported as a single-frame clip. Animated formats such as GIF are
/// deliberately absent: FFmpeg probes those as short video streams, which is the more useful
/// behaviour (they keep their animation and their own frame rate).
pub const IMAGE_EXTENSIONS: [&str; 12] = [
    "png", "jpg", "jpeg", "jpe", "webp", "bmp", "tif", "tiff", "avif", "heic", "heif", "jxl",
];

/// Container-based stills that the `image2` demuxer cannot open (they are ISO-BMFF, like MP4).
const CONTAINER_IMAGE_EXTENSIONS: [&str; 3] = ["avif", "heic", "heif"];

/// Formats whose decode support depends on how the local FFmpeg was built (libheif/libjxl/
/// libdav1d). Everything else (PNG/JPEG/WebP/BMP/TIFF) is assumed decodable and returned as-is.
const RISKY_IMAGE_EXTENSIONS: [&str; 4] = ["avif", "heic", "heif", "jxl"];

/// Whether `path` names a still image, decided by extension. FFmpeg reports a single image as a
/// video stream with a bogus duration (or none at all, for HEIC), so the extension is the
/// reliable signal at probe time and also matches the import dialog's filter.
pub fn is_image_path(path: &Path) -> bool {
    has_extension(path, &IMAGE_EXTENSIONS)
}

/// Whether the still image needs the `-stream_loop` decoding path rather than `image2 -loop`.
pub fn is_container_image_path(path: &Path) -> bool {
    has_extension(path, &CONTAINER_IMAGE_EXTENSIONS)
}

/// FFmpeg input arguments (placed before `-i`) that turn `path` into an endless stream so the
/// filter graph can `trim`/`overlay` it for any clip length. `frame_rate` only matters for the
/// `image2` path, where it sets the generated frame rate.
pub fn image_loop_input_args(path: &Path, frame_rate: f64) -> Vec<String> {
    if is_container_image_path(path) {
        vec!["-stream_loop".to_owned(), "-1".to_owned()]
    } else {
        vec![
            "-f".to_owned(),
            "image2".to_owned(),
            "-loop".to_owned(),
            "1".to_owned(),
            "-framerate".to_owned(),
            frame_rate.to_string(),
        ]
    }
}

fn has_extension(path: &Path, known: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            known
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

/// Returns a path FFmpeg can decode for `path`, converting to a cached PNG when the local build
/// cannot decode the source itself (a common case for HEIC/HEIF/AVIF/JXL depending on how FFmpeg
/// was compiled) or cannot decode it through the simple filtergraph the preview/export pipelines
/// use (a tiled HEIC, whose tiles FFmpeg only assembles in an automatic *complex* filtergraph).
/// The original file is never modified, and the caller keeps using it as the asset path, so
/// projects stay portable: the conversion only feeds the preview/export pipeline.
pub fn ensure_decodable_image(path: &Path) -> PathBuf {
    if !has_extension(path, &RISKY_IMAGE_EXTENSIONS) {
        return path.to_path_buf();
    }
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(resolved) = guard.get(path)
    {
        return resolved.clone();
    }
    let resolved = if ffmpeg_decodes(path) {
        path.to_path_buf()
    } else {
        convert_to_cached_png(path).unwrap_or_else(|| path.to_path_buf())
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), resolved.clone());
    }
    resolved
}

/// Whether FFmpeg can decode a frame of `path` through the *simple* filtergraph the preview and
/// export pipelines rely on. The trivial `-vf null` is deliberate: a still whose pixels live in a
/// *stream group* (an HEIC tile grid, e.g. the one a phone camera writes for a large photo) is
/// assembled by FFmpeg into an automatic complex filtergraph, and layering a simple `-vf` on top
/// fails with "Simple and complex filtering cannot be used together" even though a bare decode
/// succeeds. Treating those as undecodable routes them through `convert_to_cached_png`, which
/// flattens the grid into a single-stream image both pipelines can handle.
fn ffmpeg_decodes(path: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-vf", "null", "-frames:v", "1", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Converts `source` to a PNG in the image cache using whichever decoder is installed
/// (libheif's `heif-convert`, then ImageMagick). Returns `None` when no tool can read the file.
fn convert_to_cached_png(source: &Path) -> Option<PathBuf> {
    let directory = image_cache_dir();
    std::fs::create_dir_all(&directory).ok()?;
    let output = directory.join(format!("{}.png", cache_key(source)));
    if output.is_file() {
        return Some(output);
    }
    for program in ["heif-convert", "magick", "convert"] {
        let ok = Command::new(program)
            .arg(source)
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if ok && output.is_file() {
            return Some(output);
        }
    }
    None
}

/// Stable cache file name derived from the source path, size and mtime, so re-importing the same
/// untouched file reuses the conversion instead of redoing it.
fn cache_key(source: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    if let Ok(metadata) = std::fs::metadata(source) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified() {
            modified.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn image_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("yuki-video-editor").join("images")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_still_images_case_insensitively() {
        for name in [
            "a.png", "a.PNG", "a.jpg", "a.JPEG", "a.webp", "a.bmp", "a.tiff", "a.heic", "a.HEIF",
        ] {
            assert!(is_image_path(Path::new(name)), "{name} should be an image");
        }
    }

    #[test]
    fn does_not_treat_video_or_audio_as_images() {
        // Animated GIFs are handled as short video streams, not stills.
        for name in ["a.mp4", "a.mov", "a.gif", "a.wav", "a", "a.png.mp4"] {
            assert!(
                !is_image_path(Path::new(name)),
                "{name} should not be an image"
            );
        }
    }

    #[test]
    fn container_stills_use_the_stream_loop_path() {
        assert!(is_container_image_path(Path::new("photo.heic")));
        assert!(!is_container_image_path(Path::new("photo.png")));

        assert_eq!(
            image_loop_input_args(Path::new("photo.heic"), 30.0),
            vec!["-stream_loop".to_owned(), "-1".to_owned()]
        );
        assert_eq!(
            image_loop_input_args(Path::new("photo.png"), 30.0),
            vec![
                "-f".to_owned(),
                "image2".to_owned(),
                "-loop".to_owned(),
                "1".to_owned(),
                "-framerate".to_owned(),
                "30".to_owned(),
            ]
        );
    }

    #[test]
    fn plain_images_are_not_tested_or_converted() {
        // PNG/JPEG/... are always decodable, so the resolver must return them untouched without
        // spawning FFmpeg or a converter.
        let path = Path::new("/tmp/definitely-absent.png");
        assert_eq!(ensure_decodable_image(path), path.to_path_buf());
    }
}
