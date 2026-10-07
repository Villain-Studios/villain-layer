//! What the user does in the Browser panel, as Chrome's input events
//! (BRW-9): the panel sends what the window's own events said, and this
//! says it again in the protocol's terms.

use serde::Deserialize;
use serde_json::{json, Value};

use super::keys;

/// One event from the panel. Coordinates are the page's CSS pixels.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserInput {
    Mouse {
        /// `mousePressed`, `mouseReleased` or `mouseMoved`.
        r#type: String,
        x: f64,
        y: f64,
        /// `left`, `right`, `middle` or `none`.
        button: String,
        /// Which buttons are held, as `MouseEvent.buttons` has it.
        buttons: u32,
        click_count: u32,
        modifiers: u32,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        modifiers: u32,
    },
    Key {
        /// `keyDown` or `keyUp`.
        r#type: String,
        key: String,
        code: String,
        key_code: u32,
        /// What the key types, if anything.
        #[serde(default)]
        text: Option<String>,
        modifiers: u32,
        #[serde(default)]
        location: u32,
    },
    /// Text pasted, or typed by an input method.
    Text { text: String },
}

const MOUSE: &[&str] = &["mousePressed", "mouseReleased", "mouseMoved"];
const BUTTONS: &[&str] = &["left", "right", "middle", "none"];

/// The protocol method and its parameters, or None for an event that is
/// not one the panel sends.
pub fn to_cdp(input: &BrowserInput) -> Option<(&'static str, Value)> {
    Some(match input {
        BrowserInput::Mouse { r#type, x, y, button, buttons, click_count, modifiers } => {
            if !MOUSE.contains(&r#type.as_str()) || !BUTTONS.contains(&button.as_str()) {
                return None;
            }
            (
                "Input.dispatchMouseEvent",
                json!({ "type": r#type, "x": x, "y": y, "button": button, "buttons": buttons,
                        "clickCount": click_count, "modifiers": modifiers }),
            )
        }
        BrowserInput::Wheel { x, y, dx, dy, modifiers } => (
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy, "modifiers": modifiers }),
        ),
        BrowserInput::Key { r#type, key, code, key_code, text, modifiers, location } => {
            let down = match r#type.as_str() {
                "keyDown" => true,
                "keyUp" => false,
                _ => return None,
            };
            let text = text.as_deref().filter(|t| !t.is_empty() && down);
            let mut p = json!({
                // A key that types nothing is a raw key-down: as a keyDown,
                // Chrome would type its key name.
                "type": if !down { "keyUp" } else if text.is_some() { "keyDown" } else { "rawKeyDown" },
                "key": key, "code": code, "windowsVirtualKeyCode": key_code,
                "modifiers": modifiers, "location": location,
            });
            if let Some(t) = text {
                p["text"] = json!(t);
                p["unmodifiedText"] = json!(t);
            }
            let commands = if down { keys::mac_commands(key, *modifiers) } else { Vec::new() };
            if !commands.is_empty() {
                p["commands"] = json!(commands);
            }
            ("Input.dispatchKeyEvent", p)
        }
        BrowserInput::Text { text } => ("Input.insertText", json!({ "text": text })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: &str, key: &str, text: Option<&str>, modifiers: u32) -> BrowserInput {
        BrowserInput::Key {
            r#type: t.into(),
            key: key.into(),
            code: String::new(),
            key_code: 0,
            text: text.map(str::to_string),
            modifiers,
            location: 0,
        }
    }

    #[test]
    fn a_key_that_types_is_a_key_down_with_its_text_and_one_that_does_not_is_raw() {
        let (_, p) = to_cdp(&key("keyDown", "a", Some("a"), 0)).unwrap();
        assert_eq!((p["type"].as_str(), p["text"].as_str()), (Some("keyDown"), Some("a")));
        let (_, p) = to_cdp(&key("keyDown", "ArrowLeft", None, 0)).unwrap();
        assert_eq!(p["type"], "rawKeyDown");
        let (_, p) = to_cdp(&key("keyUp", "a", Some("a"), 0)).unwrap();
        assert!(p.get("text").is_none(), "only the down types");
    }

    #[test]
    fn command_a_in_the_panel_selects_all_in_the_page() {
        let (_, p) = to_cdp(&key("keyDown", "a", None, keys::META)).unwrap();
        assert_eq!(p["commands"], json!(["selectAll"]));
    }

    #[test]
    fn an_event_the_panel_never_sends_is_dropped() {
        assert!(to_cdp(&key("char", "a", None, 0)).is_none());
        let m = BrowserInput::Mouse {
            r#type: "mouseWheel".into(), x: 0.0, y: 0.0, button: "left".into(), buttons: 0, click_count: 1, modifiers: 0,
        };
        assert!(to_cdp(&m).is_none());
    }
}
