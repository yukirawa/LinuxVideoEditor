//! Selectable UI themes. A theme is only a set of [`egui::Visuals`]; colours that carry
//! meaning (clip kinds, playhead red, preview letterbox) stay hardcoded so they read the
//! same in every theme.

use eframe::egui::{Color32, Stroke, Visuals, ecolor::Hsva};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    /// egui's stock dark look — the editor's original appearance.
    #[default]
    GrayDark,
    PureWhite,
    OledBlack,
    RandomColor,
}

impl AppTheme {
    /// The 表示 ▸ テーマ submenu, in display order.
    pub const MENU: [(AppTheme, &'static str); 4] = [
        (AppTheme::PureWhite, "⚪ 純白モード"),
        (AppTheme::GrayDark, "🌑 灰色ダーク（標準）"),
        (AppTheme::OledBlack, "⬛ OLED 完全黒"),
        (AppTheme::RandomColor, "🎲 ランダムカラー"),
    ];
}

/// Builds the visuals for `theme`. `random_hue` is `(hue, saturation)` in `0..=1` and is only
/// used by [`AppTheme::RandomColor`]; re-roll it to redraw that palette.
pub fn build_visuals(theme: AppTheme, random_hue: (f32, f32)) -> Visuals {
    match theme {
        AppTheme::GrayDark => Visuals::dark(),
        AppTheme::PureWhite => pure_white(),
        AppTheme::OledBlack => oled_black(),
        AppTheme::RandomColor => random_color(random_hue.0, random_hue.1),
    }
}

/// Fresh `(hue, saturation)` for [`AppTheme::RandomColor`]. Derived from the clock so no RNG
/// dependency is needed; the mix keeps consecutive clicks from collapsing onto one hue.
pub fn roll_random_seed() -> (f32, f32) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let mut mixed = nanos ^ (nanos >> 33);
    mixed = mixed.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    mixed ^= mixed >> 33;
    let hue = (mixed % 360) as f32 / 360.0;
    let saturation = 0.30 + ((mixed >> 16) % 56) as f32 / 100.0;
    (hue, saturation)
}

fn pure_white() -> Visuals {
    let mut visuals = Visuals::light();
    let white = Color32::from_rgb(255, 255, 255);
    let edge = Color32::from_rgb(220, 220, 220);
    visuals.panel_fill = white;
    visuals.window_fill = white;
    visuals.extreme_bg_color = white;
    visuals.faint_bg_color = Color32::from_rgb(245, 245, 245);
    visuals.code_bg_color = Color32::from_rgb(240, 240, 240);
    visuals.text_edit_bg_color = Some(white);
    visuals.window_stroke = Stroke::new(1.0, edge);
    visuals.widgets.noninteractive.bg_fill = white;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(245, 245, 245);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, edge);
    visuals
}

fn oled_black() -> Visuals {
    let mut visuals = Visuals::dark();
    let black = Color32::BLACK;
    let edge = Color32::from_rgb(70, 70, 70);
    visuals.panel_fill = black;
    visuals.window_fill = black;
    visuals.extreme_bg_color = black;
    visuals.faint_bg_color = Color32::from_rgb(12, 12, 12);
    visuals.code_bg_color = Color32::from_rgb(12, 12, 12);
    visuals.text_edit_bg_color = Some(black);
    visuals.window_stroke = Stroke::new(1.0, edge);
    // Widgets sit one step above pure black so their frames stay readable.
    visuals.widgets.noninteractive.bg_fill = black;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(18, 18, 18);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(18, 18, 18);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(32, 32, 32);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(32, 32, 32);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(110, 110, 110));
    visuals.widgets.active.bg_fill = Color32::from_rgb(40, 40, 40);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(40, 40, 40);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(130, 130, 130));
    visuals
}

fn random_color(hue: f32, saturation: f32) -> Visuals {
    // Surfaces stay dark-ish so the video preview remains readable whatever hue is drawn.
    let mut visuals = Visuals::dark();
    let hue = hue.rem_euclid(1.0);
    let saturation = saturation.clamp(0.25, 0.85);
    let accent = Color32::from(Hsva::new(hue, saturation, 0.85, 1.0));
    let accent_soft = Color32::from(Hsva::new(hue, saturation * 0.55, 0.42, 1.0));
    let surface = Color32::from(Hsva::new(hue, 0.18, 0.12, 1.0));
    let surface_high = Color32::from(Hsva::new(hue, 0.22, 0.20, 1.0));
    let extreme = Color32::from(Hsva::new(hue, 0.22, 0.08, 1.0));
    let edge = Color32::from(Hsva::new(hue, 0.25, 0.38, 1.0));
    visuals.panel_fill = surface;
    visuals.window_fill = surface;
    visuals.extreme_bg_color = extreme;
    visuals.faint_bg_color = surface_high;
    visuals.code_bg_color = extreme;
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, accent);
    visuals.window_stroke = Stroke::new(1.0, edge);
    visuals.widgets.noninteractive.bg_fill = surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.inactive.bg_fill = surface_high;
    visuals.widgets.inactive.weak_bg_fill = surface_high;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, edge);
    visuals.widgets.hovered.bg_fill = accent_soft;
    visuals.widgets.hovered.weak_bg_fill = accent_soft;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    // A bright accent fill needs dark text to stay readable.
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    visuals.widgets.open.bg_fill = accent_soft;
    visuals.widgets.open.weak_bg_fill = accent_soft;
    visuals
}
