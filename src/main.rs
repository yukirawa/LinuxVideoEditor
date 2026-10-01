use yuki_video_editor::app::VideoEditorApp;

fn main() -> eframe::Result {
    prefer_x11_for_file_drag_and_drop();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Yuki Video Editor")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 640.0])
            // Windows may disable OS drag-and-drop unless explicitly requested; the editor
            // relies on it to import media straight onto the timeline.
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Yuki Video Editor",
        options,
        Box::new(|creation_context| Ok(Box::new(VideoEditorApp::new(creation_context)))),
    )
}

/// Prefers the X11 (XWayland) backend on a Wayland session so OS file drag-and-drop works.
///
/// `winit` 0.30 has no file drop support in its Wayland backend: it never emits
/// `WindowEvent::DroppedFile`/`HoveredFile`, so dropping media onto the window would silently do
/// nothing and the only way to add a file would be the "＋ファイル" button. The X11/XWayland
/// backend does implement drop events, so when `DISPLAY` is available we unset `WAYLAND_DISPLAY`
/// before the event loop is created to make winit pick X11. Without XWayland the app keeps
/// running natively on Wayland (drag-and-drop stays unavailable); set
/// `YUKI_VIDEO_EDITOR_BACKEND=wayland` to keep Wayland even when XWayland is present.
fn prefer_x11_for_file_drag_and_drop() {
    #[cfg(target_os = "linux")]
    {
        use std::env;

        // winit treats an empty variable as unset, so mirror that here.
        let is_set = |key: &str| env::var_os(key).is_some_and(|value| !value.is_empty());
        let on_wayland = is_set("WAYLAND_DISPLAY") || is_set("WAYLAND_SOCKET");
        let has_x11 = is_set("DISPLAY");
        let keep_wayland = env::var("YUKI_VIDEO_EDITOR_BACKEND")
            .is_ok_and(|backend| backend.eq_ignore_ascii_case("wayland"));

        if on_wayland && has_x11 && !keep_wayland {
            // SAFETY: called at the very start of `main`, before any other thread exists (and
            // before the winit event loop reads the environment), so nothing can race on it.
            unsafe {
                env::remove_var("WAYLAND_DISPLAY");
                env::remove_var("WAYLAND_SOCKET");
            }
        } else if on_wayland && !has_x11 {
            eprintln!(
                "XWayland (DISPLAY) is unavailable; running on Wayland without file drag-and-drop"
            );
        }
    }
}
