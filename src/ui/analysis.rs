//! The AI workspace: analysis console, semantic search and the editing assistant.
//!
//! This panel drives the Video Knowledge Engine (`crate::knowledge`) and presents its result. It
//! makes the analyzed medium fully legible (scenes, shots, keyframes, OCR, transcript, plus a VIR
//! export for external tools), ranks scenes by meaning, and hosts the AI assistant that proposes
//! AEP edit plans for the editor to validate and apply.

use super::chat::{ChatState, assistant_section};
use super::model_config::model_config_editor;
use super::panels::{PanelContext, UiAction};
use super::semantic::{EmbeddingStatus, ProbeStatus, SemanticState, semantic_section};
use crate::domain::{AssetId, Project, TimelineTime};
use crate::knowledge::{
    AnalysisOptions, AnalysisProgress, AnalysisResult, AnalysisStage, KnowledgeCapabilities,
};
use crate::media::{HardwareCapabilities, PowerProfile, VideoEncoder};
use crate::models::{PresetUse, presets_for};
use crate::protocol::{VIR_ANALYSIS_VERSION, VIR_SCHEMA_VERSION, VirFrame};
use eframe::egui::{self, TextureHandle, TextureOptions, Ui};
use image::GenericImageView;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub enum AnalysisStatus {
    #[default]
    Idle,
    Running(AnalysisProgress),
    Done {
        from_cache: bool,
    },
    Failed(String),
}

/// UI-side state for the analysis workspace. Textures are cached per keyframe id and created
/// on demand, so opening a large analyzed project never decodes every thumbnail at once.
#[derive(Default)]
pub struct AnalysisState {
    pub capabilities: KnowledgeCapabilities,
    pub capabilities_ready: bool,
    pub options: AnalysisOptions,
    pub selected_asset: Option<AssetId>,
    pub status: AnalysisStatus,
    pub result: Option<AnalysisResult>,
    pub thumbnails: HashMap<Uuid, Option<TextureHandle>>,
    /// Model selection and semantic search (Phase 3 groundwork).
    pub semantic: SemanticState,
    /// AI assistant conversation and edit plans (Phase 4/5).
    pub chat: ChatState,
}

impl AnalysisState {
    pub fn new(options: AnalysisOptions) -> Self {
        Self {
            options,
            ..Self::default()
        }
    }

    pub fn apply_capabilities(&mut self, capabilities: KnowledgeCapabilities) {
        self.capabilities = capabilities;
        self.capabilities_ready = true;
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, AnalysisStatus::Running(_))
    }

    /// The asset the analysis targets: the explicit selection, else the first asset with video,
    /// else any asset.
    pub fn default_asset(&self, project: &Project) -> Option<AssetId> {
        if let Some(selected) = self.selected_asset
            && project.asset(selected).is_some()
        {
            return Some(selected);
        }
        project
            .assets
            .iter()
            .find(|asset| asset.metadata.has_video)
            .or_else(|| project.assets.first())
            .map(|asset| asset.id)
    }

    pub fn begin(&mut self) {
        self.result = None;
        self.thumbnails.clear();
        self.status = AnalysisStatus::Running(AnalysisProgress {
            stage: AnalysisStage::Metadata,
            completed: 0,
            total: 1,
            detail: "開始".to_owned(),
        });
    }

    pub fn finish(&mut self, outcome: Result<AnalysisResult, AnalysisErrorView>) {
        match outcome {
            Ok(result) => {
                let from_cache = result.from_cache;
                // Hydrate the embedding status from disk, but only when the stored vectors were
                // derived from this exact analysis: otherwise they are stale.
                self.semantic.embedding = crate::knowledge::load_embeddings(&result.cache_dir)
                    .filter(|set| set.is_from_analysis(&result.signature))
                    .map(|set| EmbeddingStatus::Ready {
                        model: set.model,
                        dimensions: set.dimensions,
                        scenes: set.vectors.len(),
                    })
                    .unwrap_or(EmbeddingStatus::Idle);
                self.semantic.results = None;
                self.status = AnalysisStatus::Done { from_cache };
                self.selected_asset = Some(result.media.media_id);
                self.result = Some(result);
                self.thumbnails.clear();
            }
            Err(AnalysisErrorView::Cancelled) => self.status = AnalysisStatus::Idle,
            Err(AnalysisErrorView::Message(message)) => {
                self.status = AnalysisStatus::Failed(message)
            }
        }
    }

    pub fn clear_result(&mut self) {
        self.result = None;
        self.thumbnails.clear();
        self.semantic.embedding = EmbeddingStatus::Idle;
        self.semantic.results = None;
        self.status = AnalysisStatus::Idle;
    }
}

/// Minimal view of the analysis failure reason, so the UI state does not depend on the
/// knowledge module's error internals.
pub enum AnalysisErrorView {
    Cancelled,
    Message(String),
}

pub(super) fn ai_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    analysis_panel(
        ui,
        context.project,
        context.analysis,
        context.actions,
        context.playhead,
        context.power_profile,
        context.hardware,
    );
}

#[allow(clippy::too_many_arguments)]
fn analysis_panel(
    ui: &mut Ui,
    project: &Project,
    analysis: &mut AnalysisState,
    actions: &mut Vec<UiAction>,
    playhead: TimelineTime,
    power_profile: &mut PowerProfile,
    hardware: &HardwareCapabilities,
) {
    ui.heading("AIワークスペース");
    ui.label(
        "① モデルを設定 → ② 素材を解析 → ③ 作りたい動画を指示。AIは解析・検索・編集・声入れ・字幕・書き出しまで自分で実行できます。",
    );
    ui.separator();

    model_section(ui, analysis);
    ui.add_space(8.0);

    ui.heading("② 素材の解析");
    capabilities_section(ui, analysis);
    ui.add_space(6.0);

    if analysis.capabilities_ready && !analysis.capabilities.ffmpeg {
        ui.colored_label(
            egui::Color32::from_rgb(220, 120, 60),
            "FFmpegが見つからないため解析できません。FFmpeg/FFprobeをインストールしてください。",
        );
    }

    target_section(ui, project, analysis);
    ui.add_space(6.0);
    options_section(ui, analysis);
    ui.add_space(6.0);
    let target = analysis.default_asset(project);
    controls_section(ui, analysis, actions, target.is_some());
    ui.add_space(6.0);
    report_section(ui, analysis, actions, playhead, target);
    ui.add_space(8.0);

    assistant_section(ui, analysis, actions);
    ui.add_space(8.0);

    semantic_section(ui, analysis, actions);
    ui.add_space(6.0);
    runtime_section(ui, power_profile, hardware);
}

/// Step ①: both model slots in one place, so the AI panel is a single workflow rather than
/// several disconnected settings blocks.
fn model_section(ui: &mut Ui, analysis: &mut AnalysisState) {
    egui::CollapsingHeader::new("① AIモデル")
        .default_open(true)
        .show(ui, |ui| {
            ui.small(
                "チャット（AIアシスタント）と埋め込み（意味検索）を個別に設定します。DeepSeekは埋め込みAPIを提供しないため、意味検索には別のモデルを選びます。入力したAPIキーは保存されません。",
            );
            egui::CollapsingHeader::new("チャットモデル（AIアシスタント）")
                .default_open(true)
                .show(ui, |ui| {
                    let waiting = analysis.chat.is_waiting();
                    ui.add_enabled_ui(!waiting, |ui| {
                        let presets: Vec<_> = presets_for(PresetUse::Chat).collect();
                        model_config_editor(ui, &mut analysis.chat.config, &presets, "chat_model");
                    });
                });
            egui::CollapsingHeader::new("埋め込みモデル（意味検索・任意）")
                .default_open(false)
                .show(ui, |ui| {
                    let running =
                        matches!(analysis.semantic.embedding, EmbeddingStatus::Running { .. });
                    let previous_preset = analysis.semantic.config.preset_id.clone();
                    ui.add_enabled_ui(!running, |ui| {
                        let presets: Vec<_> = presets_for(PresetUse::Embedding).collect();
                        model_config_editor(
                            ui,
                            &mut analysis.semantic.config,
                            &presets,
                            "semantic_model",
                        );
                    });
                    if analysis.semantic.config.preset_id != previous_preset {
                        analysis.semantic.probe = ProbeStatus::Idle;
                        analysis.semantic.error = None;
                    }
                });
        });
}

/// Preview power profile and detected hardware/codec support. This lived in the Phase 1 AI
/// placeholder tab; it is kept here so nothing regresses when that tab became the analysis
/// console.
fn runtime_section(ui: &mut Ui, power_profile: &mut PowerProfile, hardware: &HardwareCapabilities) {
    egui::CollapsingHeader::new("動作環境")
        .default_open(false)
        .show(ui, |ui| {
            egui::ComboBox::from_label("プレビュープロファイル")
                .selected_text(match power_profile {
                    PowerProfile::Eco => "省電力",
                    PowerProfile::Balanced => "標準",
                    PowerProfile::Performance => "高画質",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(power_profile, PowerProfile::Eco, "省電力 (360p / 15fps)");
                    ui.selectable_value(
                        power_profile,
                        PowerProfile::Balanced,
                        "標準 (540p / 30fps)",
                    );
                    ui.selectable_value(
                        power_profile,
                        PowerProfile::Performance,
                        "高画質 (1080p / 60fps)",
                    );
                });
            ui.label(format!("QSV: {}", yes_no(hardware.qsv_available)));
            ui.label(format!(
                "QSV H.264出力: {}",
                yes_no(hardware.qsv_h264_encode)
            ));
            ui.label(format!(
                "AV1出力: {}",
                yes_no(hardware.supports_video_encoder(VideoEncoder::Av1Svt.ffmpeg_name()))
            ));
            ui.label(format!(
                "VAAPI H.264: {}",
                yes_no(hardware.vaapi_h264_encode)
            ));
            ui.label(format!(
                "NVENC H.264: {}",
                yes_no(hardware.nvenc_h264_encode)
            ));
            ui.label(format!("Vulkan: {}", yes_no(hardware.vulkan_available)));
        });
}

fn capabilities_section(ui: &mut Ui, analysis: &mut AnalysisState) {
    egui::CollapsingHeader::new("解析エンジン")
        .default_open(false)
        .show(ui, |ui| {
            if !analysis.capabilities_ready {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.small("検出中…");
                });
                return;
            }
            ui.label(format!(
                "FFmpeg シーン検出: {}",
                yes_no(analysis.capabilities.scene_detection)
            ));
            ui.label(format!(
                "OCR (tesseract): {}",
                analysis.capabilities.ocr_label()
            ));
            ui.label(format!(
                "文字起こし (whisper.cpp): {}",
                analysis
                    .capabilities
                    .asr
                    .clone()
                    .unwrap_or_else(|| "未検出".to_owned())
            ));
            ui.small(
                "解析エンジンは交換可能です。モデル接続はAIアシスタントと意味検索で設定します。",
            );
        });
}

fn target_section(ui: &mut Ui, project: &Project, analysis: &mut AnalysisState) {
    let running = analysis.is_running();
    ui.horizontal(|ui| {
        ui.label("対象メディア");
        let selected_label = analysis
            .default_asset(project)
            .and_then(|id| project.asset(id))
            .map(|asset| asset.name.clone())
            .unwrap_or_else(|| "（メディアなし）".to_owned());
        egui::ComboBox::from_id_salt("analysis_asset")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for asset in &project.assets {
                    let suffix = if asset.metadata.has_video {
                        ""
                    } else {
                        "（音声のみ）"
                    };
                    ui.add_enabled_ui(!running, |ui| {
                        ui.selectable_value(
                            &mut analysis.selected_asset,
                            Some(asset.id),
                            format!("{}{suffix}", asset.name),
                        );
                    });
                }
            });
    });
    if project.assets.is_empty() {
        ui.small("メディアを追加すると解析できます。");
    }
}

fn options_section(ui: &mut Ui, analysis: &mut AnalysisState) {
    let running = analysis.is_running();
    egui::CollapsingHeader::new("解析設定")
        .default_open(true)
        .show(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| {
                ui.add(
                    egui::Slider::new(&mut analysis.options.cut_threshold, 0.05..=0.95)
                        .text("カット検出しきい値"),
                );
                ui.add(
                    egui::Slider::new(&mut analysis.options.scene_threshold, 0.05..=0.99)
                        .text("シーン境界しきい値"),
                );
                ui.add(
                    egui::Slider::new(&mut analysis.options.max_scene_seconds, 2.0..=120.0)
                        .text("最大シーン長 (秒)"),
                );
                ui.add(
                    egui::Slider::new(&mut analysis.options.thumbnail_width, 64..=1280)
                        .text("サムネイル幅 (px)"),
                );
                ui.add(
                    egui::Slider::new(&mut analysis.options.threads, 1..=16).text("解析スレッド数"),
                );

                ui.separator();
                ui.checkbox(&mut analysis.options.ocr, "OCR（キーフレームの文字認識）");
                if analysis.options.ocr {
                    ui.horizontal(|ui| {
                        ui.label("言語");
                        ui.add(
                            egui::TextEdit::singleline(&mut analysis.options.ocr_languages)
                                .hint_text("例: jpn+eng")
                                .desired_width(120.0),
                        );
                        ui.small("空欄なら利用可能な言語を自動選択");
                    });
                }

                ui.checkbox(
                    &mut analysis.options.transcript,
                    "文字起こし（whisper.cpp）",
                );
                if analysis.options.transcript {
                    ui.horizontal(|ui| {
                        ui.label("モデル");
                        ui.add(
                            egui::TextEdit::singleline(&mut analysis.options.asr_model_path)
                                .hint_text("/path/to/ggml-base.bin")
                                .desired_width(240.0),
                        );
                    });
                    ui.small("whisper.cpp のモデルファイルを指定してください。");
                }
            });
        });
}

fn controls_section(
    ui: &mut Ui,
    analysis: &mut AnalysisState,
    actions: &mut Vec<UiAction>,
    has_asset: bool,
) {
    let running = analysis.is_running();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !running && has_asset && analysis.capabilities.ffmpeg,
                egui::Button::new("▶ 解析開始"),
            )
            .clicked()
        {
            actions.push(UiAction::StartAnalysis);
        }
        if ui
            .add_enabled(
                !running && has_asset && analysis.capabilities.ffmpeg,
                egui::Button::new("⟳ 再解析"),
            )
            .on_hover_text("キャッシュを無視して最初から解析し直します")
            .clicked()
        {
            actions.push(UiAction::ReanalyzeAnalysis);
        }
        if ui
            .add_enabled(running, egui::Button::new("■ 中止"))
            .clicked()
        {
            actions.push(UiAction::CancelAnalysis);
        }
        ui.separator();
        if ui
            .add_enabled(
                analysis.result.is_some(),
                egui::Button::new("⭳ VIRを書き出す"),
            )
            .on_hover_text("解析結果をJSONとして保存し、外部ツールへ渡せるようにします")
            .clicked()
        {
            actions.push(UiAction::ExportVir);
        }
        if ui
            .add_enabled(!running, egui::Button::new("🗑 キャッシュ削除"))
            .clicked()
        {
            actions.push(UiAction::ClearAnalysisCache);
        }
    });

    match &analysis.status {
        AnalysisStatus::Idle => {}
        AnalysisStatus::Running(progress) => {
            ui.add(egui::ProgressBar::new(progress.fraction()).show_percentage());
            ui.small(format!("{}: {}", progress.stage.label(), progress.detail));
        }
        AnalysisStatus::Done { from_cache } => {
            ui.small(if *from_cache {
                "完了（キャッシュから復元）"
            } else {
                "完了"
            });
        }
        AnalysisStatus::Failed(message) => {
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                format!("失敗: {message}"),
            );
        }
    }
}

fn report_section(
    ui: &mut Ui,
    analysis: &mut AnalysisState,
    actions: &mut Vec<UiAction>,
    playhead: TimelineTime,
    target: Option<AssetId>,
) {
    let AnalysisState {
        result, thumbnails, ..
    } = analysis;
    let Some(result) = result.as_ref() else {
        return;
    };
    let Some(media) = result.vir.media.first() else {
        return;
    };
    ui.separator();
    if target != Some(media.media_id) {
        ui.small("選択中のメディアはまだ解析されていません。「解析開始」で解析できます。");
        return;
    }
    ui.heading("解析結果");
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("シーン {}", media.scenes.len()));
        ui.separator();
        let shots: usize = media.scenes.iter().map(|scene| scene.shots.len()).sum();
        ui.label(format!("ショット {shots}"));
        ui.separator();
        ui.label(format!("発話 {}", media.transcript.len()));
        ui.separator();
        ui.label(format!(
            "VIR {VIR_SCHEMA_VERSION} / 解析 {VIR_ANALYSIS_VERSION}"
        ));
        if result.from_cache {
            ui.separator();
            ui.small("キャッシュ");
        }
    });
    provenance_summary(ui, media);

    if !result.warnings.is_empty() {
        egui::CollapsingHeader::new(format!("⚠ 警告 ({})", result.warnings.len()))
            .default_open(false)
            .show(ui, |ui| {
                for warning in &result.warnings {
                    ui.small(warning);
                }
            });
    }

    if media.scenes.is_empty() {
        ui.label("シーンは検出されませんでした。");
        return;
    }

    let row_height = 104.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, media.scenes.len(), |ui, range| {
            for index in range {
                let scene = &media.scenes[index];
                let current = scene.start <= playhead && playhead < scene.end;
                let frame = ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        for keyframe in &scene.keyframes {
                            match thumbnail(ui.ctx(), result, thumbnails, keyframe) {
                                Some(texture) => {
                                    ui.add(
                                        egui::Image::new(&texture)
                                            .fit_to_exact_size(egui::vec2(160.0, 90.0)),
                                    );
                                }
                                None => thumbnail_placeholder(ui),
                            }
                        }
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                let color = if current {
                                    egui::Color32::from_rgb(120, 190, 255)
                                } else {
                                    ui.visuals().strong_text_color()
                                };
                                ui.colored_label(color, format!("{} – {}", scene.start, scene.end));
                                if let Some(importance) = scene.importance {
                                    ui.small(format!("重要度 {importance:.2}"));
                                }
                                ui.small(format!("ショット {}", scene.shots.len()));
                            });
                            if !scene.summary.is_empty() {
                                ui.label(&scene.summary);
                            }
                            if let Some(transcript) = &scene.transcript {
                                ui.small(format!("🗣 {}", ellipsize(transcript, 80)));
                            }
                            if !scene.ocr.is_empty() {
                                ui.small(format!("🔤 {}", ellipsize(&scene.ocr.join(" / "), 80)));
                            }
                        });
                    });
                });
                let response = ui.interact(
                    frame.response.rect,
                    ui.id().with(("analysis-scene", index)),
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
        });
}

fn provenance_summary(ui: &mut Ui, media: &crate::protocol::VirMedia) {
    let fingerprint = &media.fingerprint;
    ui.small(format!(
        "{} ・ {}x{} ・ {:.2}fps ・ 指紋 {}",
        media.path.rsplit('/').next().unwrap_or(&media.path),
        media.width,
        media.height,
        media.fps,
        &fingerprint[..fingerprint.len().min(8)]
    ));
}

pub(super) fn thumbnail_placeholder(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 90.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
}

/// Loads a keyframe thumbnail into an egui texture on first display and caches the handle.
/// A failed decode is cached as `None` so it is not retried every frame.
pub(super) fn thumbnail(
    context: &egui::Context,
    result: &AnalysisResult,
    cache: &mut HashMap<Uuid, Option<TextureHandle>>,
    keyframe: &VirFrame,
) -> Option<TextureHandle> {
    if let Some(existing) = cache.get(&keyframe.frame_id) {
        return existing.clone();
    }
    let texture = keyframe.thumbnail.as_ref().and_then(|file_name| {
        let path = result.cache_dir.join(file_name);
        let image = image::open(&path).ok()?;
        let (width, height) = image.dimensions();
        let color = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            image.to_rgba8().as_raw(),
        );
        Some(context.load_texture(
            format!("vir-thumb-{}", keyframe.frame_id),
            color,
            TextureOptions::LINEAR,
        ))
    });
    cache.insert(keyframe.frame_id, texture.clone());
    texture
}

pub(super) fn ellipsize(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let shortened: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "利用可能" } else { "未検出" }
}
