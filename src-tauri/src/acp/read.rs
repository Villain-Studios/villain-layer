//! Reading what an agent sends, loosely on purpose. The agents add fields of
//! their own (`_meta`, Copilot's model prices), and a newer one will send
//! kinds this does not know: an unknown one is skipped, never an error that
//! loses the rest of the message.

use serde_json::Value;

use super::entry::ToolContent;

/// The most of one tool's text output, or one side of a diff, that is kept.
/// A read of a large file comes back whole, and the window is sent each
/// entry that changed.
const MAX_TOOL_TEXT: usize = 64 * 1024;

pub(super) fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(super) fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A conversation's title as a pane's name: one line, at most 80 characters,
/// as PANE-12 cuts a CLI's own.
pub(super) fn topic(title: &str) -> String {
    title.lines().next().unwrap_or_default().trim().chars().take(80).collect()
}

/// The text of a content block: text as it is, anything else named.
pub(super) fn block_text(block: Option<&Value>) -> String {
    let Some(block) = block else { return String::new() };
    match block.get("type").and_then(Value::as_str) {
        Some("text") => str_at(block, "text").unwrap_or_default(),
        Some("resource_link") => format!("[{}]", str_at(block, "name").or_else(|| str_at(block, "uri")).unwrap_or_default()),
        Some("resource") => block
            .get("resource")
            .and_then(|r| str_at(r, "text").or_else(|| str_at(r, "uri")))
            .unwrap_or_default(),
        Some("image") => "[image]".into(),
        Some("audio") => "[audio]".into(),
        _ => String::new(),
    }
}

pub(super) fn cut(text: String) -> String {
    if text.len() <= MAX_TOOL_TEXT {
        return text;
    }
    let mut end = MAX_TOOL_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… cut: {} KB more", &text[..end], (text.len() - end) / 1024)
}

pub(super) fn tool_content(items: &[Value]) -> Vec<ToolContent> {
    items
        .iter()
        .filter_map(|c| match c.get("type").and_then(Value::as_str)? {
            "content" => {
                let text = block_text(c.get("content"));
                (!text.is_empty()).then(|| ToolContent::Text { text: cut(text) })
            }
            "diff" => Some(ToolContent::Diff {
                path: str_at(c, "path")?,
                old: str_at(c, "oldText").map(cut),
                new: cut(str_at(c, "newText").unwrap_or_default()),
            }),
            // The app does not run terminals for agents (`terminal/*` is not
            // offered), so none is ever named here.
            _ => None,
        })
        .collect()
}
