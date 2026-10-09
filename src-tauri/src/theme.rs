//! The window's own colours, which the page cannot set (SET-5).
//!
//! What shows before the page has drawn, and in the strip a live resize
//! uncovers, is the window's background and not the page's: a light theme in
//! a dark window flashed dark at every launch. And the appearance macOS gives
//! the app decides the page's `prefers-color-scheme`, its scrollbars and its
//! native dialogs.

use tauri::{window::Color, AppHandle, Manager, Runtime, Theme, Window};

/// `--bg` of each theme, in `src/theme.css`.
const DARK: Color = Color(0x0c, 0x0c, 0x11, 0xff);
const LIGHT: Color = Color(0xff, 0xff, 0xff, 0xff);

/// Give the app the appearance `choice` names ("dark", "light", "system"),
/// and paint the window to match.
pub fn apply<R: Runtime>(app: &AppHandle<R>, choice: &str) {
    let Some(window) = app.get_webview_window("main") else { return };
    let _ = window.set_theme(match choice {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    });
    repaint(&window.as_ref().window());
}

/// The window's background, in whichever appearance it has now: also when
/// macOS switches between light and dark under "system".
pub fn repaint<R: Runtime>(window: &Window<R>) {
    let light = matches!(window.theme(), Ok(Theme::Light));
    let _ = window.set_background_color(Some(if light { LIGHT } else { DARK }));
}
