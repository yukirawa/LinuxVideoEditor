//! Scene embeddings (Phase 3 groundwork).
//!
//! The embedding model is whichever the user selected — local or network (see `crate::models`).
//! Only text derived from Phase 2 analysis is embedded (transcript, OCR, summary); no raw video
//! is sent anywhere, which keeps context small and privacy reasonable.
//!
//! Vectors are stored next to the VIR in the media cache, keyed by scene ID, and the VIR only
//! carries a reference (`VirScene::embedding_ref`), matching the VIR specification.

use crate::domain::AssetId;
use crate::models::{ModelError, OpenAiCompatible};
use crate::protocol::{VirMedia, VirScene};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use thiserror::Error;
use uuid::Uuid;

pub const EMBEDDING_SCHEMA_VERSION: &str = "0.1";

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("埋め込みを保存できませんでした: {0}")]
    Io(#[from] std::io::Error),
    #[error("埋め込みを生成できるシーンがありません（先に解析を実行してください）")]
    NoDocuments,
    #[error("埋め込みの生成はキャンセルされました")]
    Cancelled,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct EmbeddingSet {
    pub schema_version: String,
    /// Which model produced these vectors. Vectors from different models are not comparable,
    /// so a change invalidates the set instead of silently mixing spaces.
    pub model: String,
    /// Analysis signature the vectors were derived from. A re-analysis that changes scene
    /// content must not be searched with stale vectors, even when the model is unchanged.
    #[serde(default)]
    pub analysis_signature: String,
    pub dimensions: usize,
    /// Scene ID → embedding.
    pub vectors: BTreeMap<Uuid, Vec<f32>>,
}

impl EmbeddingSet {
    pub fn is_compatible_with(&self, model: &str, analysis_signature: &str) -> bool {
        self.model == model && self.is_from_analysis(analysis_signature)
    }

    /// Whether these vectors were derived from the given analysis, regardless of model.
    pub fn is_from_analysis(&self, analysis_signature: &str) -> bool {
        self.analysis_signature == analysis_signature
    }
}

pub fn embedding_file(directory: &Path) -> PathBuf {
    directory.join("embeddings.json")
}

pub fn load(directory: &Path) -> Option<EmbeddingSet> {
    let bytes = std::fs::read(embedding_file(directory)).ok()?;
    let set: EmbeddingSet = serde_json::from_slice(&bytes).ok()?;
    (set.schema_version == EMBEDDING_SCHEMA_VERSION).then_some(set)
}

pub fn save(directory: &Path, set: &EmbeddingSet) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let bytes = serde_json::to_vec_pretty(set).map_err(std::io::Error::other)?;
    std::fs::write(embedding_file(directory), bytes)
}

/// Builds the text that represents a scene for retrieval. Transcript first (it carries the most
/// meaning), then OCR, then the summary, with duplicates removed.
pub fn document_text(scene: &VirScene) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !value.is_empty() && !parts.iter().any(|existing| existing == &value) {
            parts.push(value);
        }
    };
    if let Some(transcript) = &scene.transcript {
        push(transcript);
    }
    for line in &scene.ocr {
        push(line);
    }
    push(&scene.summary);
    let combined = parts.join(" / ");
    combined.chars().take(2000).collect()
}

/// Scene IDs paired with their embedding input, skipping scenes with no usable text.
pub fn documents(media: &VirMedia) -> Vec<(Uuid, String)> {
    media
        .scenes
        .iter()
        .map(|scene| (scene.scene_id, document_text(scene)))
        .filter(|(_, text)| !text.trim().is_empty())
        .collect()
}

/// Embeds every scene of `media` that has text. `analysis_signature` records which analysis the
/// vectors belong to so a re-analysis invalidates them. Reports progress as `(completed, total)`.
pub fn embed_media(
    provider: &OpenAiCompatible,
    media: &VirMedia,
    analysis_signature: &str,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(usize, usize),
) -> Result<EmbeddingSet, EmbeddingError> {
    let documents = documents(media);
    if documents.is_empty() {
        return Err(EmbeddingError::NoDocuments);
    }
    provider.config().validate()?;

    let total = documents.len();
    let batch_size = provider.config().batch_size.max(1);
    let mut vectors: BTreeMap<Uuid, Vec<f32>> = BTreeMap::new();
    let mut dimensions = 0usize;

    let mut completed = 0usize;
    for chunk in documents.chunks(batch_size) {
        if cancel.load(Ordering::Relaxed) {
            return Err(EmbeddingError::Cancelled);
        }
        let inputs: Vec<String> = chunk.iter().map(|(_, text)| text.clone()).collect();
        let embedded = provider.embeddings(&inputs)?;
        if embedded.len() != chunk.len() {
            return Err(EmbeddingError::Model(ModelError::InvalidOutput(format!(
                "埋め込み数が一致しません（要求 {} / 応答 {}）",
                chunk.len(),
                embedded.len()
            ))));
        }
        for ((scene_id, _), vector) in chunk.iter().zip(embedded) {
            dimensions = dimensions.max(vector.len());
            vectors.insert(*scene_id, vector);
        }
        completed += chunk.len();
        report(completed, total);
    }

    Ok(EmbeddingSet {
        schema_version: EMBEDDING_SCHEMA_VERSION.to_owned(),
        model: provider.config().model.clone(),
        analysis_signature: analysis_signature.to_owned(),
        dimensions,
        vectors,
    })
}

/// Result of embedding one asset's scenes.
#[derive(Clone, Debug)]
pub struct EmbeddedMedia {
    pub media_id: AssetId,
    pub model: String,
    pub dimensions: usize,
    pub scenes: usize,
}

pub enum EmbeddingEvent {
    Progress { completed: usize, total: usize },
    Finished(Box<Result<EmbeddedMedia, EmbeddingError>>),
}

/// Runs `embed_media` on a worker thread so a slow local model or a network round-trip never
/// blocks the UI.
pub struct EmbeddingWorker {
    cancel: Arc<AtomicBool>,
    events: Receiver<EmbeddingEvent>,
}

impl EmbeddingWorker {
    pub fn spawn(
        provider: OpenAiCompatible,
        media: VirMedia,
        directory: PathBuf,
        analysis_signature: String,
        repaint: impl Fn() + Send + 'static,
    ) -> Self {
        let (sender, events) = channel::<EmbeddingEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        let job_cancel = Arc::clone(&cancel);
        thread::Builder::new()
            .name("scene-embedding".into())
            .spawn(move || {
                let mut report = |completed: usize, total: usize| {
                    let _ = sender.send(EmbeddingEvent::Progress { completed, total });
                    repaint();
                };
                let outcome = embed_media(
                    &provider,
                    &media,
                    &analysis_signature,
                    &job_cancel,
                    &mut report,
                )
                .and_then(|set| {
                    save(&directory, &set)?;
                    Ok(EmbeddedMedia {
                        media_id: media.media_id,
                        model: set.model,
                        dimensions: set.dimensions,
                        scenes: set.vectors.len(),
                    })
                });
                let _ = sender.send(EmbeddingEvent::Finished(Box::new(outcome)));
                repaint();
            })
            .expect("failed to start embedding worker");
        Self { cancel, events }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn try_event(&self) -> Option<EmbeddingEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for EmbeddingWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TimelineTime;
    use crate::models::{HttpResponse, HttpTransport, ModelConfig, ModelLocation, ProviderKind};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct FakeTransport {
        responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    }

    impl FakeTransport {
        fn with(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
            }
        }
    }

    impl HttpTransport for FakeTransport {
        fn request(
            &self,
            _method: &str,
            _url: &str,
            _headers: &[String],
            _body: Option<&str>,
            _timeout_seconds: u32,
        ) -> Result<HttpResponse, ModelError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ModelError::Connection("no fake response queued".to_owned()))
        }
    }

    fn response(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_owned(),
        }
    }

    fn config(batch_size: usize) -> ModelConfig {
        ModelConfig {
            location: ModelLocation::Local,
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: "bge-m3".to_owned(),
            api_key: String::new(),
            api_key_env: String::new(),
            timeout_seconds: 30,
            batch_size,
            preset_id: "ollama".to_owned(),
        }
    }

    fn scene(summary: &str, transcript: Option<&str>, ocr: &[&str]) -> VirScene {
        VirScene {
            scene_id: Uuid::new_v4(),
            start: TimelineTime::ZERO,
            end: TimelineTime::from_seconds(1.0),
            summary: summary.to_owned(),
            transcript: transcript.map(str::to_owned),
            ocr: ocr.iter().map(|line| (*line).to_owned()).collect(),
            objects: Vec::new(),
            events: Vec::new(),
            mood: Vec::new(),
            importance: None,
            shots: Vec::new(),
            keyframes: Vec::new(),
        }
    }

    fn media_with_scenes(count: usize) -> VirMedia {
        let mut media = VirMedia::empty(Uuid::new_v4());
        media.scenes = (0..count)
            .map(|index| scene(&format!("シーン{index}"), None, &[]))
            .collect();
        media
    }

    #[test]
    fn document_prefers_transcript_then_ocr_and_drops_duplicates() {
        let scene = scene("旭川駅", Some("まもなく発車します"), &["旭川駅", "ホーム"]);
        let text = document_text(&scene);
        assert!(text.starts_with("まもなく発車します"));
        assert!(text.contains("ホーム"));
        // "旭川駅" appears as OCR and as the summary but must only appear once.
        assert_eq!(text.matches("旭川駅").count(), 1);
    }

    #[test]
    fn documents_skip_scenes_without_text() {
        let mut media = VirMedia::empty(Uuid::new_v4());
        media.scenes = vec![scene("", None, &[]), scene("scene", None, &[])];
        let documents = documents(&media);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].1, "scene");
    }

    #[test]
    fn embedding_sets_only_match_their_own_model_and_analysis() {
        let set = EmbeddingSet {
            schema_version: EMBEDDING_SCHEMA_VERSION.to_owned(),
            model: "bge-m3".to_owned(),
            analysis_signature: "sig-1".to_owned(),
            dimensions: 2,
            vectors: BTreeMap::new(),
        };
        assert!(set.is_compatible_with("bge-m3", "sig-1"));
        assert!(!set.is_compatible_with("text-embedding-3-small", "sig-1"));
        // A re-analysis with the same model still invalidates the vectors.
        assert!(!set.is_compatible_with("bge-m3", "sig-2"));
    }

    #[test]
    fn embeds_every_scene_across_batches_and_round_trips_through_disk() {
        let media = media_with_scenes(3);
        let transport = FakeTransport::with(vec![
            response(
                r#"{"data":[{"index":0,"embedding":[1.0,0.0]},{"index":1,"embedding":[0.0,1.0]}]}"#,
            ),
            response(r#"{"data":[{"index":0,"embedding":[0.5,0.5]}]}"#),
        ]);
        let provider = OpenAiCompatible::with_transport(config(2), Box::new(transport));
        let cancel = AtomicBool::new(false);
        let mut reports = Vec::new();
        let set = embed_media(
            &provider,
            &media,
            "sig-1",
            &cancel,
            &mut |completed, total| {
                reports.push((completed, total));
            },
        )
        .unwrap();

        assert_eq!(set.vectors.len(), 3);
        assert_eq!(set.dimensions, 2);
        assert_eq!(set.model, "bge-m3");
        assert_eq!(set.analysis_signature, "sig-1");
        // Two batches of sizes 2 and 1.
        assert_eq!(reports, vec![(2, 3), (3, 3)]);

        let directory = std::env::temp_dir().join(format!("yuki-embed-{}", Uuid::new_v4()));
        save(&directory, &set).unwrap();
        let loaded = load(&directory).unwrap();
        assert_eq!(loaded.vectors.len(), 3);
        assert_eq!(loaded.model, "bge-m3");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn cancellation_stops_before_requesting_the_model() {
        let media = media_with_scenes(2);
        let transport = FakeTransport::with(vec![response(
            r#"{"data":[{"index":0,"embedding":[1.0]}]}"#,
        )]);
        let provider = OpenAiCompatible::with_transport(config(1), Box::new(transport));
        let cancel = AtomicBool::new(true);
        let error = embed_media(&provider, &media, "sig-1", &cancel, &mut |_, _| {}).unwrap_err();
        assert!(matches!(error, EmbeddingError::Cancelled));
    }

    #[test]
    fn scenes_without_text_cannot_be_embedded() {
        let mut media = VirMedia::empty(Uuid::new_v4());
        media.scenes = vec![scene("", None, &[])];
        let transport = FakeTransport::with(vec![]);
        let provider = OpenAiCompatible::with_transport(config(8), Box::new(transport));
        let cancel = AtomicBool::new(false);
        let error = embed_media(&provider, &media, "sig-1", &cancel, &mut |_, _| {}).unwrap_err();
        assert!(matches!(error, EmbeddingError::NoDocuments));
    }
}
