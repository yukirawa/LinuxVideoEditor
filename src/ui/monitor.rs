//! メディアブラウザ / 音声波形 / 音量モニタ — the monitor panels opened from the 表示 menu.
//!
//! Waveform peaks are decoded on demand (one background `ffmpeg` pass per asset) and kept in
//! [`WaveformCache`], so drawing the panels never re-decodes media.

use crate::domain::{AssetId, Clip, MediaAsset, TimelineTime};
use crate::ui::panels::{PanelContext, UiAction};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

/// Mono PCM decode rate for peak extraction. Peaks are a visual aid, not sample-accurate
/// meters, so 8 kHz keeps an hour of audio at a few MB of peaks.
const PEAK_SAMPLE_RATE_HZ: f64 = 8000.0;
/// Samples folded into one (min, max) bucket: 32 ms at 8 kHz.
const PEAK_BUCKET_SAMPLES: usize = 256;
/// Window around the playhead that the level meter reads a peak from.
const LEVEL_WINDOW_SECS: f64 = 0.08;

#[derive(Clone, Debug)]
pub struct WaveformPeaks {
    /// (min, max) amplitude per bucket, normalized to -1.0..=1.0.
    buckets: Vec<(f32, f32)>,
}

impl WaveformPeaks {
    fn from_mono_s16(samples: &[i16]) -> Self {
        let buckets = samples
            .chunks(PEAK_BUCKET_SAMPLES)
            .map(|chunk| {
                let mut min = 0.0f32;
                let mut max = 0.0f32;
                for sample in chunk {
                    let value = *sample as f32 / i16::MAX as f32;
                    min = min.min(value);
                    max = max.max(value);
                }
                (min, max)
            })
            .collect();
        Self { buckets }
    }

    pub fn duration_seconds(&self) -> f64 {
        self.buckets.len() as f64 * PEAK_BUCKET_SAMPLES as f64 / PEAK_SAMPLE_RATE_HZ
    }

    /// Min/max amplitude inside `[start, end)` seconds of source time. Empty spans read as
    /// silence, so a view window outside the decoded range simply draws flat.
    pub fn minmax_in(&self, start: f64, end: f64) -> (f32, f32) {
        if !start.is_finite() || !end.is_finite() || end <= start {
            return (0.0, 0.0);
        }
        let bucket_secs = PEAK_BUCKET_SAMPLES as f64 / PEAK_SAMPLE_RATE_HZ;
        let first = (start / bucket_secs).floor().max(0.0) as usize;
        let last = ((end / bucket_secs).ceil().max(0.0) as usize).min(self.buckets.len());
        if first >= last {
            return (0.0, 0.0);
        }
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        for &(bucket_min, bucket_max) in &self.buckets[first..last] {
            min = min.min(bucket_min);
            max = max.max(bucket_max);
        }
        (min, max)
    }

    /// Peak amplitude (0.0..=1.0) inside `[start, end)` seconds of source time.
    pub fn peak_in(&self, start: f64, end: f64) -> f32 {
        let (min, max) = self.minmax_in(start, end);
        min.abs().max(max.abs())
    }
}

enum WaveformEntry {
    Pending(Receiver<Result<Arc<WaveformPeaks>, String>>),
    Ready(Arc<WaveformPeaks>),
    Failed(String),
}

pub enum WaveformStatus {
    /// Nothing has been requested for this asset yet.
    Missing,
    Pending,
    Ready(Arc<WaveformPeaks>),
    Failed(String),
}

/// Cross-frame peak cache keyed by [`AssetId`]. Each asset is decoded at most once unless the
/// user explicitly retries a failure.
#[derive(Default)]
pub struct WaveformCache {
    entries: HashMap<AssetId, WaveformEntry>,
}

impl WaveformCache {
    /// Starts a background peak decode for `asset_id` unless one is already cached or running.
    pub fn request(&mut self, asset_id: AssetId, path: &Path, repaint: egui::Context) {
        if self.entries.contains_key(&asset_id) {
            return;
        }
        let path = path.to_path_buf();
        let (sender, receiver) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("waveform-peaks".into())
            .spawn(move || {
                let result = compute_peaks(&path);
                let _ = sender.send(result);
                repaint.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.entries.insert(asset_id, WaveformEntry::Pending(receiver));
            }
            Err(error) => {
                self.entries.insert(
                    asset_id,
                    WaveformEntry::Failed(format!("波形計算用スレッドを開始できません: {error}")),
                );
            }
        }
    }

    /// Drops the cached entry (or cancels tracking an in-flight decode) so the next `request`
    /// tries again. Used by the retry button after a failure.
    pub fn forget(&mut self, asset_id: AssetId) {
        self.entries.remove(&asset_id);
    }

    /// Polls a pending decode, then reports the current state. Cheap to call every frame.
    pub fn status(&mut self, asset_id: AssetId) -> WaveformStatus {
        if matches!(
            self.entries.get(&asset_id),
            Some(WaveformEntry::Pending(_))
        ) {
            if let Some(WaveformEntry::Pending(receiver)) = self.entries.remove(&asset_id) {
                let entry = match receiver.try_recv() {
                    Ok(Ok(peaks)) => WaveformEntry::Ready(peaks),
                    Ok(Err(message)) => WaveformEntry::Failed(message),
                    Err(TryRecvError::Empty) => WaveformEntry::Pending(receiver),
                    Err(TryRecvError::Disconnected) => {
                        WaveformEntry::Failed("波形計算スレッドが中断しました".to_owned())
                    }
                };
                self.entries.insert(asset_id, entry);
            }
        }
        match self.entries.get(&asset_id) {
            None => WaveformStatus::Missing,
            Some(WaveformEntry::Pending(_)) => WaveformStatus::Pending,
            Some(WaveformEntry::Ready(peaks)) => WaveformStatus::Ready(Arc::clone(peaks)),
            Some(WaveformEntry::Failed(message)) => {
                WaveformStatus::Failed(message.clone())
            }
        }
    }
}

fn compute_peaks(path: &Path) -> Result<Arc<WaveformPeaks>, String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &PEAK_SAMPLE_RATE_HZ.round().to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .map_err(|error| format!("ffmpegを実行できません: {error}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if message.is_empty() {
            "音声をデコードできませんでした（音声トラックが無い可能性があります）".to_owned()
        } else {
            message
        });
    }
    let samples: Vec<i16> = output
        .stdout
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    Ok(Arc::new(WaveformPeaks::from_mono_s16(&samples)))
}

/// Everything a monitor panel needs about one clip, copied out of the project so the panel can
/// borrow the peak cache mutably at the same time.
struct ClipRow {
    asset_id: AssetId,
    name: String,
    path: PathBuf,
    source_in_secs: f64,
    source_out_secs: f64,
    timeline_start_secs: f64,
    timeline_end_secs: f64,
    speed: f64,
    gain_db: f32,
}

impl ClipRow {
    fn from_clip(clip: &Clip, playhead: TimelineTime) -> Self {
        Self {
            asset_id: clip.asset_id,
            name: clip.name.clone(),
            path: PathBuf::new(),
            source_in_secs: clip.source_in.as_seconds(),
            source_out_secs: clip.source_out.as_seconds(),
            timeline_start_secs: clip.timeline_start.as_seconds(),
            timeline_end_secs: clip.timeline_end().as_seconds(),
            speed: clip.speed(),
            gain_db: clip.effective_gain_db(clip.anim_offset_at(playhead)),
        }
    }

    fn contains_playhead(&self, playhead_secs: f64) -> bool {
        self.timeline_start_secs <= playhead_secs && playhead_secs < self.timeline_end_secs
    }

    /// Source-media seconds for a timeline position (accounts for playback speed).
    fn source_time_at(&self, timeline_secs: f64) -> f64 {
        let elapsed = timeline_secs - self.timeline_start_secs;
        self.source_in_secs + elapsed.max(0.0) * self.speed
    }
}

fn asset_kind_icon(asset: &MediaAsset) -> &'static str {
    if asset.metadata.is_image {
        "🖼"
    } else if asset.metadata.has_video {
        "🎬"
    } else if asset.metadata.has_audio {
        "🎵"
    } else {
        "📄"
    }
}

fn asset_kind_label(asset: &MediaAsset) -> &'static str {
    if asset.metadata.is_image {
        "画像"
    } else if asset.metadata.has_video && asset.metadata.has_audio {
        "映像＋音声"
    } else if asset.metadata.has_video {
        "映像"
    } else if asset.metadata.has_audio {
        "音声"
    } else {
        "不明"
    }
}

pub(super) fn media_browser_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("メディアブラウザ");
    ui.small("プロジェクトに取り込み済みのメディア一覧。クリックで再生ヘッド位置に配置します。");
    ui.separator();

    if ui
        .button("＋ ファイルを追加…")
        .on_hover_text("ファイルダイアログからメディアを取り込みます")
        .clicked()
    {
        context.actions.push(UiAction::ImportMediaDialog);
    }
    ui.add_space(4.0);

    if context.project.assets.is_empty() {
        ui.label("メディアがまだありません。「ファイルを追加…」か、タイムラインへのドラッグ＆ドロップで取り込みます。");
        return;
    }

    let playhead = context.playhead;
    let playhead_label = playhead.to_string();
    egui::ScrollArea::vertical()
        .id_salt("media_browser_assets")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for asset in &context.project.assets {
                let button = ui.add(
                    egui::Button::new(
                        egui::RichText::new(format!("{}  {}", asset_kind_icon(asset), asset.name))
                            .strong(),
                    )
                    .min_size(Vec2::new(ui.available_width(), 26.0)),
                );
                if button.clicked() {
                    context.actions.push(UiAction::AddAssetAt(asset.id, playhead));
                }
                button.on_hover_text(format!("再生ヘッド位置（{playhead_label}）に配置"));

                ui.horizontal_wrapped(|ui| {
                    ui.small(asset_kind_label(asset));
                    ui.small("·");
                    if asset.metadata.is_image {
                        ui.small("静止画");
                    } else {
                        ui.small(asset.metadata.duration.to_string());
                    }
                    if asset.metadata.has_video || asset.metadata.is_image {
                        ui.small("·");
                        ui.small(format!(
                            "{}×{} / {:.2}fps",
                            asset.metadata.width,
                            asset.metadata.height,
                            asset.metadata.frame_rate
                        ));
                    }
                });
                if let Some(folder) = asset.path.parent() {
                    ui.small(format!("📁 {}", folder.to_string_lossy()));
                }
                ui.add_space(6.0);
            }
        });
}

fn waveform_rows(context: &PanelContext<'_>) -> Vec<ClipRow> {
    let playhead = context.playhead;
    let mut rows = Vec::new();
    if let Some(item) = context.selected_item.and_then(|id| context.project.item(id)) {
        if let Some(clip) = item.audio_clip() {
            if context
                .project
                .asset(clip.asset_id)
                .is_some_and(|asset| asset.metadata.has_audio)
            {
                rows.push(ClipRow::from_clip(clip, playhead));
            }
        }
    }
    if rows.is_empty() {
        for clip in context.project.audible_clips_at(playhead) {
            rows.push(ClipRow::from_clip(clip, playhead));
        }
    }
    for row in &mut rows {
        if let Some(asset) = context.project.asset(row.asset_id) {
            row.path = asset.path.clone();
        }
    }
    rows.retain(|row| !row.path.as_os_str().is_empty());
    rows
}

pub(super) fn waveform_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("音声波形");
    ui.small(
        "選択中クリップ、または再生ヘッド上の音声クリップの波形（8kHz・概算ピーク）。サンプル精度ではありません。",
    );
    ui.separator();

    let rows = waveform_rows(context);
    if rows.is_empty() {
        ui.label(
            "波形を表示できる音声クリップがありません。クリップを選択するか、再生ヘッドを音声クリップの上に合わせてください。",
        );
        return;
    }

    let repaint = ui.ctx().clone();
    let playhead_secs = context.playhead.as_seconds();
    for row in &rows {
        context.peak_cache.request(row.asset_id, &row.path, repaint.clone());

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&row.name).strong());
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if ui.button("波形を再計算").clicked() {
                        context.peak_cache.forget(row.asset_id);
                        context.peak_cache.request(row.asset_id, &row.path, repaint.clone());
                    }
                },
            );
        });

        let view_end = row.source_out_secs.max(row.source_in_secs + 0.001);
        let playhead_source = row
            .contains_playhead(playhead_secs)
            .then(|| row.source_time_at(playhead_secs));

        match context.peak_cache.status(row.asset_id) {
            WaveformStatus::Missing | WaveformStatus::Pending => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("波形を計算中…");
                });
                // Keep the spinner (and the cache poll) alive until the decode lands.
                ui.ctx().request_repaint();
            }
            WaveformStatus::Failed(message) => {
                ui.colored_label(
                    Color32::from_rgb(220, 130, 120),
                    format!("波形を計算できません: {message}"),
                );
                if ui.button("再試行").clicked() {
                    context.peak_cache.forget(row.asset_id);
                    context.peak_cache.request(row.asset_id, &row.path, repaint.clone());
                }
            }
            WaveformStatus::Ready(peaks) => {
                ui.horizontal(|ui| {
                    ui.small(format!(
                        "{} 〜 {}",
                        TimelineTime::from_seconds(row.source_in_secs),
                        TimelineTime::from_seconds(row.source_out_secs)
                    ));
                    if let Some(source) = playhead_source {
                        ui.separator();
                        ui.small(format!(
                            "▶ {}",
                            TimelineTime::from_seconds(source)
                        ));
                    }
                });
                draw_waveform(ui, &peaks, row.source_in_secs, view_end, playhead_source);
            }
        }
        ui.add_space(8.0);
    }
}

fn draw_waveform(
    ui: &mut Ui,
    peaks: &WaveformPeaks,
    view_start: f64,
    view_end: f64,
    playhead_source: Option<f64>,
) {
    let height = 72.0;
    let (rect, _response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Outside,
    );

    let mid_y = rect.center().y;
    painter.line_segment(
        [
            Pos2::new(rect.left() + 1.0, mid_y),
            Pos2::new(rect.right() - 1.0, mid_y),
        ],
        Stroke::new(1.0, Color32::from_gray(70)),
    );

    let span = (view_end - view_start).max(0.001);
    let half_height = (rect.height() * 0.5 - 3.0).max(1.0);
    let columns = (rect.width().round() as usize).max(1);
    let wave = Color32::from_rgb(120, 190, 255);
    for column in 0..columns {
        let t0 = view_start + span * (column as f64 / columns as f64);
        let t1 = view_start + span * ((column + 1) as f64 / columns as f64);
        let (min, max) = peaks.minmax_in(t0, t1);
        let x = rect.left() + column as f32 + 0.5;
        let y0 = mid_y - max * half_height;
        let y1 = mid_y - min * half_height;
        painter.line_segment(
            [Pos2::new(x, y0.max(rect.top())), Pos2::new(x, y1.min(rect.bottom()))],
            Stroke::new(1.0, wave),
        );
    }

    if let Some(source) = playhead_source {
        let fraction = ((source - view_start) / span).clamp(0.0, 1.0) as f32;
        let x = rect.left() + rect.width() * fraction;
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(2.0, Color32::from_rgb(255, 210, 80)),
        );
    }
}

fn db_to_linear(db: f32) -> f32 {
    if !db.is_finite() {
        return 1.0;
    }
    10f32.powf(db / 20.0)
}

fn meter_bar(ui: &mut Ui, value: f32, max: f32, fill: Color32) {
    let height = 14.0;
    let (rect, _response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let fraction = if max > 0.0 {
        (value / max).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if fraction > 0.0 {
        let filled = Rect::from_min_max(
            rect.min,
            Pos2::new(rect.min.x + rect.width() * fraction, rect.max.y),
        );
        painter.rect_filled(filled, 2.0, fill);
    }
    painter.rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Outside,
    );
}

pub(super) fn level_meter_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("音量モニタ");
    ui.small(
        "再生ヘッド上で鳴っているクリップのゲインと、波形ピークからの概算レベルです。サンプル精度のメーターではありません。",
    );
    ui.separator();

    let playhead_secs = context.playhead.as_seconds();
    let mut rows: Vec<ClipRow> = context
        .project
        .audible_clips_at(context.playhead)
        .into_iter()
        .map(|clip| ClipRow::from_clip(clip, context.playhead))
        .collect();
    for row in &mut rows {
        if let Some(asset) = context.project.asset(row.asset_id) {
            row.path = asset.path.clone();
        }
    }
    rows.retain(|row| !row.path.as_os_str().is_empty());

    if rows.is_empty() {
        ui.label("再生ヘッド上で鳴っている音声クリップがありません。");
        return;
    }

    let repaint = ui.ctx().clone();
    let mut total_linear = 0.0f32;
    egui::ScrollArea::vertical()
        .id_salt("level_meter_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                context.peak_cache.request(row.asset_id, &row.path, repaint.clone());
                let linear = db_to_linear(row.gain_db);
                total_linear += linear;

                ui.label(egui::RichText::new(&row.name).strong());
                ui.horizontal(|ui| {
                    ui.small(format!("{:+.1} dB", row.gain_db));
                    ui.separator();
                    ui.small(format!("×{:.2}", linear));
                });
                // Gain is a known quantity (settings + keyframes); show it as the solid bar.
                meter_bar(ui, linear, 2.0, Color32::from_rgb(120, 190, 255));

                let level = match context.peak_cache.status(row.asset_id) {
                    WaveformStatus::Ready(peaks) => {
                        let source = row.source_time_at(playhead_secs);
                        peaks.peak_in(
                            source - LEVEL_WINDOW_SECS * 0.5,
                            source + LEVEL_WINDOW_SECS * 0.5,
                        )
                    }
                    _ => {
                        ui.ctx().request_repaint();
                        0.0
                    }
                };
                ui.horizontal(|ui| {
                    ui.small("レベル（目安）");
                    ui.separator();
                    ui.small(format!("{:.0}%", level * 100.0));
                });
                meter_bar(ui, level, 1.0, Color32::from_rgb(140, 220, 140));
                ui.add_space(10.0);
            }
        });

    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("合計（目安）").strong());
        ui.separator();
        ui.small(format!("Σゲイン ×{:.2}", total_linear));
    });
    meter_bar(
        ui,
        total_linear,
        2.0,
        Color32::from_rgb(255, 210, 80),
    );
}

