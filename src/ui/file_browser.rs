//! ファイルブラウザ — in-app filesystem navigation so editing never needs the
//! desktop file manager.
//!
//! [`FileBrowserState`] is app memory only: navigation history, listing cache and
//! selection never touch a `.yukivproj`.

use crate::ui::panels::{PanelContext, UiAction};
use eframe::egui::{self, Color32, Ui, Vec2};
use std::path::{Path, PathBuf};

/// Video/audio extensions accepted alongside [`crate::media::IMAGE_EXTENSIONS`].
const AV_EXTENSIONS: [&str; 13] = [
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "mp3", "wav", "m4a", "aac", "flac", "ogg", "gif",
];

const MAX_RECENT_DIRS: usize = 6;

/// Longest file name shown on the 「〜を再生位置に追加」 button before eliding.
const ADD_BUTTON_NAME_CHARS: usize = 24;

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Expands a leading `~` (or bare `~/...`) to the home directory. The path box is a
/// Linux convenience, so `~user` is intentionally left untouched.
fn expand_user_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        return home_dir();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(trimmed)
}

fn is_media_path(path: &Path) -> bool {
    let Some(ext) = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };
    AV_EXTENSIONS.iter().any(|known| *known == ext)
        || crate::media::IMAGE_EXTENSIONS
            .iter()
            .any(|known| *known == ext)
}

fn media_icon(path: &Path) -> &'static str {
    let Some(ext) = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return "❔";
    };
    if crate::media::IMAGE_EXTENSIONS
        .iter()
        .any(|known| *known == ext)
        || ext == "gif"
    {
        "🖼"
    } else if ["mp3", "wav", "m4a", "aac", "flac", "ogg"].contains(&ext.as_str()) {
        "🎵"
    } else {
        "🎬"
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn truncate_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_owned();
    }
    let mut truncated: String = name.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

/// Compact label for a directory button: `~`-relative under home, else the last
/// component (full path stays available as a hover tooltip).
fn short_dir_label(path: &Path) -> String {
    let home = home_dir();
    if path == home {
        return "🏠 ~".to_owned();
    }
    if let Ok(rest) = path.strip_prefix(&home) {
        let rest = rest.to_string_lossy().into_owned();
        if !rest.is_empty() {
            return format!("~/{rest}");
        }
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

#[derive(Clone, Debug)]
struct BrowserEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: Option<u64>,
}

/// Navigation state for the ファイルブラウザ panel. Lives in app memory only —
/// never written to a project file.
pub struct FileBrowserState {
    cwd: PathBuf,
    entries: Vec<BrowserEntry>,
    /// Cache key `(directory, media_only)`. The listing is re-read only when this
    /// no longer matches, so the paint path never hits `read_dir` on a hot frame.
    listed_for: Option<(PathBuf, bool)>,
    show_media_only: bool,
    selected: Option<PathBuf>,
    error: Option<String>,
    /// Most recently visited directories, most recent first.
    recent: Vec<PathBuf>,
    /// Persistent TextEdit buffer; egui keeps only the cursor across frames, so a
    /// string rebuilt from `cwd` every frame would drop every keystroke.
    path_draft: Option<String>,
    reset_scroll: bool,
}

impl Default for FileBrowserState {
    fn default() -> Self {
        Self {
            cwd: home_dir(),
            entries: Vec::new(),
            listed_for: None,
            show_media_only: true,
            selected: None,
            error: None,
            recent: Vec::new(),
            path_draft: None,
            reset_scroll: false,
        }
    }
}

impl FileBrowserState {
    fn navigate(&mut self, dir: PathBuf) {
        self.cwd = dir.clone();
        self.selected = None;
        self.listed_for = None;
        self.error = None;
        self.reset_scroll = true;
        self.push_recent(dir);
    }

    /// Validates and enters the path typed into the path box. Invalid input leaves
    /// the current directory alone and reports why in `error`.
    fn navigate_text(&mut self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let path = expand_user_path(trimmed);
        if !path.exists() {
            self.error = Some(format!("パスが見つかりません: {}", path.display()));
            return;
        }
        if !path.is_dir() {
            self.error = Some(format!("フォルダではありません: {}", path.display()));
            return;
        }
        self.navigate(path);
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.navigate(parent);
        }
    }

    fn refresh(&mut self) {
        self.listed_for = None;
    }

    fn push_recent(&mut self, dir: PathBuf) {
        self.recent.retain(|seen| seen != &dir);
        self.recent.insert(0, dir);
        self.recent.truncate(MAX_RECENT_DIRS);
    }

    /// Moves the selection to the next/previous non-directory entry. Directories are
    /// skipped so the selection always names something the add button can import.
    fn move_selection(&mut self, step: i32) {
        self.listing();
        let files: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| !entry.is_dir)
            .map(|(index, _)| index)
            .collect();
        if files.is_empty() {
            self.selected = None;
            return;
        }
        let current = files
            .iter()
            .position(|&index| self.selected.as_ref() == Some(&self.entries[index].path));
        let next = match current {
            Some(position) => (position as i32 + step).clamp(0, files.len() as i32 - 1) as usize,
            None if step >= 0 => 0,
            None => files.len() - 1,
        };
        self.selected = Some(self.entries[files[next]].path.clone());
    }

    fn listing(&mut self) -> &[BrowserEntry] {
        let key = (self.cwd.clone(), self.show_media_only);
        if self.listed_for.as_ref() == Some(&key) {
            return &self.entries;
        }
        self.entries.clear();
        self.error = None;
        match std::fs::read_dir(&self.cwd) {
            Ok(reader) => {
                for entry in reader.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    // Directories stay listed even in media-only mode so a folder full of
                    // non-media files is still enterable.
                    if !is_dir && self.show_media_only && !is_media_path(&path) {
                        continue;
                    }
                    let size = if is_dir {
                        None
                    } else {
                        entry.metadata().ok().map(|m| m.len())
                    };
                    self.entries.push(BrowserEntry {
                        path,
                        name,
                        is_dir,
                        size,
                    });
                }
                self.entries
                    .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            }
            Err(error) => {
                self.error = Some(format!("フォルダを読み込めません: {error}"));
            }
        }
        self.listed_for = Some(key);
        // Selection survives refreshes as long as the file is still listed; a vanished
        // file (or one the media-only filter just hid) must not keep a stale add button.
        if let Some(selected) = &self.selected
            && !self.entries.iter().any(|entry| &entry.path == selected)
        {
            self.selected = None;
        }
        &self.entries
    }
}

pub(super) fn file_browser_panel(ui: &mut Ui, context: &mut PanelContext<'_>) {
    ui.heading("ファイルブラウザ");
    ui.small("PC内のフォルダをここから移動できます。メディアをダブルクリックで再生位置に追加、選択してから追加ボタンでも同じ。");
    ui.separator();

    let state = &mut *context.file_browser;
    ui.horizontal(|ui| {
        if ui.button("⬆ 上へ").on_hover_text("親フォルダへ (Backspace)").clicked() {
            state.go_up();
        }
        if ui.button("🔄").on_hover_text("再読み込み").clicked() {
            state.refresh();
        }
        if ui
            .checkbox(&mut state.show_media_only, "メディアのみ")
            .changed()
        {
            state.refresh();
        }
    });

    let home = home_dir();
    let asset_dir = context
        .project
        .assets
        .first()
        .and_then(|asset| asset.path.parent())
        .map(Path::to_path_buf);
    let project_dir = context
        .project_path
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| asset_dir.clone());
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("🏠 ホーム").clicked() {
            state.navigate(home);
        }
        if ui.small_button("📂 ルート /").clicked() {
            state.navigate(PathBuf::from("/"));
        }
        if let Some(dir) = project_dir
            && ui
                .small_button("📁 プロジェクトのあるフォルダ")
                .on_hover_text(dir.display().to_string())
                .clicked()
        {
            state.navigate(dir);
        }
        if let Some(dir) = asset_dir
            && ui
                .small_button("🎞 素材フォルダ")
                .on_hover_text(dir.display().to_string())
                .clicked()
        {
            state.navigate(dir);
        }
    });

    let recents = state.recent.clone();
    if !recents.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.small("最近:");
            for dir in recents {
                let label = short_dir_label(&dir);
                if ui
                    .small_button(&label)
                    .on_hover_text(dir.display().to_string())
                    .clicked()
                {
                    state.navigate(dir);
                }
            }
        });
    }

    let mut path_interacting = false;
    ui.horizontal(|ui| {
        ui.label("パス:");
        if state.path_draft.is_none() {
            state.path_draft = Some(state.cwd.to_string_lossy().into_owned());
        }
        let mut path_text = state.path_draft.take().expect("seeded above");
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut path_text)
                    .id_salt("file_browser_path")
                    .desired_width(ui.available_width() - 60.0),
            )
            .on_hover_text("Enter で移動。先頭の ~ はホームフォルダに展開されます");
        let move_clicked = ui.button("移動").clicked();
        path_interacting = response.has_focus() || response.lost_focus();
        let submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if submit || move_clicked {
            state.navigate_text(&path_text);
            state.path_draft = None;
        } else if response.has_focus() {
            state.path_draft = Some(path_text);
        } else {
            state.path_draft = None;
        }
    });

    if let Some(error) = &state.error {
        ui.colored_label(Color32::from_rgb(240, 120, 120), error);
    }

    let playhead = context.playhead;
    let playhead_label = playhead.to_string();
    let mut import_path: Option<PathBuf> = None;
    let mut keyboard_moved = false;

    // Keyboard shortcuts belong to the browser only when no text field wants the keys;
    // the path box needs Enter/Backspace/arrows for its own editing.
    if !path_interacting && !ui.ctx().egui_wants_keyboard_input() {
        let (up, down, enter, backspace) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Backspace),
            )
        });
        if up || down {
            state.move_selection(if up { -1 } else { 1 });
            keyboard_moved = true;
        }
        if enter
            && let Some(selected) = state.selected.clone()
            && is_media_path(&selected)
        {
            import_path = Some(selected);
        }
        if backspace {
            state.go_up();
        }
    }

    let listing: Vec<BrowserEntry> = state.listing().to_vec();

    if listing.is_empty() && state.error.is_none() {
        ui.label(if state.show_media_only {
            "このフォルダにメディアがありません。「メディアのみ」を外すと全ファイルを表示します。"
        } else {
            "このフォルダは空です。"
        });
    }

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("file_browser_entries")
        .auto_shrink([false, false]);
    if state.reset_scroll {
        scroll = scroll.scroll_offset(Vec2::ZERO);
        state.reset_scroll = false;
    }
    scroll.show(ui, |ui| {
        let mut scroll_to: Option<egui::Rect> = None;
        for entry in &listing {
            let selected = state.selected.as_ref() == Some(&entry.path);
            let label = if entry.is_dir {
                format!("📁 {}", entry.name)
            } else {
                let icon = media_icon(&entry.path);
                match entry.size {
                    Some(size) => format!("{icon} {}  · {}", entry.name, format_size(size)),
                    None => format!("{icon} {}", entry.name),
                }
            };
            let text = if entry.is_dir {
                egui::RichText::new(label).strong()
            } else {
                egui::RichText::new(label)
            };
            let mut button =
                egui::Button::new(text).min_size(Vec2::new(ui.available_width(), 24.0));
            if selected {
                button = button.fill(Color32::from_rgb(60, 100, 180));
            }
            let response = ui.add(button);
            if selected {
                scroll_to = Some(response.rect);
            }
            if entry.is_dir {
                let response = response.on_hover_text("クリックでこのフォルダを開く");
                if response.clicked() {
                    state.navigate(entry.path.clone());
                    // The rest of this snapshot describes the directory we just left.
                    return;
                }
            } else {
                let media = is_media_path(&entry.path);
                let response = if media {
                    response.on_hover_text(format!(
                        "クリックで選択／ダブルクリックで再生位置（{playhead_label}）に追加"
                    ))
                } else {
                    response
                        .on_hover_text("メディア形式ではないのでタイムラインには追加できません")
                };
                if response.clicked() {
                    state.selected = Some(entry.path.clone());
                }
                if media && response.double_clicked() {
                    import_path = Some(entry.path.clone());
                }
            }
        }
        if keyboard_moved
            && let Some(rect) = scroll_to
        {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
    });

    if let Some(selected) = state.selected.clone()
        && is_media_path(&selected)
    {
        ui.separator();
        ui.horizontal(|ui| {
            let name = selected
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| selected.display().to_string());
            if ui
                .button(format!(
                    "▶ 「{}」を再生位置に追加",
                    truncate_name(&name, ADD_BUTTON_NAME_CHARS)
                ))
                .on_hover_text(format!("{name} を再生位置（{playhead_label}）に追加"))
                .clicked()
            {
                import_path = Some(selected);
            }
            if ui.button("クリア").clicked() {
                state.selected = None;
            }
        });
    }

    if let Some(path) = import_path {
        context
            .actions
            .push(UiAction::ImportFilesAt(vec![path], playhead));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("yuki_file_browser_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn expand_user_path_expands_tilde() {
        let home = home_dir();
        assert_eq!(expand_user_path("~"), home);
        assert_eq!(expand_user_path("~/Videos"), home.join("Videos"));
        assert_eq!(expand_user_path("/tmp/clip.mp4"), PathBuf::from("/tmp/clip.mp4"));
        assert_eq!(expand_user_path("  /tmp  "), PathBuf::from("/tmp"));
    }

    #[test]
    fn truncate_name_elides_long_names() {
        assert_eq!(truncate_name("short.mp4", 10), "short.mp4");
        assert_eq!(truncate_name("0123456789A", 10), "012345678…");
    }

    #[test]
    fn media_detection_covers_av_and_images() {
        assert!(is_media_path(Path::new("clips/shot.MP4")));
        assert!(is_media_path(Path::new("still.png")));
        assert!(!is_media_path(Path::new("notes.txt")));
        assert!(!is_media_path(Path::new("Makefile")));
    }

    #[test]
    fn navigate_text_validates_and_expands() {
        let mut state = FileBrowserState::default();
        let before = state.cwd.clone();
        state.navigate_text("/definitely/not/a/real/dir");
        assert_eq!(state.cwd, before, "invalid path must not move the browser");
        assert!(state.error.is_some());
        state.navigate_text("~");
        assert_eq!(state.cwd, home_dir());
        assert!(state.error.is_none());
    }

    #[test]
    fn media_only_keeps_directories_and_toggle_relists() {
        let temp = TempDir::new();
        std::fs::write(temp.0.join("clip.txt"), b"x").unwrap();
        std::fs::create_dir(temp.0.join("footage")).unwrap();
        let mut state = FileBrowserState::default();
        state.navigate(temp.0.clone());
        state.listing();
        let names: Vec<&str> = state.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["footage"], "non-media file hidden, folder stays");
        assert!(state.entries[0].is_dir);
        assert_eq!(state.listed_for, Some((temp.0.clone(), true)));

        state.show_media_only = false;
        state.listing();
        let names: Vec<&str> = state.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"clip.txt") && names.contains(&"footage"));
    }

    #[test]
    fn selection_survives_refresh_and_clears_when_file_vanishes() {
        let temp = TempDir::new();
        let file = temp.0.join("clip.mp4");
        std::fs::write(&file, b"x").unwrap();
        let mut state = FileBrowserState::default();
        state.navigate(temp.0.clone());
        state.listing();
        state.selected = Some(file.clone());
        state.refresh();
        state.listing();
        assert_eq!(state.selected.as_ref(), Some(&file));

        std::fs::remove_file(&file).unwrap();
        state.refresh();
        state.listing();
        assert!(state.selected.is_none());
    }

    #[test]
    fn recent_dirs_are_unique_and_capped() {
        let mut state = FileBrowserState::default();
        for i in 0..8 {
            state.navigate(PathBuf::from(format!("/tmp/yuki_recent_{i}")));
        }
        assert_eq!(state.recent.len(), MAX_RECENT_DIRS);
        assert_eq!(state.recent[0], PathBuf::from("/tmp/yuki_recent_7"));

        let revisit = PathBuf::from("/tmp/yuki_recent_2");
        state.navigate(revisit.clone());
        assert_eq!(state.recent.len(), MAX_RECENT_DIRS);
        assert_eq!(state.recent[0], revisit);
        assert_eq!(state.recent.iter().filter(|seen| *seen == &revisit).count(), 1);
    }

    #[test]
    fn keyboard_selection_moves_through_files_only() {
        let temp = TempDir::new();
        std::fs::create_dir(temp.0.join("a_dir")).unwrap();
        std::fs::write(temp.0.join("b.mp4"), b"x").unwrap();
        std::fs::write(temp.0.join("c.mp4"), b"x").unwrap();
        let mut state = FileBrowserState::default();
        state.navigate(temp.0.clone());
        state.move_selection(1);
        assert_eq!(state.selected, Some(temp.0.join("b.mp4")));
        state.move_selection(1);
        assert_eq!(state.selected, Some(temp.0.join("c.mp4")));
        state.move_selection(1);
        assert_eq!(state.selected, Some(temp.0.join("c.mp4")), "clamped at last file");
        state.move_selection(-1);
        assert_eq!(state.selected, Some(temp.0.join("b.mp4")));
        state.move_selection(-1);
        assert_eq!(state.selected, Some(temp.0.join("b.mp4")), "clamped at first file");
    }
}
