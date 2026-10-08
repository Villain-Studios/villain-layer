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

/// What Claude Code records of a local command (a slash command it runs
/// itself, not the model) in the transcript, each in its own element.
const LOCAL_COMMAND_TAGS: &[&str] =
    &["command-name", "command-message", "command-args", "local-command-stdout", "local-command-stderr", "local-command-caveat"];

/// What the user wrote, out of a message a conversation replays: without
/// Claude Code's record of its local commands. `claude-agent-acp` replays
/// the transcript as it is, and the adapter runs `/model` itself as every
/// session starts, so a picked-up conversation opened with a message the
/// user never sent, run together with the first one they did.
pub(super) fn user_words(text: &str) -> String {
    let mut out = text.to_string();
    let mut cut = false;
    for tag in LOCAL_COMMAND_TAGS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        while let Some(start) = out.find(&open) {
            let Some(end) = out[start..].find(&close).map(|e| start + e + close.len()) else { break };
            out.replace_range(start..end, "");
            cut = true;
        }
    }
    // Only where something was cut: a message streamed in pieces keeps the
    // spaces between them.
    if cut { out.trim().to_string() } else { out }
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

/// Tool output without the code fence `claude-agent-acp` wraps it in. It is
/// shown as plain text already, so the fence showed as two lines of
/// backticks around every result. Fences inside it are kept.
fn unfence(text: String) -> String {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("```")
        .and_then(|rest| rest.split_once('\n'))
        .filter(|(lang, _)| !lang.contains('`') && !lang.contains(' '))
        .and_then(|(_, body)| body.strip_suffix("```"))
        .filter(|body| !body.lines().any(|l| l.trim_start().starts_with("```")));
    match inner {
        Some(body) => body.trim_end_matches('\n').to_string(),
        None => text,
    }
}

pub(super) fn tool_content(items: &[Value]) -> Vec<ToolContent> {
    items
        .iter()
        .filter_map(|c| match c.get("type").and_then(Value::as_str)? {
            "content" => {
                let text = unfence(block_text(c.get("content")));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_loses_the_fence_around_it_and_keeps_fences_inside_it() {
        assert_eq!(unfence("```\n<tool_use_error>no</tool_use_error>\n```".into()), "<tool_use_error>no</tool_use_error>");
        assert_eq!(unfence("```console\n$ ls\na b\n```\n".into()), "$ ls\na b");
        let two = "```\na\n```\ntext\n```\nb\n```";
        assert_eq!(unfence(two.into()), two);
        assert_eq!(unfence("plain".into()), "plain");
    }
}
