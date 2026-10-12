use crate::agent::{self, tools};
use crate::domain::{
    AssetId, Character, Clip, ClipId, ItemId, Layer, LayerId, MediaAsset, Project, ShapeItem,
    ShapeKind, SpeechItem, TextItem, TextTemplate, TimelineItem, TimelineTime, Transform,
    VoiceSettings,
};
use crate::editing::{EditCommand, EditHistory};
use crate::knowledge::{
    AnalysisError, AnalysisEvent, AnalysisOptions, AnalysisRequest, AnalysisWorker, EmbeddingEvent,
    EmbeddingWorker, KnowledgeCapabilities, SceneHit, analysis_cache_dir, detect_capabilities,
    rank,
};
use crate::media::{
    AudioPreview, ExportOptions, HardwareCapabilities, IMAGE_EXTENSIONS, PowerProfile,
    PreviewRequest, PreviewWorker, detect_hardware_capabilities, export_frame_png,
    export_project_with_options,
    fallback_font_families, list_font_families, preview_font_key, probe_media, resolve_font_file,
    synthesize,
    voicevox_cache_dir,
};
use crate::models::{ModelConfig, ModelError, OpenAiCompatible, ProbeResult};
use crate::project_store;
use crate::protocol::{AiError, AiImage, AiToolCall, AiToolResponse, validate_aep};
use crate::ui::{
    AnalysisErrorView, AnalysisState, AnalysisStatus, AppTheme, ChatStatus, EmbeddingStatus,
    EditorTab, FileBrowserState, PanelContext, PreviewGalleyCache, ProbeStatus, UiAction,
    WaveformCache, build_visuals, create_default_dock, roll_random_seed,
};
use eframe::egui::{
    self, ColorImage, Key, KeyboardShortcut, Modifiers, TextureHandle, TextureOptions,
};
use egui_dock::{DockArea, DockState, Style};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel, sync_channel};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct VideoEditorApp {
    project: Project,
    project_path: Option<PathBuf>,
    history: EditHistory,
    dock: DockState<crate::ui::EditorTab>,
    selected_item: Option<ItemId>,
    selected_layer: Option<LayerId>,
    /// Last item copied with Ctrl+C. Pasted as a fresh-id duplicate via `EditCommand::PasteItem`.
    clipboard: Option<TimelineItem>,
    playhead: TimelineTime,
    playing: bool,
    last_tick: Instant,
    preview_generation: u64,
    preview_request_seq: u64,
    preview_frame_seq: u64,
    last_preview_time: Option<TimelineTime>,
    /// Clip ids requested on the previous preview pass. A changed visible set (layer toggled,
    /// item moved) must bypass the playhead-delta throttle or the new clip would never appear.
    last_requested_clips: Vec<ItemId>,
    /// The front-most visible clip; its decode failure is the one surfaced as `preview_error`.
    preview_main_clip: Option<ItemId>,
    /// Per-clip still-image short-circuit: the asset + profile a cached texture was decoded
    /// with. While these match, a still image needs no new frame at any playhead position.
    preview_stills: HashMap<ItemId, (AssetId, PowerProfile)>,
    preview_worker: PreviewWorker,
    audio_preview: AudioPreview,
    /// One texture per visible visual clip, painted as a layer stack (back-to-front).
    preview_textures: HashMap<ItemId, TextureHandle>,
    preview_error: Option<String>,
    preview_qsv_active: bool,
    pixels_per_second: f32,
    snap_enabled: bool,
    /// Screen rect of the timeline canvas from the previous frame, used to place OS file drops.
    timeline_drop_target: Option<egui::Rect>,
    anim_property_draft: crate::domain::AnimProperty,
    export_options: ExportOptions,
    power_profile: PowerProfile,
    hardware: HardwareCapabilities,
    /// Hardware/codec detection runs on a worker thread so the first frame is not blocked by
    /// probing FFmpeg and the GPU.
    hardware_receiver: Option<Receiver<HardwareCapabilities>>,
    /// System font families for the inspector picker. Listed on a worker thread (`fc-list`
    /// can be slow on machines with many fonts) so the first frame is not blocked; until the
    /// list arrives the picker shows the built-in fallback names.
    available_fonts: Vec<String>,
    fonts_receiver: Option<Receiver<Vec<String>>>,
    /// Set when timeline text content may have changed, so `ensure_preview_fonts` rescans.
    /// Idle frames skip the per-item font walk entirely.
    fonts_dirty: bool,
    /// egui font definitions with system fonts registered so far. Kept here because egui
    /// does not expose its current definitions for reading back.
    font_definitions: egui::FontDefinitions,
    /// Preview font keys (see `media::preview_font_key`) that are already active in egui.
    preview_fonts: HashSet<String>,
    /// Preview font keys that `set_fonts` was called for, but which only become active in
    /// egui at the start of the next pass (frame).
    preview_fonts_queued: HashSet<String>,
    /// Cross-frame cache of laid-out preview text galleys (see `ui::PreviewGalleyCache`).
    preview_galley_cache: PreviewGalleyCache,
    /// On-demand waveform peak cache shared by the 音声波形 / 音量モニタ panels.
    peak_cache: WaveformCache,
    /// Filesystem navigation for the ファイルブラウザ panel (app memory only).
    file_browser: FileBrowserState,
    templates: Vec<TextTemplate>,
    voicevox_url: String,
    voicevox_speaker: u32,
    voicevox_text: String,
    /// Cast members (YMM4-style characters): each bundles a VOICEVOX speaker, its own voice
    /// tuning, and the subtitle template applied to its lines. One speaker may appear as several
    /// characters with different delivery, so the configuration unit is the character, not the id.
    characters: Vec<Character>,
    /// Character selected in the VOICEVOX bar. `None` means a bare speaker id is used instead.
    voicevox_character: Option<Uuid>,
    voicevox_receiver: Option<Receiver<Result<(String, MediaAsset), String>>>,
    status: String,
    dirty: bool,
    export_result: Option<Receiver<Result<PathBuf, String>>>,
    /// Single-frame thumbnail (`samune.png`) export running in the background. Independent of
    /// `export_result` so a video export and a thumbnail export do not block each other.
    thumbnail_result: Option<Receiver<Result<PathBuf, String>>>,
    /// Selected UI theme and the `(hue, saturation)` that [`AppTheme::RandomColor`] was last
    /// drawn from, so a restart restores the exact random palette.
    app_theme: AppTheme,
    random_theme_seed: (f32, f32),
    /// Phase 2 analysis workspace: target, options, progress, VIR result and thumbnail textures.
    analysis: AnalysisState,
    analysis_worker: Option<AnalysisWorker>,
    /// Analysis-backend detection runs on a worker thread like the hardware probe, so the first
    /// frame is never blocked by probing FFmpeg/tesseract/whisper.
    knowledge_receiver: Option<Receiver<KnowledgeCapabilities>>,
    /// Scene embedding runs on its own worker because it talks to a model (local or network).
    embedding_worker: Option<EmbeddingWorker>,
    /// One-shot model connection test.
    probe_task: Option<Receiver<Result<ProbeResult, ModelError>>>,
    /// One-shot semantic search: embeds the query, then ranks the cached scene vectors.
    search_task: Option<Receiver<Result<Vec<SceneHit>, ModelError>>>,
    /// One-shot assistant turn. Chat can be slow (a reasoner model especially), so it never runs
    /// on the UI thread.
    chat_task: Option<Receiver<Result<AiToolResponse, AiError>>>,
    /// The async tool the agent is currently waiting on (VOICEVOX synthesis or export), if any.
    agent_async: Option<AgentAsync>,
    /// Result channel for an `AgentAsync::Job` (for example an agent-run scene search).
    agent_job: Option<Receiver<Result<String, String>>>,
    /// A frame fetched by `get_frame`, attached to the conversation once the tool result is in.
    pending_agent_images: Vec<(String, AiImage)>,
    /// Model turns spent in the current request, to bound a runaway tool loop.
    agent_rounds: usize,
}

/// Upper bound on request/request+tools cycles for one user message.
const MAX_AGENT_ROUNDS: usize = 40;

/// An agent tool that finishes later than the frame that started it.
enum AgentAsync {
    Speech {
        call_id: String,
        start: TimelineTime,
        template: Option<Uuid>,
    },
    Export {
        call_id: String,
        destination: PathBuf,
    },
    /// A Phase 2 analysis running for the agent.
    Analyze { call_id: String },
    /// A self-contained background job whose result is a ready-made tool result string.
    Job { call_id: String, name: String },
}

impl VideoEditorApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        let font_definitions = base_font_definitions();
        creation_context
            .egui_ctx
            .set_fonts(font_definitions.clone());
        let app_theme: AppTheme = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "app_theme"))
            .unwrap_or_default();
        let random_theme_seed: (f32, f32) = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "random_theme_seed"))
            .unwrap_or_else(roll_random_seed);
        creation_context
            .egui_ctx
            .set_visuals(build_visuals(app_theme, random_theme_seed));
        configure_style(&creation_context.egui_ctx);
        let repaint_context = creation_context.egui_ctx.clone();
        let power_profile = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "power_profile"))
            .unwrap_or_default();
        let dock = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "dock_layout_v6"))
            .unwrap_or_else(create_default_dock);
        let project = Project::new("無題のプロジェクト");
        let selected_layer = project.sequence.layers.first().map(|layer| layer.id);
        let (hardware_tx, hardware_rx) = sync_channel(1);
        let _ = std::thread::Builder::new()
            .name("hardware-detect".into())
            .spawn(move || {
                let _ = hardware_tx.send(detect_hardware_capabilities());
            });
        let (knowledge_tx, knowledge_rx) = sync_channel(1);
        let _ = std::thread::Builder::new()
            .name("knowledge-detect".into())
            .spawn(move || {
                let _ = knowledge_tx.send(detect_capabilities());
            });
        let (fonts_tx, fonts_rx) = sync_channel(1);
        let _ = std::thread::Builder::new()
            .name("font-list".into())
            .spawn(move || {
                let _ = fonts_tx.send(list_font_families());
            });
        let analysis_options: AnalysisOptions = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "analysis_options"))
            .unwrap_or_default();
        let model_config = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "model_config"))
            .unwrap_or_default();
        let mut analysis = AnalysisState::new(analysis_options);
        analysis.semantic.config = model_config;
        analysis.semantic.config.migrate_timeout();
        analysis.chat.config = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "chat_config"))
            .unwrap_or_else(crate::models::default_chat_config);
        analysis.chat.config.migrate_timeout();
        let mut templates: Vec<TextTemplate> = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "text_templates"))
            .unwrap_or_default();
        migrate_template_positions(&mut templates);
        let voicevox_url = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "voicevox_url"))
            .unwrap_or_else(|| "http://127.0.0.1:50021".to_owned());
        let voicevox_speaker = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "voicevox_speaker"))
            .unwrap_or(1u32);
        // Cast members are app-level settings like templates. An earlier build stored only a
        // speaker → template map; on first run with the new format migrate those bindings into
        // characters so existing setups keep their styles. Keyed on the stored value being absent
        // (not merely empty) so deleting every character does not resurrect them next launch.
        let stored_characters: Option<Vec<Character>> = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "characters"));
        let mut characters = match stored_characters {
            Some(characters) => characters,
            None => {
                let legacy: HashMap<u32, Uuid> = creation_context
                    .storage
                    .and_then(|storage| eframe::get_value(storage, "speaker_templates"))
                    .unwrap_or_default();
                let mut speakers: Vec<u32> = legacy.keys().copied().collect();
                speakers.sort_unstable();
                speakers
                    .into_iter()
                    .map(|speaker| {
                        let mut character = Character::new(format!("話者 {speaker}"), speaker);
                        character.template = legacy.get(&speaker).copied();
                        character
                    })
                    .collect()
            }
        };
        characters.retain(|character| {
            character
                .template
                .is_none_or(|id| templates.iter().any(|template| template.id == id))
        });
        let voicevox_character = creation_context
            .storage
            .and_then(|storage| eframe::get_value(storage, "voicevox_character"))
            .filter(|id| characters.iter().any(|character| character.id == *id));
        Self {
            project,
            project_path: None,
            history: EditHistory::default(),
            dock,
            selected_item: None,
            selected_layer,
            clipboard: None,
            playhead: TimelineTime::ZERO,
            playing: false,
            last_tick: Instant::now(),
            preview_generation: 0,
            preview_request_seq: 0,
            preview_frame_seq: 0,
            last_preview_time: None,
            last_requested_clips: Vec::new(),
            preview_main_clip: None,
            preview_stills: HashMap::new(),
            preview_worker: PreviewWorker::spawn(move || repaint_context.request_repaint()),
            audio_preview: AudioPreview::new(),
            preview_textures: HashMap::new(),
            preview_error: None,
            preview_qsv_active: false,
            pixels_per_second: 48.0,
            snap_enabled: true,
            timeline_drop_target: None,
            anim_property_draft: crate::domain::AnimProperty::X,
            export_options: ExportOptions::default(),
            power_profile,
            hardware: HardwareCapabilities::default(),
            hardware_receiver: Some(hardware_rx),
            available_fonts: fallback_font_families(),
            fonts_receiver: Some(fonts_rx),
            fonts_dirty: true,
            font_definitions,
            preview_fonts: HashSet::new(),
            preview_fonts_queued: HashSet::new(),
            preview_galley_cache: PreviewGalleyCache::default(),
            peak_cache: WaveformCache::default(),
            file_browser: FileBrowserState::default(),
            templates,
            voicevox_url,
            voicevox_speaker,
            voicevox_text: String::new(),
            characters,
            voicevox_character,
            voicevox_receiver: None,
            status: "準備完了".into(),
            dirty: false,
            export_result: None,
            thumbnail_result: None,
            app_theme,
            random_theme_seed,
            analysis,
            analysis_worker: None,
            knowledge_receiver: Some(knowledge_rx),
            embedding_worker: None,
            probe_task: None,
            search_task: None,
            chat_task: None,
            agent_async: None,
            agent_job: None,
            pending_agent_images: Vec::new(),
            agent_rounds: 0,
        }
    }

    fn execute(&mut self, command: EditCommand) -> bool {
        // Only commands that change the decoded video frame invalidate the preview. Text and
        // shape edits are drawn as overlays on top of the frame, so re-decoding on every drag
        // would be pure waste (and would interrupt audio playback).
        let invalidates_video = match &command {
            EditCommand::ImportAsset { .. }
            | EditCommand::SplitClip { .. }
            | EditCommand::TrimClip { .. }
            | EditCommand::MoveClip { .. }
            | EditCommand::DeleteClip { .. }
            | EditCommand::SetClipTransform { .. }
            | EditCommand::SetClipOpacity { .. }
            | EditCommand::SetClipSpeed { .. }
            | EditCommand::SetClipEffects { .. }
            | EditCommand::SetAnimTrack { .. }
            | EditCommand::RemoveAnimTrack { .. }
            | EditCommand::SetKeyframe { .. }
            | EditCommand::RemoveKeyframe { .. }
            | EditCommand::AddLayer { .. }
            | EditCommand::RemoveLayer { .. }
            | EditCommand::SetLayerHidden { .. }
            | EditCommand::ReorderLayer { .. } => true,
            EditCommand::MoveItem { item_id, .. }
            | EditCommand::ResizeItemStart { item_id, .. }
            | EditCommand::ResizeItemEnd { item_id, .. }
            | EditCommand::DeleteItem { item_id } => self.project.clip(*item_id).is_some(),
            // A pasted clip is new decoded footage; text, shape and speech subtitles are
            // overlays. A pasted speech only adds synthesized audio (handled below).
            EditCommand::PasteItem { item, .. } => item.as_clip().is_some(),
            _ => false,
        };
        let invalidates_audio = invalidates_video
            || matches!(
                &command,
                EditCommand::SetClipGain { .. }
                    | EditCommand::SetClipMuted { .. }
                    | EditCommand::SetClipSpeed { .. }
                    | EditCommand::SetKeyframe { .. }
                    | EditCommand::RemoveKeyframe { .. }
                    | EditCommand::SetAnimTrack { .. }
                    | EditCommand::RemoveAnimTrack { .. }
                    | EditCommand::SetLayerMuted { .. }
                    | EditCommand::AddSpeech { .. }
                    | EditCommand::SplitSpeech { .. }
            )
            || matches!(
                &command,
                EditCommand::PasteItem { item, .. } if item.as_speech().is_some()
            );
        match self.history.execute(&mut self.project, command) {
            Ok(()) => {
                if invalidates_audio {
                    self.audio_preview.stop();
                }
                self.dirty = true;
                self.fonts_dirty = true;
                if invalidates_video {
                    self.preview_generation = self.preview_generation.wrapping_add(1);
                    self.last_preview_time = None;
                }
                self.status = "編集を適用しました".into();
                if invalidates_video && !self.playing {
                    self.request_preview();
                }
                true
            }
            Err(error) => {
                self.status = format!("編集できません: {error}");
                false
            }
        }
    }

    fn undo(&mut self) {
        if self.history.undo(&mut self.project) {
            self.dirty = true;
            self.fonts_dirty = true;
            self.status = "元に戻しました".into();
            self.preview_generation = self.preview_generation.wrapping_add(1);
            self.last_preview_time = None;
            self.request_preview();
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.project) {
            self.dirty = true;
            self.fonts_dirty = true;
            self.status = "やり直しました".into();
            self.preview_generation = self.preview_generation.wrapping_add(1);
            self.last_preview_time = None;
            self.request_preview();
        }
    }

    /// Finds a layer that can host a new item of `duration` starting at `start` without
    /// overlapping anything, preferring the currently active layer. Creates a brand new
    /// layer on top if nothing suitable exists. This is what lets media/text be dropped
    /// straight onto the timeline without ever visiting a separate media browser.
    fn resolve_target_layer(&mut self, start: TimelineTime, duration: TimelineTime) -> LayerId {
        // Hidden layers are deliberately excluded: dropping new content onto a layer that
        // is not drawn would look like the item was never added at all.
        let fits = |layer: &Layer| -> bool {
            !layer.locked
                && !layer.hidden
                && layer.items.iter().all(|item| {
                    let item_start = item.timeline_start();
                    let item_end = item.timeline_end();
                    start >= item_end || start + duration <= item_start
                })
        };
        if let Some(id) = self.selected_layer
            && let Some(layer) = self.project.layer(id)
            && fits(layer)
        {
            return id;
        }
        if let Some(layer) = self
            .project
            .sequence
            .layers
            .iter()
            .find(|layer| fits(layer))
        {
            return layer.id;
        }
        let new_layer = Layer::new(format!(
            "レイヤー {}",
            self.project.sequence.layers.len() + 1
        ));
        let id = new_layer.id;
        self.execute(EditCommand::AddLayer {
            layer: new_layer,
            index: Some(0),
        });
        self.selected_layer = Some(id);
        id
    }

    fn import_media(&mut self) {
        const AV_EXTENSIONS: [&str; 13] = [
            "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "aac", "flac", "ogg",
            "gif",
        ];
        let all_extensions: Vec<&str> = AV_EXTENSIONS
            .iter()
            .copied()
            .chain(IMAGE_EXTENSIONS.iter().copied())
            .collect();
        // Multiple files can be selected at once and are appended in order, matching what a
        // multi-file OS drop already does.
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("動画・音声・画像", &all_extensions)
            .add_filter("動画・音声", &AV_EXTENSIONS)
            .add_filter("画像", &IMAGE_EXTENSIONS)
            .pick_files()
        else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let timeline_start = self.project.duration();
        self.import_paths_at(paths, timeline_start);
    }

    fn save_project(&mut self, force_dialog: bool) {
        let path = if force_dialog || self.project_path.is_none() {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Yuki Video Project", &["yukivproj"])
                .set_file_name(format!("{}.yukivproj", self.project.title))
                .save_file()
            else {
                return;
            };
            path
        } else {
            self.project_path.clone().expect("checked above")
        };
        match project_store::save(&self.project, &path) {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.dirty = false;
                self.status = format!("{} に保存しました", path.display());
            }
            Err(error) => self.status = format!("保存に失敗しました: {error}"),
        }
    }

    fn open_project(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Yuki Video Project", &["yukivproj"])
            .pick_file()
        else {
            return;
        };
        match project_store::load(&path) {
            Ok(project) => {
                self.project = project;
                self.project_path = Some(path.clone());
                self.history.clear();
                self.selected_item = None;
                self.selected_layer = self.project.sequence.layers.first().map(|layer| layer.id);
                self.playhead = TimelineTime::ZERO;
                self.playing = false;
                self.audio_preview.stop();
                self.preview_generation = self.preview_generation.wrapping_add(1);
                self.last_preview_time = None;
                self.dirty = false;
                self.fonts_dirty = true;
                self.status = format!("{} を開きました", path.display());
                self.request_preview();
            }
            Err(error) => self.status = format!("開けませんでした: {error}"),
        }
    }

    fn start_export(&mut self, context: &egui::Context) {
        if self.export_result.is_some() {
            return;
        }
        let Some(destination) = rfd::FileDialog::new()
            .add_filter("MP4動画", &["mp4"])
            .set_file_name(format!("{}.mp4", self.project.title))
            .save_file()
        else {
            return;
        };
        self.start_export_to(destination, context);
    }

    fn poll_export(&mut self, context: &egui::Context) {
        let Some(receiver) = &self.export_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(path)) => {
                self.export_result = None;
                self.status = format!("{} へ書き出しました", path.display());
                if let Some((call_id, destination)) = self.take_agent_export() {
                    self.analysis.chat.notice =
                        Some(format!("動画を書き出しました: {}", destination.display()));
                    self.finish_agent_tool(
                        &call_id,
                        "export_video",
                        format!("書き出し完了: {}", destination.display()),
                        context,
                    );
                }
            }
            Ok(Err(error)) => {
                self.export_result = None;
                self.status = format!("書き出しに失敗しました: {error}");
                if let Some((call_id, _)) = self.take_agent_export() {
                    self.finish_agent_tool(
                        &call_id,
                        "export_video",
                        format!("エラー: {error}"),
                        context,
                    );
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.export_result = None;
                if let Some((call_id, _)) = self.take_agent_export() {
                    self.finish_agent_tool(
                        &call_id,
                        "export_video",
                        "エラー: 書き出しが中断されました".to_owned(),
                        context,
                    );
                }
            }
        }
    }

    fn poll_thumbnail(&mut self) {
        let Some(receiver) = &self.thumbnail_result else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(path)) => {
                self.thumbnail_result = None;
                self.status = format!(
                    "{} を保存しました",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            Ok(Err(error)) => {
                self.thumbnail_result = None;
                self.status = format!("サムネの書き出しに失敗しました: {error}");
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.thumbnail_result = None;
                self.status = "サムネの書き出しに失敗しました: ワーカーが終了しました".into();
            }
        }
    }

    /// Picks up the result of the background hardware/codec probe. Until it arrives the editor
    /// simply uses the software paths, so startup is never blocked by FFmpeg/GPU probing.
    fn poll_hardware(&mut self) {
        let Some(receiver) = &self.hardware_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(hardware) => {
                self.hardware = hardware;
                self.hardware_receiver = None;
                // A newly available QSV path changes how frames are decoded; refresh the preview.
                self.preview_generation = self.preview_generation.wrapping_add(1);
                self.last_preview_time = None;
                self.request_preview();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.hardware_receiver = None;
            }
        }
    }

    /// Picks up the system font family list. Until it arrives the inspector picker shows the
    /// built-in fallback names, so startup is never blocked by `fc-list`.
    fn poll_fonts(&mut self) {
        let Some(receiver) = &self.fonts_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(families) => {
                if !families.is_empty() {
                    self.available_fonts = families;
                }
                self.fonts_receiver = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.fonts_receiver = None;
            }
        }
    }

    /// Picks up analysis-backend detection and drains analysis progress/result events. Events
    /// are collected before being handled so the worker borrow ends before the state mutates.
    fn poll_analysis(&mut self, context: &egui::Context) {
        if let Some(receiver) = &self.knowledge_receiver
            && let Ok(capabilities) = receiver.try_recv()
        {
            self.analysis.apply_capabilities(capabilities);
            self.knowledge_receiver = None;
        }

        let mut events = Vec::new();
        if let Some(worker) = &self.analysis_worker {
            while let Some(event) = worker.try_event() {
                events.push(event);
            }
        }
        if events.is_empty() {
            return;
        }

        let mut finished = false;
        for event in events {
            match event {
                AnalysisEvent::Progress(progress) => {
                    self.analysis.status = AnalysisStatus::Running(progress);
                }
                AnalysisEvent::Finished(outcome) => {
                    let view = match *outcome {
                        Ok(result) => Ok(result),
                        Err(AnalysisError::Cancelled) => Err(AnalysisErrorView::Cancelled),
                        Err(error) => Err(AnalysisErrorView::Message(error.to_string())),
                    };
                    let agent_summary = match &view {
                        Ok(result) => format!(
                            "解析が完了しました: シーン {} 件、長さ {:.2}s",
                            result.media.scenes.len(),
                            result.media.duration.as_seconds()
                        ),
                        Err(AnalysisErrorView::Cancelled) => {
                            "エラー: 解析が中止されました".to_owned()
                        }
                        Err(AnalysisErrorView::Message(message)) => {
                            format!("エラー: 解析に失敗しました: {message}")
                        }
                    };
                    match &view {
                        Ok(result) => {
                            self.status = if result.from_cache {
                                format!(
                                    "解析を読み込みました（シーン {}）",
                                    result.media.scenes.len()
                                )
                            } else {
                                format!(
                                    "解析が完了しました（シーン {}）",
                                    result.media.scenes.len()
                                )
                            };
                        }
                        Err(AnalysisErrorView::Cancelled) => {
                            self.status = "解析を中止しました".into();
                        }
                        Err(AnalysisErrorView::Message(message)) => {
                            self.status = format!("解析に失敗しました: {message}");
                        }
                    }
                    self.analysis.finish(view);
                    finished = true;
                    if let Some(call_id) = self.take_agent_analyze() {
                        self.finish_agent_tool(&call_id, "analyze_media", agent_summary, context);
                    }
                }
            }
        }
        if finished {
            self.analysis_worker = None;
        }
    }

    /// Starts analyzing the selected medium (or the first video asset) on a worker thread.
    fn start_analysis(&mut self, force: bool, context: &egui::Context) {
        let Some(asset_id) = self.analysis.default_asset(&self.project) else {
            self.status = "解析するメディアがありません".into();
            return;
        };
        self.start_analysis_for(asset_id, force, context);
    }

    /// Runs analysis for one asset. Shared by the manual button and the agent's `analyze_media`.
    fn start_analysis_for(&mut self, asset_id: AssetId, force: bool, context: &egui::Context) {
        if self.analysis_worker.is_some() {
            self.status = "解析が実行中です".into();
            return;
        }
        let Some(asset) = self.project.asset(asset_id).cloned() else {
            return;
        };

        let request = AnalysisRequest {
            asset,
            project_id: self.project.id,
            options: self.analysis.options.clone(),
            cache_root: analysis_cache_dir(),
            force,
        };
        self.analysis.selected_asset = Some(asset_id);
        self.analysis.begin();
        let repaint = context.clone();
        self.analysis_worker = Some(AnalysisWorker::spawn(request, move || {
            repaint.request_repaint();
        }));
        self.status = if force {
            "メディアを再解析しています…".into()
        } else {
            "メディアを解析しています…".into()
        };
    }

    fn cancel_analysis(&mut self) {
        if let Some(worker) = self.analysis_worker.take() {
            worker.cancel();
        }
        self.analysis.status = AnalysisStatus::Idle;
        self.status = "解析を中止しました".into();
    }

    /// Exports the current VIR so an external chat AI or another tool can consume it
    /// (AI-TOOLS.md section 8).
    fn export_vir(&mut self) {
        let Some(result) = &self.analysis.result else {
            self.status = "書き出せる解析結果がありません".into();
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("VIR (JSON)", &["json"])
            .set_file_name(format!("{}.vir.json", self.project.title))
            .save_file()
        else {
            return;
        };
        match serde_json::to_vec_pretty(&result.vir) {
            Ok(bytes) => match std::fs::write(&path, bytes) {
                Ok(()) => self.status = format!("VIRを書き出しました: {}", path.display()),
                Err(error) => self.status = format!("VIRを書き出せません: {error}"),
            },
            Err(error) => self.status = format!("VIRを書き出せません: {error}"),
        }
    }

    fn clear_analysis_cache(&mut self) {
        self.analysis.clear_result();
        match crate::knowledge::clear_analysis_cache(&analysis_cache_dir()) {
            Ok(()) => self.status = "解析キャッシュを削除しました".into(),
            Err(error) => self.status = format!("キャッシュを削除できません: {error}"),
        }
    }

    /// Drains the model-related background work: the connection probe, semantic search, and
    /// scene embedding progress/result.
    fn poll_model_tasks(&mut self) {
        if let Some(receiver) = &self.probe_task {
            let outcome = match receiver.try_recv() {
                Ok(value) => Some(value),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(ModelError::Connection(
                    "接続テストが中断されました".to_owned(),
                ))),
            };
            if let Some(outcome) = outcome {
                self.analysis.semantic.probe = match outcome {
                    Ok(probe) => ProbeStatus::Ok(probe.models),
                    Err(error) => ProbeStatus::Failed(error.to_string()),
                };
                self.probe_task = None;
            }
        }

        if let Some(receiver) = &self.search_task {
            let outcome = match receiver.try_recv() {
                Ok(value) => Some(value),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err(ModelError::Connection(
                    "検索が中断されました".to_owned(),
                ))),
            };
            if let Some(outcome) = outcome {
                match outcome {
                    Ok(hits) => {
                        let count = hits.len();
                        self.analysis.semantic.results = Some(hits);
                        self.analysis.semantic.error = None;
                        self.status = format!("意味検索: {count} 件");
                    }
                    Err(error) => {
                        self.analysis.semantic.results = None;
                        self.analysis.semantic.error = Some(format!("検索に失敗しました: {error}"));
                        self.status = format!("検索に失敗しました: {error}");
                    }
                }
                self.search_task = None;
            }
        }

        let mut events = Vec::new();
        if let Some(worker) = &self.embedding_worker {
            while let Some(event) = worker.try_event() {
                events.push(event);
            }
        }
        if events.is_empty() {
            return;
        }
        let mut finished = false;
        for event in events {
            match event {
                EmbeddingEvent::Progress { completed, total } => {
                    self.analysis.semantic.embedding =
                        EmbeddingStatus::Running { completed, total };
                }
                EmbeddingEvent::Finished(outcome) => {
                    match *outcome {
                        Ok(embedded) => {
                            self.analysis.semantic.embedding = EmbeddingStatus::Ready {
                                model: embedded.model,
                                dimensions: embedded.dimensions,
                                scenes: embedded.scenes,
                            };
                            self.status =
                                format!("埋め込みを生成しました（{} シーン）", embedded.scenes);
                        }
                        Err(error) => {
                            self.status = format!("埋め込みに失敗しました: {error}");
                            self.analysis.semantic.embedding =
                                EmbeddingStatus::Failed(error.to_string());
                        }
                    }
                    finished = true;
                }
            }
        }
        if finished {
            self.embedding_worker = None;
        }
    }

    /// Collects a finished agent turn. If it asked for tools, they are executed and the loop
    /// continues; if it is a final answer, the turn ends.
    fn poll_chat(&mut self, context: &egui::Context) {
        let Some(receiver) = &self.chat_task else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(value) => Some(value),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err(AiError::Request("AIの応答が中断されました".to_owned())))
            }
        };
        let Some(outcome) = outcome else {
            return;
        };
        self.chat_task = None;
        match outcome {
            Ok(response) => {
                let AiToolResponse {
                    content,
                    tool_calls,
                } = response;
                if tool_calls.is_empty() {
                    self.analysis
                        .chat
                        .push_assistant(content.clone(), Vec::new());
                    self.analysis.chat.draft = agent::extract_aep(&content, self.project.id);
                    self.analysis.chat.status = ChatStatus::Idle;
                    self.analysis.chat.activity = None;
                    self.status = "AIが応答しました".into();
                } else if !self.analysis.chat.allow_tools {
                    self.analysis.chat.push_assistant(content, tool_calls);
                    self.analysis.chat.status = ChatStatus::Idle;
                    self.analysis.chat.activity = None;
                    self.analysis.chat.notice = Some(
                        "ツール実行はオフです。上の操作案を確認し、実行するなら「ツール実行を許可」をオンにして再送してください。"
                            .to_owned(),
                    );
                } else {
                    self.analysis
                        .chat
                        .push_assistant(content, tool_calls.clone());
                    self.analysis.chat.pending_calls = tool_calls;
                    self.run_pending_tools(context);
                }
            }
            Err(error) => {
                self.analysis.chat.status = ChatStatus::Failed(error.to_string());
                self.analysis.chat.activity = None;
                self.analysis.chat.pending_calls.clear();
                self.status = format!("AIへの問い合わせに失敗しました: {error}");
            }
        }
    }

    /// Collects the result of a self-contained agent job (for example a scene search) and resumes
    /// the loop.
    fn poll_agent_job(&mut self, context: &egui::Context) {
        let Some(receiver) = &self.agent_job else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(value) => Some(value),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("バックグラウンド処理が中断されました".to_owned()))
            }
        };
        let Some(outcome) = outcome else {
            return;
        };
        self.agent_job = None;
        if let Some((call_id, name)) = self.take_agent_job() {
            let result = match outcome {
                Ok(text) => text,
                Err(error) => format!("エラー: {error}"),
            };
            self.finish_agent_tool(&call_id, &name, result, context);
        }
    }

    /// Asks the model for the next step of the agent loop.
    fn start_agent_request(&mut self, context: &egui::Context) {
        if self.chat_task.is_some() {
            return;
        }
        if self.agent_rounds >= MAX_AGENT_ROUNDS {
            self.analysis.chat.status = ChatStatus::Idle;
            self.analysis.chat.activity = None;
            self.analysis.chat.notice = Some(
                "AIの操作回数が上限に達したため停止しました。指示を分けて試してください。"
                    .to_owned(),
            );
            return;
        }
        let config = self.analysis.chat.config.clone();
        if let Err(error) = {
            let mut checked = config.clone();
            checked.clamp();
            checked.validate()
        } {
            self.analysis.chat.status = ChatStatus::Failed(error.to_string());
            self.analysis.chat.activity = None;
            return;
        }
        let media = self
            .analysis
            .result
            .as_ref()
            .map(|result| result.media.clone());
        let system = format!(
            "{}\n\n{}",
            agent::agent_system_prompt(),
            agent::project_context(&self.project, media.as_ref())
        );
        let turns = self.analysis.chat.turns.clone();
        let tools = tools::tool_specs();
        self.agent_rounds += 1;
        self.analysis.chat.set_activity("考えています…");
        self.analysis.chat.status = ChatStatus::Waiting;
        self.chat_task = Some(spawn_task(context, move || {
            agent::ask_tools_config(config, system, turns, tools)
        }));
        self.status = "AIが作業しています…".into();
    }

    /// Executes queued tool calls in order. Synchronous tools run inline; the first asynchronous
    /// tool takes over and the loop resumes when it finishes. When the queue drains, the model is
    /// asked for the next step.
    fn run_pending_tools(&mut self, context: &egui::Context) {
        if self.agent_async.is_some() {
            return;
        }
        loop {
            let Some(call) = self.analysis.chat.pending_calls.first().cloned() else {
                self.attach_pending_images();
                self.start_agent_request(context);
                return;
            };
            self.analysis.chat.pending_calls.remove(0);
            match tools::parse_call(&call) {
                Err(message) => {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        format!("エラー: {message}"),
                    );
                }
                Ok(invocation) if invocation.is_async() => {
                    if self.start_async_tool(&call, invocation, context) {
                        return;
                    }
                }
                Ok(invocation) => {
                    let result = self.execute_tool(invocation);
                    self.analysis
                        .chat
                        .push_tool_result(&call.id, &call.name, result);
                }
            }
        }
    }

    /// Appends frames the model asked for as one image turn. This happens only after every tool
    /// call in the batch has been answered, because the provider requires all `tool` messages to
    /// directly follow the assistant turn that requested them.
    fn attach_pending_images(&mut self) {
        if self.pending_agent_images.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_agent_images);
        let caption = pending
            .iter()
            .map(|(caption, _)| caption.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        let images = pending.into_iter().map(|(_, image)| image).collect();
        self.analysis.chat.push_images(caption, images);
    }

    /// Records an async tool result and resumes the loop.
    fn finish_agent_tool(
        &mut self,
        call_id: &str,
        name: &str,
        result: String,
        context: &egui::Context,
    ) {
        self.analysis.chat.activity = None;
        self.analysis.chat.push_tool_result(call_id, name, result);
        self.run_pending_tools(context);
    }

    fn cancel_chat(&mut self) {
        self.chat_task = None;
        // Drop the receivers for work this agent started, so a late result is not applied to the
        // timeline after the user stopped it.
        if matches!(self.agent_async, Some(AgentAsync::Speech { .. })) {
            self.voicevox_receiver = None;
        }
        if matches!(self.agent_async, Some(AgentAsync::Export { .. })) {
            self.export_result = None;
        }
        self.agent_async = None;
        self.analysis.chat.pending_calls.clear();
        self.pending_agent_images.clear();
        self.analysis.chat.activity = None;
        self.analysis.chat.status = ChatStatus::Idle;
        self.analysis.chat.notice = Some("AIの作業を中止しました。".to_owned());
        self.status = "AIの作業を中止しました".into();
    }

    /// Sends the composed message and starts the agent loop.
    fn send_chat(&mut self, context: &egui::Context) {
        if self.analysis.chat.is_waiting() {
            return;
        }
        let message = self.analysis.chat.input.trim().to_owned();
        if message.is_empty() {
            return;
        }
        self.analysis.chat.push_user(message);
        self.analysis.chat.input.clear();
        self.analysis.chat.draft = None;
        self.analysis.chat.notice = None;
        self.analysis.chat.pending_calls.clear();
        self.pending_agent_images.clear();
        self.agent_rounds = 0;
        self.start_agent_request(context);
    }

    /// Executes a synchronous tool directly against the project. Returns the result text the model
    /// will read; failures are reported as text so it can correct itself.
    fn execute_tool(&mut self, invocation: tools::ToolInvocation) -> String {
        match invocation {
            tools::ToolInvocation::GetProjectState => {
                let media = self
                    .analysis
                    .result
                    .as_ref()
                    .map(|result| result.media.clone());
                let mut lines = vec![agent::project_context(&self.project, media.as_ref())];
                lines.push(format!(
                    "playhead: {:.2}s / duration: {:.2}s",
                    self.playhead.as_seconds(),
                    self.project.duration().as_seconds()
                ));
                lines.push("assets (name => path):".to_owned());
                for asset in &self.project.assets {
                    lines.push(format!("- {} => {}", asset.name, asset.path.display()));
                }
                lines.join("\n")
            }
            tools::ToolInvocation::Seek { timestamp } => {
                self.seek_to(TimelineTime::from_seconds(timestamp));
                format!("再生位置を {timestamp:.2}s にしました")
            }
            tools::ToolInvocation::AddMedia {
                path,
                asset_name,
                timeline_start,
            } => {
                let asset = if let Some(name) = asset_name {
                    self.project.assets.iter().find(|a| a.name == name).cloned()
                } else if let Some(path) = path {
                    let path = PathBuf::from(path);
                    self.project
                        .assets
                        .iter()
                        .find(|a| a.path == path)
                        .cloned()
                        .or_else(|| probe_media(&path).ok())
                } else {
                    None
                };
                let Some(asset) = asset else {
                    return "エラー: 素材が見つかりません（path か asset_name を指定してください）"
                        .to_owned();
                };
                let start = timeline_start
                    .map(TimelineTime::from_seconds)
                    .unwrap_or(self.playhead);
                let duration = asset.metadata.duration;
                let layer_id = self.resolve_target_layer(start, duration);
                let before = self.item_ids();
                if !self.execute(EditCommand::ImportAsset {
                    asset: asset.clone(),
                    layer_id,
                    timeline_start: start,
                }) {
                    return format!("エラー: 追加できませんでした（{}）", self.status);
                }
                let new_id = self.item_ids().into_iter().find(|id| !before.contains(id));
                match new_id {
                    Some(id) => format!(
                        "追加しました: clip_id={id}, asset={}, {:.2}s–{:.2}s",
                        asset.name,
                        start.as_seconds(),
                        (start + duration).as_seconds()
                    ),
                    None => format!("追加しました: asset={}", asset.name),
                }
            }
            tools::ToolInvocation::SplitClip { clip_id, timestamp } => {
                if self.execute(EditCommand::SplitClip {
                    clip_id,
                    timeline_time: TimelineTime::from_seconds(timestamp),
                }) {
                    format!("分割しました: {clip_id} @ {timestamp:.2}s")
                } else {
                    format!("エラー: 分割できませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::TrimClip {
                clip_id,
                source_in,
                source_out,
            } => {
                if self.execute(EditCommand::TrimClip {
                    clip_id,
                    source_in: TimelineTime::from_seconds(source_in),
                    source_out: TimelineTime::from_seconds(source_out),
                }) {
                    format!("トリムしました: {clip_id} {source_in:.2}s–{source_out:.2}s")
                } else {
                    format!("エラー: トリムできませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::DeleteClip { clip_id } => {
                if self.execute(EditCommand::DeleteClip { clip_id }) {
                    self.selected_item = None;
                    format!("削除しました: {clip_id}")
                } else {
                    format!("エラー: 削除できませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::MoveClip {
                clip_id,
                timeline_start,
            } => {
                if self.execute(EditCommand::MoveClip {
                    clip_id,
                    timeline_start: TimelineTime::from_seconds(timeline_start),
                }) {
                    format!("移動しました: {clip_id} → {timeline_start:.2}s")
                } else {
                    format!(
                        "エラー: 移動できませんでした（他のclipと重なっている可能性があります: {}）",
                        self.status
                    )
                }
            }
            tools::ToolInvocation::SetVolume { clip_id, gain_db } => {
                if self.execute(EditCommand::SetClipGain { clip_id, gain_db }) {
                    format!("音量を設定しました: {clip_id} {gain_db:.1} dB")
                } else {
                    format!("エラー: 音量を設定できませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::MuteClip { clip_id, muted } => {
                if self.execute(EditCommand::SetClipMuted { clip_id, muted }) {
                    format!("ミュートを設定しました: {clip_id} {muted}")
                } else {
                    format!("エラー: ミュートを設定できませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::AddText {
                text,
                timeline_start,
                duration,
                template,
            } => {
                let start = timeline_start
                    .map(TimelineTime::from_seconds)
                    .unwrap_or(self.playhead);
                let duration = TimelineTime::from_seconds(duration.unwrap_or(3.0));
                let frame = self.project.sequence.settings.frame_size();
                let item = match template
                    .as_deref()
                    .and_then(|name| self.templates.iter().find(|t| t.name == name))
                {
                    Some(template) => template.build_text(text.clone(), start, duration),
                    None => {
                        let mut item = TextItem::new(text.clone(), start, frame);
                        item.duration = duration;
                        item
                    }
                };
                let id = item.id;
                let layer_id = self.resolve_target_layer(start, duration);
                if self.execute(EditCommand::AddText {
                    layer_id,
                    text: item,
                }) {
                    format!(
                        "テキストを追加しました: id={id}, {:.2}s–{:.2}s, 「{}」",
                        start.as_seconds(),
                        (start + duration).as_seconds(),
                        text
                    )
                } else {
                    format!("エラー: テキストを追加できませんでした（{}）", self.status)
                }
            }
            tools::ToolInvocation::GetFrame {
                scene_id,
                timestamp,
            } => {
                let target = timestamp
                    .map(TimelineTime::from_seconds)
                    .unwrap_or(self.playhead);
                let located = self.analysis.result.as_ref().and_then(|result| {
                    let media = &result.media;
                    let scene = match scene_id
                        .as_deref()
                        .and_then(|raw| Uuid::parse_str(raw.trim()).ok())
                    {
                        Some(id) => media.scenes.iter().find(|scene| scene.scene_id == id),
                        None => media.scenes.iter().min_by_key(|scene| {
                            (scene.start.as_micros() - target.as_micros()).abs()
                        }),
                    }?;
                    let frame = scene.keyframes.first()?;
                    let file = frame.thumbnail.as_ref()?;
                    Some((
                        result.cache_dir.join(file),
                        scene.scene_id,
                        frame.timestamp.as_seconds(),
                    ))
                });
                let Some((path, scene_id, at)) = located else {
                    return "エラー: フレームを取得できません（先に analyze_media を実行してください）"
                        .to_owned();
                };
                let Ok(bytes) = std::fs::read(&path) else {
                    return format!("エラー: 画像を読み込めません: {}", path.display());
                };
                self.pending_agent_images.push((
                    format!("シーン {scene_id} のフレーム（{at:.2}s）。見た目を確認してください。"),
                    AiImage::jpeg(base64_encode(&bytes)),
                ));
                format!(
                    "フレームを取得しました: scene={scene_id} {at:.2}s（次のメッセージに画像を添付します）"
                )
            }
            // Asynchronous tools are started by `start_async_tool`, never here.
            tools::ToolInvocation::AnalyzeMedia { .. }
            | tools::ToolInvocation::SearchScenes { .. }
            | tools::ToolInvocation::AddSpeech { .. }
            | tools::ToolInvocation::ExportVideo { .. } => {
                "エラー: 非同期ツールが同期経路に渡されました".to_owned()
            }
        }
    }

    fn item_ids(&self) -> HashSet<ItemId> {
        self.project
            .sequence
            .layers
            .iter()
            .flat_map(|layer| layer.items.iter().map(|item| item.id()))
            .collect()
    }

    /// Starts an asynchronous tool. Returns whether it actually started; when it could not (for
    /// example VOICEVOX is not configured) the error is already recorded as a tool result and the
    /// loop continues with the other queued calls.
    fn start_async_tool(
        &mut self,
        call: &AiToolCall,
        invocation: tools::ToolInvocation,
        context: &egui::Context,
    ) -> bool {
        match invocation {
            tools::ToolInvocation::AnalyzeMedia { asset_name } => {
                let asset_id = match asset_name {
                    Some(name) => self
                        .project
                        .assets
                        .iter()
                        .find(|asset| asset.name == name)
                        .map(|asset| asset.id),
                    None => self.analysis.default_asset(&self.project),
                };
                let Some(asset_id) = asset_id else {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        "エラー: 解析できる素材がありません（先に素材を追加してください）"
                            .to_owned(),
                    );
                    return false;
                };
                if self.analysis_worker.is_some() {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        "エラー: 既に解析が実行中です".to_owned(),
                    );
                    return false;
                }
                self.start_analysis_for(asset_id, false, context);
                self.agent_async = Some(AgentAsync::Analyze {
                    call_id: call.id.clone(),
                });
                self.analysis.chat.set_activity("素材を解析しています…");
                true
            }
            tools::ToolInvocation::SearchScenes { query } => {
                let Some(result) = self.analysis.result.as_ref() else {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        "エラー: 解析結果がありません。先に analyze_media を呼んでください。"
                            .to_owned(),
                    );
                    return false;
                };
                let media = result.media.clone();
                let directory = result.cache_dir.clone();
                let signature = result.signature.clone();
                let config = self.analysis.semantic.config.clone();
                let repaint = context.clone();
                let (tx, rx) = sync_channel(1);
                std::thread::Builder::new()
                    .name("agent-search".into())
                    .spawn(move || {
                        let outcome =
                            search_scenes_job(&config, &media, &directory, &signature, &query);
                        let _ = tx.send(outcome);
                        repaint.request_repaint();
                    })
                    .expect("failed to start search thread");
                self.agent_job = Some(rx);
                self.agent_async = Some(AgentAsync::Job {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                });
                self.analysis.chat.set_activity("シーンを検索しています…");
                true
            }
            tools::ToolInvocation::AddSpeech {
                text,
                timeline_start,
                speaker,
                character,
                template,
            } => {
                let url = self.voicevox_url.trim().to_owned();
                if url.is_empty() {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        "エラー: VOICEVOXのURLが設定されていません（字幕だけ add_text で追加できます）"
                            .to_owned(),
                    );
                    return false;
                }
                let start = timeline_start
                    .map(TimelineTime::from_seconds)
                    .unwrap_or(self.playhead);
                let settings = resolve_speech(
                    &self.characters,
                    self.voicevox_character,
                    self.voicevox_speaker,
                    character.as_deref(),
                    speaker,
                    template.as_deref(),
                    &self.templates,
                );
                let speaker_id = settings.speaker;
                let voice = settings.voice;
                let template_id = settings.template;
                let output_dir = voicevox_cache_dir();
                let repaint = context.clone();
                let (tx, rx) = sync_channel(1);
                let text_for_thread = text.clone();
                std::thread::Builder::new()
                    .name("voicevox".into())
                    .spawn(move || {
                        let result =
                            synthesize(&url, speaker_id, &voice, &text_for_thread, &output_dir)
                                .map(|asset| (text_for_thread, asset))
                                .map_err(|error| error.to_string());
                        let _ = tx.send(result);
                        repaint.request_repaint();
                    })
                    .expect("failed to start voicevox thread");
                self.voicevox_receiver = Some(rx);
                self.agent_async = Some(AgentAsync::Speech {
                    call_id: call.id.clone(),
                    start,
                    template: template_id,
                });
                self.analysis
                    .chat
                    .set_activity("VOICEVOXで音声を生成しています…");
                true
            }
            tools::ToolInvocation::ExportVideo { file_name } => {
                if self.export_result.is_some() {
                    self.analysis.chat.push_tool_result(
                        &call.id,
                        &call.name,
                        "エラー: 既に書き出しが実行中です".to_owned(),
                    );
                    return false;
                }
                let destination = self.default_export_path(file_name.as_deref());
                if let Some(parent) = destination.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                self.start_export_to(destination.clone(), context);
                self.agent_async = Some(AgentAsync::Export {
                    call_id: call.id.clone(),
                    destination,
                });
                self.analysis.chat.set_activity("動画を書き出しています…");
                true
            }
            _ => false,
        }
    }

    fn take_agent_speech(&mut self) -> Option<(String, TimelineTime, Option<Uuid>)> {
        if matches!(self.agent_async, Some(AgentAsync::Speech { .. }))
            && let Some(AgentAsync::Speech {
                call_id,
                start,
                template,
            }) = self.agent_async.take()
        {
            return Some((call_id, start, template));
        }
        None
    }

    fn take_agent_export(&mut self) -> Option<(String, PathBuf)> {
        if matches!(self.agent_async, Some(AgentAsync::Export { .. }))
            && let Some(AgentAsync::Export {
                call_id,
                destination,
            }) = self.agent_async.take()
        {
            return Some((call_id, destination));
        }
        None
    }

    fn take_agent_analyze(&mut self) -> Option<String> {
        if matches!(self.agent_async, Some(AgentAsync::Analyze { .. }))
            && let Some(AgentAsync::Analyze { call_id }) = self.agent_async.take()
        {
            return Some(call_id);
        }
        None
    }

    fn take_agent_job(&mut self) -> Option<(String, String)> {
        if matches!(self.agent_async, Some(AgentAsync::Job { .. }))
            && let Some(AgentAsync::Job { call_id, name }) = self.agent_async.take()
        {
            return Some((call_id, name));
        }
        None
    }

    /// Where an agent-requested export goes when the model does not name a file: next to a saved
    /// project, else the user's Videos folder, else the temp directory.
    fn default_export_path(&self, file_name: Option<&str>) -> PathBuf {
        let name = file_name.unwrap_or(&self.project.title);
        let file = format!("{}.mp4", sanitize_file_name(name));
        let directory = self
            .project_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Videos")))
            .unwrap_or_else(std::env::temp_dir);
        directory.join(file)
    }

    /// Starts an export to a known path (no file dialog). Shared by the menu action and the agent.
    fn start_export_to(&mut self, destination: PathBuf, context: &egui::Context) {
        if self.export_result.is_some() {
            return;
        }
        let project = self.project.clone();
        let options = self.export_options;
        let repaint_context = context.clone();
        let (tx, rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                let result = export_project_with_options(&project, &destination, options)
                    .map(|()| destination)
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
                repaint_context.request_repaint();
            })
            .expect("failed to start export thread");
        self.export_result = Some(rx);
        self.status = "書き出し中…".into();
    }

    /// Where the timeline thumbnail goes: next to a saved project, else the user's Videos
    /// folder, else the temp directory. Same fallback as [`Self::default_export_path`].
    fn thumbnail_export_path(&self) -> PathBuf {
        let directory = self
            .project_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Videos")))
            .unwrap_or_else(std::env::temp_dir);
        directory.join("samune.png")
    }

    /// Exports the composite frame at the playhead (clips + text + shapes) to `samune.png`.
    fn export_thumbnail(&mut self, context: &egui::Context) {
        if self.thumbnail_result.is_some() {
            return;
        }
        if self
            .project
            .sequence
            .layers
            .iter()
            .all(|layer| layer.items.is_empty())
        {
            self.status = "タイムラインが空のためサムネを書き出せません".into();
            return;
        }
        let destination = self.thumbnail_export_path();
        let project = self.project.clone();
        let time_sec = self.playhead.as_seconds();
        let repaint_context = context.clone();
        let (tx, rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("thumbnail-export".into())
            .spawn(move || {
                let result = export_frame_png(&project, time_sec, &destination)
                    .map(|()| destination)
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
                repaint_context.request_repaint();
            })
            .expect("failed to start thumbnail export thread");
        self.thumbnail_result = Some(rx);
        self.status = "サムネを書き出し中…".into();
    }

    /// Validates the proposed AEP against the current project and applies it as one undoable
    /// transaction (AEP-SPEC.md section 5).
    fn apply_chat_plan(&mut self) {
        let Some(Ok(document)) = self.analysis.chat.draft.clone() else {
            return;
        };
        let commands = match validate_aep(&self.project, &document) {
            Ok(commands) => commands,
            Err(error) => {
                self.analysis.chat.notice = Some(format!("適用できません: {error}"));
                return;
            }
        };
        if commands.is_empty() {
            self.analysis.chat.notice = Some("適用する操作がありません".to_owned());
            return;
        }
        let count = commands.len();
        match self.history.execute_batch(&mut self.project, commands) {
            Ok(()) => {
                self.dirty = true;
                self.fonts_dirty = true;
                self.preview_generation = self.preview_generation.wrapping_add(1);
                self.last_preview_time = None;
                self.audio_preview.stop();
                self.analysis.chat.draft = None;
                self.analysis.chat.notice = Some(format!(
                    "編集プランを適用しました（{count} 操作）。「元に戻す」で取り消せます。"
                ));
                self.status = "AIの編集プランを適用しました".into();
                if !self.playing {
                    self.request_preview();
                }
            }
            Err(error) => {
                self.analysis.chat.notice = Some(format!("適用できませんでした: {error}"));
            }
        }
    }

    fn test_model_connection(&mut self, context: &egui::Context) {
        let config = self.analysis.semantic.config.clone();
        self.analysis.semantic.probe = ProbeStatus::Testing;
        self.analysis.semantic.error = None;
        self.probe_task = Some(spawn_task(context, move || {
            OpenAiCompatible::new(config).probe()
        }));
        self.status = "モデルへの接続を確認しています…".into();
    }

    fn start_embeddings(&mut self, context: &egui::Context) {
        if self.embedding_worker.is_some() {
            self.status = "埋め込みを生成中です".into();
            return;
        }
        let Some(result) = self.analysis.result.as_ref() else {
            self.status = "先にメディアを解析してください".into();
            return;
        };
        let media = result.media.clone();
        let directory = result.cache_dir.clone();
        let analysis_signature = result.signature.clone();
        let config = self.analysis.semantic.config.clone();
        self.analysis.semantic.error = None;
        if let Err(error) = {
            let mut checked = config.clone();
            checked.clamp();
            checked.validate()
        } {
            self.analysis.semantic.embedding = EmbeddingStatus::Failed(error.to_string());
            return;
        }

        let total = crate::knowledge::documents(&media).len();
        if total == 0 {
            self.analysis.semantic.embedding =
                EmbeddingStatus::Failed("埋め込み対象のテキストがありません".to_owned());
            return;
        }
        self.analysis.semantic.embedding = EmbeddingStatus::Running {
            completed: 0,
            total,
        };
        self.analysis.semantic.results = None;
        let repaint = context.clone();
        self.embedding_worker = Some(EmbeddingWorker::spawn(
            OpenAiCompatible::new(config),
            media,
            directory,
            analysis_signature,
            move || repaint.request_repaint(),
        ));
        self.status = format!("{} シーンの埋め込みを生成しています…", total);
    }

    fn cancel_embeddings(&mut self) {
        if let Some(worker) = self.embedding_worker.take() {
            worker.cancel();
        }
        self.analysis.semantic.embedding = EmbeddingStatus::Idle;
        self.status = "埋め込みを中止しました".into();
    }

    /// Embeds the query text with the configured model, then ranks the cached scene vectors.
    /// Both steps run off the UI thread because either can be a network round-trip.
    fn run_search(&mut self, context: &egui::Context) {
        let Some(result) = self.analysis.result.as_ref() else {
            self.status = "先にメディアを解析してください".into();
            return;
        };
        let directory = result.cache_dir.clone();
        let analysis_signature = result.signature.clone();
        let Some(embeddings) = crate::knowledge::load_embeddings(&directory) else {
            self.analysis.semantic.error =
                Some("埋め込みがありません。先に「埋め込みを生成」を実行してください。".to_owned());
            return;
        };
        let config = self.analysis.semantic.config.clone();
        // The query must be embedded by the same model that produced the vectors, and the
        // vectors must belong to the current analysis.
        if !embeddings.is_compatible_with(config.model.trim(), &analysis_signature) {
            self.analysis.semantic.error = Some(format!(
                "埋め込みが現在の解析と一致しません（現在: {} / 生成済み: {}）。再生成してください。",
                config.model.trim(),
                embeddings.model
            ));
            return;
        }
        let query = self.analysis.semantic.query.trim().to_owned();
        if query.is_empty() {
            return;
        }

        self.analysis.semantic.error = None;
        let vectors = embeddings.vectors;
        self.search_task = Some(spawn_task(context, move || {
            let provider = OpenAiCompatible::new(config);
            let mut embedded = provider.embeddings(&[query])?;
            let query_vector = embedded.pop().ok_or_else(|| {
                ModelError::InvalidOutput("クエリの埋め込みが空でした".to_owned())
            })?;
            Ok(rank(&query_vector, &vectors, 20))
        }));
        self.status = "意味検索を実行しています…".into();
    }

    /// Imports files dropped anywhere on the window. The OS drag-and-drop protocol usually does
    /// not deliver pointer-motion events while dragging, so egui's pointer position is stale at
    /// drop time; the timeline rect from the previous frame is used when it still lines up, and
    /// otherwise the media is placed at the playhead.
    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let paths: Vec<PathBuf> = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        if paths.is_empty() {
            return;
        }
        let timeline_start = self.drop_timeline_start(context);
        self.import_paths_at(paths, timeline_start);
    }

    fn drop_timeline_start(&self, context: &egui::Context) -> TimelineTime {
        if let (Some(rect), Some(pointer)) =
            (self.timeline_drop_target, context.pointer_latest_pos())
            && rect.contains(pointer)
        {
            let seconds = ((pointer.x - rect.left()) / self.pixels_per_second.max(1.0)).max(0.0);
            return TimelineTime::from_seconds(seconds as f64);
        }
        self.playhead
    }

    fn toggle_playback(&mut self) {
        if self.project.duration() == TimelineTime::ZERO {
            return;
        }
        if self.playhead >= self.project.duration() {
            self.playhead = TimelineTime::ZERO;
        }
        self.playing = !self.playing;
        self.last_tick = Instant::now();
        // Playhead-only move: do NOT bump preview_generation. Bumping it here forced the warm
        // FFmpeg decoder to restart on every play/pause, which is exactly the scrub lag we
        // removed in seek_to. A structural generation bump is only for project edits.
        self.last_preview_time = None;
        self.request_preview();
        if self.playing {
            self.sync_audio_preview();
        } else {
            self.audio_preview.stop();
        }
    }

    fn step_frame(&mut self, delta: i32) {
        self.playing = false;
        self.audio_preview.stop();
        let frame_duration =
            TimelineTime::from_seconds(1.0 / self.project.sequence.settings.frame_rate);
        let offset = TimelineTime::from_micros(frame_duration.as_micros() * delta.abs() as i64);
        let new_time = if delta >= 0 {
            self.playhead + offset
        } else if self.playhead > offset {
            self.playhead - offset
        } else {
            TimelineTime::ZERO
        };
        self.playhead = new_time.max(TimelineTime::ZERO);
        if self.playhead > self.project.duration() {
            self.playhead = self.project.duration();
        }
        // Playhead-only move: keep the decoder warm (see seek_to).
        self.last_preview_time = None;
        self.request_preview();
    }

    fn seek_to(&mut self, time: TimelineTime) {
        self.playhead = time.max(TimelineTime::ZERO).min(self.project.duration());
        self.playing = false;
        self.audio_preview.stop();
        // Deliberately no preview_generation bump: a playhead move must reuse the warm
        // FFmpeg decoder. Bumping generation here made can_continue() fail on every scrub
        // step, restarting the process and leaving the preview one beat behind the cursor.
        self.last_preview_time = None;
        self.request_preview();
    }

    fn advance_playback(&mut self, context: &egui::Context) {
        if !self.playing {
            self.last_tick = Instant::now();
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_tick);
        self.last_tick = now;
        self.playhead = self.playhead + TimelineTime::from_seconds(elapsed.as_secs_f64());
        if self.playhead >= self.project.duration() {
            self.playhead = self.project.duration();
            self.playing = false;
            self.audio_preview.stop();
        }
        self.request_preview();
        self.sync_audio_preview();
        if self.playing {
            context.request_repaint_after(Duration::from_secs_f64(
                1.0 / self.power_profile.preview_fps(),
            ));
        }
    }

    fn sync_audio_preview(&mut self) {
        if !self.playing {
            return;
        }
        // Every clip audible at the playhead plays concurrently (one ffplay each); the
        // device mixes them the way export's `amix` does. Overlapping dialogue and video
        // audio must both be heard.
        let wanted: Vec<(ClipId, PathBuf, TimelineTime, TimelineTime, f32, f64)> = self
            .project
            .audible_clips_at(self.playhead)
            .into_iter()
            .filter_map(|clip| {
                let asset = self.project.asset(clip.asset_id)?;
                Some((
                    clip.id,
                    asset.path.clone(),
                    clip.source_time_at(self.playhead),
                    clip.timeline_end() - self.playhead,
                    clip.effective_gain_db(clip.anim_offset_at(self.playhead)),
                    clip.speed(),
                ))
            })
            .collect();
        let wanted_ids: HashSet<ClipId> = wanted.iter().map(|(id, ..)| *id).collect();
        for clip_id in self.audio_preview.playing_clips() {
            if !wanted_ids.contains(&clip_id) {
                self.audio_preview.stop_clip(clip_id);
            }
        }
        for (clip_id, path, source_time, remaining, gain_db, speed) in wanted {
            if self.audio_preview.is_playing_clip(clip_id) {
                continue;
            }
            if let Err(error) =
                self.audio_preview
                    .play(clip_id, &path, source_time, remaining, gain_db, speed)
            {
                self.status = format!("音声プレビューを開始できません: {error}");
            }
        }
    }

    fn request_preview(&mut self) {
        let minimum_delta = TimelineTime::from_seconds(1.0 / self.power_profile.preview_fps());
        let visible = self.project.visible_visual_clips_at(self.playhead);
        let visible_ids: Vec<ItemId> = visible.iter().map(|clip| clip.id).collect();
        // The clip set is part of the throttle key: hiding a layer or moving an item must
        // repaint even when the playhead did not move.
        let set_changed = visible_ids != self.last_requested_clips;
        if !set_changed
            && self.last_preview_time.is_some_and(|previous| {
                let delta = if previous > self.playhead {
                    previous - self.playhead
                } else {
                    self.playhead - previous
                };
                delta < minimum_delta
            })
        {
            return;
        }
        if self.preview_main_clip != visible_ids.last().copied() {
            self.preview_error = None;
        }
        self.preview_main_clip = visible_ids.last().copied();
        // Textures and still short-circuit state only matter for clips on screen right now.
        self.preview_textures.retain(|id, _| visible_ids.contains(id));
        self.preview_stills.retain(|id, _| visible_ids.contains(id));

        let mut batch = Vec::with_capacity(visible.len());
        self.preview_request_seq = self.preview_request_seq.wrapping_add(1);
        let request_seq = self.preview_request_seq;
        for clip in &visible {
            let Some(asset) = self.project.asset(clip.asset_id) else {
                continue;
            };
            // A still image renders identically at every time, so while the same image is on
            // screen and nothing else changed there is nothing to decode — this stops
            // playback/scrubbing over images from re-uploading an identical frame every tick.
            if asset.metadata.is_image
                && self.preview_textures.contains_key(&clip.id)
                && self
                    .preview_stills
                    .get(&clip.id)
                    .is_some_and(|(asset_id, profile)| {
                        *asset_id == asset.id && *profile == self.power_profile
                    })
            {
                continue;
            }
            if asset.metadata.is_image {
                self.preview_stills
                    .insert(clip.id, (asset.id, self.power_profile));
            }
            batch.push(PreviewRequest {
                request_seq,
                generation: self.preview_generation,
                clip_id: clip.id,
                path: asset.path.clone(),
                source_time: clip.source_time_at(self.playhead),
                profile: self.power_profile,
                // Still images are software-decoded; asking for QSV there breaks FFmpeg's filter graph.
                use_qsv: self.hardware.qsv_available && !asset.metadata.is_image,
                is_image: asset.metadata.is_image,
            });
        }
        if !batch.is_empty() {
            self.preview_worker.request_batch(batch);
        }
        self.last_requested_clips = visible_ids;
        self.last_preview_time = Some(self.playhead);
    }

    fn consume_preview(&mut self, context: &egui::Context) {
        let frames = self.preview_worker.try_frames();
        if frames.is_empty() {
            return;
        }
        for frame in frames {
            // Drop frames from before a structural project change, and never let a slow
            // decode overwrite a fresher frame (rapid scrubbing). request_seq is monotonic
            // per request batch, so "older than the last accepted" is a simple comparison.
            if frame.generation != self.preview_generation
                || frame.request_seq < self.preview_frame_seq
            {
                self.preview_worker.recycle(frame.pixels);
                continue;
            }
            self.preview_frame_seq = frame.request_seq;
            self.preview_qsv_active = frame.qsv_active;
            if let Some(error) = frame.error {
                // Drop any stale frame so the message is actually visible instead of an old picture.
                self.preview_textures.remove(&frame.clip_id);
                self.preview_stills.remove(&frame.clip_id);
                // Only the main (front-most) clip's failure is user-visible; a decorative
                // back layer failing must not blank the whole preview.
                if self.preview_main_clip == Some(frame.clip_id) {
                    self.preview_error = Some(error);
                }
                self.preview_worker.recycle(frame.pixels);
                continue;
            }
            let image =
                ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.pixels);
            // The pixels now live in the texture; return the decode buffer for reuse.
            self.preview_worker.recycle(frame.pixels);
            if let Some(texture) = self.preview_textures.get_mut(&frame.clip_id) {
                texture.set(image, TextureOptions::LINEAR);
            } else {
                self.preview_textures.insert(
                    frame.clip_id,
                    context.load_texture(
                        format!("preview-frame-{}", frame.clip_id),
                        image,
                        TextureOptions::LINEAR,
                    ),
                );
            }
            if self.preview_main_clip == Some(frame.clip_id) {
                self.preview_error = None;
            }
        }
    }

    fn process_actions(&mut self, actions: Vec<UiAction>, context: &egui::Context) {
        for action in actions {
            match action {
                UiAction::SelectItem(id) => {
                    self.selected_item = Some(id);
                    if let Some(layer) = self.project.layer_of_item(id) {
                        self.selected_layer = Some(layer.id);
                    }
                }
                UiAction::Deselect => self.selected_item = None,
                UiAction::Seek(time) => self.seek_to(time),
                UiAction::TogglePlayback => self.toggle_playback(),
                UiAction::StepFrame(delta) => self.step_frame(delta),
                UiAction::SplitAtPlayhead => self.split_selected(),
                UiAction::DeleteSelected => self.delete_selected(),
                UiAction::Undo => self.undo(),
                UiAction::Redo => self.redo(),
                UiAction::ImportMediaDialog => self.import_media(),
                UiAction::ImportFilesAt(paths, timeline_start) => {
                    self.import_paths_at(paths, timeline_start);
                }
                UiAction::AddAssetAt(asset_id, timeline_start) => {
                    if let Some(asset) = self.project.asset(asset_id).cloned() {
                        let layer_id =
                            self.resolve_target_layer(timeline_start, asset.metadata.duration);
                        self.execute(EditCommand::ImportAsset {
                            asset,
                            layer_id,
                            timeline_start,
                        });
                    }
                }
                UiAction::AddText(timeline_start) => {
                    let frame = self.project.sequence.settings.frame_size();
                    let text = TextItem::new("ゆっくりしていってね", timeline_start, frame);
                    let duration = text.duration;
                    let layer_id = self.resolve_target_layer(timeline_start, duration);
                    let text_id = text.id;
                    if self.execute(EditCommand::AddText { layer_id, text }) {
                        self.selected_item = Some(text_id);
                        self.selected_layer = Some(layer_id);
                    }
                }
                UiAction::AddShape(timeline_start) => {
                    let frame = self.project.sequence.settings.frame_size();
                    let shape = ShapeItem::new(ShapeKind::Rectangle, timeline_start, frame);
                    let duration = shape.duration;
                    let layer_id = self.resolve_target_layer(timeline_start, duration);
                    let shape_id = shape.id;
                    if self.execute(EditCommand::AddShape { layer_id, shape }) {
                        self.selected_item = Some(shape_id);
                        self.selected_layer = Some(layer_id);
                    }
                }
                UiAction::UpdateShape(shape) => {
                    self.execute(EditCommand::UpdateShape { shape });
                }
                UiAction::AddLayer => {
                    let layer = Layer::new(format!(
                        "レイヤー {}",
                        self.project.sequence.layers.len() + 1
                    ));
                    let layer_id = layer.id;
                    if self.execute(EditCommand::AddLayer {
                        layer,
                        index: Some(0),
                    }) {
                        self.selected_layer = Some(layer_id);
                    }
                }
                UiAction::RemoveLayer(layer_id) => {
                    if self.execute(EditCommand::RemoveLayer { layer_id })
                        && self.selected_layer == Some(layer_id)
                    {
                        self.selected_layer = self.project.sequence.layers.first().map(|l| l.id);
                    }
                }
                UiAction::RenameLayer(layer_id, name) => {
                    self.execute(EditCommand::RenameLayer { layer_id, name });
                }
                UiAction::SetLayerLocked(layer_id, locked) => {
                    self.execute(EditCommand::SetLayerLocked { layer_id, locked });
                }
                UiAction::SetLayerHidden(layer_id, hidden) => {
                    self.execute(EditCommand::SetLayerHidden { layer_id, hidden });
                }
                UiAction::SetLayerMuted(layer_id, muted) => {
                    self.execute(EditCommand::SetLayerMuted { layer_id, muted });
                }
                UiAction::MoveLayerUp(layer_id) => self.nudge_layer(layer_id, -1),
                UiAction::MoveLayerDown(layer_id) => self.nudge_layer(layer_id, 1),
                UiAction::SelectLayer(layer_id) => self.selected_layer = Some(layer_id),
                UiAction::MoveItem(item_id, target_layer_id, timeline_start) => {
                    self.execute(EditCommand::MoveItem {
                        item_id,
                        target_layer_id,
                        timeline_start,
                    });
                }
                UiAction::ResizeItemStart(item_id, timeline_start) => {
                    self.execute(EditCommand::ResizeItemStart {
                        item_id,
                        timeline_start,
                    });
                }
                UiAction::ResizeItemEnd(item_id, timeline_end) => {
                    self.execute(EditCommand::ResizeItemEnd {
                        item_id,
                        timeline_end,
                    });
                }
                UiAction::DeleteItem(item_id) => {
                    if self.execute(EditCommand::DeleteItem { item_id })
                        && self.selected_item == Some(item_id)
                    {
                        self.selected_item = None;
                    }
                }
                UiAction::TrimClip(clip_id, source_in, source_out) => {
                    self.execute(EditCommand::TrimClip {
                        clip_id,
                        source_in,
                        source_out,
                    });
                }
                UiAction::SetGain(clip_id, gain_db) => {
                    self.execute(EditCommand::SetClipGain { clip_id, gain_db });
                }
                UiAction::SetMuted(clip_id, muted) => {
                    self.execute(EditCommand::SetClipMuted { clip_id, muted });
                }
                UiAction::SetClipTransform(clip_id, transform) => {
                    self.execute(EditCommand::SetClipTransform { clip_id, transform });
                }
                UiAction::SetClipOpacity(clip_id, opacity) => {
                    self.execute(EditCommand::SetClipOpacity { clip_id, opacity });
                }
                UiAction::SetClipSpeed(clip_id, speed) => {
                    self.execute(EditCommand::SetClipSpeed { clip_id, speed });
                }
                UiAction::SetClipEffects(clip_id, effects) => {
                    self.execute(EditCommand::SetClipEffects { clip_id, effects });
                }
                UiAction::SetKeyframe(clip_id, property, keyframe) => {
                    self.execute(EditCommand::SetKeyframe {
                        clip_id,
                        property,
                        keyframe,
                    });
                }
                UiAction::RemoveKeyframe(clip_id, property, offset) => {
                    self.execute(EditCommand::RemoveKeyframe {
                        clip_id,
                        property,
                        offset,
                    });
                }
                UiAction::RemoveAnimTrack(clip_id, property) => {
                    self.execute(EditCommand::RemoveAnimTrack { clip_id, property });
                }
                UiAction::SetSequenceSettings(settings) => {
                    self.execute(EditCommand::SetSequenceSettings { settings });
                }
                UiAction::SetExportOptions(options) => {
                    self.export_options = options;
                }
                UiAction::UpdateText(text) => {
                    self.execute(EditCommand::UpdateText { text });
                }
                UiAction::StartAnalysis => self.start_analysis(false, context),
                UiAction::ReanalyzeAnalysis => self.start_analysis(true, context),
                UiAction::CancelAnalysis => self.cancel_analysis(),
                UiAction::ExportVir => self.export_vir(),
                UiAction::ClearAnalysisCache => self.clear_analysis_cache(),
                UiAction::ExportThumbnail => self.export_thumbnail(&context),
                UiAction::TestModelConnection => self.test_model_connection(context),
                UiAction::GenerateEmbeddings => self.start_embeddings(context),
                UiAction::CancelEmbeddings => self.cancel_embeddings(),
                UiAction::SearchScenes => self.run_search(context),
                UiAction::SendChat => self.send_chat(context),
                UiAction::CancelChat => self.cancel_chat(),
                UiAction::ClearChat => self.analysis.chat.clear(),
                UiAction::ApplyChatPlan => self.apply_chat_plan(),
                UiAction::DiscardChatPlan => {
                    self.analysis.chat.draft = None;
                    self.analysis.chat.notice = None;
                }
            }
        }
    }

    fn nudge_layer(&mut self, layer_id: LayerId, delta: i32) {
        let Some(current_index) = self
            .project
            .sequence
            .layers
            .iter()
            .position(|layer| layer.id == layer_id)
        else {
            return;
        };
        let new_index = (current_index as i32 + delta)
            .clamp(0, self.project.sequence.layers.len() as i32 - 1)
            as usize;
        if new_index != current_index {
            self.execute(EditCommand::ReorderLayer {
                layer_id,
                new_index,
            });
        }
    }

    fn import_paths_at(&mut self, paths: Vec<PathBuf>, mut timeline_start: TimelineTime) {
        for path in paths {
            match probe_media(&path) {
                Ok(asset) => {
                    let duration = asset.metadata.duration;
                    let layer_id = self.resolve_target_layer(timeline_start, duration);
                    if self.execute(EditCommand::ImportAsset {
                        asset,
                        layer_id,
                        timeline_start,
                    }) {
                        timeline_start = timeline_start + duration;
                        self.status = format!("{} をタイムラインへ追加しました", path.display());
                    } else {
                        break;
                    }
                }
                Err(error) => {
                    self.status = format!("{} を読み込めません: {error}", path.display());
                    break;
                }
            }
        }
        self.request_preview();
    }

    fn split_selected(&mut self) {
        let Some(item_id) = self.selected_item else {
            self.status = "分割するアイテムを選択してください".into();
            return;
        };
        // Clips split their source range; a speech splits audio and subtitle together (handy for
        // cutting a line short). Everything else cannot be split.
        let split = if self.project.clip(item_id).is_some() {
            EditCommand::SplitClip {
                clip_id: item_id,
                timeline_time: self.playhead,
            }
        } else if self
            .project
            .item(item_id)
            .and_then(|item| item.as_speech())
            .is_some()
        {
            EditCommand::SplitSpeech {
                speech_id: item_id,
                timeline_time: self.playhead,
            }
        } else {
            self.status = "分割できるのは動画クリップとセリフのみです".into();
            return;
        };
        if self.execute(split) {
            self.status = "再生位置で分割しました".into();
        }
    }

    fn delete_selected(&mut self) {
        if let Some(item_id) = self.selected_item.take() {
            self.execute(EditCommand::DeleteItem { item_id });
        }
    }

    fn copy_selected(&mut self) {
        let Some(item) = self
            .selected_item
            .and_then(|id| self.project.item(id))
            .cloned()
        else {
            self.status = "コピーするアイテムを選択してください".into();
            return;
        };
        self.clipboard = Some(item);
        self.status = "アイテムをコピーしました".into();
    }

    fn paste_clipboard(&mut self) {
        let Some(template) = self.clipboard.clone() else {
            self.status = "クリップボードが空です".into();
            return;
        };
        let mut item = template.detached_copy();
        item.set_timeline_start(self.playhead);
        let new_id = item.id();
        let duration = item.duration();
        // A free layer is resolved first so the end-of-command overlap check cannot reject
        // the paste when the playhead still sits inside the source item.
        let layer_id = self.resolve_target_layer(self.playhead, duration);
        if self.execute(EditCommand::PasteItem { item, layer_id }) {
            self.selected_item = Some(new_id);
            self.selected_layer = Some(layer_id);
            self.status = "再生位置にペーストしました".into();
        }
    }

    fn voicevox_synthesize(&mut self, context: &egui::Context) {
        if self.voicevox_receiver.is_some() {
            return;
        }
        let text = self.voicevox_text.trim().to_owned();
        if text.is_empty() {
            self.status = "読み上げるテキストを入力してください".into();
            return;
        }
        let url = self.voicevox_url.trim().to_owned();
        if url.is_empty() {
            self.status = "VOICEVOXのURLを設定してください".into();
            return;
        }
        // The manual button follows the character selected in the VOICEVOX bar, falling back to
        // the bare speaker id when "直接指定" is chosen.
        let settings = resolve_speech(
            &self.characters,
            self.voicevox_character,
            self.voicevox_speaker,
            None,
            None,
            None,
            &self.templates,
        );
        let speaker = settings.speaker;
        let voice = settings.voice;
        let output_dir = voicevox_cache_dir();
        let repaint = context.clone();
        let (tx, rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("voicevox".into())
            .spawn(move || {
                let result = synthesize(&url, speaker, &voice, &text, &output_dir)
                    .map(|asset| (text, asset))
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
                repaint.request_repaint();
            })
            .expect("failed to start voicevox thread");
        self.voicevox_receiver = Some(rx);
        self.status = "VOICEVOXで音声を生成中…".into();
    }

    fn poll_voicevox(&mut self, context: &egui::Context) {
        let Some(receiver) = &self.voicevox_receiver else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok((text, asset))) => {
                self.voicevox_receiver = None;
                match self.take_agent_speech() {
                    Some((call_id, start, template)) => {
                        let result = self.apply_voicevox_at(text, asset, start, template);
                        self.finish_agent_tool(&call_id, "add_speech", result, context);
                    }
                    None => {
                        // A manual line (typed in the VOICEVOX bar): clear the input once it has
                        // landed on the timeline. Only clear if the box still holds exactly the
                        // submitted text, so edits made while synthesis ran are not lost.
                        let submitted = text.clone();
                        self.apply_voicevox(text, asset);
                        if self.voicevox_text.trim() == submitted.as_str() {
                            self.voicevox_text.clear();
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                self.voicevox_receiver = None;
                self.status = format!("VOICEVOXエラー: {error}");
                if let Some((call_id, _, _)) = self.take_agent_speech() {
                    self.finish_agent_tool(
                        &call_id,
                        "add_speech",
                        format!("エラー: {error}"),
                        context,
                    );
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.voicevox_receiver = None;
                if let Some((call_id, _, _)) = self.take_agent_speech() {
                    self.finish_agent_tool(
                        &call_id,
                        "add_speech",
                        "エラー: 音声生成が中断されました".to_owned(),
                        context,
                    );
                }
            }
        }
    }

    fn apply_voicevox(&mut self, text: String, asset: MediaAsset) {
        let start = self.playhead;
        // The just-synthesized line came from the character selected in the VOICEVOX bar, so its
        // subtitle uses that character's style.
        let template = resolve_speech(
            &self.characters,
            self.voicevox_character,
            self.voicevox_speaker,
            None,
            None,
            None,
            &self.templates,
        )
        .template;
        self.apply_voicevox_at(text, asset, start, template);
        self.request_preview();
    }

    /// Adds a synthesized line as a single speech item (audio + subtitle together) at an explicit
    /// position and returns a short summary, which is what the agent reads back as the
    /// `add_speech` tool result. Stopping on a failed `execute` keeps the real error from being
    /// overwritten by a success message.
    fn apply_voicevox_at(
        &mut self,
        text: String,
        asset: MediaAsset,
        start: TimelineTime,
        template: Option<Uuid>,
    ) -> String {
        let duration = asset.metadata.duration;
        let layer_id = self.resolve_target_layer(start, duration);
        let frame = self.project.sequence.settings.frame_size();
        let audio = Clip::new(&asset, start, frame);
        let subtitle = match template.and_then(|id| self.templates.iter().find(|t| t.id == id)) {
            Some(template) => template.build_text(text.clone(), start, duration),
            None => {
                let mut item = TextItem::new(text.clone(), start, frame);
                item.duration = duration;
                item
            }
        };
        let speech = SpeechItem::new(audio, subtitle);
        let speech_id = speech.id();
        if self.execute(EditCommand::AddSpeech {
            asset,
            layer_id,
            speech,
        }) {
            self.selected_item = Some(speech_id);
            self.selected_layer = Some(layer_id);
            self.status = "セリフ（音声＋字幕）をタイムラインに追加しました".into();
            format!(
                "セリフを追加しました: 「{}」 {:.2}s–{:.2}s",
                text,
                start.as_seconds(),
                (start + duration).as_seconds()
            )
        } else {
            format!("エラー: セリフを追加できませんでした（{}）", self.status)
        }
    }

    /// Registers the system fonts referenced by text items with egui, so the live preview
    /// renders the same typeface (and weight/style) as the FFmpeg export. Each family/style
    /// combination is loaded at most once.
    fn ensure_preview_fonts(&mut self, context: &egui::Context) {
        if !self.fonts_dirty {
            return;
        }
        self.fonts_dirty = false;
        let mut needed: Vec<(String, bool, bool)> = Vec::new();
        let mut seen = HashSet::new();
        for layer in &self.project.sequence.layers {
            for item in &layer.items {
                if let Some(text) = item.as_text()
                    && let Some(family) = &text.font_family
                {
                    let key = (family.clone(), text.bold, text.italic);
                    if seen.insert(key.clone()) {
                        needed.push(key);
                    }
                }
            }
        }

        for (family, bold, italic) in needed {
            let key = preview_font_key(&family, bold, italic);
            // Already active or already queued: nothing to do this frame.
            if self.preview_fonts.contains(&key) || self.preview_fonts_queued.contains(&key) {
                continue;
            }
            let Some(path) = resolve_font_file(Some(family), bold, italic) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // Use the family key as the font-data name; it is unique per family/style.
            self.font_definitions
                .font_data
                .insert(key.clone(), egui::FontData::from_owned(bytes).into());
            self.font_definitions.families.insert(
                egui::FontFamily::Name(key.clone().into()),
                vec![key.clone()],
            );
            // Mark as queued (not active) because `set_fonts` applies next pass.
            self.preview_fonts_queued.insert(key);
            context.set_fonts(self.font_definitions.clone());
        }
    }

    fn shortcuts(&mut self, context: &egui::Context) {
        // Text fields must keep their own keys (caret movement, deletion, text undo). These
        // shortcuts run before any widget sees the events, so consuming them unconditionally
        // would silently swallow them mid-sentence — and Ctrl+Z would undo the timeline while
        // the user is editing text.
        //
        // `text_edit_focused` rather than `egui_wants_keyboard_input`: every clickable widget
        // is focusable in egui, so after clicking a clip or a button the whole app would lose
        // Space/arrows otherwise.
        let text_edit_focused = context.text_edit_focused();
        if context.input_mut(|input| {
            input.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::S))
        }) {
            self.save_project(false);
        }
        if !text_edit_focused {
            if context.input_mut(|input| {
                input.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Z))
            }) {
                self.undo();
            }
            if context.input_mut(|input| {
                input.consume_shortcut(&KeyboardShortcut::new(
                    Modifiers::CTRL | Modifiers::SHIFT,
                    Key::Z,
                ))
            }) {
                self.redo();
            }
        }
        // Ctrl+C/V must stay available to focused text fields (inspector, VOICEVOX, chat, ...)
        // so their OS clipboard still works; only consume them outside text input.
        if !text_edit_focused {
            if context.input_mut(|input| {
                input.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::C))
            }) {
                self.copy_selected();
            }
            if context.input_mut(|input| {
                input.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::V))
            }) {
                self.paste_clipboard();
            }
        }
        if !text_edit_focused {
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Space)) {
                self.toggle_playback();
            }
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Delete)) {
                self.delete_selected();
            }
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowRight)) {
                self.step_frame(1);
            }
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowLeft)) {
                self.step_frame(-1);
            }
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Home)) {
                self.seek_to(TimelineTime::ZERO);
            }
            if context.input_mut(|input| input.consume_key(Modifiers::NONE, Key::End)) {
                let duration = self.project.duration();
                self.seek_to(duration);
            }
        }
    }
}

impl eframe::App for VideoEditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = ui.ctx().clone();
        self.poll_export(&context);
        self.poll_thumbnail();
        self.poll_voicevox(&context);
        self.poll_hardware();
        self.poll_fonts();
        self.poll_analysis(&context);
        self.poll_model_tasks();
        self.poll_chat(&context);
        self.poll_agent_job(&context);
        self.handle_dropped_files(&context);
        self.consume_preview(&context);
        self.advance_playback(&context);
        // `egui::Context::set_fonts` only takes effect at the start of the next pass, so
        // fonts queued last frame are now safe to use.
        let newly_active: Vec<String> = self.preview_fonts_queued.drain().collect();
        self.preview_fonts.extend(newly_active);
        self.ensure_preview_fonts(&context);
        self.shortcuts(&context);

        egui::Panel::top("menu_and_transport").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("ファイル", |ui| {
                    if ui.button("プロジェクトを開く…").clicked() {
                        ui.close();
                        self.open_project();
                    }
                    if ui.button("保存").clicked() {
                        ui.close();
                        self.save_project(false);
                    }
                    if ui.button("名前を付けて保存…").clicked() {
                        ui.close();
                        self.save_project(true);
                    }
                    ui.separator();
                    if ui.button("メディアを追加…").clicked() {
                        ui.close();
                        self.import_media();
                    }
                    ui.separator();
                    if ui.button("動画を書き出す…").clicked() {
                        ui.close();
                        self.start_export(&context);
                    }
                });
                ui.menu_button("編集", |ui| {
                    if ui.button("コピー (Ctrl+C)").clicked() {
                        ui.close();
                        self.copy_selected();
                    }
                    if ui.button("ペースト (Ctrl+V)").clicked() {
                        ui.close();
                        self.paste_clipboard();
                    }
                });
                ui.menu_button("表示", |ui| {
                    ui.small("スナップウィンドウ（パネル）の表示切り替え");
                    ui.separator();
                    for tab in EditorTab::ALL {
                        let mut open = self.dock.find_tab(&tab).is_some();
                        if ui.checkbox(&mut open, tab.label()).clicked() {
                            if open {
                                self.dock.push_to_focused_leaf(tab);
                            } else {
                                self.dock.retain_tabs(|other| *other != tab);
                            }
                        }
                    }
                    ui.separator();
                    ui.menu_button("テーマ", |ui| {
                        for (theme, label) in AppTheme::MENU {
                            if ui
                                .selectable_label(self.app_theme == theme, label)
                                .clicked()
                            {
                                self.app_theme = theme;
                                // ランダムカラーは選択中でも再クリックで配色を引き直す。
                                if theme == AppTheme::RandomColor {
                                    self.random_theme_seed = roll_random_seed();
                                }
                                context
                                    .set_visuals(build_visuals(self.app_theme, self.random_theme_seed));
                            }
                        }
                    });
                });
                ui.separator();
                if ui
                    .add_enabled(self.history.can_undo(), egui::Button::new("↶ 元に戻す"))
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(self.history.can_redo(), egui::Button::new("↷ やり直す"))
                    .clicked()
                {
                    self.redo();
                }
                if self.export_result.is_some() {
                    ui.separator();
                    ui.spinner();
                    ui.label("書き出し中");
                }
            });
        });

        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(if self.dirty { "●" } else { "○" });
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "{} | {}",
                        self.project.title,
                        self.project.duration()
                    ));
                });
            });
        });

        egui::Panel::bottom("voicevox_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("🎙 VOICEVOX");
                ui.add(
                    egui::TextEdit::singleline(&mut self.voicevox_url)
                        .hint_text("http://127.0.0.1:50021")
                        .desired_width(200.0),
                );
                ui.label("キャラ");
                // Voice and subtitle style come from the selected character, so the bar picks a
                // character rather than a raw speaker id. "直接指定" keeps the old behaviour for
                // users who have not registered characters yet.
                let character_label = self
                    .voicevox_character
                    .and_then(|id| self.characters.iter().find(|character| character.id == id))
                    .map(|character| character.name.clone())
                    .unwrap_or_else(|| "直接指定".to_owned());
                egui::ComboBox::from_id_salt("voicevox_character")
                    .selected_text(character_label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.voicevox_character, None, "直接指定");
                        for character in &self.characters {
                            ui.selectable_value(
                                &mut self.voicevox_character,
                                Some(character.id),
                                &character.name,
                            );
                        }
                    });
                if self.voicevox_character.is_none() {
                    ui.label("話者");
                    ui.add(egui::DragValue::new(&mut self.voicevox_speaker).range(1..=100));
                }
                let response = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.voicevox_text)
                            // A stable id keeps focus (and the IME) across layout changes.
                            .id_salt("voicevox_line")
                            .hint_text("読み上げるテキストを入力（Enterで追加）")
                            .desired_width(260.0),
                    )
                    .on_hover_text("Escで入力リセット（IMEが文字を渡さなくなったとき）");
                // Enter submits the line, matching the AI chat composer.
                let submitted =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                // Esc blurs the field at the start of the frame (egui does that for us). Taking
                // the focus back within the same frame leaves the IME disabled for exactly one
                // frame, which forces the compositor to enable it again — the no-restart
                // recovery when a Japanese IME stops committing text into a focused field.
                // `had_focus_last_frame` keeps this from stealing focus back from a neighbour
                // when the field was already left some frames ago.
                let reset_input = response.ctx.memory(|memory| {
                    memory.had_focus_last_frame(response.id)
                        && ui.input(|input| input.key_pressed(egui::Key::Escape))
                });
                let busy = self.voicevox_receiver.is_some();
                let can_add = !busy && !self.voicevox_text.trim().is_empty();
                let clicked = ui.add_enabled(can_add, egui::Button::new("生成")).clicked();
                if (submitted || clicked) && can_add {
                    self.voicevox_synthesize(&context);
                }
                // A single-line `TextEdit` always drops focus on Enter (and clicking the button
                // moves it), so take it back: otherwise the next line cannot be typed without
                // clicking the box again, and the Japanese IME has to be re-enabled each time.
                // Requesting focus while it is already held would interrupt an in-progress IME
                // composition, so only take it back when it is actually gone.
                if (submitted || clicked || reset_input) && !response.has_focus() {
                    response.request_focus();
                }
                if busy {
                    ui.spinner();
                    ui.small("生成中");
                }
            });
        });

        let power_profile_before = self.power_profile;
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            let mut viewer = PanelContext {
                project: &self.project,
                selected_item: self.selected_item,
                selected_layer: self.selected_layer,
                playhead: self.playhead,
                playing: self.playing,
                can_undo: self.history.can_undo(),
                can_redo: self.history.can_redo(),
                preview_textures: &self.preview_textures,
                preview_error: self.preview_error.as_deref(),
                preview_qsv_active: self.preview_qsv_active,
                pixels_per_second: &mut self.pixels_per_second,
                snap_enabled: &mut self.snap_enabled,
                export_options: &mut self.export_options,
                power_profile: &mut self.power_profile,
                hardware: &self.hardware,
                available_fonts: &self.available_fonts,
                preview_fonts: &self.preview_fonts,
                preview_galley_cache: &mut self.preview_galley_cache,
                templates: &mut self.templates,
                characters: &mut self.characters,
                analysis: &mut self.analysis,
                timeline_drop_target: &mut self.timeline_drop_target,
                anim_property_draft: &mut self.anim_property_draft,
                peak_cache: &mut self.peak_cache,
                file_browser: &mut self.file_browser,
                project_path: self.project_path.as_deref(),
                actions: &mut actions,
            };
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_inside(ui, &mut viewer);
        });
        self.process_actions(actions, &context);
        // A character can be deleted from the cast panel while it is the selected one; drop the
        // dangling selection so the bar falls back to the bare speaker id.
        if self
            .voicevox_character
            .is_some_and(|id| self.characters.iter().all(|character| character.id != id))
        {
            self.voicevox_character = None;
        }
        if self.power_profile != power_profile_before {
            self.preview_generation = self.preview_generation.wrapping_add(1);
            self.last_preview_time = None;
            self.request_preview();
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "power_profile", &self.power_profile);
        eframe::set_value(storage, "dock_layout_v6", &self.dock);
        eframe::set_value(storage, "text_templates", &self.templates);
        eframe::set_value(storage, "voicevox_url", &self.voicevox_url);
        eframe::set_value(storage, "voicevox_speaker", &self.voicevox_speaker);
        eframe::set_value(storage, "characters", &self.characters);
        eframe::set_value(storage, "voicevox_character", &self.voicevox_character);
        eframe::set_value(storage, "analysis_options", &self.analysis.options);
        eframe::set_value(storage, "model_config", &self.analysis.semantic.config);
        eframe::set_value(storage, "chat_config", &self.analysis.chat.config);
        eframe::set_value(storage, "app_theme", &self.app_theme);
        eframe::set_value(storage, "random_theme_seed", &self.random_theme_seed);
    }
}

/// Runs `work` on a detached worker thread and hands back its result through a channel, asking
/// egui for a repaint once it is ready. Used for model calls (connection test, query embedding)
/// that must never block the UI thread.
fn spawn_task<T: Send + 'static>(
    context: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
) -> Receiver<T> {
    let (sender, receiver) = channel();
    let repaint = context.clone();
    std::thread::Builder::new()
        .name("model-task".into())
        .spawn(move || {
            let _ = sender.send(work());
            repaint.request_repaint();
        })
        .expect("failed to start model task");
    receiver
}

/// Scene search for the agent: reuses cached embeddings when they match the analysis, otherwise
/// generates them, then ranks the query. Runs off the UI thread and returns a ready-made result
/// string for the model.
fn search_scenes_job(
    config: &ModelConfig,
    media: &crate::protocol::VirMedia,
    directory: &Path,
    signature: &str,
    query: &str,
) -> Result<String, String> {
    let provider = OpenAiCompatible::new(config.clone());
    let set = match crate::knowledge::load_embeddings(directory)
        .filter(|set| set.is_compatible_with(config.model.trim(), signature))
    {
        Some(set) => set,
        None => {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let set =
                crate::knowledge::embed_media(&provider, media, signature, &cancel, &mut |_, _| {})
                    .map_err(|error| error.to_string())?;
            crate::knowledge::save_embeddings(directory, &set)
                .map_err(|error| error.to_string())?;
            set
        }
    };

    let mut embedded = provider
        .embeddings(&[query.to_owned()])
        .map_err(|error| error.to_string())?;
    let query_vector = embedded
        .pop()
        .ok_or_else(|| "クエリの埋め込みが空でした".to_owned())?;
    let hits = crate::knowledge::rank(&query_vector, &set.vectors, 8);
    if hits.is_empty() {
        return Ok("該当するシーンは見つかりませんでした".to_owned());
    }
    let mut lines = vec![format!("{} 件:", hits.len())];
    for hit in hits {
        let Some(scene) = media
            .scenes
            .iter()
            .find(|scene| scene.scene_id == hit.scene_id)
        else {
            continue;
        };
        let text = scene
            .transcript
            .clone()
            .or_else(|| scene.ocr.first().cloned())
            .unwrap_or_else(|| scene.summary.clone());
        lines.push(format!(
            "- score={:.3} scene={} {:.2}s–{:.2}s: {}",
            hit.score,
            scene.scene_id,
            scene.start.as_seconds(),
            scene.end.as_seconds(),
            truncate_text(&text, 80)
        ));
    }
    Ok(lines.join("\n"))
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    flat.chars().take(max_chars).collect::<String>() + "…"
}

/// Standard base64 (with padding). Implemented here because the only consumer is the vision
/// attachment, and avoiding a dependency for thirty lines keeps the tree lean.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Keeps a project title usable as a file name for an agent-triggered export.
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "video".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Templates created before the default-position fix were stored at (0, 0) with a centered
/// anchor, which places generated text almost entirely outside the canvas (and therefore
/// looks like nothing was added). Repair exactly that legacy default; intentional positions
/// are left untouched.
fn migrate_template_positions(templates: &mut [TextTemplate]) {
    let legacy_default = Transform::centered((0.0, 0.0));
    for template in templates {
        if template.transform == legacy_default {
            template.transform = Transform::centered((1920.0, 1080.0));
        }
    }
}

/// The concrete configuration used to synthesize one line and style its subtitle.
#[derive(Clone, Debug, PartialEq)]
struct SpeechSettings {
    speaker: u32,
    voice: VoiceSettings,
    template: Option<Uuid>,
}

/// Resolves a line's speaker, voice tuning and subtitle template.
///
/// The configuration unit is a character, so resolution prefers one in this order: an explicit
/// character name, a character matching an explicit speaker id, then the character selected in
/// the VOICEVOX bar. Only when none applies is the bare fallback speaker used (with neutral voice
/// and no template). An explicit template name always overrides the style otherwise chosen.
fn resolve_speech(
    characters: &[Character],
    selected: Option<Uuid>,
    fallback_speaker: u32,
    requested_character: Option<&str>,
    requested_speaker: Option<u32>,
    requested_template: Option<&str>,
    templates: &[TextTemplate],
) -> SpeechSettings {
    let base = if let Some(name) = requested_character {
        characters.iter().find(|character| character.name == name)
    } else if let Some(speaker) = requested_speaker {
        characters
            .iter()
            .find(|character| character.speaker == speaker)
    } else {
        selected.and_then(|id| characters.iter().find(|character| character.id == id))
    };

    let speaker = requested_speaker
        .or_else(|| base.map(|character| character.speaker))
        .unwrap_or(fallback_speaker);
    let voice = base
        .map(|character| character.voice.clone())
        .unwrap_or_default();
    let template = requested_template
        .and_then(|name| templates.iter().find(|template| template.name == name))
        .map(|template| template.id)
        .or_else(|| base.and_then(|character| character.template));

    SpeechSettings {
        speaker,
        voice,
        template,
    }
}

fn configure_style(context: &egui::Context) {
    context.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 3.0);
    });
}

/// The base egui font set: built-in fonts plus a Japanese-capable fallback so both the UI
/// and Japanese subtitles render without missing glyphs. System fonts chosen for text items
/// are added on top of this by `ensure_preview_fonts`.
fn base_font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let Some(path) = find_japanese_font() else {
        return fonts;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return fonts;
    };
    fonts.font_data.insert(
        "japanese".to_owned(),
        egui::FontData::from_owned(bytes).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("japanese".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("japanese".to_owned());
    fonts
}

fn find_japanese_font() -> Option<PathBuf> {
    crate::media::resolve_font_file(None, false, false)
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, resolve_speech};
    use crate::domain::{Character, TextTemplate, VoiceSettings};

    #[test]
    fn base64_matches_the_reference_examples() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Many"), "TWFueQ==");
        // JPEG magic bytes (/9j/) are what a data URL starts with.
        assert_eq!(base64_encode(&[0xFF, 0xD8, 0xFF]), "/9j/");
    }

    #[test]
    fn speech_settings_follow_the_requested_character() {
        let mut heroine = Character::new("ヒロイン", 8);
        heroine.voice.speed = 1.3;
        let subtitle = TextTemplate::new("ヒロイン字幕");
        heroine.template = Some(subtitle.id);
        let characters = vec![heroine];
        let templates = vec![subtitle.clone()];

        // By name: speaker, voice tuning and subtitle all come from the character.
        let settings = resolve_speech(
            &characters,
            None,
            1,
            Some("ヒロイン"),
            None,
            None,
            &templates,
        );
        assert_eq!(settings.speaker, 8);
        assert_eq!(settings.voice.speed, 1.3);
        assert_eq!(settings.template, Some(subtitle.id));
    }

    #[test]
    fn explicit_arguments_override_the_character() {
        let mut narrator = Character::new("ナレーター", 2);
        narrator.voice.pitch = 0.1;
        let characters = vec![narrator];
        let heading = TextTemplate::new("見出し");
        let templates = vec![heading.clone()];

        let settings = resolve_speech(
            &characters,
            None,
            1,
            None,
            Some(2),        // matches the narrator, so its voice tuning is adopted
            Some("見出し"), // an explicit template name wins over the character's
            &templates,
        );
        assert_eq!(settings.speaker, 2);
        assert_eq!(settings.voice.pitch, 0.1);
        assert_eq!(settings.template, Some(heading.id));
    }

    #[test]
    fn unknown_requests_fall_back_to_the_bare_speaker() {
        let characters = vec![Character::new("誰か", 7)];
        let settings = resolve_speech(&characters, None, 5, None, Some(99), None, &[]);
        assert_eq!(settings.speaker, 99);
        assert_eq!(settings.voice, VoiceSettings::default());
        assert_eq!(settings.template, None);

        // The character selected in the UI is used when nothing else is requested.
        let selected = resolve_speech(
            &characters,
            Some(characters[0].id),
            5,
            None,
            None,
            None,
            &[],
        );
        assert_eq!(selected.speaker, 7);
    }
}
