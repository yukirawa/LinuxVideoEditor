//! Analysis cache and options.
//!
//! VIR is derived data (PROJECT-FORMAT.md section 5), so it is cached under the user cache
//! directory rather than stored inside the project. The cache key follows the *file*
//! (canonical path + size + mtime), so re-importing the same media into another project reuses
//! the analysis, while the project itself only stores references.
//!
//! A cached document is only valid when both the analysis pipeline version and the options
//! that produced it match; anything else forces a re-analysis (VIR spec section 10).

use crate::protocol::{VIR_ANALYSIS_VERSION, VIR_SCHEMA_VERSION, VirDocument};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Bumped whenever the on-disk cache layout changes.
pub const ANALYSIS_CACHE_FORMAT: &str = "1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AnalysisOptions {
    /// Minimum FFmpeg scene score for a frame to count as a cut (detection threshold).
    pub cut_threshold: f32,
    /// A cut at or above this score starts a new scene; weaker cuts only start a new shot.
    pub scene_threshold: f32,
    pub min_shot_seconds: f64,
    pub max_scene_seconds: f64,
    pub thumbnail_width: u32,
    pub ocr: bool,
    /// tesseract language string, e.g. `jpn+eng`. Empty means "pick automatically".
    pub ocr_languages: String,
    pub transcript: bool,
    /// whisper.cpp model file. Empty means transcription is unavailable.
    pub asr_model_path: String,
    pub threads: u8,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            cut_threshold: 0.25,
            scene_threshold: 0.40,
            min_shot_seconds: 0.35,
            max_scene_seconds: 30.0,
            thumbnail_width: 480,
            ocr: false,
            ocr_languages: String::new(),
            transcript: false,
            asr_model_path: String::new(),
            threads: 2,
        }
    }
}

impl AnalysisOptions {
    pub fn clamp(&mut self) {
        self.cut_threshold = self.cut_threshold.clamp(0.05, 0.95);
        self.scene_threshold = self.scene_threshold.clamp(0.05, 0.99);
        self.min_shot_seconds = self.min_shot_seconds.clamp(0.0, 5.0);
        self.max_scene_seconds = self.max_scene_seconds.clamp(2.0, 600.0);
        self.thumbnail_width = self.thumbnail_width.clamp(64, 1920);
        self.threads = self.threads.clamp(1, 16);
    }

    /// Everything that can change the analysis result, so the cache can be invalidated when
    /// any of it changes.
    pub fn signature(&self) -> String {
        format!(
            "{ANALYSIS_CACHE_FORMAT}|{VIR_SCHEMA_VERSION}|{VIR_ANALYSIS_VERSION}|{:.4}|{:.4}|{:.3}|{:.1}|{}|{}|{}|{}|{}|{}",
            self.cut_threshold,
            self.scene_threshold,
            self.min_shot_seconds,
            self.max_scene_seconds,
            self.thumbnail_width,
            self.ocr,
            self.ocr_languages,
            self.transcript,
            self.asr_model_path,
            self.threads,
        )
    }
}

pub fn analysis_cache_dir() -> PathBuf {
    cache_home().join("yuki-video-editor").join("analysis")
}

fn cache_home() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Stable cache key for a media file: canonical path, size and modification time. Hashing the
/// contents would be stricter but far more expensive on multi-gigabyte footage.
pub fn media_key(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.to_string_lossy().hash(&mut hasher);
    if let Ok(metadata) = std::fs::metadata(path) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified()
            && let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            elapsed.as_nanos().hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

pub fn media_dir(root: &Path, key: &str) -> PathBuf {
    root.join(key)
}

#[derive(Deserialize, Serialize)]
struct CacheMeta {
    format: String,
    signature: String,
}

pub fn load(root: &Path, key: &str, signature: &str) -> Option<VirDocument> {
    let directory = media_dir(root, key);
    let meta: CacheMeta =
        serde_json::from_str(&std::fs::read_to_string(directory.join("meta.json")).ok()?).ok()?;
    if meta.format != ANALYSIS_CACHE_FORMAT || meta.signature != signature {
        return None;
    }
    let bytes = std::fs::read(directory.join("vir.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save(
    root: &Path,
    key: &str,
    signature: &str,
    document: &VirDocument,
) -> std::io::Result<()> {
    let directory = media_dir(root, key);
    std::fs::create_dir_all(&directory)?;
    let meta = CacheMeta {
        format: ANALYSIS_CACHE_FORMAT.to_owned(),
        signature: signature.to_owned(),
    };
    let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(std::io::Error::other)?;
    let vir_bytes = serde_json::to_vec_pretty(document).map_err(std::io::Error::other)?;
    // The metadata is written last: a half-written cache then reads as a miss rather than as a
    // truncated document.
    std::fs::write(directory.join("vir.json"), vir_bytes)?;
    std::fs::write(directory.join("meta.json"), meta_bytes)?;
    Ok(())
}

pub fn clear_all(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProjectId;
    use crate::protocol::{VirDocument, VirMedia};
    use uuid::Uuid;

    fn document() -> VirDocument {
        let media_id = Uuid::new_v4();
        VirDocument {
            schema_version: VIR_SCHEMA_VERSION.to_owned(),
            analysis_version: VIR_ANALYSIS_VERSION.to_owned(),
            provenance: Default::default(),
            project_id: ProjectId::nil(),
            media: vec![VirMedia::empty(media_id)],
        }
    }

    #[test]
    fn signature_tracks_options() {
        let base = AnalysisOptions::default();
        let mut changed = base.clone();
        changed.ocr = !base.ocr;
        assert_ne!(base.signature(), changed.signature());
        assert_eq!(base.signature(), base.clone().signature());
    }

    #[test]
    fn cache_round_trips_and_invalidates() {
        let root = std::env::temp_dir().join(format!("yuki-analysis-{}", Uuid::new_v4()));
        let key = "media-key";
        assert!(load(&root, key, "sig-a").is_none());

        save(&root, key, "sig-a", &document()).unwrap();
        assert!(load(&root, key, "sig-a").is_some());
        // A different signature (changed options or version) must miss.
        assert!(load(&root, key, "sig-b").is_none());

        clear_all(&root).unwrap();
        assert!(!root.exists());
    }
}
