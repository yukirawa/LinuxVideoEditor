use crate::domain::{AssetId, ProjectId, TimelineTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// VIR schema version. Bumped to 0.2 when Phase 2 (Video Intelligence) started emitting
/// shots, keyframes and provenance; older documents still load because every added field is
/// `serde`-defaulted.
pub const VIR_SCHEMA_VERSION: &str = "0.2";

/// Version of the analysis pipeline that produced a document. Cached analysis is invalidated
/// when either this or the schema version changes (see `knowledge::cache`).
pub const VIR_ANALYSIS_VERSION: &str = "2026.1";

/// Which analyzer produced each part of the document. Per VIR spec section 10, derived data
/// must be attributable and model-independent: these are opaque `provider:model` strings, not
/// provider-specific structures.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VirProvenance {
    #[serde(default)]
    pub scene_detector: Option<String>,
    #[serde(default)]
    pub vision_model: Option<String>,
    #[serde(default)]
    pub asr_model: Option<String>,
    #[serde(default)]
    pub ocr_model: Option<String>,
    #[serde(default)]
    pub embedding_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirDocument {
    pub schema_version: String,
    #[serde(default)]
    pub analysis_version: String,
    #[serde(default)]
    pub provenance: VirProvenance,
    pub project_id: ProjectId,
    #[serde(default)]
    pub media: Vec<VirMedia>,
}

impl VirDocument {
    pub fn media_entry(&self, media_id: AssetId) -> Option<&VirMedia> {
        self.media.iter().find(|media| media.media_id == media_id)
    }

    /// Replaces (or inserts) the analysis for one media asset. The cache stores one document
    /// per media file, so this is how a freshly analyzed medium joins an existing document.
    pub fn upsert_media(&mut self, media: VirMedia) {
        match self
            .media
            .iter_mut()
            .find(|existing| existing.media_id == media.media_id)
        {
            Some(existing) => *existing = media,
            None => self.media.push(media),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirMedia {
    pub media_id: AssetId,
    /// Content fingerprint used as the analysis cache key. Distinct from `media_id`, which is
    /// regenerated on every import: the fingerprint follows the file, not the import.
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub duration: TimelineTime,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub has_video: bool,
    #[serde(default)]
    pub has_audio: bool,
    #[serde(default)]
    pub scenes: Vec<VirScene>,
    /// Full transcript, independent of scene grouping. Scenes additionally carry the slice of
    /// transcript that overlaps them.
    #[serde(default)]
    pub transcript: Vec<VirTranscriptSegment>,
}

impl VirMedia {
    pub fn empty(media_id: AssetId) -> Self {
        Self {
            media_id,
            fingerprint: String::new(),
            path: String::new(),
            duration: TimelineTime::ZERO,
            width: 0,
            height: 0,
            fps: 0.0,
            has_video: false,
            has_audio: false,
            scenes: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirScene {
    pub scene_id: Uuid,
    pub start: TimelineTime,
    pub end: TimelineTime,
    /// Deterministic short description derived from transcript/OCR; semantic captioning is a
    /// later phase that needs a vision model.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub ocr: Vec<String>,
    #[serde(default)]
    pub objects: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub mood: Vec<String>,
    /// Editorial estimate, not ground truth (VIR spec section 8).
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub shots: Vec<VirShot>,
    #[serde(default)]
    pub keyframes: Vec<VirFrame>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirShot {
    pub shot_id: Uuid,
    pub start: TimelineTime,
    pub end: TimelineTime,
    /// Scene-change score of the cut that starts this shot (0 for the first shot).
    #[serde(default)]
    pub scene_score: f32,
    #[serde(default)]
    pub visual_energy: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirFrame {
    pub frame_id: Uuid,
    pub timestamp: TimelineTime,
    /// File name relative to the media's cache directory, so the cache stays relocatable.
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub ocr: Vec<String>,
    #[serde(default)]
    pub quality_score: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VirTranscriptSegment {
    pub start: TimelineTime,
    pub end: TimelineTime,
    pub text: String,
}
