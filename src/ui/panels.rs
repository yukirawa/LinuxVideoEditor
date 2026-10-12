use crate::domain::{
    Anchor, AssetId, Character, Clip, ItemId, Layer, LayerId, Project, SequenceSettings, ShapeItem,
    ShapeKind, SpeechItem, TextAlign, TextId, TextItem, TextTemplate, TimelineItem, TimelineTime,
    Transform,
};
use crate::media::{
    AudioCodec, EncoderPreset, ExportOptions, HardwareCapabilities, PowerProfile, VideoEncoder,
};
use crate::ui::AnalysisState;
use eframe::egui::{
    self, Color32, Id, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui, Vec2, WidgetText,
};
use egui_dock::{DockState, NodeIndex, TabViewer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum EditorTab {
    Preview,
    Timeline,
    Inspector,
    Settings,
    Ai,
    Templates,
    Characters,
    MediaBrowser,
    FileBrowser,
    Waveform,
    LevelMeter,
}

impl EditorTab {
    /// Every panel the 表示 menu can show or hide. Order matches the menu.
    pub const ALL: [EditorTab; 11] = [
        EditorTab::Preview,
        EditorTab::Timeline,
        EditorTab::FileBrowser,
        EditorTab::MediaBrowser,
        EditorTab::Waveform,
        EditorTab::LevelMeter,
        EditorTab::Inspector,
        EditorTab::Settings,
        EditorTab::Ai,
        EditorTab::Templates,
        EditorTab::Characters,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EditorTab::Preview => "プレビュー",
            EditorTab::Timeline => "タイムライン",
            EditorTab::Inspector => "アイテム設定",
            EditorTab::Settings => "プロジェクト設定",
            EditorTab::Ai => "AI",
            EditorTab::Templates => "テンプレート",
            EditorTab::Characters => "キャラクター",
            EditorTab::MediaBrowser => "メディアブラウザ",
            EditorTab::FileBrowser => "ファイルブラウザ",
            EditorTab::Waveform => "音声波形",
            EditorTab::LevelMeter => "音量モニタ",
        }
    }
}

/// Default layout: preview on top, timeline below it, inspector/file stack on the right.
/// File browser and media browser open by default so importing never needs the
/// desktop file manager; every other panel opens from the 表示 menu.
pub fn create_default_dock() -> DockState<EditorTab> {
    let mut dock = DockState::new(vec![EditorTab::Preview]);
    let [left, _right] = dock.main_surface_mut().split_right(
        NodeIndex::root(),
        0.76,
        vec![
            EditorTab::FileBrowser,
            EditorTab::MediaBrowser,
            EditorTab::Inspector,
            EditorTab::Settings,
            EditorTab::Ai,
            EditorTab::Templates,
            EditorTab::Characters,
        ],
    );
    dock.main_surface_mut()
        .split_below(left, 0.62, vec![EditorTab::Timeline]);
    dock
}

#[derive(Clone, Debug)]
pub enum UiAction {
    SelectItem(ItemId),
    Deselect,
    Seek(TimelineTime),
    TogglePlayback,
    StepFrame(i32),
    SplitAtPlayhead,
    DeleteSelected,
    Undo,
    Redo,
    ImportMediaDialog,
    ImportFilesAt(Vec<PathBuf>, TimelineTime),
    AddAssetAt(AssetId, TimelineTime),
    AddText(TimelineTime),
    AddShape(TimelineTime),
    UpdateShape(ShapeItem),
    AddLayer,
    RemoveLayer(LayerId),
    RenameLayer(LayerId, String),
    SetLayerLocked(LayerId, bool),
    SetLayerHidden(LayerId, bool),
    SetLayerMuted(LayerId, bool),
    MoveLayerUp(LayerId),
    MoveLayerDown(LayerId),
    SelectLayer(LayerId),
    MoveItem(ItemId, LayerId, TimelineTime),
    ResizeItemStart(ItemId, TimelineTime),
    ResizeItemEnd(ItemId, TimelineTime),
    DeleteItem(ItemId),
    TrimClip(ItemId, TimelineTime, TimelineTime),
    SetGain(ItemId, f32),
    SetMuted(ItemId, bool),
    SetClipTransform(ItemId, Transform),
    SetClipOpacity(ItemId, f32),
    SetClipSpeed(ItemId, f64),
    SetClipEffects(ItemId, crate::domain::ClipEffects),
    SetKeyframe(ItemId, crate::domain::AnimProperty, crate::domain::Keyframe),
    RemoveKeyframe(ItemId, crate::domain::AnimProperty, TimelineTime),
    RemoveAnimTrack(ItemId, crate::domain::AnimProperty),
    SetSequenceSettings(SequenceSettings),
    SetExportOptions(ExportOptions),
    UpdateText(TextItem),
    StartAnalysis,
    ReanalyzeAnalysis,
    CancelAnalysis,
    ExportVir,
    ClearAnalysisCache,
    ExportThumbnail,
    TestModelConnection,
    GenerateEmbeddings,
    CancelEmbeddings,
    SearchScenes,
    SendChat,
    CancelChat,
    ClearChat,
    ApplyChatPlan,
    DiscardChatPlan,
}

/// Layout inputs that force a preview galley to be rebuilt when they change.
#[derive(PartialEq)]
struct GalleyKey {
    text: String,
    font_size: f32,
    font_family: egui::FontFamily,
    line_height: f32,
    align: TextAlign,
}

struct CachedGalley {
    key: GalleyKey,
    galley: Arc<egui::Galley>,
}

/// Cross-frame cache of laid-out preview text galleys. Glyph shaping is expensive and runs
/// for every visible text item on every frame, but the result only depends on the text
/// content and a handful of layout inputs, so it is keyed per text id and reused until one
/// of those inputs changes.
#[derive(Default)]
pub struct PreviewGalleyCache {
    entries: HashMap<TextId, CachedGalley>,
}

pub struct PanelContext<'a> {
    pub project: &'a Project,
    pub selected_item: Option<ItemId>,
    pub selected_layer: Option<LayerId>,
    pub playhead: TimelineTime,
    pub playing: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub preview_textures: &'a HashMap<ItemId, TextureHandle>,
    pub preview_error: Option<&'a str>,
    pub preview_qsv_active: bool,
    pub pixels_per_second: &'a mut f32,
    pub snap_enabled: &'a mut bool,
    pub export_options: &'a mut ExportOptions,
    pub power_profile: &'a mut PowerProfile,
    pub hardware: &'a HardwareCapabilities,
    pub available_fonts: &'a [String],
    /// Preview font keys registered with egui by the app (see `media::preview_font_key`).
    pub preview_fonts: &'a HashSet<String>,
    /// Cross-frame cache of laid-out preview text galleys (see [`PreviewGalleyCache`]).
    pub preview_galley_cache: &'a mut PreviewGalleyCache,
    pub templates: &'a mut Vec<TextTemplate>,
    /// Cast members (YMM4-style characters) that group a speaker, its voice tuning and its
    /// subtitle template.
    pub characters: &'a mut Vec<Character>,
    /// Phase 2 analysis workspace state (target, options, progress, VIR result, thumbnails).
    pub analysis: &'a mut AnalysisState,
    /// Screen rect of the timeline canvas as last laid out. The app uses it to map an OS
    /// file drop onto a timeline position, even on platforms that do not report a pointer
    /// position during a drag.
    pub timeline_drop_target: &'a mut Option<Rect>,
    /// Draft property selected in the clip keyframe UI (persists across frames).
    pub anim_property_draft: &'a mut crate::domain::AnimProperty,
    /// On-demand waveform peak cache shared by the 音声波形 and 音量モニタ panels.
    pub peak_cache: &'a mut super::monitor::WaveformCache,
    /// Filesystem navigation state for the ファイルブラウザ panel.
    pub file_browser: &'a mut super::file_browser::FileBrowserState,
    /// Saved project file path, if any — the ファイルブラウザ quick jump to
    /// 「プロジェクトのあるフォルダ」 derives from it.
    pub project_path: Option<&'a Path>,
    pub actions: &'a mut Vec<UiAction>,
}

impl TabViewer for PanelContext<'_> {
    type Tab = EditorTab;

    fn id(&mut self, tab: &mut Self::Tab) -> Id {
        Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
        tab.label().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Self::Tab) {
        match tab {
            EditorTab::Preview => preview_panel(ui, self),
            EditorTab::Timeline => timeline_panel(ui, self),
            EditorTab::Inspector => inspector_panel(ui, self),
            EditorTab::Settings => settings_panel(ui, self),
            EditorTab::Ai => super::analysis::ai_panel(ui, self),
            EditorTab::Templates => templates_panel(ui, self),
            EditorTab::Characters => characters_panel(ui, self),
            EditorTab::MediaBrowser => super::monitor::media_browser_panel(ui, self),
            EditorTab::FileBrowser => super::file_browser::file_browser_panel(ui, self),
            EditorTab::Waveform => super::monitor::waveform_panel(ui, self),
            EditorTab::LevelMeter => super::monitor::level_meter_panel(ui, self),
        }
    }

    fn closeable(&mut self, _tab: &mut Self::Tab) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Preview: canvas + direct object manipulation + YMM4-style tool row.
// ---------------------------------------------------------------------------

fn preview_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    let frame = context.project.sequence.settings.frame_size();

    ui.horizontal(|ui| {
        ui.monospace(context.playhead.to_string());
        ui.separator();
        ui.small(if context.preview_qsv_active {
            "QSVプレビュー"
        } else {
            "互換プレビュー"
        });
        if let Some(id) = context.selected_item
            && let Some(item) = context.project.item(id)
        {
            ui.separator();
            ui.small(format!("選択中: {}", item.label()));
        }
    });

    let toolbar_height = 36.0;
    let available = ui.available_size();
    let canvas_size = Vec2::new(available.x, (available.y - toolbar_height).max(60.0));
    let (canvas_rect, _response) = ui.allocate_exact_size(canvas_size, Sense::hover());
    let painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, ui.visuals().extreme_bg_color);

    let scale = (canvas_rect.width() / frame.0)
        .min(canvas_rect.height() / frame.1)
        .max(0.0001);
    let content_size = Vec2::new(frame.0 * scale, frame.1 * scale);
    let content_rect = Rect::from_center_size(canvas_rect.center(), content_size);
    painter.rect_filled(content_rect, 0.0, Color32::BLACK);

    if context.preview_textures.is_empty() {
        let message = context
            .preview_error
            .unwrap_or("タイムラインへ動画やテキストを追加してください");
        painter.text(
            content_rect.center(),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(15.0),
            Color32::LIGHT_GRAY,
        );
    } else if let Some(error) = context.preview_error {
        // The main (front-most) clip failed; the rest of the stack still paints below.
        painter.text(
            content_rect.center(),
            egui::Align2::CENTER_CENTER,
            error,
            egui::FontId::proportional(13.0),
            Color32::YELLOW,
        );
    }

    // A big, low-priority background region: clicking empty canvas deselects. Registered
    // before the (smaller) item hit-boxes below so those naturally take priority.
    let background_response = ui.interact(content_rect, Id::new("preview-empty"), Sense::click());
    if background_response.clicked() {
        context.actions.push(UiAction::Deselect);
    }

    // Composite bottom-to-top in export's z-order: clips, shapes and texts interleaved per
    // layer (front-most layer's items drawn last, on top).
    let project = context.project;
    for layer in project.sequence.layers.iter().rev() {
        if layer.hidden {
            continue;
        }
        for item in &layer.items {
            match item {
                TimelineItem::Clip(clip) => {
                    if !clip.contains_timeline_time(context.playhead) {
                        continue;
                    }
                    let Some(texture) = context.preview_textures.get(&clip.id) else {
                        continue;
                    };
                    let Some(asset) = project.asset(clip.asset_id) else {
                        continue;
                    };
                    if !asset.metadata.has_video {
                        continue;
                    }
                    let offset = context.playhead - clip.timeline_start;
                    let transform = clip.effective_transform(offset);
                    let opacity = clip.effective_opacity(offset);
                    let effects = clip.effective_effects(offset);
                    let (anchor_x, anchor_y) = transform.anchor.fraction();
                    let draw_w = frame.0 * scale * transform.scale / 100.0;
                    let draw_h = frame.1 * scale * transform.scale / 100.0;
                    // Match export: the box's top-left is (x,y) minus anchor * size; rotation
                    // pivots around the box centre.
                    let top_left_x =
                        content_rect.left() + transform.x * scale - draw_w * anchor_x;
                    let top_left_y = content_rect.top() + transform.y * scale - draw_h * anchor_y;
                    let center = Pos2::new(top_left_x + draw_w * 0.5, top_left_y + draw_h * 0.5);
                    let tint = effect_tint(effects);
                    paint_textured_quad(
                        &painter,
                        texture.id(),
                        center,
                        Vec2::new(draw_w, draw_h),
                        transform.rotation_degrees.to_radians(),
                        (opacity / 100.0).clamp(0.0, 1.0),
                        tint,
                    );
                }
                TimelineItem::Text(_) | TimelineItem::Speech(_) => {
                    if let Some(text) = item.as_text()
                        && text.contains_timeline_time(context.playhead)
                    {
                        draw_preview_text(ui, &painter, context, text, content_rect, scale);
                    }
                }
                TimelineItem::Shape(_) => {
                    if let Some(shape) = item.as_shape()
                        && shape.contains_timeline_time(context.playhead)
                    {
                        draw_preview_shape(ui, &painter, context, shape, content_rect, scale);
                    }
                }
            }
        }
    }

    ui.add_space(4.0);
    preview_toolbar(ui, context);
}

/// Approximate the export-side colour effects as a vertex tint (brightness / contrast /
/// saturation / grayscale / invert). Blur is export-only.
fn effect_tint(effects: crate::domain::ClipEffects) -> Color32 {
    if effects.is_default() {
        return Color32::WHITE;
    }
    let mut r = 128.0 + effects.brightness * 127.0;
    let mut g = 128.0 + effects.brightness * 127.0;
    let mut b = 128.0 + effects.brightness * 127.0;
    let contrast = effects.contrast;
    r = (r - 128.0) * contrast + 128.0;
    g = (g - 128.0) * contrast + 128.0;
    b = (b - 128.0) * contrast + 128.0;
    let gray = 0.299 * r + 0.587 * g + 0.114 * b;
    let sat = effects.saturation;
    r = gray + (r - gray) * sat;
    g = gray + (g - gray) * sat;
    b = gray + (b - gray) * sat;
    if effects.grayscale {
        r = gray;
        g = gray;
        b = gray;
    }
    if effects.invert {
        r = 255.0 - r;
        g = 255.0 - g;
        b = 255.0 - b;
    }
    Color32::from_rgb(
        r.clamp(0.0, 255.0) as u8,
        g.clamp(0.0, 255.0) as u8,
        b.clamp(0.0, 255.0) as u8,
    )
}

/// Paints a textured quad centred at `center`, rotated by `rotation_rad`, tinted and multiplied
/// by `opacity`. Used so clip transform / effects show up in the live preview.
fn paint_textured_quad(
    painter: &egui::Painter,
    texture: egui::TextureId,
    center: Pos2,
    size: Vec2,
    rotation_rad: f32,
    opacity: f32,
    tint: Color32,
) {
    let hw = size.x * 0.5;
    let hh = size.y * 0.5;
    let (sin, cos) = (rotation_rad.sin(), rotation_rad.cos());
    let corners = [
        Vec2::new(-hw, -hh),
        Vec2::new(hw, -hh),
        Vec2::new(hw, hh),
        Vec2::new(-hw, hh),
    ];
    let uvs = [
        Pos2::new(0.0, 0.0),
        Pos2::new(1.0, 0.0),
        Pos2::new(1.0, 1.0),
        Pos2::new(0.0, 1.0),
    ];
    let mut mesh = egui::Mesh::with_texture(texture);
    let a = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    let color = Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), a);
    for i in 0..4 {
        let v = corners[i];
        let rotated = Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center + rotated,
            uv: uvs[i],
            color,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

fn draw_preview_text(
    ui: &Ui,
    painter: &egui::Painter,
    context: &mut PanelContext<'_>,
    text: &TextItem,
    content_rect: Rect,
    scale: f32,
) {
    let selected = context.selected_item == Some(text.id);
    let font_px = (text.font_size * text.transform.scale / 100.0 * scale).max(1.0);
    let font_id = preview_text_font(context, text, font_px);
    let galley = layout_preview_text(
        painter,
        context.preview_galley_cache,
        text,
        font_id,
        scale,
    );
    let text_size = galley.size().max(Vec2::splat(6.0));
    let (anchor_x, anchor_y) = text.transform.anchor.fraction();
    let center = Pos2::new(
        content_rect.left() + text.transform.x * scale,
        content_rect.top() + text.transform.y * scale,
    );
    // `galley.rect` is the text's logical box. For center/right alignment egui shifts the rows
    // so that its left edge is negative and the origin sits in the middle of the text, so the
    // anchor must place the *box* and the paint origin must compensate for that shift. Without
    // this the selection/background rectangle lands half (or a full) text-width away from the
    // glyphs.
    let box_min = Pos2::new(
        center.x - text_size.x * anchor_x,
        center.y - text_size.y * anchor_y,
    );
    let nominal_rect = Rect::from_min_size(box_min, text_size);

    let move_id = Id::new(("preview-move", text.id));
    let move_response = ui.interact(nominal_rect, move_id, Sense::click_and_drag());

    // Capture the grab offset once, then track the live position from the pointer so the
    // commit also works on the release frame (where `dragged()` is already false).
    if move_response.drag_started()
        && let Some(pointer) = move_response.interact_pointer_pos()
    {
        ui.data_mut(|data| {
            data.insert_temp(
                move_id.with("preview-grab"),
                PreviewMoveGrab {
                    offset: pointer - nominal_rect.min,
                },
            );
        });
    }
    let mut draw_rect = nominal_rect;
    if move_response.dragged() || move_response.drag_stopped() {
        if let Some(pointer) = move_response.interact_pointer_pos() {
            let grab = ui
                .data(|data| data.get_temp::<PreviewMoveGrab>(move_id.with("preview-grab")))
                .map(|grab| grab.offset)
                .unwrap_or(Vec2::ZERO);
            draw_rect = Rect::from_min_size(pointer - grab, nominal_rect.size());
        }
    }

    let paint_pos = draw_rect.min - galley.rect.min.to_vec2();
    paint_preview_text(
        painter,
        &galley,
        paint_pos,
        draw_rect,
        text,
        context.playhead,
        scale,
    );

    painter.rect_stroke(
        draw_rect,
        2.0,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                Color32::from_rgb(255, 210, 80)
            } else {
                Color32::from_white_alpha(60)
            },
        ),
        StrokeKind::Outside,
    );

    if move_response.clicked() {
        context.actions.push(UiAction::SelectItem(text.id));
    }
    if move_response.drag_stopped() {
        if let Some(pointer) = move_response.interact_pointer_pos() {
            let grab = ui
                .data(|data| data.get_temp::<PreviewMoveGrab>(move_id.with("preview-grab")))
                .map(|grab| grab.offset)
                .unwrap_or(Vec2::ZERO);
            let new_top_left = pointer - grab;
            let new_center =
                new_top_left + Vec2::new(text_size.x * anchor_x, text_size.y * anchor_y);
            let mut updated = text.clone();
            updated.transform.x = (new_center.x - content_rect.left()) / scale;
            updated.transform.y = (new_center.y - content_rect.top()) / scale;
            context.actions.push(UiAction::UpdateText(updated));
            ui.data_mut(|data| {
                data.remove::<PreviewMoveGrab>(move_id.with("preview-grab"));
            });
        }
    }

    if selected {
        let handle_id = Id::new(("preview-resize", text.id));
        let handle_rect = Rect::from_center_size(draw_rect.right_bottom(), Vec2::splat(10.0));
        let handle_response = ui.interact(handle_rect, handle_id, Sense::drag());
        if handle_response.drag_started()
            && let Some(pointer) = handle_response.interact_pointer_pos()
        {
            ui.data_mut(|data| {
                data.insert_temp(
                    handle_id.with("preview-resize-grab"),
                    PreviewResizeGrab {
                        start_pointer_x: pointer.x,
                        start_width: text_size.x,
                        start_font_size: text.font_size,
                    },
                );
            });
        }
        let mut handle_draw_rect = handle_rect;
        if handle_response.dragged() || handle_response.drag_stopped() {
            if let Some(pointer) = handle_response.interact_pointer_pos() {
                let grab = ui
                    .data(|data| {
                        data.get_temp::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"))
                    })
                    .unwrap_or(PreviewResizeGrab {
                        start_pointer_x: pointer.x,
                        start_width: text_size.x,
                        start_font_size: text.font_size,
                    });
                handle_draw_rect =
                    handle_rect.translate(Vec2::new(pointer.x - grab.start_pointer_x, 0.0));
            }
        }
        painter.rect_filled(handle_draw_rect, 2.0, Color32::from_rgb(255, 210, 80));
        if handle_response.drag_stopped() {
            if let Some(pointer) = handle_response.interact_pointer_pos() {
                let grab = ui
                    .data(|data| {
                        data.get_temp::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"))
                    })
                    .unwrap_or(PreviewResizeGrab {
                        start_pointer_x: pointer.x,
                        start_width: text_size.x,
                        start_font_size: text.font_size,
                    });
                let new_width = (grab.start_width + pointer.x - grab.start_pointer_x).max(4.0);
                let ratio = new_width / grab.start_width.max(1.0);
                let mut updated = text.clone();
                updated.font_size = (grab.start_font_size * ratio).clamp(1.0, 1000.0);
                context.actions.push(UiAction::UpdateText(updated));
                ui.data_mut(|data| {
                    data.remove::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"));
                });
            }
        }
    }
}

/// Font for preview text: the system font chosen in the inspector when it has already been
/// registered with egui, otherwise the built-in proportional family (which still covers
/// Japanese thanks to the fallback font loaded at startup).
fn preview_text_font(context: &PanelContext<'_>, text: &TextItem, size: f32) -> egui::FontId {
    if let Some(family) = &text.font_family {
        let key = crate::media::preview_font_key(family, text.bold, text.italic);
        if context.preview_fonts.contains(&key) {
            return egui::FontId::new(size, egui::FontFamily::Name(key.into()));
        }
    }
    egui::FontId::proportional(size)
}

/// Lays out preview text honoring alignment and extra line spacing. The color is left as a
/// placeholder so the same galley can be re-painted in the outline/shadow/fill colors.
/// Glyph shaping is expensive, so the result is cached per text id and reused until any
/// layout input changes.
fn layout_preview_text(
    painter: &egui::Painter,
    cache: &mut PreviewGalleyCache,
    text: &TextItem,
    font_id: egui::FontId,
    scale: f32,
) -> Arc<egui::Galley> {
    let line_height = (font_id.size + text.line_spacing * scale).max(1.0);
    let key = GalleyKey {
        text: text.text.clone(),
        font_size: font_id.size,
        font_family: font_id.family.clone(),
        line_height,
        align: text.align,
    };
    if let Some(hit) = cache.entries.get(&text.id)
        && hit.key == key
    {
        return Arc::clone(&hit.galley);
    }
    let mut job = egui::text::LayoutJob {
        halign: match text.align {
            TextAlign::Left => egui::Align::Min,
            TextAlign::Center => egui::Align::Center,
            TextAlign::Right => egui::Align::Max,
        },
        wrap: egui::text::TextWrapping {
            max_width: f32::INFINITY,
            ..Default::default()
        },
        ..Default::default()
    };
    job.append(
        &text.text,
        0.0,
        egui::TextFormat {
            font_id,
            color: Color32::PLACEHOLDER,
            line_height: Some(line_height),
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    cache.entries.insert(
        text.id,
        CachedGalley {
            key,
            galley: Arc::clone(&galley),
        },
    );
    galley
}

/// Draws text with the same visual features as the export (background box, shadow, outline,
/// fill) so the preview can be trusted as a WYSIWYG canvas. `paint_pos` is where the galley's
/// (possibly offset) origin goes, while `box_rect` is the text's logical bounding box.
fn paint_preview_text(
    painter: &egui::Painter,
    galley: &Arc<egui::Galley>,
    paint_pos: Pos2,
    box_rect: Rect,
    text: &TextItem,
    time: TimelineTime,
    scale: f32,
) {
    let alpha = text_fade_alpha(text, time);

    if text.background_enabled {
        let padding = (text.background_padding * scale).max(0.0);
        painter.rect_filled(
            box_rect.expand(padding),
            0.0,
            with_alpha(rgba_to_color32(text.background_color_rgba), alpha),
        );
    }

    if text.shadow_enabled {
        let offset = Vec2::new(text.shadow_offset_x, text.shadow_offset_y) * scale;
        painter.galley(
            paint_pos + offset,
            galley.clone(),
            with_alpha(rgba_to_color32(text.shadow_color_rgba), alpha),
        );
    }

    if text.outline_enabled && text.outline_width > 0.0 {
        let width = (text.outline_width * scale).max(0.5);
        let outline = with_alpha(rgba_to_color32(text.outline_color_rgba), alpha);
        // Approximate a text stroke by stamping the glyphs around the fill in 8 directions.
        for (dx, dy) in [
            (1.0, 0.0),
            (-1.0, 0.0),
            (0.0, 1.0),
            (0.0, -1.0),
            (0.7, 0.7),
            (-0.7, 0.7),
            (0.7, -0.7),
            (-0.7, -0.7),
        ] {
            painter.galley(
                paint_pos + Vec2::new(dx * width, dy * width),
                galley.clone(),
                outline,
            );
        }
    }

    painter.galley(
        paint_pos,
        galley.clone(),
        with_alpha(rgba_to_color32(text.color_rgba), alpha),
    );
}

/// Linear fade-in/out opacity at `time`, matching the export's alpha expression.
fn text_fade_alpha(text: &TextItem, time: TimelineTime) -> f32 {
    let mut alpha = 1.0_f32;
    if text.fade_in > TimelineTime::ZERO {
        let elapsed = (time - text.timeline_start).as_seconds() as f32;
        alpha = alpha.min(elapsed / text.fade_in.as_seconds().max(0.001) as f32);
    }
    if text.fade_out > TimelineTime::ZERO {
        let remaining = (text.timeline_end() - time).as_seconds() as f32;
        alpha = alpha.min(remaining / text.fade_out.as_seconds().max(0.001) as f32);
    }
    alpha.clamp(0.0, 1.0)
}

fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    let a = (color.a() as f32 * alpha).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

fn draw_preview_shape(
    ui: &Ui,
    painter: &egui::Painter,
    context: &mut PanelContext<'_>,
    shape: &ShapeItem,
    content_rect: Rect,
    scale: f32,
) {
    let selected = context.selected_item == Some(shape.id);
    let size = Vec2::new(
        (shape.width * shape.transform.scale / 100.0 * scale).max(1.0),
        (shape.height * shape.transform.scale / 100.0 * scale).max(1.0),
    );
    let (anchor_x, anchor_y) = shape.transform.anchor.fraction();
    let center = Pos2::new(
        content_rect.left() + shape.transform.x * scale,
        content_rect.top() + shape.transform.y * scale,
    );
    let top_left = Pos2::new(center.x - size.x * anchor_x, center.y - size.y * anchor_y);
    let nominal_rect = Rect::from_min_size(top_left, size);

    let move_id = Id::new(("preview-shape-move", shape.id));
    let move_response = ui.interact(nominal_rect, move_id, Sense::click_and_drag());
    if move_response.drag_started()
        && let Some(pointer) = move_response.interact_pointer_pos()
    {
        ui.data_mut(|data| {
            data.insert_temp(
                move_id.with("preview-grab"),
                PreviewMoveGrab {
                    offset: pointer - nominal_rect.min,
                },
            );
        });
    }
    let mut draw_rect = nominal_rect;
    if move_response.dragged() || move_response.drag_stopped() {
        if let Some(pointer) = move_response.interact_pointer_pos() {
            let grab = ui
                .data(|data| data.get_temp::<PreviewMoveGrab>(move_id.with("preview-grab")))
                .map(|grab| grab.offset)
                .unwrap_or(Vec2::ZERO);
            draw_rect = Rect::from_min_size(pointer - grab, nominal_rect.size());
        }
    }

    let color = shape_color(shape);
    match shape.kind {
        ShapeKind::Rectangle => {
            painter.rect_filled(draw_rect, 0.0, color);
        }
        ShapeKind::Ellipse => {
            let points = ellipse_points(
                draw_rect.center(),
                draw_rect.width() / 2.0,
                draw_rect.height() / 2.0,
            );
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
    }

    if selected {
        painter.rect_stroke(
            draw_rect,
            2.0,
            Stroke::new(2.0, Color32::from_rgb(255, 210, 80)),
            StrokeKind::Outside,
        );
    } else if move_response.hovered() {
        painter.rect_stroke(
            draw_rect,
            2.0,
            Stroke::new(1.0, Color32::from_white_alpha(60)),
            StrokeKind::Outside,
        );
    }

    if move_response.clicked() {
        context.actions.push(UiAction::SelectItem(shape.id));
    }
    if move_response.drag_stopped()
        && let Some(pointer) = move_response.interact_pointer_pos()
    {
        let grab = ui
            .data(|data| data.get_temp::<PreviewMoveGrab>(move_id.with("preview-grab")))
            .map(|grab| grab.offset)
            .unwrap_or(Vec2::ZERO);
        let new_top_left = pointer - grab;
        let new_center = new_top_left + Vec2::new(size.x * anchor_x, size.y * anchor_y);
        let mut updated = shape.clone();
        updated.transform.x = (new_center.x - content_rect.left()) / scale;
        updated.transform.y = (new_center.y - content_rect.top()) / scale;
        context.actions.push(UiAction::UpdateShape(updated));
        ui.data_mut(|data| {
            data.remove::<PreviewMoveGrab>(move_id.with("preview-grab"));
        });
    }

    if selected {
        let handle_id = Id::new(("preview-shape-resize", shape.id));
        let handle_rect = Rect::from_center_size(draw_rect.right_bottom(), Vec2::splat(10.0));
        let handle_response = ui.interact(handle_rect, handle_id, Sense::drag());
        // `start_font_size` is reused to carry the starting uniform scale percentage.
        if handle_response.drag_started()
            && let Some(pointer) = handle_response.interact_pointer_pos()
        {
            ui.data_mut(|data| {
                data.insert_temp(
                    handle_id.with("preview-resize-grab"),
                    PreviewResizeGrab {
                        start_pointer_x: pointer.x,
                        start_width: size.x,
                        start_font_size: shape.transform.scale,
                    },
                );
            });
        }
        let mut handle_draw_rect = handle_rect;
        if handle_response.dragged() || handle_response.drag_stopped() {
            if let Some(pointer) = handle_response.interact_pointer_pos() {
                let grab = ui
                    .data(|data| {
                        data.get_temp::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"))
                    })
                    .unwrap_or(PreviewResizeGrab {
                        start_pointer_x: pointer.x,
                        start_width: size.x,
                        start_font_size: shape.transform.scale,
                    });
                handle_draw_rect =
                    handle_rect.translate(Vec2::new(pointer.x - grab.start_pointer_x, 0.0));
            }
        }
        painter.rect_filled(handle_draw_rect, 2.0, Color32::from_rgb(255, 210, 80));
        if handle_response.drag_stopped()
            && let Some(pointer) = handle_response.interact_pointer_pos()
        {
            let grab = ui
                .data(|data| {
                    data.get_temp::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"))
                })
                .unwrap_or(PreviewResizeGrab {
                    start_pointer_x: pointer.x,
                    start_width: size.x,
                    start_font_size: shape.transform.scale,
                });
            let new_width = (grab.start_width + pointer.x - grab.start_pointer_x).max(4.0);
            let ratio = new_width / grab.start_width.max(1.0);
            let mut updated = shape.clone();
            updated.transform.scale = (grab.start_font_size * ratio).clamp(1.0, 1000.0);
            context.actions.push(UiAction::UpdateShape(updated));
            ui.data_mut(|data| {
                data.remove::<PreviewResizeGrab>(handle_id.with("preview-resize-grab"));
            });
        }
    }
}

fn shape_color(shape: &ShapeItem) -> Color32 {
    let alpha = (shape.opacity / 100.0).clamp(0.0, 1.0) * (shape.color_rgba[3] as f32 / 255.0);
    let a = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(
        shape.color_rgba[0],
        shape.color_rgba[1],
        shape.color_rgba[2],
        a,
    )
}

fn ellipse_points(center: Pos2, rx: f32, ry: f32) -> Vec<Pos2> {
    const SEGMENTS: usize = 64;
    (0..SEGMENTS)
        .map(|i| {
            let angle = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
            Pos2::new(center.x + rx * angle.cos(), center.y + ry * angle.sin())
        })
        .collect()
}

fn preview_toolbar(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("先頭へ").clicked() {
            context.actions.push(UiAction::Seek(TimelineTime::ZERO));
        }
        if ui.button("◀").on_hover_text("1フレーム戻る (←)").clicked() {
            context.actions.push(UiAction::StepFrame(-1));
        }
        if ui
            .button(if context.playing { "⏸" } else { "▶" })
            .on_hover_text("再生 / 一時停止 (Space)")
            .clicked()
        {
            context.actions.push(UiAction::TogglePlayback);
        }
        if ui.button("▶|").on_hover_text("1フレーム進む (→)").clicked() {
            context.actions.push(UiAction::StepFrame(1));
        }
        if ui.button("⏭").on_hover_text("末尾へ").clicked() {
            context
                .actions
                .push(UiAction::Seek(context.project.duration()));
        }
        ui.separator();
        if ui
            .button("✂ 分割")
            .on_hover_text("再生位置でクリップ／セリフを分割")
            .clicked()
        {
            context.actions.push(UiAction::SplitAtPlayhead);
        }
        if ui
            .add_enabled(context.selected_item.is_some(), egui::Button::new("🗑 削除"))
            .clicked()
        {
            context.actions.push(UiAction::DeleteSelected);
        }
        ui.separator();
        if ui
            .add_enabled(context.can_undo, egui::Button::new("↶"))
            .on_hover_text("元に戻す")
            .clicked()
        {
            context.actions.push(UiAction::Undo);
        }
        if ui
            .add_enabled(context.can_redo, egui::Button::new("↷"))
            .on_hover_text("やり直す")
            .clicked()
        {
            context.actions.push(UiAction::Redo);
        }
        ui.separator();
        if ui
            .button("🔤 ＋テキスト")
            .on_hover_text("再生位置にテキストを追加")
            .clicked()
        {
            context.actions.push(UiAction::AddText(context.playhead));
        }
        if ui
            .button("⬛ ＋図形")
            .on_hover_text("再生位置に図形を追加")
            .clicked()
        {
            context.actions.push(UiAction::AddShape(context.playhead));
        }
        if ui
            .button("🎞 ＋メディア")
            .on_hover_text("動画・音声・画像ファイルを選んでタイムラインへ追加")
            .clicked()
        {
            context.actions.push(UiAction::ImportMediaDialog);
        }
    });
}

// ---------------------------------------------------------------------------
// Timeline: thin general-purpose layers, drag-to-move/trim/reorder, snapping,
// and direct file import (no separate media library tab).
// ---------------------------------------------------------------------------

const LABEL_WIDTH: f32 = 210.0;
const RULER_HEIGHT: f32 = 22.0;
const ROW_HEIGHT: f32 = 32.0;
const EDGE_HANDLE_WIDTH: f32 = 7.0;

/// Per-drag state cached in egui's temporary data store so a dragged item can be
/// committed correctly on the release frame (when `Response::dragged()` is already false).
#[derive(Clone, Copy)]
struct GrabOffset(f32);

#[derive(Clone, Copy)]
struct PreviewMoveGrab {
    offset: Vec2,
}

#[derive(Clone, Copy)]
struct PreviewResizeGrab {
    start_pointer_x: f32,
    start_width: f32,
    start_font_size: f32,
}

/// Mouse wheel (Line/Page) over the timeline pans the time axis horizontally instead of
/// scrolling the layer list vertically. Trackpad (Point) deltas are left alone so two-finger
/// scrolling still moves layers.
///
/// `InputState::wheel` is private in egui, so the multi-frame smoothing buffer cannot be
/// rotated at the source. Instead a short-lived temp flag keeps rotating the per-frame
/// `smooth_scroll_delta` snapshot while that buffer drains (a few frames per notch).
fn rotate_mouse_wheel_to_horizontal(ui: &mut Ui) {
    let rotate_id = Id::new("timeline-wheel-horizontal");
    let time = ui.input(|input| input.time);
    let (wheel_event, trackpad_event) = ui.input(|input| {
        let mut wheel = false;
        let mut trackpad = false;
        for event in &input.raw.events {
            if let egui::Event::MouseWheel { unit, .. } = event {
                match unit {
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => wheel = true,
                    egui::MouseWheelUnit::Point => trackpad = true,
                }
            }
        }
        (wheel, trackpad)
    });
    // Ctrl/Cmd+wheel is global zoom; do not steal it or its leftover smoothing.
    if ui.input(|input| input.modifiers.matches_any(egui::Modifiers::COMMAND)) {
        ui.data_mut(|data| {
            data.remove_temp::<f64>(rotate_id);
        });
        return;
    }
    if wheel_event {
        ui.data_mut(|data| {
            data.insert_temp(rotate_id, time);
        });
    } else if trackpad_event {
        // A trackpad gesture takes over: keep its native vertical behaviour.
        ui.data_mut(|data| {
            data.remove_temp::<f64>(rotate_id);
        });
        return;
    } else {
        let draining = ui
            .data(|data| data.get_temp::<f64>(rotate_id))
            .is_some_and(|since| time - since < 0.30);
        if !draining {
            return;
        }
    }
    ui.input_mut(|input| {
        input.smooth_scroll_delta.x += input.smooth_scroll_delta.y;
        input.smooth_scroll_delta.y = 0.0;
    });
}

fn timeline_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    if ui.rect_contains_pointer(ui.max_rect()) {
        rotate_mouse_wheel_to_horizontal(ui);
    }
    ui.horizontal(|ui| {
        if ui
            .button("＋レイヤー")
            .on_hover_text("新しい空のレイヤーを追加")
            .clicked()
        {
            context.actions.push(UiAction::AddLayer);
        }
        if ui
            .button("＋ファイル")
            .on_hover_text("動画・音声・画像ファイルを選んでタイムラインへ直接追加")
            .clicked()
        {
            context.actions.push(UiAction::ImportMediaDialog);
        }
        if ui
            .button("＋テキスト")
            .on_hover_text("再生位置にテキストを追加")
            .clicked()
        {
            context.actions.push(UiAction::AddText(context.playhead));
        }
        if ui
            .button("＋図形")
            .on_hover_text("再生位置に図形を追加")
            .clicked()
        {
            context.actions.push(UiAction::AddShape(context.playhead));
        }
        if !context.project.assets.is_empty() {
            ui.menu_button("📁 素材を再利用", |ui| {
                for asset in &context.project.assets {
                    if ui.button(&asset.name).clicked() {
                        context
                            .actions
                            .push(UiAction::AddAssetAt(asset.id, context.playhead));
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        ui.label("ズーム");
        ui.add(egui::Slider::new(context.pixels_per_second, 10.0..=240.0).logarithmic(true));
        ui.checkbox(context.snap_enabled, "スナップ");
    });
    ui.separator();

    let layer_count = context.project.sequence.layers.len().max(1);
    let total_row_height = RULER_HEIGHT + ROW_HEIGHT * layer_count as f32;
    let timeline_width =
        (context.project.duration().as_seconds() as f32 * *context.pixels_per_second + 400.0)
            .max(ui.available_width() - LABEL_WIDTH);

    egui::ScrollArea::vertical()
        .id_salt("timeline-vscroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(LABEL_WIDTH);
                    ui.add_space(RULER_HEIGHT);
                    for layer in &context.project.sequence.layers {
                        layer_header_row(ui, context, layer);
                    }
                });
                ui.separator();
                egui::ScrollArea::horizontal()
                    .id_salt("timeline-hscroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        timeline_canvas(ui, context, timeline_width, total_row_height);
                    });
            });
        });

    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button("🖼 サムネ出力")
            .on_hover_text("現在の再生位置のフレームを samune.png として書き出す")
            .clicked()
        {
            context.actions.push(UiAction::ExportThumbnail);
        }
    });
}

fn layer_header_row(ui: &mut Ui, context: &mut PanelContext<'_>, layer: &Layer) {
    ui.allocate_ui_with_layout(
        Vec2::new(LABEL_WIDTH, ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false),
        |ui| {
            let is_active = context.selected_layer == Some(layer.id);
            if is_active {
                ui.painter().rect_filled(
                    ui.max_rect(),
                    0.0,
                    ui.visuals().selection.bg_fill.linear_multiply(0.25),
                );
            }
            let lock_icon = if layer.locked { "🔒" } else { "🔓" };
            if ui
                .small_button(lock_icon)
                .on_hover_text("ロック / ロック解除")
                .clicked()
            {
                context
                    .actions
                    .push(UiAction::SetLayerLocked(layer.id, !layer.locked));
            }
            let hidden_icon = if layer.hidden { "🚫" } else { "👁" };
            if ui
                .small_button(hidden_icon)
                .on_hover_text("表示 / 非表示")
                .clicked()
            {
                context
                    .actions
                    .push(UiAction::SetLayerHidden(layer.id, !layer.hidden));
            }
            let mute_icon = if layer.muted { "🔇" } else { "🔊" };
            if ui
                .small_button(mute_icon)
                .on_hover_text("音声ミュート")
                .clicked()
            {
                context
                    .actions
                    .push(UiAction::SetLayerMuted(layer.id, !layer.muted));
            }
            let mut name = layer.name.clone();
            let response = ui.add(
                egui::TextEdit::singleline(&mut name)
                    .desired_width(ui.available_width())
                    .hint_text("レイヤー名"),
            );
            if response.gained_focus() || response.clicked() {
                context.actions.push(UiAction::SelectLayer(layer.id));
            }
            if response.lost_focus() && name != layer.name && !name.trim().is_empty() {
                context.actions.push(UiAction::RenameLayer(layer.id, name));
            }
            response.context_menu(|ui| {
                if ui.button("⬆ 上のレイヤーへ移動").clicked() {
                    context.actions.push(UiAction::MoveLayerUp(layer.id));
                    ui.close();
                }
                if ui.button("⬇ 下のレイヤーへ移動").clicked() {
                    context.actions.push(UiAction::MoveLayerDown(layer.id));
                    ui.close();
                }
                if context.project.sequence.layers.len() > 1
                    && ui.button("🗑 レイヤーを削除").clicked()
                {
                    context.actions.push(UiAction::RemoveLayer(layer.id));
                    ui.close();
                }
            });
        },
    );
}

fn timeline_canvas(
    ui: &mut Ui,
    context: &mut PanelContext<'_>,
    timeline_width: f32,
    total_row_height: f32,
) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(timeline_width, total_row_height),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

    let pixels_per_second = *context.pixels_per_second;
    let origin_x = rect.left();

    let visible_seconds = timeline_width / pixels_per_second;
    let ruler_step = if pixels_per_second > 100.0 {
        1
    } else if pixels_per_second > 35.0 {
        5
    } else {
        10
    };
    for second in (0..=visible_seconds.ceil() as usize).step_by(ruler_step.max(1)) {
        let x = origin_x + second as f32 * pixels_per_second;
        painter.line_segment(
            [
                Pos2::new(x, rect.top()),
                Pos2::new(x, rect.top() + RULER_HEIGHT),
            ],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.fg_stroke.color),
        );
        painter.text(
            Pos2::new(x + 3.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{second}s"),
            egui::FontId::monospace(9.0),
            ui.visuals().text_color(),
        );
    }

    for (row_index, layer) in context.project.sequence.layers.iter().enumerate() {
        let top = rect.top() + RULER_HEIGHT + row_index as f32 * ROW_HEIGHT;
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.left(), top),
            Vec2::new(rect.width(), ROW_HEIGHT),
        );
        let is_active = context.selected_layer == Some(layer.id);
        painter.rect_filled(
            row_rect,
            0.0,
            if is_active {
                ui.visuals().faint_bg_color
            } else {
                ui.visuals().extreme_bg_color
            },
        );
        painter.line_segment(
            [
                Pos2::new(rect.left(), top + ROW_HEIGHT),
                Pos2::new(rect.right(), top + ROW_HEIGHT),
            ],
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );
        if layer.hidden {
            painter.rect_filled(row_rect, 0.0, Color32::from_black_alpha(70));
        }
    }

    let snap_points: Vec<f64> = if *context.snap_enabled {
        let mut points = vec![0.0, context.playhead.as_seconds()];
        for layer in &context.project.sequence.layers {
            for item in &layer.items {
                points.push(item.timeline_start().as_seconds());
                points.push(item.timeline_end().as_seconds());
            }
        }
        points
    } else {
        Vec::new()
    };
    let snap_threshold = (8.0 / pixels_per_second) as f64;

    for (row_index, layer) in context.project.sequence.layers.iter().enumerate() {
        let top = rect.top() + RULER_HEIGHT + row_index as f32 * ROW_HEIGHT;
        for item in &layer.items {
            draw_timeline_item(
                ui,
                &painter,
                context,
                item,
                layer,
                top,
                rect,
                pixels_per_second,
                &snap_points,
                snap_threshold,
            );
        }
    }

    let playhead_x = origin_x + context.playhead.as_seconds() as f32 * pixels_per_second;
    painter.line_segment(
        [
            Pos2::new(playhead_x, rect.top()),
            Pos2::new(playhead_x, rect.bottom()),
        ],
        Stroke::new(2.0, Color32::from_rgb(255, 92, 92)),
    );

    // The playhead itself can be grabbed and dragged to scrub, with a wider invisible
    // hit target so it is easy to catch (the visible line is only a couple of pixels).
    let scrub_handle_rect = Rect::from_min_max(
        Pos2::new(playhead_x - 6.0, rect.top()),
        Pos2::new(playhead_x + 6.0, rect.bottom()),
    );
    let scrub_response = ui.interact(scrub_handle_rect, Id::new("playhead-scrub"), Sense::drag());
    if (scrub_response.dragged() || scrub_response.drag_stopped())
        && let Some(pointer) = scrub_response.interact_pointer_pos()
    {
        context
            .actions
            .push(UiAction::Seek(timeline_time_from_pointer(
                pointer.x,
                origin_x,
                pixels_per_second,
            )));
    }

    // Highlight the whole canvas while OS files are hovering: the drag-and-drop protocol does
    // not reliably track the pointer, so the cue must not depend on its position.
    if ui.ctx().input(|input| !input.raw.hovered_files.is_empty()) {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0, Color32::LIGHT_GREEN),
            StrokeKind::Inside,
        );
    }

    // Remember where the canvas is so the app can turn a file drop into a timeline time. The
    // OS drag-and-drop protocol does not reliably update the egui pointer, so the drop itself
    // is handled globally in the app rather than here.
    *context.timeline_drop_target = Some(rect);

    // Clicking the ruler or an empty area of a row seeks; dragging scrubs continuously.
    if (response.dragged() || response.clicked() || response.drag_stopped())
        && let Some(pointer) = response.interact_pointer_pos()
        && pointer.x >= origin_x
    {
        context
            .actions
            .push(UiAction::Seek(timeline_time_from_pointer(
                pointer.x,
                origin_x,
                pixels_per_second,
            )));
    }
}

fn timeline_time_from_pointer(
    pointer_x: f32,
    origin_x: f32,
    pixels_per_second: f32,
) -> TimelineTime {
    let seconds = ((pointer_x - origin_x) / pixels_per_second).max(0.0);
    TimelineTime::from_seconds(seconds as f64)
}

#[allow(clippy::too_many_arguments)]
fn draw_timeline_item(
    ui: &Ui,
    painter: &egui::Painter,
    context: &mut PanelContext<'_>,
    item: &TimelineItem,
    layer: &Layer,
    row_top: f32,
    canvas_rect: Rect,
    pixels_per_second: f32,
    snap_points: &[f64],
    snap_threshold: f64,
) {
    let item_id = item.id();
    let is_clip = item.as_clip().is_some();
    let is_shape = item.as_shape().is_some();
    let is_speech = item.as_speech().is_some();
    let is_image = item.as_clip().is_some_and(|clip| {
        context
            .project
            .asset(clip.asset_id)
            .is_some_and(|asset| asset.metadata.is_image)
    });
    let is_audio_only = item.as_clip().is_some_and(|clip| {
        context
            .project
            .asset(clip.asset_id)
            .is_some_and(|asset| !asset.metadata.has_video)
    });
    let selected = context.selected_item == Some(item_id);
    let base_color = if is_speech {
        if selected {
            Color32::from_rgb(80, 210, 200)
        } else {
            Color32::from_rgb(45, 135, 130)
        }
    } else if is_audio_only {
        if selected {
            Color32::from_rgb(90, 210, 120)
        } else {
            Color32::from_rgb(60, 140, 85)
        }
    } else if is_shape {
        if selected {
            Color32::from_rgb(195, 120, 255)
        } else {
            Color32::from_rgb(125, 70, 175)
        }
    } else if is_clip {
        if selected {
            Color32::from_rgb(90, 150, 255)
        } else {
            Color32::from_rgb(55, 95, 165)
        }
    } else if selected {
        Color32::from_rgb(235, 150, 60)
    } else {
        Color32::from_rgb(160, 90, 40)
    };

    let origin_x = canvas_rect.left();
    let start_seconds = item.timeline_start().as_seconds();
    let duration_seconds = item.duration().as_seconds();
    let nominal_x = origin_x + start_seconds as f32 * pixels_per_second;
    let nominal_width =
        (duration_seconds as f32 * pixels_per_second).max(EDGE_HANDLE_WIDTH * 2.0 + 4.0);
    let nominal_rect = Rect::from_min_size(
        Pos2::new(nominal_x, row_top + 3.0),
        Vec2::new(nominal_width, ROW_HEIGHT - 6.0),
    );
    let body_rect = Rect::from_min_max(
        Pos2::new(nominal_rect.left() + EDGE_HANDLE_WIDTH, nominal_rect.top()),
        Pos2::new(
            nominal_rect.right() - EDGE_HANDLE_WIDTH,
            nominal_rect.bottom(),
        ),
    );

    let locked = layer.locked;
    let move_id = Id::new(("timeline-move", item_id));
    let move_response = ui.interact(
        body_rect,
        move_id,
        if locked {
            Sense::click()
        } else {
            Sense::click_and_drag()
        },
    );
    let start_response = (!locked).then(|| {
        let handle_rect = Rect::from_min_size(
            nominal_rect.left_top(),
            Vec2::new(EDGE_HANDLE_WIDTH, nominal_rect.height()),
        );
        ui.interact(
            handle_rect,
            Id::new(("timeline-resize-start", item_id)),
            Sense::drag(),
        )
    });
    let end_response = (!locked).then(|| {
        let handle_rect = Rect::from_min_size(
            Pos2::new(nominal_rect.right() - EDGE_HANDLE_WIDTH, nominal_rect.top()),
            Vec2::new(EDGE_HANDLE_WIDTH, nominal_rect.height()),
        );
        ui.interact(
            handle_rect,
            Id::new(("timeline-resize-end", item_id)),
            Sense::drag(),
        )
    });

    let mut draw_rect = nominal_rect;
    let mut resize_start_seconds = start_seconds;
    let mut resize_end_seconds = start_seconds + duration_seconds;

    // On the final release frame `dragged()` is already false, which makes
    // `total_drag_delta()` return `None` and would silently drop the edit. To avoid
    // that, the grab offset is captured when the drag starts and reused against the
    // (still-valid) pointer position on the release frame.
    if move_response.dragged() || move_response.drag_stopped() {
        if move_response.drag_started()
            && let Some(pointer) = move_response.interact_pointer_pos()
        {
            ui.data_mut(|data| {
                data.insert_temp(
                    move_id.with("grab-offset"),
                    GrabOffset(pointer.x - nominal_x),
                );
            });
        }
        let grab_offset = ui
            .data(|data| data.get_temp::<GrabOffset>(move_id.with("grab-offset")))
            .map(|offset| offset.0)
            .unwrap_or(0.0);
        if let Some(pointer) = move_response.interact_pointer_pos() {
            let tentative_x = pointer.x - grab_offset;
            let tentative = snap_seconds(
                ((tentative_x - origin_x) / pixels_per_second) as f64,
                snap_points,
                snap_threshold,
            )
            .max(0.0);
            let mut top = nominal_rect.top();
            if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                let target_row = target_row_index(context, canvas_rect, pointer);
                top = canvas_rect.top() + RULER_HEIGHT + target_row as f32 * ROW_HEIGHT + 3.0;
            }
            let tentative_rect = Rect::from_min_size(
                Pos2::new(origin_x + tentative as f32 * pixels_per_second, top),
                nominal_rect.size(),
            );
            draw_rect = tentative_rect;
            resize_start_seconds = tentative;
            resize_end_seconds = tentative + duration_seconds;
        }
    }
    if let Some(response) = &start_response
        && (response.dragged() || response.drag_stopped())
    {
        if let Some(pointer) = response.interact_pointer_pos() {
            let candidate = snap_seconds(
                ((pointer.x - origin_x) / pixels_per_second) as f64,
                snap_points,
                snap_threshold,
            );
            resize_start_seconds = candidate.clamp(0.0, resize_end_seconds - 0.05);
            let ghost_x = origin_x + resize_start_seconds as f32 * pixels_per_second;
            draw_rect = Rect::from_min_max(
                Pos2::new(ghost_x, nominal_rect.top()),
                nominal_rect.right_bottom(),
            );
        }
    }
    if let Some(response) = &end_response
        && (response.dragged() || response.drag_stopped())
    {
        if let Some(pointer) = response.interact_pointer_pos() {
            let candidate = snap_seconds(
                ((pointer.x - origin_x) / pixels_per_second) as f64,
                snap_points,
                snap_threshold,
            );
            resize_end_seconds = candidate.max(resize_start_seconds + 0.05);
            let ghost_right = origin_x + resize_end_seconds as f32 * pixels_per_second;
            draw_rect = Rect::from_min_max(
                nominal_rect.left_top(),
                Pos2::new(ghost_right, nominal_rect.bottom()),
            );
        }
    }

    painter.rect_filled(draw_rect, 4.0, base_color);
    painter.rect_stroke(
        draw_rect,
        4.0,
        Stroke::new(if selected { 2.0 } else { 1.0 }, Color32::WHITE),
        StrokeKind::Inside,
    );
    // Labels stay inside their clip box: a short clip must not spill its name over
    // neighbouring items. The painter's clip rect is the clip itself, and the string is
    // ellipsized to the remaining width so what is shown reads cleanly.
    let label = if is_image {
        format!("🖼 {}", item.label())
    } else if is_audio_only {
        format!("🎵 {}", item.label())
    } else if is_shape {
        format!("⬛ {}", item.label())
    } else if is_speech {
        format!("💬 {}", item.label())
    } else {
        item.label().to_owned()
    };
    let label_font = egui::FontId::proportional(11.0);
    let label_max_width = (draw_rect.width() - 12.0).max(0.0);
    if label_max_width > 8.0 {
        let label = ellipsize_to_width(&painter, &label, &label_font, label_max_width);
        painter
            .with_clip_rect(draw_rect)
            .text(
                draw_rect.left_top() + Vec2::new(6.0, 3.0),
                egui::Align2::LEFT_TOP,
                label,
                label_font,
                Color32::WHITE,
            );
    }
    if locked {
        painter
            .with_clip_rect(draw_rect)
            .text(
                draw_rect.right_top() + Vec2::new(-4.0, 3.0),
                egui::Align2::RIGHT_TOP,
                "🔒",
                egui::FontId::proportional(10.0),
                Color32::WHITE,
            );
    } else if selected || move_response.hovered() {
        let grip = Color32::from_white_alpha(90);
        painter.rect_filled(
            Rect::from_min_size(
                draw_rect.left_top(),
                Vec2::new(EDGE_HANDLE_WIDTH, draw_rect.height()),
            ),
            0.0,
            grip,
        );
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(draw_rect.right() - EDGE_HANDLE_WIDTH, draw_rect.top()),
                Vec2::new(EDGE_HANDLE_WIDTH, draw_rect.height()),
            ),
            0.0,
            grip,
        );
    }

    if move_response.clicked() {
        context.actions.push(UiAction::SelectItem(item_id));
    }
    if move_response.drag_stopped() {
        if let Some(pointer) = move_response.interact_pointer_pos() {
            let grab_offset = ui
                .data(|data| data.get_temp::<GrabOffset>(move_id.with("grab-offset")))
                .map(|offset| offset.0)
                .unwrap_or(0.0);
            let tentative = snap_seconds(
                ((pointer.x - grab_offset - origin_x) / pixels_per_second) as f64,
                snap_points,
                snap_threshold,
            )
            .max(0.0);
            let target_layer_id = ui
                .ctx()
                .pointer_interact_pos()
                .map(|pointer| {
                    let row = target_row_index(context, canvas_rect, pointer);
                    context.project.sequence.layers[row].id
                })
                .unwrap_or(layer.id);
            context.actions.push(UiAction::MoveItem(
                item_id,
                target_layer_id,
                TimelineTime::from_seconds(tentative),
            ));
            ui.data_mut(|data| {
                data.remove::<GrabOffset>(move_id.with("grab-offset"));
            });
        }
    }
    if let Some(response) = &start_response
        && response.drag_stopped()
        && (resize_start_seconds - start_seconds).abs() > 0.001
    {
        context.actions.push(UiAction::ResizeItemStart(
            item_id,
            TimelineTime::from_seconds(resize_start_seconds),
        ));
    }
    if let Some(response) = &end_response
        && response.drag_stopped()
        && (resize_end_seconds - (start_seconds + duration_seconds)).abs() > 0.001
    {
        context.actions.push(UiAction::ResizeItemEnd(
            item_id,
            TimelineTime::from_seconds(resize_end_seconds),
        ));
    }

    let hover = item_hover_text(context, item);
    move_response.on_hover_text(hover).context_menu(|ui| {
        if ui.button("選択").clicked() {
            context.actions.push(UiAction::SelectItem(item_id));
            ui.close();
        }
        if (is_clip || is_speech) && ui.button("✂ 再生位置で分割").clicked() {
            context.actions.push(UiAction::SelectItem(item_id));
            context.actions.push(UiAction::SplitAtPlayhead);
            ui.close();
        }
        if ui.button("🗑 削除").clicked() {
            context.actions.push(UiAction::DeleteItem(item_id));
            ui.close();
        }
    });
}

/// Compact summary shown when hovering a timeline item: what it is, its size for clips, and the
/// time span it occupies.
fn item_hover_text(context: &PanelContext<'_>, item: &TimelineItem) -> String {
    let span = format!("{} – {}", item.timeline_start(), item.timeline_end());
    match item {
        TimelineItem::Clip(clip) => match context.project.asset(clip.asset_id) {
            Some(asset) if asset.metadata.is_image => format!(
                "{}\n画像 {}×{}\n{span}",
                clip.name, asset.metadata.width, asset.metadata.height
            ),
            Some(_) => format!("{}\n動画クリップ\n{span}", clip.name),
            None => format!("{}\n{span}", clip.name),
        },
        TimelineItem::Text(text) => format!("テキスト: {}\n{span}", text.text),
        TimelineItem::Shape(shape) => format!("図形: {}\n{span}", shape.kind.label()),
        TimelineItem::Speech(speech) => {
            format!("セリフ: {}\n（音声＋字幕）\n{span}", speech.subtitle.text)
        }
    }
}

fn target_row_index(context: &PanelContext<'_>, canvas_rect: Rect, pointer: Pos2) -> usize {
    let row = ((pointer.y - canvas_rect.top() - RULER_HEIGHT) / ROW_HEIGHT).floor();
    let max_row = (context.project.sequence.layers.len() as f32 - 1.0).max(0.0);
    row.clamp(0.0, max_row) as usize
}

fn snap_seconds(candidate: f64, points: &[f64], threshold: f64) -> f64 {
    if threshold <= 0.0 || points.is_empty() {
        return candidate;
    }
    points
        .iter()
        .copied()
        .map(|point| (point, (point - candidate).abs()))
        .filter(|(_, distance)| *distance <= threshold)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(point, _)| point)
        .unwrap_or(candidate)
}

/// Shortens `text` with an ellipsis so it paints within `max_width` for `font_id`.
fn ellipsize_to_width(
    painter: &egui::Painter,
    text: &str,
    font_id: &egui::FontId,
    max_width: f32,
) -> String {
    let width = |value: &str| {
        painter
            .layout_no_wrap(value.to_owned(), font_id.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= max_width {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "…";
        if width(&candidate) <= max_width {
            return candidate;
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Inspector: deep, per-kind property panel (position, outline, shadow, ...).
// ---------------------------------------------------------------------------

fn inspector_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    let Some(item_id) = context.selected_item else {
        ui.label("タイムラインのアイテムを選択してください。");
        ui.add_space(8.0);
        ui.small("クリップやテキストをクリックすると、ここに詳細設定が表示されます。プレビュー内のテキストも直接ドラッグできます。");
        return;
    };
    let Some(item) = context.project.item(item_id) else {
        ui.label("選択中のアイテムが見つかりません。");
        return;
    };

    egui::ScrollArea::vertical().show(ui, |ui| {
        general_section(ui, context, item_id, item);
        ui.add_space(6.0);
        match item {
            TimelineItem::Clip(clip) => clip_inspector(ui, context, clip),
            TimelineItem::Text(text) => text_inspector(ui, context, text.clone()),
            TimelineItem::Shape(shape) => shape_inspector(ui, context, shape.clone()),
            TimelineItem::Speech(speech) => speech_inspector(ui, context, speech.clone()),
        }
    });
}

fn general_section(
    ui: &mut Ui,
    context: &mut PanelContext<'_>,
    item_id: ItemId,
    item: &TimelineItem,
) {
    egui::CollapsingHeader::new("🔧 全般")
        .default_open(true)
        .show(ui, |ui| {
            let kind_label = if item.as_clip().is_some_and(|clip| {
                context
                    .project
                    .asset(clip.asset_id)
                    .is_some_and(|asset| asset.metadata.is_image)
            }) {
                "画像クリップ"
            } else if item.as_clip().is_some() {
                "動画クリップ"
            } else if item.as_speech().is_some() {
                "セリフ（音声＋字幕）"
            } else if item.as_shape().is_some() {
                "図形"
            } else {
                "テキスト"
            };
            ui.label(format!("種類: {kind_label}"));
            let frame_rate = context.project.sequence.settings.frame_rate;
            let start_frame = (item.timeline_start().as_seconds() * frame_rate).round() as i64;
            let length_frames = (item.duration().as_seconds() * frame_rate).round().max(1.0) as i64;
            ui.label(format!(
                "開始フレーム: {start_frame}　長さ: {length_frames} フレーム"
            ));

            let current_layer_id = context.project.layer_of_item(item_id).map(|layer| layer.id);
            let mut selected_layer = current_layer_id;
            egui::ComboBox::from_label("レイヤー")
                .selected_text(
                    current_layer_id
                        .and_then(|id| context.project.layer(id))
                        .map(|layer| layer.name.clone())
                        .unwrap_or_default(),
                )
                .show_ui(ui, |ui| {
                    for layer in &context.project.sequence.layers {
                        ui.selectable_value(&mut selected_layer, Some(layer.id), &layer.name);
                    }
                });
            if selected_layer != current_layer_id
                && let Some(target) = selected_layer
            {
                context
                    .actions
                    .push(UiAction::MoveItem(item_id, target, item.timeline_start()));
            }

            if let Some(layer) = current_layer_id.and_then(|id| context.project.layer(id)) {
                ui.horizontal(|ui| {
                    if layer.locked {
                        ui.colored_label(Color32::from_rgb(230, 170, 60), "🔒 レイヤーがロック中");
                    }
                    if layer.hidden {
                        ui.colored_label(Color32::GRAY, "🚫 レイヤーが非表示");
                    }
                });
            }
        });
}

fn clip_inspector(ui: &mut Ui, context: &mut PanelContext<'_>, clip: &Clip) {
    let Some(asset) = context.project.asset(clip.asset_id) else {
        return;
    };

    egui::CollapsingHeader::new("🎬 クリップ")
        .default_open(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&clip.name).strong());
            if asset.metadata.is_image {
                ui.small(format!(
                    "{}×{} / 画像",
                    asset.metadata.width, asset.metadata.height
                ));
            } else if asset.metadata.has_video {
                ui.small(format!(
                    "{}×{} / {:.3}fps",
                    asset.metadata.width, asset.metadata.height, asset.metadata.frame_rate
                ));
            } else {
                ui.small(format!(
                    "音声のみ / {}",
                    asset.metadata.audio_codec.as_deref().unwrap_or("audio")
                ));
            }
            ui.add_space(4.0);
            let mut start_seconds = clip.timeline_start.as_seconds();
            if ui
                .add(
                    egui::DragValue::new(&mut start_seconds)
                        .range(0.0..=1_000_000.0)
                        .speed(0.05)
                        .prefix("開始 ")
                        .suffix(" 秒"),
                )
                .changed()
            {
                let layer_id = context
                    .project
                    .layer_of_item(clip.id)
                    .map(|layer| layer.id)
                    .unwrap_or_default();
                context.actions.push(UiAction::MoveItem(
                    clip.id,
                    layer_id,
                    TimelineTime::from_seconds(start_seconds),
                ));
            }
            ui.add_space(6.0);
            ui.label(if asset.metadata.is_image {
                "ソース範囲（表示時間）"
            } else {
                "ソース範囲（トリム）"
            });
            if asset.metadata.is_image {
                ui.small("画像はループ表示されます。タイムラインの右端ドラッグや OUT で長さを変えられます。");
            }
            let mut source_in = clip.source_in.as_seconds();
            let mut source_out = clip.source_out.as_seconds();
            // A still image can be stretched to any length, so it is not capped by the asset's
            // nominal duration (which is only the default length chosen at import time).
            let out_max = if asset.metadata.is_image {
                source_out.max(86_400.0)
            } else {
                asset.metadata.duration.as_seconds()
            };
            let changed_in = ui
                .add(
                    egui::DragValue::new(&mut source_in)
                        .range(0.0..=source_out)
                        .speed(0.05)
                        .prefix("IN  "),
                )
                .changed();
            let changed_out = ui
                .add(
                    egui::DragValue::new(&mut source_out)
                        .range(source_in..=out_max)
                        .speed(0.05)
                        .prefix("OUT "),
                )
                .changed();
            if changed_in || changed_out {
                context.actions.push(UiAction::TrimClip(
                    clip.id,
                    TimelineTime::from_seconds(source_in),
                    TimelineTime::from_seconds(source_out),
                ));
            }
            ui.add_space(6.0);
            let mut speed = clip.speed();
            let speed_changed = ui
                .horizontal(|ui| {
                    let mut changed = ui
                        .add(
                            egui::DragValue::new(&mut speed)
                                .range(0.1..=8.0)
                                .speed(0.05)
                                .fixed_decimals(2)
                                .prefix("再生速度 ")
                                .suffix(" ×"),
                        )
                        .changed();
                    for preset in [0.25_f64, 0.5, 1.0, 1.5, 2.0, 4.0] {
                        if ui
                            .small_button(format!(
                                "{}",
                                if preset == 1.0 {
                                    "1×".to_owned()
                                } else {
                                    format!("{preset}×")
                                }
                            ))
                            .clicked()
                        {
                            speed = preset;
                            changed = true;
                        }
                    }
                    changed
                })
                .inner;
            if speed_changed {
                context.actions.push(UiAction::SetClipSpeed(clip.id, speed));
            }
            ui.small(format!(
                "尺 {:.2} 秒（元 {:.2} 秒）",
                clip.duration().as_seconds(),
                (clip.source_out - clip.source_in).as_seconds()
            ));
        });

    ui.add_space(6.0);
    if asset.metadata.has_video {
        egui::CollapsingHeader::new("🖼 表示（位置・拡大・回転・不透明度）")
            .default_open(true)
            .show(ui, |ui| {
                let mut transform = clip.transform;
                let mut changed = false;
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut transform.x)
                                .speed(1.0)
                                .prefix("X ")
                                .suffix(" px"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut transform.y)
                                .speed(1.0)
                                .prefix("Y ")
                                .suffix(" px"),
                        )
                        .changed();
                });
                changed |= ui
                    .add(egui::Slider::new(&mut transform.scale, 1.0..=400.0).text("拡大率 %"))
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut transform.rotation_degrees, -360.0..=360.0)
                            .text("回転 °"),
                    )
                    .changed();
                ui.label("基準点");
                changed |= anchor_picker(ui, &mut transform.anchor);
                if changed {
                    context
                        .actions
                        .push(UiAction::SetClipTransform(clip.id, transform));
                }

                let mut opacity = clip.opacity;
                if ui
                    .add(egui::Slider::new(&mut opacity, 0.0..=100.0).text("不透明度 %"))
                    .changed()
                {
                    context
                        .actions
                        .push(UiAction::SetClipOpacity(clip.id, opacity));
                }
            });
        ui.add_space(6.0);
        egui::CollapsingHeader::new("✨ エフェクト")
            .default_open(false)
            .show(ui, |ui| {
                let mut effects = clip.effects;
                let mut changed = false;
                changed |= ui
                    .add(
                        egui::Slider::new(&mut effects.brightness, -1.0..=1.0)
                            .text("明るさ")
                            .fixed_decimals(2),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut effects.contrast, 0.0..=2.0)
                            .text("コントラスト")
                            .fixed_decimals(2),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut effects.saturation, 0.0..=2.0)
                            .text("彩度")
                            .fixed_decimals(2),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut effects.hue_degrees, -180.0..=180.0)
                            .text("色相 °"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut effects.blur_radius, 0.0..=32.0)
                            .text("ぼかし")
                            .fixed_decimals(1),
                    )
                    .changed();
                changed |= ui.checkbox(&mut effects.grayscale, "モノクロ").changed();
                changed |= ui.checkbox(&mut effects.invert, "色反転").changed();
                if ui.button("エフェクトをリセット").clicked() {
                    effects = crate::domain::ClipEffects::default();
                    changed = true;
                }
                if changed {
                    context
                        .actions
                        .push(UiAction::SetClipEffects(clip.id, effects));
                }
            });
        ui.add_space(6.0);
        egui::CollapsingHeader::new("🎬 アニメーション（キーフレーム）")
            .default_open(false)
            .show(ui, |ui| {
                let offset = context.playhead - clip.timeline_start;
                ui.small(format!(
                    "再生位置: {:.2}s / クリップ尺 {:.2}s",
                    offset.as_seconds().max(0.0),
                    clip.duration().as_seconds()
                ));
                if clip.anim.is_empty() {
                    ui.small("キーフレームはまだありません。プロパティを選び「現在位置に追加」してください。");
                }
                for track in &clip.anim {
                    ui.horizontal(|ui| {
                        ui.label(track.property.label());
                        ui.small(format!("{} キー", track.keys.len()));
                        if ui.small_button("削除").clicked() {
                            context
                                .actions
                                .push(UiAction::RemoveAnimTrack(clip.id, track.property));
                        }
                    });
                    for key in &track.keys {
                        ui.horizontal(|ui| {
                            ui.small(format!(
                                "  {:+.2}s = {:.2}",
                                key.offset.as_seconds(),
                                key.value
                            ));
                            if ui.small_button("消").clicked() {
                                context.actions.push(UiAction::RemoveKeyframe(
                                    clip.id,
                                    track.property,
                                    key.offset,
                                ));
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut property = *context.anim_property_draft;
                    egui::ComboBox::from_id_salt("clip-anim-prop")
                        .selected_text(property.label())
                        .show_ui(ui, |ui| {
                            for candidate in crate::domain::AnimProperty::ALL {
                                ui.selectable_value(&mut property, candidate, candidate.label());
                            }
                        });
                    *context.anim_property_draft = property;
                    let mut value = clip
                        .sample_anim(property, offset)
                        .unwrap_or_else(|| match property {
                            crate::domain::AnimProperty::X => clip.transform.x,
                            crate::domain::AnimProperty::Y => clip.transform.y,
                            crate::domain::AnimProperty::Scale => clip.transform.scale,
                            crate::domain::AnimProperty::Rotation => clip.transform.rotation_degrees,
                            crate::domain::AnimProperty::Opacity => clip.opacity,
                            crate::domain::AnimProperty::Volume => clip.audio.gain_db,
                            crate::domain::AnimProperty::Brightness => clip.effects.brightness,
                            crate::domain::AnimProperty::Contrast => clip.effects.contrast,
                            crate::domain::AnimProperty::Saturation => clip.effects.saturation,
                            crate::domain::AnimProperty::Hue => clip.effects.hue_degrees,
                            crate::domain::AnimProperty::Blur => clip.effects.blur_radius,
                        });
                    ui.add(
                        egui::DragValue::new(&mut value)
                            .speed(0.1)
                            .fixed_decimals(2)
                            .prefix("値 "),
                    );
                    if ui.button("現在位置に追加").clicked() {
                        context.actions.push(UiAction::SetKeyframe(
                            clip.id,
                            property,
                            crate::domain::Keyframe {
                                offset: offset.max(TimelineTime::ZERO),
                                value,
                                easing: crate::domain::Easing::Linear,
                            },
                        ));
                    }
                });
            });
        ui.add_space(6.0);
    }
    if asset.metadata.has_audio {
        egui::CollapsingHeader::new("🔊 音声")
            .default_open(true)
            .show(ui, |ui| {
                let mut gain = clip.audio.gain_db;
                if ui
                    .add(egui::Slider::new(&mut gain, -60.0..=12.0).suffix(" dB"))
                    .changed()
                {
                    context.actions.push(UiAction::SetGain(clip.id, gain));
                }
                let mut muted = clip.audio.muted;
                if ui.checkbox(&mut muted, "ミュート").changed() {
                    context.actions.push(UiAction::SetMuted(clip.id, muted));
                }
            });
    }
}

/// Inspector for a VOICEVOX line: the synthesized audio's level, plus the full text editor for
/// the subtitle. Editing the subtitle's span trims (or stretches) the audio so the two stay one
/// item.
fn speech_inspector(ui: &mut Ui, context: &mut PanelContext<'_>, speech: SpeechItem) {
    ui.label(egui::RichText::new("セリフ（音声＋字幕）").strong());
    if let Some(asset) = context.project.asset(speech.audio.asset_id) {
        ui.small(format!(
            "音声 {:.2}s / {}",
            speech.audio.duration().as_seconds(),
            asset.metadata.audio_codec.as_deref().unwrap_or("audio")
        ));
    }
    ui.add_space(6.0);
    egui::CollapsingHeader::new("🔊 音声")
        .default_open(true)
        .show(ui, |ui| {
            let mut gain = speech.audio.audio.gain_db;
            if ui
                .add(egui::Slider::new(&mut gain, -60.0..=12.0).suffix(" dB"))
                .changed()
            {
                context.actions.push(UiAction::SetGain(speech.id(), gain));
            }
            let mut muted = speech.audio.audio.muted;
            if ui.checkbox(&mut muted, "ミュート").changed() {
                context.actions.push(UiAction::SetMuted(speech.id(), muted));
            }
        });
    ui.add_space(6.0);
    text_inspector(ui, context, speech.subtitle);
}

fn text_inspector(ui: &mut Ui, context: &mut PanelContext<'_>, mut text: TextItem) {
    let mut changed = false;

    egui::CollapsingHeader::new("📝 内容とタイミング")
        .default_open(true)
        .show(ui, |ui| {
            changed |= ui.text_edit_multiline(&mut text.text).changed();
            let mut start = text.timeline_start.as_seconds();
            let mut duration = text.duration.as_seconds();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut start)
                        .range(0.0..=1_000_000.0)
                        .speed(0.05)
                        .prefix("開始 ")
                        .suffix(" 秒"),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut duration)
                        .range(0.05..=3600.0)
                        .speed(0.05)
                        .prefix("長さ ")
                        .suffix(" 秒"),
                )
                .changed();
            text.timeline_start = TimelineTime::from_seconds(start);
            text.duration = TimelineTime::from_seconds(duration);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("🔤 フォント")
        .default_open(true)
        .show(ui, |ui| {
            egui::ComboBox::from_label("フォント")
                .selected_text(
                    text.font_family
                        .clone()
                        .unwrap_or_else(|| "デフォルト".to_owned()),
                )
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(&mut text.font_family, None, "デフォルト")
                        .changed();
                    for family in context.available_fonts {
                        changed |= ui
                            .selectable_value(
                                &mut text.font_family,
                                Some(family.clone()),
                                family.as_str(),
                            )
                            .changed();
                    }
                });
            changed |= ui
                .add(egui::Slider::new(&mut text.font_size, 8.0..=300.0).text("文字サイズ"))
                .changed();
            ui.horizontal(|ui| {
                changed |= ui.toggle_value(&mut text.bold, "B 太字").changed();
                changed |= ui.toggle_value(&mut text.italic, "I 斜体").changed();
            });
            ui.horizontal(|ui| {
                ui.label("整列");
                changed |= ui
                    .selectable_value(&mut text.align, TextAlign::Left, "左")
                    .changed();
                changed |= ui
                    .selectable_value(&mut text.align, TextAlign::Center, "中央")
                    .changed();
                changed |= ui
                    .selectable_value(&mut text.align, TextAlign::Right, "右")
                    .changed();
            });
            changed |= ui
                .add(egui::Slider::new(&mut text.line_spacing, -20.0..=60.0).text("行間 (px)"))
                .changed();
            changed |= color_row(ui, "文字色", &mut text.color_rgba);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("📐 位置・拡大")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut text.transform.x)
                            .speed(1.0)
                            .prefix("X ")
                            .suffix(" px"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut text.transform.y)
                            .speed(1.0)
                            .prefix("Y ")
                            .suffix(" px"),
                    )
                    .changed();
            });
            changed |= ui
                .add(egui::Slider::new(&mut text.transform.scale, 10.0..=400.0).text("拡大率 %"))
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut text.transform.rotation_degrees, -360.0..=360.0)
                        .text("回転 °"),
                )
                .changed();
            ui.label("基準点（プレビュー内のドラッグでも移動できます）");
            changed |= anchor_picker(ui, &mut text.transform.anchor);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("✏ 縁取り（アウトライン）")
        .default_open(text.outline_enabled)
        .show(ui, |ui| {
            changed |= ui
                .checkbox(&mut text.outline_enabled, "縁取りを有効化")
                .changed();
            ui.add_enabled_ui(text.outline_enabled, |ui| {
                changed |= ui
                    .add(egui::Slider::new(&mut text.outline_width, 0.0..=40.0).text("太さ (px)"))
                    .changed();
                changed |= color_row(ui, "色", &mut text.outline_color_rgba);
            });
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("🌑 影（シャドウ）")
        .default_open(text.shadow_enabled)
        .show(ui, |ui| {
            changed |= ui
                .checkbox(&mut text.shadow_enabled, "影を有効化")
                .changed();
            ui.add_enabled_ui(text.shadow_enabled, |ui| {
                ui.horizontal(|ui| {
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut text.shadow_offset_x)
                                .speed(0.5)
                                .prefix("X ")
                                .suffix(" px"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut text.shadow_offset_y)
                                .speed(0.5)
                                .prefix("Y ")
                                .suffix(" px"),
                        )
                        .changed();
                });
                changed |= color_row(ui, "色", &mut text.shadow_color_rgba);
            });
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("🟪 背景ボックス")
        .default_open(text.background_enabled)
        .show(ui, |ui| {
            changed |= ui
                .checkbox(&mut text.background_enabled, "背景を有効化")
                .changed();
            ui.add_enabled_ui(text.background_enabled, |ui| {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut text.background_padding, 0.0..=100.0)
                            .text("パディング (px)"),
                    )
                    .changed();
                changed |= color_row(ui, "色", &mut text.background_color_rgba);
            });
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("🌗 フェード")
        .default_open(false)
        .show(ui, |ui| {
            let mut fade_in = text.fade_in.as_seconds();
            let mut fade_out = text.fade_out.as_seconds();
            changed |= ui
                .add(egui::Slider::new(&mut fade_in, 0.0..=5.0).text("フェードイン (秒)"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut fade_out, 0.0..=5.0).text("フェードアウト (秒)"))
                .changed();
            text.fade_in = TimelineTime::from_seconds(fade_in);
            text.fade_out = TimelineTime::from_seconds(fade_out);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("📚 テンプレート")
        .default_open(false)
        .show(ui, |ui| {
            ui.small("このテキストの見た目をテンプレートとして保存、または既存テンプレートを適用できます。");
            let mut template_name = text.text.chars().take(20).collect::<String>();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut template_name).hint_text("テンプレート名"));
                if ui.button("💾 保存").clicked() {
                    let name = if template_name.trim().is_empty() {
                        "テンプレート".to_owned()
                    } else {
                        template_name.trim().to_owned()
                    };
                    context
                        .templates
                        .push(TextTemplate::from_text(name, &text));
                }
            });

            let mut apply_template: Option<Uuid> = None;
            egui::ComboBox::from_label("テンプレートを適用")
                .selected_text("選択…")
                .show_ui(ui, |ui| {
                    for template in context.templates.iter() {
                        ui.selectable_value(
                            &mut apply_template,
                            Some(template.id),
                            &template.name,
                        );
                    }
                });
            if let Some(template_id) = apply_template
                && let Some(template) = context.templates.iter().find(|t| t.id == template_id)
            {
                let mut updated = text.clone();
                template.apply_to(&mut updated);
                text = updated;
                changed = true;
            }
        });

    if changed {
        context.actions.push(UiAction::UpdateText(text.clone()));
    }
    ui.add_space(8.0);
    if ui.button("🗑 このテキストを削除").clicked() {
        context.actions.push(UiAction::DeleteItem(text.id));
    }
}

fn shape_inspector(ui: &mut Ui, context: &mut PanelContext<'_>, mut shape: ShapeItem) {
    let mut changed = false;

    egui::CollapsingHeader::new("⬛ 図形")
        .default_open(true)
        .show(ui, |ui| {
            egui::ComboBox::from_label("種類")
                .selected_text(shape.kind.label())
                .show_ui(ui, |ui| {
                    for kind in ShapeKind::ALL {
                        changed |= ui
                            .selectable_value(&mut shape.kind, kind, kind.label())
                            .changed();
                    }
                });
            let mut start = shape.timeline_start.as_seconds();
            let mut duration = shape.duration.as_seconds();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut start)
                        .range(0.0..=1_000_000.0)
                        .speed(0.05)
                        .prefix("開始 ")
                        .suffix(" 秒"),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut duration)
                        .range(0.05..=3600.0)
                        .speed(0.05)
                        .prefix("長さ ")
                        .suffix(" 秒"),
                )
                .changed();
            shape.timeline_start = TimelineTime::from_seconds(start);
            shape.duration = TimelineTime::from_seconds(duration);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("📐 大きさ・縦横比")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut shape.width)
                            .range(1.0..=7680.0)
                            .speed(1.0)
                            .prefix("幅 ")
                            .suffix(" px"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut shape.height)
                            .range(1.0..=4320.0)
                            .speed(1.0)
                            .prefix("高さ ")
                            .suffix(" px"),
                    )
                    .changed();
            });
            changed |= ui
                .add(egui::Slider::new(&mut shape.transform.scale, 1.0..=400.0).text("拡大率 %"))
                .changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut shape.transform.rotation_degrees, -360.0..=360.0)
                        .text("回転 °"),
                )
                .changed();
            ui.horizontal(|ui| {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut shape.transform.x)
                            .speed(1.0)
                            .prefix("X ")
                            .suffix(" px"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut shape.transform.y)
                            .speed(1.0)
                            .prefix("Y ")
                            .suffix(" px"),
                    )
                    .changed();
            });
            ui.label("基準点");
            changed |= anchor_picker(ui, &mut shape.transform.anchor);
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("🎨 色・濃度")
        .default_open(true)
        .show(ui, |ui| {
            changed |= color_row(ui, "色", &mut shape.color_rgba);
            changed |= ui
                .add(egui::Slider::new(&mut shape.opacity, 0.0..=100.0).text("濃度（不透明度） %"))
                .changed();
        });

    if changed {
        context.actions.push(UiAction::UpdateShape(shape.clone()));
    }
    ui.add_space(8.0);
    if ui.button("🗑 この図形を削除").clicked() {
        context.actions.push(UiAction::DeleteItem(shape.id));
    }
}

fn anchor_picker(ui: &mut Ui, anchor: &mut Anchor) -> bool {
    let mut changed = false;
    let labels = ["↖", "↑", "↗", "←", "●", "→", "↙", "↓", "↘"];
    egui::Grid::new(("anchor-grid", ui.next_auto_id()))
        .spacing(Vec2::new(2.0, 2.0))
        .show(ui, |ui| {
            for row in 0..3 {
                for col in 0..3 {
                    let candidate = Anchor::ALL[row * 3 + col];
                    let selected = *anchor == candidate;
                    if ui
                        .selectable_label(selected, labels[row * 3 + col])
                        .on_hover_text(candidate.label())
                        .clicked()
                    {
                        *anchor = candidate;
                        changed = true;
                    }
                }
                ui.end_row();
            }
        });
    changed
}

fn color_row(ui: &mut Ui, label: &str, rgba: &mut [u8; 4]) -> bool {
    let mut color = rgba_to_color32(*rgba);
    let changed = ui
        .horizontal(|ui| {
            ui.label(label);
            ui.color_edit_button_srgba(&mut color)
        })
        .inner
        .changed();
    if changed {
        *rgba = color.to_array();
    }
    changed
}

fn rgba_to_color32(rgba: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
}

// ---------------------------------------------------------------------------
// Project settings and AI status.
// ---------------------------------------------------------------------------

fn settings_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("プロジェクト設定");
    ui.small("出力解像度とフレームレートをプロジェクト単位で管理します。");
    ui.separator();
    let current = &context.project.sequence.settings;
    ui.label("プリセット");
    ui.horizontal_wrapped(|ui| {
        for (label, width, height) in [
            ("HD 720p", 1280, 720),
            ("フルHD 1080p", 1920, 1080),
            ("4K UHD", 3840, 2160),
        ] {
            if ui.button(label).clicked() {
                context
                    .actions
                    .push(UiAction::SetSequenceSettings(SequenceSettings {
                        width,
                        height,
                        ..current.clone()
                    }));
            }
        }
    });
    ui.add_space(8.0);
    let mut settings = current.clone();
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::DragValue::new(&mut settings.width)
                    .range(16..=7680)
                    .prefix("幅 "),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut settings.height)
                    .range(16..=4320)
                    .prefix("高さ "),
            )
            .changed();
    });
    egui::ComboBox::from_label("フレームレート")
        .selected_text(format!("{:.3} fps", settings.frame_rate))
        .show_ui(ui, |ui| {
            for fps in [23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
                changed |= ui
                    .selectable_value(&mut settings.frame_rate, fps, format!("{fps:.3} fps"))
                    .changed();
            }
        });
    egui::ComboBox::from_label("音声サンプルレート")
        .selected_text(format!("{} Hz", settings.sample_rate))
        .show_ui(ui, |ui| {
            changed |= ui
                .selectable_value(&mut settings.sample_rate, 44_100, "44100 Hz")
                .changed();
            changed |= ui
                .selectable_value(&mut settings.sample_rate, 48_000, "48000 Hz")
                .changed();
        });
    if changed {
        context
            .actions
            .push(UiAction::SetSequenceSettings(settings));
    }
    ui.separator();
    ui.label(format!(
        "現在: {}×{} / {:.3}fps / {}Hz",
        current.width, current.height, current.frame_rate, current.sample_rate
    ));
    ui.label(if context.hardware.qsv_available {
        "QSV: 有効（プレビューと書き出しで優先使用）"
    } else {
        "QSV: 利用不可（自動的に互換処理を使用）"
    });

    ui.separator();
    ui.heading("書き出し設定 (FFmpeg)");
    ui.small("AV1/HEVC/VP9など高効率コーデックに対応。利用できないエンコーダーは選択不可で、エラー時は自動的に libx264 へ切替えます。");
    let mut options = *context.export_options;
    let mut export_changed = false;
    egui::ComboBox::from_label("映像コーデック")
        .selected_text(options.video_encoder.label())
        .show_ui(ui, |ui| {
            for encoder in VideoEncoder::ALL {
                let available = encoder.is_available(context.hardware);
                let label = if available {
                    encoder.label().to_owned()
                } else {
                    format!("{}（利用不可）", encoder.label())
                };
                let response = ui
                    .add_enabled_ui(available, |ui| {
                        ui.selectable_label(options.video_encoder == encoder, label)
                            .clicked()
                    })
                    .inner;
                if response {
                    options.video_encoder = encoder;
                    export_changed = true;
                }
            }
        });
    let (min_quality, max_quality) = options.video_encoder.quality_range();
    options.crf = options.crf.clamp(min_quality, max_quality);
    export_changed |= ui
        .add(
            egui::Slider::new(&mut options.crf, min_quality..=max_quality)
                .text("品質 CRF（低いほど高画質）"),
        )
        .changed();
    egui::ComboBox::from_label("プリセット")
        .selected_text(options.preset.label())
        .show_ui(ui, |ui| {
            for preset in EncoderPreset::ALL {
                export_changed |= ui
                    .selectable_value(&mut options.preset, preset, preset.label())
                    .changed();
            }
        });
    egui::ComboBox::from_label("音声コーデック")
        .selected_text(options.audio_codec.label())
        .show_ui(ui, |ui| {
            for codec in AudioCodec::ALL {
                export_changed |= ui
                    .selectable_value(&mut options.audio_codec, codec, codec.label())
                    .changed();
            }
        });
    if options.audio_codec != AudioCodec::None {
        egui::ComboBox::from_label("音声ビットレート")
            .selected_text(format!("{} kbps", options.audio_bitrate_kbps))
            .show_ui(ui, |ui| {
                for bitrate in [96u32, 128, 160, 192, 256, 320] {
                    export_changed |= ui
                        .selectable_value(
                            &mut options.audio_bitrate_kbps,
                            bitrate,
                            format!("{bitrate} kbps"),
                        )
                        .changed();
                }
            });
    }
    export_changed |= ui
        .add(egui::Slider::new(&mut options.threads, 1..=8).text("エンコードスレッド数"))
        .changed();
    if export_changed {
        context.actions.push(UiAction::SetExportOptions(options));
    }
}

fn templates_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("テキストテンプレート");
    ui.small("フォント・サイズ・位置・色・縁取りなどをプリセットとして保存し、テキストやVOICEVOX読み上げに使い回せます。話者ごとに使うには「キャラクター」タブでキャラに割り当てます。");
    ui.separator();

    let frame = context.project.sequence.settings.frame_size();
    if ui.button("＋ 新規テンプレート").clicked() {
        let name = format!("テンプレート {}", context.templates.len() + 1);
        let mut template = TextTemplate::new(name);
        template.transform.x = frame.0 / 2.0;
        template.transform.y = frame.1 * 0.82;
        context.templates.push(template);
    }

    ui.add_space(8.0);
    if context.templates.is_empty() {
        ui.label("テンプレートはまだありません。テキスト設定パネルの「💾 保存」や上のボタンから作成できます。");
        return;
    }

    let available_fonts = context.available_fonts;
    let mut delete_ids: Vec<Uuid> = Vec::new();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for template in context.templates.iter_mut() {
            let id = template.id;
            let title = template.name.clone();
            egui::CollapsingHeader::new(title)
                .id_salt(("template", id))
                .default_open(false)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("名前");
                        let mut name = template.name.clone();
                        if ui
                            .add(egui::TextEdit::singleline(&mut name).desired_width(160.0))
                            .changed()
                        {
                            template.name = name;
                        }
                        if ui.button("🗑 削除").clicked() {
                            delete_ids.push(id);
                        }
                    });
                    template_style_editor(ui, template, available_fonts, frame);
                });
        }
    });

    if !delete_ids.is_empty() {
        context.templates.retain(|t| !delete_ids.contains(&t.id));
    }
}

fn template_style_editor(
    ui: &mut Ui,
    template: &mut TextTemplate,
    available_fonts: &[String],
    frame: (f32, f32),
) {
    egui::ComboBox::from_label("フォント")
        .selected_text(
            template
                .font_family
                .clone()
                .unwrap_or_else(|| "デフォルト".to_owned()),
        )
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut template.font_family, None, "デフォルト");
            for family in available_fonts {
                ui.selectable_value(
                    &mut template.font_family,
                    Some(family.clone()),
                    family.as_str(),
                );
            }
        });
    ui.add(egui::Slider::new(&mut template.font_size, 8.0..=300.0).text("文字サイズ"));
    ui.horizontal(|ui| {
        ui.toggle_value(&mut template.bold, "B 太字");
        ui.toggle_value(&mut template.italic, "I 斜体");
    });
    ui.horizontal(|ui| {
        ui.label("整列");
        ui.selectable_value(&mut template.align, TextAlign::Left, "左");
        ui.selectable_value(&mut template.align, TextAlign::Center, "中央");
        ui.selectable_value(&mut template.align, TextAlign::Right, "右");
    });
    ui.add(egui::Slider::new(&mut template.line_spacing, -20.0..=60.0).text("行間 (px)"));
    color_row(ui, "文字色", &mut template.color_rgba);

    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(&mut template.transform.x)
                .speed(1.0)
                .prefix("X ")
                .suffix(" px"),
        );
        ui.add(
            egui::DragValue::new(&mut template.transform.y)
                .speed(1.0)
                .prefix("Y ")
                .suffix(" px"),
        );
        if ui
            .small_button("既定位置")
            .on_hover_text("画面下中央へ移動")
            .clicked()
        {
            template.transform.x = frame.0 / 2.0;
            template.transform.y = frame.1 * 0.82;
        }
    });
    ui.add(egui::Slider::new(&mut template.transform.scale, 10.0..=400.0).text("拡大率 %"));
    ui.label("基準点");
    anchor_picker(ui, &mut template.transform.anchor);

    ui.separator();
    ui.checkbox(&mut template.outline_enabled, "縁取り");
    if template.outline_enabled {
        ui.add(egui::Slider::new(&mut template.outline_width, 0.0..=40.0).text("縁取り太さ (px)"));
        color_row(ui, "縁取り色", &mut template.outline_color_rgba);
    }
    ui.checkbox(&mut template.shadow_enabled, "影");
    if template.shadow_enabled {
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut template.shadow_offset_x)
                    .speed(0.5)
                    .prefix("影X "),
            );
            ui.add(
                egui::DragValue::new(&mut template.shadow_offset_y)
                    .speed(0.5)
                    .prefix("影Y "),
            );
        });
        color_row(ui, "影の色", &mut template.shadow_color_rgba);
    }
    ui.checkbox(&mut template.background_enabled, "背景ボックス");
    if template.background_enabled {
        ui.add(
            egui::Slider::new(&mut template.background_padding, 0.0..=100.0).text("背景パディング"),
        );
        color_row(ui, "背景色", &mut template.background_color_rgba);
    }
}

fn characters_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("キャラクター");
    ui.small(
        "VOICEVOXの話者をキャラとして登録し、声の調整（話速・音高・抑諦・音量・無音）と字幕スタイルをキャラごとにまとめて設定できます。同じ話者で喋り方を変えたキャラも作れます。",
    );
    ui.separator();

    if ui.button("＋ キャラを追加").clicked() {
        let speaker = context
            .characters
            .first()
            .map(|character| character.speaker)
            .unwrap_or(1);
        let name = format!("キャラ {}", context.characters.len() + 1);
        context.characters.push(Character::new(name, speaker));
    }

    if context.characters.is_empty() {
        ui.add_space(8.0);
        ui.label("キャラがまだありません。「＋ キャラを追加」で作成できます。");
        return;
    }

    // Snapshot the template names so the list can be shown while characters are edited mutably.
    let templates: Vec<(Uuid, String)> = context
        .templates
        .iter()
        .map(|template| (template.id, template.name.clone()))
        .collect();

    let mut duplicate: Option<usize> = None;
    let mut delete: Option<usize> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (index, character) in context.characters.iter_mut().enumerate() {
            let title = if character.name.trim().is_empty() {
                "（無名）".to_owned()
            } else {
                character.name.clone()
            };
            egui::CollapsingHeader::new(title)
                .id_salt(("character", character.id))
                .default_open(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("名前");
                        ui.add(
                            egui::TextEdit::singleline(&mut character.name).desired_width(140.0),
                        );
                        ui.label("話者");
                        ui.add(egui::DragValue::new(&mut character.speaker).range(1..=100));
                    });
                    ui.add(egui::Slider::new(&mut character.voice.speed, 0.5..=2.0).text("話速"));
                    ui.add(
                        egui::Slider::new(&mut character.voice.pitch, -0.15..=0.15).text("音高"),
                    );
                    ui.add(
                        egui::Slider::new(&mut character.voice.intonation, 0.0..=2.0).text("抑諦"),
                    );
                    ui.add(egui::Slider::new(&mut character.voice.volume, 0.0..=2.0).text("音量"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut character.voice.pre_silence, 0.0..=1.0)
                                .text("前の無音 (s)"),
                        );
                        ui.add(
                            egui::Slider::new(&mut character.voice.post_silence, 0.0..=1.0)
                                .text("後の無音 (s)"),
                        );
                    });

                    let template_label = character
                        .template
                        .and_then(|id| templates.iter().find(|(tid, _)| *tid == id))
                        .map(|(_, name)| name.clone())
                        .unwrap_or_else(|| "デフォルト".to_owned());
                    egui::ComboBox::from_label("字幕テンプレート")
                        .selected_text(template_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut character.template, None, "デフォルト");
                            for (id, name) in &templates {
                                ui.selectable_value(&mut character.template, Some(*id), name);
                            }
                        });

                    ui.horizontal(|ui| {
                        if ui.button("複製").clicked() {
                            duplicate = Some(index);
                        }
                        if ui.button("🗑 削除").clicked() {
                            delete = Some(index);
                        }
                    });
                });
        }
    });

    if let Some(index) = duplicate {
        let mut copy = context.characters[index].clone();
        copy.id = Uuid::new_v4();
        copy.name = format!("{} のコピー", copy.name);
        context.characters.push(copy);
    }
    if let Some(index) = delete {
        context.characters.remove(index);
    }
}
