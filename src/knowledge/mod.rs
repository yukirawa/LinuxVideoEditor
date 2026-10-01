//! Video Knowledge Engine.
//!
//! Phase 2 of the roadmap: turn source media into a VIR (`protocol::vir`) by extracting
//! metadata, detecting scenes and shots, extracting keyframes (the thumbnail system) and,
//! when the required backends are installed, OCR and transcript.
//!
//! Design rules borrowed from the project doctrine:
//! - analysis is derived data: everything here is rebuildable and cached, never authoritative;
//! - the engine is backend-neutral: it shells out to well-known system tools (FFmpeg,
//!   tesseract, whisper.cpp) and degrades gracefully when one is missing;
//! - the engine never touches the source media or the editing core.

mod cache;
mod embedding;
mod frames;
mod job;
mod ocr;
mod scene;
mod search;
mod transcript;

pub use cache::{
    AnalysisOptions, analysis_cache_dir, clear_all as clear_analysis_cache, media_key,
};
pub use embedding::{
    EMBEDDING_SCHEMA_VERSION, EmbeddedMedia, EmbeddingError, EmbeddingEvent, EmbeddingSet,
    EmbeddingWorker, documents, embed_media, embedding_file, load as load_embeddings,
    save as save_embeddings,
};
pub use job::{
    AnalysisError, AnalysisEvent, AnalysisProgress, AnalysisRequest, AnalysisResult, AnalysisStage,
    AnalysisWorker, run_analysis,
};
pub use ocr::available_ocr_languages;
pub use scene::{Cut, SceneGrouping, SceneSpan, ShotSpan, detect_cuts, group_scenes};
pub use search::{SceneHit, cosine_similarity, rank};
pub use transcript::{AsrBackend, TranscriptSegment};

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::Command;
use uuid::Uuid;

/// Which analysis backends are usable on this machine. Detected once at startup on a worker
/// thread so the first frame is never blocked by probing external tools.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct KnowledgeCapabilities {
    pub ffmpeg: bool,
    pub scene_detection: bool,
    /// Name of the detected speech-to-text backend, if any.
    pub asr: Option<String>,
    pub ocr: bool,
    pub ocr_languages: Vec<String>,
}

impl KnowledgeCapabilities {
    pub fn ocr_label(&self) -> String {
        if !self.ocr {
            return "未検出".to_owned();
        }
        if self.ocr_languages.is_empty() {
            "利用可能".to_owned()
        } else {
            self.ocr_languages.join(", ")
        }
    }
}

pub fn detect_capabilities() -> KnowledgeCapabilities {
    let ffmpeg = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success());
    let ocr_languages = ocr::available_ocr_languages();
    KnowledgeCapabilities {
        ffmpeg,
        scene_detection: ffmpeg,
        asr: AsrBackend::detect().map(|backend| backend.name().to_owned()),
        ocr: !ocr_languages.is_empty(),
        ocr_languages,
    }
}

/// Builds a stable UUID from a seed string. Semantic IDs (scene/shot/frame) must survive
/// re-analysis so that a later AEP can keep referring to the same entity after the cache is
/// rebuilt, so they are hashed from the media fingerprint plus their position instead of being
/// random. Two differently salted hashes fill the 128 bits, then the RFC 4122 version/variant
/// bits are set so the result is a well-formed v4-shaped UUID.
pub fn deterministic_id(seed: &str) -> Uuid {
    let mut bytes = [0u8; 16];
    for (index, salt) in ["yuki-vir-a", "yuki-vir-b"].into_iter().enumerate() {
        let mut hasher = DefaultHasher::new();
        salt.hash(&mut hasher);
        seed.hash(&mut hasher);
        bytes[index * 8..index * 8 + 8].copy_from_slice(&hasher.finish().to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::deterministic_id;

    #[test]
    fn deterministic_ids_are_stable_and_distinct() {
        let first = deterministic_id("media/scene/0");
        let second = deterministic_id("media/scene/0");
        let other = deterministic_id("media/scene/1");
        assert_eq!(first, second);
        assert_ne!(first, other);
        // Version 4 shape so consumers can treat it as a normal UUID.
        assert_eq!(first.get_version_num(), 4);
    }
}
