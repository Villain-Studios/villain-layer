//! An ACP agent's conversation as the pane keeps it: what `session/update`
//! notifications add up to, entry by entry, and a plain-text copy of it for
//! everything that reads a pane as text (ACP-7). Read loosely (`read.rs`).

use std::collections::HashMap;

use serde_json::Value;

use super::entry::{Body, Changes, Choice, Command, Entry, Setting, SettingOption, Step, Usage};
use super::read::{block_text, str_at, tool_content, topic, user_words, value_text};

/// The most entries a pane keeps. The oldest go first; the text copy in the
/// scrollback has its own, separate limit.
pub(super) const MAX_ENTRIES: usize = 2000;

/// What changed about the session itself, beyond its entries.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    /// The conversation's title (ACP-8).
    pub title: Option<String>,
}

#[derive(Debug, Default)]
pub struct Conversation {
    entries: std::collections::VecDeque<Entry>,
    /// The index of `entries[0]`.
    base: usize,
    rev: u64,
    /// Where each tool call's entry is, by its id.
    tools: HashMap<String, usize>,
    /// This turn's plan, which each `plan` update replaces whole.
    plan: Option<usize>,
    pub settings: Vec<Setting>,
    pub commands: Vec<Command>,
    pub usage: Option<Usage>,
    /// Tool calls that finished, and those that failed, for the run log
    /// (RUN-7).
    pub calls: crate::runs::ToolCalls,
    /// The text copy written so far, waiting to be taken into the scrollback.
    text: String,
    /// What the text copy last wrote, so a new block starts on a new line.
    printing: Printing,
}

#[derive(Debug, Default, PartialEq, Clone, Copy)]
enum Printing {
    #[default]
    Nothing,
    /// Inside the agent's reply, maybe mid-line.
    Reply,
    /// A whole line, ended.
    Line,
}

impl Conversation {
    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn changes(&self, since: Option<u64>) -> Changes {
        // Entries let go from the front the window drops itself, by `base`.
        // Only a revision it cannot have seen means starting again.
        let reset = since.is_none_or(|s| s > self.rev);
        let entries = self
            .entries
            .iter()
            .filter(|e| reset || since.is_none_or(|s| e.rev > s))
            .cloned()
            .collect();
        Changes { rev: self.rev, base: self.base, len: self.base + self.entries.len(), reset, entries }
    }

    /// The text copy written since the last call (ACP-7).
    pub fn take_text(&mut self) -> String {
        std::mem::take(&mut self.text)
    }

    fn bump(&mut self) -> u64 {
        self.rev += 1;
        self.rev
    }

    fn push(&mut self, body: Body) -> usize {
        let rev = self.bump();
        let index = self.base + self.entries.len();
        self.entries.push_back(Entry { index, rev, body });
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
            self.base += 1;
        }
        index
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Entry> {
        let at = index.checked_sub(self.base)?;
        let rev = self.rev + 1;
        let entry = self.entries.get_mut(at)?;
        entry.rev = rev;
        self.rev = rev;
        Some(entry)
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index.checked_sub(self.base)?)
    }

    fn last_mut(&mut self) -> Option<&mut Body> {
        self.entries.back_mut().map(|e| &mut e.body)
    }

    /// Mark the last entry changed, after adding to it in place.
    fn touch_last(&mut self) {
        let rev = self.bump();
        if let Some(e) = self.entries.back_mut() {
            e.rev = rev;
        }
    }

    // ------------------------------------------------------------ text copy

    fn line(&mut self, text: &str) {
        if self.printing == Printing::Reply {
            self.text.push_str("\r\n");
        }
        if self.printing != Printing::Nothing {
            self.text.push_str("\r\n");
        }
        self.text.push_str(&text.replace('\n', "\r\n"));
        self.text.push_str("\r\n");
        self.printing = Printing::Line;
    }

    fn reply(&mut self, chunk: &str) {
        if self.printing == Printing::Line {
            self.text.push_str("\r\n");
        }
        self.text.push_str(&chunk.replace('\n', "\r\n"));
        self.printing = Printing::Reply;
    }

    // -------------------------------------------------------- what happens

    /// The user's prompt, sent now or waiting its turn.
    pub fn user(&mut self, text: &str, queued: bool) -> usize {
        self.plan = None;
        let quoted: Vec<String> = text.lines().map(|l| format!("› {l}")).collect();
        self.line(&quoted.join("\n"));
        self.push(Body::User { text: text.to_string(), queued, dropped: false })
    }

    /// A prompt that was waiting is sent, or will never be.
    pub fn dequeue(&mut self, index: usize, dropped: bool) {
        if let Some(Entry { body: Body::User { queued, dropped: d, .. }, .. }) = self.get_mut(index) {
            *queued = false;
            *d = dropped;
        }
    }

    pub fn note(&mut self, text: &str, error: bool) -> usize {
        self.line(&format!("{} {text}", if error { "!" } else { "·" }));
        self.push(Body::Note { text: text.to_string(), error })
    }

    /// The turn ended, for `reason`: said only when it is not the usual end.
    pub fn turn_ended(&mut self, reason: &str) {
        let said = match reason {
            "end_turn" => None,
            "cancelled" => Some("Stopped."),
            "max_tokens" => Some("Stopped: the reply reached the model's length limit."),
            "max_turn_requests" => Some("Stopped: the turn made as many model requests as it may."),
            "refusal" => Some("The agent refused to go on."),
            _ => None,
        };
        match said {
            Some(text) => {
                self.note(text, reason != "cancelled");
            }
            None if self.printing == Printing::Reply => {
                self.text.push_str("\r\n");
                self.printing = Printing::Line;
            }
            None => {}
        }
    }

    /// A permission question (ACP-5): the tool call it is about, and the
    /// options. Returns the question's entry.
    pub fn question(&mut self, tool_call: &Value, options: &Value) -> usize {
        self.tool(tool_call);
        let title = tool_call
            .get("toolCallId")
            .and_then(Value::as_str)
            .and_then(|id| self.tools.get(id))
            .and_then(|&i| self.entry(i))
            .and_then(|e| match &e.body {
                Body::Tool { title, .. } => Some(title.clone()),
                _ => None,
            })
            .or_else(|| str_at(tool_call, "title"))
            .unwrap_or_else(|| "a tool call".into());
        let options: Vec<Choice> = options
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|o| {
                        Some(Choice {
                            id: str_at(o, "optionId")?,
                            name: str_at(o, "name").unwrap_or_default(),
                            kind: str_at(o, "kind").unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Numbered for a phone's 1, 2, 3 keys (ACP-10).
        let listed: Vec<String> = options.iter().enumerate().map(|(i, o)| format!("{}) {}", i + 1, o.name)).collect();
        self.line(&format!("? {title}\n  {}", listed.join("  ")));
        self.push(Body::Question { title, options, answer: None })
    }

    pub fn answered(&mut self, index: usize, answer: &str) {
        let name = match self.entry(index).map(|e| &e.body) {
            Some(Body::Question { options, .. }) => {
                options.iter().find(|o| o.id == answer).map(|o| o.name.clone()).unwrap_or_else(|| answer.to_string())
            }
            _ => return,
        };
        if let Some(Entry { body: Body::Question { answer: a, .. }, .. }) = self.get_mut(index) {
            *a = Some(answer.to_string());
        }
        self.line(&format!("  → {name}"));
    }

    /// The session's settings, from `configOptions` wherever they come.
    pub fn set_settings(&mut self, options: &Value) {
        let Some(all) = options.as_array() else { return };
        self.settings = all
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str).is_none_or(|t| t == "select"))
            .filter_map(|o| {
                Some(Setting {
                    id: str_at(o, "id")?,
                    name: str_at(o, "name").unwrap_or_default(),
                    category: str_at(o, "category"),
                    current: o.get("currentValue").map(value_text).unwrap_or_default(),
                    options: o
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|opts| {
                            opts.iter()
                                .filter_map(|v| {
                                    Some(SettingOption {
                                        value: v.get("value").map(value_text)?,
                                        name: str_at(v, "name").unwrap_or_default(),
                                        description: str_at(v, "description"),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
            })
            .collect();
        self.bump();
    }

    /// One `session/update` notification's `update`.
    pub fn apply(&mut self, update: &Value) -> Applied {
        let mut applied = Applied::default();
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            return applied;
        };
        match kind {
            "agent_message_chunk" => {
                let text = block_text(update.get("content"));
                if text.is_empty() {
                    return applied;
                }
                self.reply(&text);
                if let Some(Body::Agent { text: t }) = self.last_mut() {
                    t.push_str(&text);
                    self.touch_last();
                } else {
                    self.push(Body::Agent { text });
                }
            }
            // Not in the text copy: a handoff wants what was done and said,
            // not the thinking between.
            "agent_thought_chunk" => {
                let text = block_text(update.get("content"));
                if let Some(Body::Thought { text: t }) = self.last_mut() {
                    t.push_str(&text);
                    self.touch_last();
                } else if !text.is_empty() {
                    self.push(Body::Thought { text });
                }
            }
            // A loaded conversation replays what the user said (ACP-9).
            "user_message_chunk" => {
                let text = user_words(&block_text(update.get("content")));
                if text.trim().is_empty() {
                    return applied;
                }
                if let Some(Body::User { text: t, .. }) = self.last_mut() {
                    t.push_str(&text);
                    self.touch_last();
                } else {
                    self.user(&text, false);
                }
            }
            "tool_call" | "tool_call_update" => self.tool(update),
            "plan" => {
                let steps: Vec<Step> = update
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|all| {
                        all.iter()
                            .map(|s| Step {
                                content: str_at(s, "content").unwrap_or_default(),
                                status: str_at(s, "status").unwrap_or_else(|| "pending".into()),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let done = steps.iter().filter(|s| s.status == "completed").count();
                let total = steps.len();
                match self.plan {
                    Some(at) if self.entry(at).is_some() => {
                        if let Some(Entry { body: Body::Plan { steps: s }, .. }) = self.get_mut(at) {
                            *s = steps;
                        }
                    }
                    _ => {
                        let listed: Vec<String> = steps.iter().map(|s| format!("  - {}", s.content)).collect();
                        self.line(&format!("Plan:\n{}", listed.join("\n")));
                        self.plan = Some(self.push(Body::Plan { steps }));
                    }
                }
                if total > 0 && done == total {
                    self.line(&format!("Plan done ({total} steps)."));
                }
            }
            "available_commands_update" => {
                self.commands = update
                    .get("availableCommands")
                    .and_then(Value::as_array)
                    .map(|all| {
                        all.iter()
                            .filter_map(|c| {
                                Some(Command {
                                    name: str_at(c, "name")?,
                                    description: str_at(c, "description").unwrap_or_default(),
                                    hint: c.get("input").and_then(|i| str_at(i, "hint")).filter(|h| !h.is_empty()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.bump();
            }
            "config_option_update" => {
                if let Some(options) = update.get("configOptions") {
                    self.set_settings(options);
                }
            }
            // Older agents report a mode change on its own; it is the setting
            // in the mode category.
            "current_mode_update" => {
                if let Some(mode) = str_at(update, "currentModeId") {
                    for s in self.settings.iter_mut().filter(|s| s.category.as_deref() == Some("mode")) {
                        s.current = mode.clone();
                    }
                    self.bump();
                }
            }
            "session_info_update" => {
                applied.title = str_at(update, "title").map(|t| topic(&t)).filter(|t| !t.is_empty());
            }
            "usage_update" => {
                let n = |k: &str| update.get(k).and_then(Value::as_u64);
                if let (Some(used), Some(size)) = (n("used"), n("size")) {
                    let cost = update.get("cost").and_then(|c| {
                        let amount = c.get("amount").and_then(Value::as_f64)?;
                        Some(format!("{amount:.2} {}", str_at(c, "currency").unwrap_or_default()))
                    });
                    self.usage = Some(Usage { used, size, cost });
                    self.bump();
                }
            }
            _ => {}
        }
        applied
    }

    /// A tool call, new or changed: only the fields it carries change.
    fn tool(&mut self, update: &Value) {
        let Some(id) = str_at(update, "toolCallId") else { return };
        let title = str_at(update, "title");
        let tool = str_at(update, "kind");
        let status = str_at(update, "status");
        let content = update.get("content").and_then(Value::as_array).map(|c| tool_content(c));
        let locations = update.get("locations").and_then(Value::as_array).map(|all| {
            all.iter()
                .filter_map(|l| {
                    let path = str_at(l, "path")?;
                    Some(match l.get("line").and_then(Value::as_u64) {
                        Some(line) => format!("{path}:{line}"),
                        None => path,
                    })
                })
                .collect::<Vec<_>>()
        });

        let done = |s: &str| s == "completed" || s == "failed";
        let known = self.tools.get(&id).copied().filter(|&i| self.entry(i).is_some());
        let Some(at) = known else {
            if let Some(status) = status.as_deref().filter(|s| done(s)) {
                self.calls.finished(status == "failed");
            }
            let title = title.unwrap_or_else(|| "Tool call".into());
            self.line(&format!("● {title}"));
            let at = self.push(Body::Tool {
                id: id.clone(),
                title,
                tool: tool.unwrap_or_else(|| "other".into()),
                status: status.unwrap_or_else(|| "pending".into()),
                content: content.unwrap_or_default(),
                locations: locations.unwrap_or_default(),
            });
            self.tools.insert(id, at);
            return;
        };
        let mut failed = None;
        let mut finished = None;
        if let Some(Entry { body: Body::Tool { title: t, tool: k, status: s, content: c, locations: l, .. }, .. }) =
            self.get_mut(at)
        {
            if let Some(title) = title {
                *t = title;
            }
            if let Some(tool) = tool {
                *k = tool;
            }
            if let Some(status) = status {
                if status == "failed" && *s != "failed" {
                    failed = Some(t.clone());
                }
                if done(&status) && !done(s) {
                    finished = Some(status == "failed");
                }
                *s = status;
            }
            if let Some(content) = content {
                *c = content;
            }
            if let Some(locations) = locations {
                *l = locations;
            }
        }
        if let Some(failed) = finished {
            self.calls.finished(failed);
        }
        if let Some(title) = failed {
            self.line(&format!("  ✗ {title} failed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::entry::ToolContent;
    use serde_json::json;

    fn chunk(kind: &str, text: &str) -> Value {
        json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
    }

    #[test]
    fn a_streamed_reply_is_one_entry_and_one_paragraph_of_text() {
        let mut c = Conversation::default();
        c.user("hello", false);
        c.apply(&chunk("agent_message_chunk", "Hi "));
        c.apply(&chunk("agent_message_chunk", "there."));
        c.turn_ended("end_turn");
        let all = c.changes(None).entries;
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].body, Body::Agent { text: "Hi there.".into() });
        assert_eq!(c.take_text(), "› hello\r\n\r\nHi there.\r\n");
    }

    #[test]
    fn a_tool_call_changes_in_place_and_keeps_what_an_update_leaves_out() {
        let mut c = Conversation::default();
        c.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Write", "kind": "edit", "status": "pending" }));
        c.apply(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "title": "Write hello.txt",
            "content": [{ "type": "diff", "path": "/w/hello.txt", "oldText": null, "newText": "hi" }] }));
        c.apply(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" }));
        let all = c.changes(None).entries;
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].body,
            Body::Tool {
                id: "t1".into(),
                title: "Write hello.txt".into(),
                tool: "edit".into(),
                status: "completed".into(),
                content: vec![ToolContent::Diff { path: "/w/hello.txt".into(), old: None, new: "hi".into() }],
                locations: vec![],
            }
        );
    }

    #[test]
    fn the_window_is_sent_only_what_changed_since_it_last_looked() {
        let mut c = Conversation::default();
        c.user("one", false);
        c.apply(&chunk("agent_message_chunk", "a"));
        let seen = c.rev();
        c.apply(&chunk("agent_message_chunk", "b"));
        c.user("two", false);
        let changes = c.changes(Some(seen));
        assert!(!changes.reset);
        let got: Vec<usize> = changes.entries.iter().map(|e| e.index).collect();
        assert_eq!(got, vec![1, 2]);
        assert_eq!(changes.len, 3);
    }

    #[test]
    fn a_question_names_its_tool_and_numbers_its_options_for_a_phone() {
        let mut c = Conversation::default();
        let at = c.question(
            &json!({ "toolCallId": "t9", "title": "Write hello.txt", "kind": "edit" }),
            &json!([
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
            ]),
        );
        let text = c.take_text();
        assert!(text.contains("? Write hello.txt"), "{text}");
        assert!(text.contains("1) Allow  2) Reject"), "{text}");
        c.answered(at, "reject");
        match &c.entry(at).unwrap().body {
            Body::Question { answer, .. } => assert_eq!(answer.as_deref(), Some("reject")),
            other => panic!("{other:?}"),
        }
        assert!(c.take_text().contains("→ Reject"));
    }

    #[test]
    fn the_oldest_entries_go_first_and_indexes_stay_put() {
        let mut c = Conversation::default();
        for i in 0..MAX_ENTRIES + 5 {
            c.note(&format!("n{i}"), false);
        }
        let all = c.changes(None);
        assert_eq!(all.base, 5);
        assert_eq!(all.entries.len(), MAX_ENTRIES);
        assert_eq!(all.entries[0].index, 5);
        assert!(c.entry(4).is_none());
    }

    /// What `claude-agent-acp` 0.22.2 replayed on `session/load`, chunk by
    /// chunk: Claude Code's record of the `/model` command the adapter itself
    /// runs, its output, then what the user wrote. All three ran together
    /// into one message from the user.
    #[test]
    fn a_replay_shows_what_the_user_wrote_not_the_agents_record_of_its_own_commands() {
        let mut c = Conversation::default();
        c.apply(&chunk("user_message_chunk", "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args>default</command-args>"));
        c.apply(&chunk("user_message_chunk", "<local-command-stdout>Set model to claude-opus-4-6[1m]</local-command-stdout>"));
        c.apply(&chunk("user_message_chunk", "Say hi in one word."));
        c.apply(&chunk("agent_message_chunk", "Hi!"));
        let all = c.changes(None).entries;
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(all[0].body, Body::User { text: "Say hi in one word.".into(), queued: false, dropped: false });
        assert!(!c.take_text().contains("command"));
        // A message replayed in pieces is still one, spaces and all.
        c.apply(&chunk("user_message_chunk", "Now say "));
        c.apply(&chunk("user_message_chunk", "bye."));
        assert_eq!(c.changes(None).entries[2].body, Body::User { text: "Now say bye.".into(), queued: false, dropped: false });
    }

    #[test]
    fn a_title_is_one_line_of_at_most_eighty_characters() {
        let mut c = Conversation::default();
        let long = format!("{}\nsecond line", "x".repeat(100));
        let applied = c.apply(&json!({ "sessionUpdate": "session_info_update", "title": long }));
        assert_eq!(applied.title.map(|t| t.len()), Some(80));
    }

    #[test]
    fn an_update_of_a_kind_not_known_is_skipped() {
        let mut c = Conversation::default();
        let before = c.rev();
        c.apply(&json!({ "sessionUpdate": "something_new", "x": 1 }));
        c.apply(&json!({ "no": "kind" }));
        assert_eq!(c.rev(), before);
    }

    #[test]
    fn settings_and_usage_are_read_from_what_the_agents_send() {
        let mut c = Conversation::default();
        c.set_settings(&json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "build",
              "options": [{ "value": "build", "name": "build" }, { "value": "plan", "name": "plan", "description": "No edits." }] },
            { "id": "flag", "name": "A toggle", "type": "boolean", "currentValue": true }
        ]));
        assert_eq!(c.settings.len(), 1);
        assert_eq!(c.settings[0].options[1].description.as_deref(), Some("No edits."));
        c.apply(&json!({ "sessionUpdate": "current_mode_update", "currentModeId": "plan" }));
        assert_eq!(c.settings[0].current, "plan");
        c.apply(&json!({ "sessionUpdate": "usage_update", "used": 40680, "size": 200000, "cost": { "amount": 0.0757, "currency": "USD" } }));
        assert_eq!(c.usage, Some(Usage { used: 40680, size: 200000, cost: Some("0.08 USD".into()) }));
    }

    /// An agent repeats a call's status as it updates its content: a call is
    /// counted when it first finishes, and one that arrives finished too.
    #[test]
    fn a_tool_call_counts_once_when_it_finishes_and_a_failure_as_one() {
        let mut c = Conversation::default();
        let call = |c: &mut Conversation, id: &str, status: &str| {
            c.apply(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": id, "status": status }));
        };
        c.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "a", "title": "Read", "status": "pending" }));
        call(&mut c, "a", "in_progress");
        call(&mut c, "a", "completed");
        call(&mut c, "a", "completed");
        c.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "b", "title": "Run tests", "status": "in_progress" }));
        call(&mut c, "b", "failed");
        c.apply(&json!({ "sessionUpdate": "tool_call", "toolCallId": "c", "title": "Grep", "status": "completed" }));
        assert_eq!(c.calls, crate::runs::ToolCalls { calls: 3, failed: 1 });
    }
}
