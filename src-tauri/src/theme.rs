//! The window's own colours, which the page cannot set (SET-5).
//!
//! What shows before the page has drawn, and in the strip a live resize
//! uncovers, is the window's background and not the page's: a light theme in
//! a dark window flashed dark at every launch. And the appearance macOS gives
//! the app decides the page's `prefers-color-scheme`, its scrollbars and its
//! native dialogs.

use tauri::{window::Color, AppHandle, Manager, Runtime, Theme, Window};

use crate::commands::AppState;

/// `--bg` of each theme, in `src/theme.css`.
const DARK: Color = Color(0x0c, 0x0c, 0x11, 0xff);
const LIGHT: Color = Color(0xff, 0xff, 0xff, 0xff);
const TOKYO_NIGHT: Color = Color(0x1a, 0x1b, 0x26, 0xff);
const CURSOR_DARK: Color = Color(0x18, 0x18, 0x18, 0xff);
const CURSOR_LIGHT: Color = Color(0xfc, 0xfc, 0xfc, 0xff);
const AYU_DARK: Color = Color(0x10, 0x14, 0x1c, 0xff);
const AYU_LIGHT: Color = Color(0xfc, 0xfc, 0xfc, 0xff);

/// The themes drawn dark on light, which macOS must draw its own parts of
/// the window for (scrollbars, dialogs, `prefers-color-scheme`) in light.
const LIGHT_THEMES: &[&str] = &["light", "cursor-light", "ayu-light"];

/// Give the app the appearance `choice` names (one of `config::THEMES`),
/// and paint the window to match.
pub fn apply<R: Runtime>(app: &AppHandle<R>, choice: &str) {
    let Some(window) = app.get_webview_window("main") else { return };
    let _ = window.set_theme(match choice {
        "system" => None,
        _ if LIGHT_THEMES.contains(&choice) => Some(Theme::Light),
        _ => Some(Theme::Dark),
    });
    paint(&window.as_ref().window(), choice);
}

/// The window's background, in whichever appearance it has now: also when
/// macOS switches between light and dark under "system".
pub fn repaint<R: Runtime>(window: &Window<R>) {
    let choice = window
        .try_state::<AppState>()
        .map(|state| state.config.read().ui.theme)
        .unwrap_or_default();
    paint(window, &choice);
}

fn paint<R: Runtime>(window: &Window<R>, choice: &str) {
    let light = matches!(window.theme(), Ok(Theme::Light));
    let _ = window.set_background_color(Some(background(choice, light)));
}

/// The window's colour under `choice`, when macOS has given it the light
/// appearance or not: which one "system" is.
fn background(choice: &str, light: bool) -> Color {
    match choice {
        "tokyo-night" => TOKYO_NIGHT,
        "cursor-dark" => CURSOR_DARK,
        "cursor-light" => CURSOR_LIGHT,
        "ayu-dark" => AYU_DARK,
        "ayu-light" => AYU_LIGHT,
        _ if light => LIGHT,
        _ => DARK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name missing from one of these lists is a theme the settings save
    /// as Dark, or a window that flashes another theme's colour at launch.
    #[test]
    fn every_theme_the_settings_take_is_drawn_and_painted_in_its_own_background() {
        let css = include_str!("../../src/theme.css");
        let listed = include_str!("../../src/lib/theme.ts");
        for name in crate::config::THEMES.iter().filter(|t| **t != "system") {
            let at = css.find(&format!("[data-theme=\"{name}\"]")).unwrap_or_else(|| panic!("{name} not in theme.css"));
            let bg = css[at..].split("--bg:").nth(1).and_then(|b| b.split(';').next()).unwrap().trim();
            let Color(r, g, b, _) = background(name, *name == "light");
            assert_eq!(bg, format!("#{r:02x}{g:02x}{b:02x}"), "{name}'s window and page backgrounds differ");
            assert!(listed.contains(&format!("id: \"{name}\"")), "{name} not offered in lib/theme.ts");
            // A light page in a window macOS draws dark has dark scrollbars
            // and dialogs, and xterm leaves faint colours faint on it.
            let light = LIGHT_THEMES.contains(name);
            let scheme = css[at..].split("color-scheme:").nth(1).and_then(|s| s.split(';').next()).unwrap().trim();
            assert_eq!(scheme, if light { "light" } else { "dark" }, "{name}'s color-scheme");
            let entry = listed.lines().find(|l| l.contains(&format!("id: \"{name}\""))).unwrap();
            assert!(entry.contains(&format!("light: {light}")), "{name} is light in one place and dark in another");
        }
    }
}
