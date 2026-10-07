//! Keys as Chrome's `Input.dispatchKeyEvent` wants them.

use serde_json::{json, Value};

use crate::error::{Error, Result};

pub const ALT: u32 = 1;
pub const CTRL: u32 = 2;
pub const META: u32 = 4;
pub const SHIFT: u32 = 8;

/// A named key: its `key`, `code`, Windows key code, and the text it types.
fn named(name: &str) -> Option<(&'static str, &'static str, u32, &'static str)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter", "Enter", 13, "\r"),
        "tab" => ("Tab", "Tab", 9, ""),
        "escape" | "esc" => ("Escape", "Escape", 27, ""),
        "backspace" => ("Backspace", "Backspace", 8, ""),
        "delete" => ("Delete", "Delete", 46, ""),
        "space" => (" ", "Space", 32, " "),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, ""),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, ""),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, ""),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, ""),
        "home" => ("Home", "Home", 36, ""),
        "end" => ("End", "End", 35, ""),
        "pageup" => ("PageUp", "PageUp", 33, ""),
        "pagedown" => ("PageDown", "PageDown", 34, ""),
        _ => return None,
    })
}

/// The editing command a headless Chrome on macOS needs alongside a key.
///
/// Without a window there is no menu bar, and on macOS it is the menu that
/// turns ⌘A into "select all": the key arrives, and nothing happens.
pub fn mac_commands(key: &str, modifiers: u32) -> Vec<&'static str> {
    if modifiers & META == 0 || modifiers & (CTRL | ALT) != 0 {
        return Vec::new();
    }
    let shift = modifiers & SHIFT != 0;
    match (key.to_ascii_lowercase().as_str(), shift) {
        ("a", false) => vec!["selectAll"],
        ("z", false) => vec!["undo"],
        ("z", true) => vec!["redo"],
        ("x", false) => vec!["cut"],
        ("c", false) => vec!["copy"],
        ("v", false) => vec!["paste"],
        ("arrowleft", false) => vec!["moveToBeginningOfLine"],
        ("arrowright", false) => vec!["moveToEndOfLine"],
        ("arrowleft", true) => vec!["moveToBeginningOfLineAndModifySelection"],
        ("arrowright", true) => vec!["moveToEndOfLineAndModifySelection"],
        ("backspace", false) => vec!["deleteToBeginningOfLine"],
        _ => Vec::new(),
    }
}

/// The key-down and key-up events for `spec`: a key's name or one
/// character, after any modifiers joined by `+` ("Enter", "a",
/// "Shift+Tab", "Meta+a").
pub fn press(spec: &str) -> Result<[Value; 2]> {
    let spec = spec.trim();
    // "Meta++" is the plus key.
    let (mods, key_part) = if spec == "+" {
        ("", "+")
    } else if let Some(m) = spec.strip_suffix("++") {
        (m, "+")
    } else {
        spec.rsplit_once('+').unwrap_or(("", spec))
    };
    let key_part = key_part.trim();
    if key_part.is_empty() {
        return Err(Error::Other("no key given".into()));
    }
    let mut modifiers = 0;
    for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" | "option" => ALT,
            "control" | "ctrl" => CTRL,
            "meta" | "cmd" | "command" => META,
            "shift" => SHIFT,
            other => return Err(Error::Other(format!("unknown modifier {other}: use Alt, Control, Meta or Shift"))),
        };
    }

    let (key, code, key_code, text) = if let Some(n) = named(key_part) {
        (n.0.to_string(), n.1.to_string(), n.2, n.3.to_string())
    } else {
        let mut chars = key_part.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return Err(Error::Other(format!(
                "unknown key {key_part}: name one (Enter, Tab, Escape, Backspace, ArrowDown, …) or give one character"
            )));
        };
        let upper = c.to_ascii_uppercase();
        let code = if c.is_ascii_alphabetic() {
            format!("Key{upper}")
        } else if c.is_ascii_digit() {
            format!("Digit{c}")
        } else {
            String::new()
        };
        let key = if modifiers & SHIFT != 0 { c.to_uppercase().to_string() } else { c.to_string() };
        let key_code = if c.is_ascii_alphanumeric() { upper as u32 } else { 0 };
        (key.clone(), code, key_code, key)
    };
    // A modified key types nothing: ⌘A selects, it does not type an "a".
    let text = if modifiers & (CTRL | META | ALT) != 0 { String::new() } else { text };
    let commands = mac_commands(&key, modifiers);

    let mut down = json!({
        "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": key, "code": code, "windowsVirtualKeyCode": key_code, "modifiers": modifiers,
    });
    if !text.is_empty() {
        down["text"] = json!(text);
        down["unmodifiedText"] = json!(text);
    }
    if !commands.is_empty() {
        down["commands"] = json!(commands);
    }
    let up = json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": key_code, "modifiers": modifiers });
    Ok([down, up])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_key_types_its_text_and_a_modified_one_types_none() {
        let [down, up] = press("Enter").unwrap();
        assert_eq!((down["type"].as_str(), down["text"].as_str()), (Some("keyDown"), Some("\r")));
        assert_eq!(up["type"], "keyUp");

        let [down, _] = press("Shift+Tab").unwrap();
        assert_eq!((down["key"].as_str(), down["modifiers"].as_u64()), (Some("Tab"), Some(SHIFT as u64)));

        let [down, _] = press("Meta+a").unwrap();
        assert_eq!(down["type"], "rawKeyDown");
        assert!(down.get("text").is_none());
        assert_eq!(down["commands"], json!(["selectAll"]));
    }

    #[test]
    fn one_character_is_a_key_and_shift_makes_it_upper_case() {
        let [down, _] = press("Shift+b").unwrap();
        assert_eq!((down["key"].as_str(), down["code"].as_str(), down["text"].as_str()), (Some("B"), Some("KeyB"), Some("B")));
        assert!(press("Hyper+x").is_err());
        assert!(press("NotAKey").is_err());
    }
}
