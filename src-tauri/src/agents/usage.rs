//! What a CLI's own transcripts say a run used (RUN-4), for an agent in a
//! terminal, which tells the app nothing of its tokens while it runs. Only
//! Claude Code's are read: each reply it writes carries its `usage`.
//!
//! Its transcript is its own format, not a promise. A line this cannot read
//! is passed over, so a change to it shows as fewer tokens or none, never as
//! a run that could not be recorded.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{find, sessions::session_dir, Integration};
use crate::runs::Tokens;

/// The tokens of the replies Claude Code wrote between `from` and `to` in
/// these conversations, and their subagents'. None when there is nothing of
/// then to read.
///
/// A conversation is an id as `hook_session` reads it, a UUID, so it cannot
/// lead out of the transcripts' folder. The window is the run's: a resumed
/// conversation's transcript holds earlier runs' replies too.
pub(crate) fn transcript_tokens(
    agent_id: &str,
    cwd: &str,
    conversations: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<Tokens> {
    if find(agent_id)?.integration != Integration::Claude || conversations.is_empty() {
        return None;
    }
    tokens_in(&session_dir(agent_id, cwd)?, conversations, from, to)
}

/// The same, in a folder of Claude Code's transcripts.
fn tokens_in(dir: &Path, conversations: &[String], from: DateTime<Utc>, to: DateTime<Utc>) -> Option<Tokens> {
    let mut replies = Replies::default();
    for id in conversations {
        replies.read(&dir.join(format!("{id}.jsonl")), from, to);
        // A subagent (the Task tool) keeps a transcript of its own.
        let Ok(subagents) = std::fs::read_dir(dir.join(id).join("subagents")) else { continue };
        for entry in subagents.flatten() {
            if entry.path().extension().is_some_and(|e| e == "jsonl") {
                replies.read(&entry.path(), from, to);
            }
        }
    }
    replies.sum()
}

/// Each reply's usage, once. Claude Code writes a reply as a line per
/// content block, every one carrying the reply's usage: summed line by line,
/// a reply with a thought, some text and two tool calls counted four times.
#[derive(Default)]
struct Replies(HashMap<String, Tokens>);

impl Replies {
    fn read(&mut self, path: &Path, from: DateTime<Utc>, to: DateTime<Utc>) {
        let Ok(file) = std::fs::File::open(path) else { return };
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let Ok(line) = line else { break };
            // Most lines are prompts and tool output; only replies have this.
            if !line.contains("\"usage\"") {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(&line) else { continue };
            if entry.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let at = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc));
            if !at.is_some_and(|at| from <= at && at <= to) {
                continue;
            }
            let Some(usage) = entry.pointer("/message/usage") else { continue };
            let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
            let id = entry
                .pointer("/message/id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("{}:{i}", path.display()), String::from);
            // The last line of a reply is written last, with its usage complete.
            self.0.insert(
                id,
                Tokens {
                    input: n("input_tokens"),
                    output: n("output_tokens"),
                    cached_read: n("cache_read_input_tokens"),
                    cached_write: n("cache_creation_input_tokens"),
                },
            );
        }
    }

    fn sum(self) -> Option<Tokens> {
        if self.0.is_empty() {
            return None;
        }
        let mut total = Tokens::default();
        for used in self.0.into_values() {
            total.add(used);
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-usage-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn reply(id: &str, at: DateTime<Utc>, output: u64) -> String {
        json!({
            "type": "assistant",
            "timestamp": at.to_rfc3339(),
            "message": { "id": id, "role": "assistant", "usage": {
                "input_tokens": 10, "output_tokens": output,
                "cache_read_input_tokens": 300, "cache_creation_input_tokens": 4,
            }},
        })
        .to_string()
    }

    fn read(path: &Path, from: DateTime<Utc>, to: DateTime<Utc>) -> Option<Tokens> {
        let mut replies = Replies::default();
        replies.read(path, from, to);
        replies.sum()
    }

    #[test]
    fn a_reply_written_as_several_lines_is_counted_once_with_its_last_usage() {
        let dir = temp_dir();
        let now = Utc::now();
        let path = dir.join("s.jsonl");
        let lines = [
            json!({ "type": "user", "timestamp": now.to_rfc3339(), "message": { "content": "the \"usage\" of a word" } }).to_string(),
            reply("msg_1", now, 5),
            reply("msg_1", now, 20),
            reply("msg_2", now, 7),
            "{ not json, \"usage\"".to_string(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let hour = chrono::TimeDelta::hours(1);
        assert_eq!(
            read(&path, now - hour, now + hour),
            Some(Tokens { input: 20, output: 27, cached_read: 600, cached_write: 8 })
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A restarted pane resumes its conversation, whose transcript already
    /// holds the replies its earlier runs were credited with.
    #[test]
    fn only_the_replies_written_during_the_run_count() {
        let dir = temp_dir();
        let now = Utc::now();
        let path = dir.join("s.jsonl");
        let before = reply("old", now - chrono::TimeDelta::hours(2), 99);
        std::fs::write(&path, [before, reply("new", now, 1)].join("\n")).unwrap();
        let from = now - chrono::TimeDelta::minutes(5);
        assert_eq!(read(&path, from, now).map(|t| t.output), Some(1));
        assert_eq!(read(&path, now + chrono::TimeDelta::minutes(1), now + chrono::TimeDelta::minutes(2)), None);
        assert_eq!(read(&dir.join("missing.jsonl"), from, now), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `/clear` starts a new conversation in the same run, and a subagent
    /// writes a transcript of its own.
    #[test]
    fn every_conversation_of_the_run_counts_with_its_subagents() {
        let dir = temp_dir();
        let now = Utc::now();
        let (first, second) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
        std::fs::write(dir.join(format!("{first}.jsonl")), reply("a", now, 1)).unwrap();
        std::fs::write(dir.join(format!("{second}.jsonl")), reply("b", now, 2)).unwrap();
        std::fs::create_dir_all(dir.join(&second).join("subagents")).unwrap();
        std::fs::write(dir.join(&second).join("subagents/agent-1.jsonl"), reply("c", now, 4)).unwrap();
        std::fs::write(dir.join(&second).join("subagents/notes.txt"), reply("d", now, 8)).unwrap();
        let hour = chrono::TimeDelta::hours(1);
        let used = tokens_in(&dir, &[first, second], now - hour, now + hour);
        assert_eq!(used.map(|t| t.output), Some(7));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Against the newest of Claude Code's own transcripts on this Mac, over
    /// its whole life: run by hand when its format may have changed.
    #[test]
    #[ignore = "reads this Mac's own Claude Code transcripts"]
    fn a_real_claude_code_transcript_reads() {
        #[allow(deprecated)]
        let projects = std::env::home_dir().unwrap().join(".claude/projects");
        let newest = std::fs::read_dir(&projects)
            .unwrap()
            .flatten()
            .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten())
            .filter(|f| f.path().extension().is_some_and(|e| e == "jsonl"))
            .max_by_key(|f| f.metadata().and_then(|m| m.modified()).ok())
            .expect("a transcript");
        let (dir, id) = (newest.path().parent().unwrap().to_path_buf(), newest.path().file_stem().unwrap().to_string_lossy().to_string());
        let used = tokens_in(&dir, &[id], DateTime::<Utc>::MIN_UTC, Utc::now()).expect("replies with usage");
        eprintln!("{}: {used:?}", newest.path().display());
        assert!(used.output > 0 && used.input + used.cached_read + used.cached_write > 0);
    }

    #[test]
    fn only_claude_codes_transcripts_are_read() {
        let now = Utc::now();
        let ids = vec![uuid::Uuid::new_v4().to_string()];
        assert_eq!(transcript_tokens("copilot", "/tmp", &ids, now, now), None);
        assert_eq!(transcript_tokens("nobody", "/tmp", &ids, now, now), None);
        assert_eq!(transcript_tokens("claude", "/tmp", &[], now, now), None);
    }
}
