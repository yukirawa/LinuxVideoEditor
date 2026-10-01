//! Model selection and semantic search (Phase 3 groundwork).
//!
//! The user picks where the embedding model runs — locally (a small laptop-friendly model behind
//! a local OpenAI-compatible server) or over the network (a hosted API) — and this panel uses
//! that choice to embed the analyzed scenes and search them by meaning.

use super::analysis::{AnalysisState, ellipsize, thumbnail, thumbnail_placeholder};
use super::panels::UiAction;
use crate::knowledge::SceneHit;
use crate::models::ModelConfig;
use crate::protocol::{VirMedia, VirScene};
use eframe::egui::{self, Ui};
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub enum ProbeStatus {
    #[default]
    Idle,
    Testing,
    Ok(Vec<String>),
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub enum EmbeddingStatus {
    #[default]
    Idle,
    Running {
        completed: usize,
        total: usize,
    },
    Ready {
        model: String,
        dimensions: usize,
        scenes: usize,
    },
    Failed(String),
}

/// Everything the semantic-search section needs, kept together so the panel stays declarative.
#[derive(Default)]
pub struct SemanticState {
    pub config: ModelConfig,
    pub probe: ProbeStatus,
    pub embedding: EmbeddingStatus,
    pub query: String,
    /// Latest search results, best first. `None` means "not searched yet".
    pub results: Option<Vec<SceneHit>>,
    pub error: Option<String>,
}

pub(super) fn semantic_section(
    ui: &mut Ui,
    analysis: &mut AnalysisState,
    actions: &mut Vec<UiAction>,
) {
    let AnalysisState {
        result,
        thumbnails,
        semantic,
        ..
    } = analysis;

    ui.separator();
    egui::CollapsingHeader::new("④ 意味検索（任意）")
        .default_open(false)
        .show(ui, |ui| {
            ui.small(
                "埋め込みモデルは「① AIモデル」で設定します。解析済みシーンを意味で検索できます（AIアシスタントも search_scenes で利用します）。",
            );
            model_actions(ui, semantic, result.is_some(), actions);
            ui.separator();
            search_section(ui, result.as_ref(), semantic, thumbnails, actions);
        });
}

fn model_actions(
    ui: &mut Ui,
    semantic: &mut SemanticState,
    has_result: bool,
    actions: &mut Vec<UiAction>,
) {
    let running = matches!(semantic.embedding, EmbeddingStatus::Running { .. });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("🔌 接続テスト"))
            .on_hover_text("エンドポイントに /models を問い合わせます")
            .clicked()
        {
            actions.push(UiAction::TestModelConnection);
        }
        let configured = semantic.config.validate().is_ok();
        if ui
            .add_enabled(
                has_result && configured && !running,
                egui::Button::new("🧠 埋め込みを生成"),
            )
            .on_hover_text("解析済みシーンのテキストをモデルでベクトル化します")
            .clicked()
        {
            actions.push(UiAction::GenerateEmbeddings);
        }
        if ui
            .add_enabled(running, egui::Button::new("■ 中止"))
            .clicked()
        {
            actions.push(UiAction::CancelEmbeddings);
        }
    });

    if !has_result {
        ui.small("埋め込みには先に「解析開始」でシーンを生成してください。");
    }

    match &semantic.probe {
        ProbeStatus::Idle => {}
        ProbeStatus::Testing => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.small("接続を確認中…");
            });
        }
        ProbeStatus::Ok(models) => {
            let label = if models.is_empty() {
                "接続OK（モデル一覧は空）".to_owned()
            } else {
                format!(
                    "接続OK: {}",
                    ellipsize(
                        &models
                            .iter()
                            .take(6)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        90
                    )
                )
            };
            ui.small(label);
        }
        ProbeStatus::Failed(message) => {
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                format!("接続失敗: {message}"),
            );
        }
    }

    match &semantic.embedding {
        EmbeddingStatus::Idle => {}
        EmbeddingStatus::Running { completed, total } => {
            let fraction = if *total == 0 {
                0.0
            } else {
                *completed as f32 / *total as f32
            };
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
            ui.small(format!("埋め込み中: {completed}/{total}"));
        }
        EmbeddingStatus::Ready {
            model,
            dimensions,
            scenes,
        } => {
            ui.small(format!(
                "埋め込み済み: {scenes} シーン / {dimensions}次元 / {model}"
            ));
        }
        EmbeddingStatus::Failed(message) => {
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                format!("埋め込み失敗: {message}"),
            );
        }
    }

    if let Some(error) = &semantic.error {
        ui.colored_label(egui::Color32::from_rgb(220, 100, 100), error);
    }
}

fn search_section(
    ui: &mut Ui,
    result: Option<&crate::knowledge::AnalysisResult>,
    semantic: &mut SemanticState,
    thumbnails: &mut std::collections::HashMap<Uuid, Option<eframe::egui::TextureHandle>>,
    actions: &mut Vec<UiAction>,
) {
    let Some(result) = result else {
        return;
    };
    let embedding_ready = matches!(semantic.embedding, EmbeddingStatus::Ready { .. })
        || crate::knowledge::load_embeddings(&result.cache_dir).is_some();

    ui.horizontal(|ui| {
        let response = ui.add_enabled(
            embedding_ready,
            egui::TextEdit::singleline(&mut semantic.query)
                .hint_text("例: 雪のホームに列車が入ってくる場面")
                .desired_width(300.0),
        );
        let submitted =
            response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if (submitted
            || ui
                .add_enabled(
                    embedding_ready && !semantic.query.trim().is_empty(),
                    egui::Button::new("🔍 検索"),
                )
                .clicked())
            && !semantic.query.trim().is_empty()
        {
            actions.push(UiAction::SearchScenes);
        }
    });
    if !embedding_ready {
        ui.small("検索するには「埋め込みを生成」を実行してください。");
        return;
    }

    let Some(hits) = &semantic.results else {
        return;
    };
    if hits.is_empty() {
        ui.small("一致するシーンが見つかりませんでした。");
        return;
    }

    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(260.0)
        .show(ui, |ui| {
            for hit in hits {
                let Some(scene) = scene_by_id(&result.media, hit.scene_id) else {
                    continue;
                };
                hit_row(ui, result, thumbnails, scene, hit.score, actions);
            }
        });
}

fn hit_row(
    ui: &mut Ui,
    result: &crate::knowledge::AnalysisResult,
    thumbnails: &mut std::collections::HashMap<Uuid, Option<egui::TextureHandle>>,
    scene: &VirScene,
    score: f32,
    actions: &mut Vec<UiAction>,
) {
    let frame = ui.horizontal(|ui| {
        match scene
            .keyframes
            .first()
            .and_then(|keyframe| thumbnail(ui.ctx(), result, thumbnails, keyframe))
        {
            Some(texture) => {
                ui.add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(96.0, 54.0)));
            }
            None => thumbnail_placeholder(ui),
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.monospace(format!("{score:.3}"));
                ui.small(format!("{} – {}", scene.start, scene.end));
            });
            let text = scene
                .transcript
                .clone()
                .or_else(|| scene.ocr.first().cloned())
                .unwrap_or_else(|| scene.summary.clone());
            ui.small(ellipsize(&text, 60));
        });
    });
    let response = ui.interact(
        frame.response.rect,
        ui.id().with(("search-hit", scene.scene_id)),
        egui::Sense::click(),
    );
    let _ = response.clone().on_hover_text("クリックでこの位置へシーク");
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        actions.push(UiAction::Seek(scene.start));
    }
    ui.separator();
}

fn scene_by_id(media: &VirMedia, scene_id: Uuid) -> Option<&VirScene> {
    media.scenes.iter().find(|scene| scene.scene_id == scene_id)
}
