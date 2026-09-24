//! The message center's log: what the app told you, kept across restarts
//! (MSG-1…5).
//!
//! In memory first. `record` never touches the disk, because it is called
//! from inside async commands (a review queue arriving) and from the main
//! thread. A writer thread of its own puts the whole log in
//! `messages.json`, beside `config.json`, a moment after it changes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::error::Result;

/// The newest this many are kept; older ones go.
pub const CAP: usize = 200;
/// The same unread message again within this is counted on the row already
/// there: a failing refresh would otherwise fill the log with one error.
const SAME_WITHIN_MS: i64 = 10 * 60_000;
const TITLE_MAX: usize = 300;
const BODY_MAX: usize = 2000;
/// Changes in a burst — four agents finishing in one pass — are one write.
const SETTLE: Duration = Duration::from_millis(300);
const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    #[default]
    Info,
    Success,
    Error,
}

/// What the message is about, for its icon. A kind this build does not know
/// (a newer build wrote it) reads as a notice rather than losing the row.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Agent,
    Review,
    Ticket,
    Pr,
    Error,
    #[default]
    #[serde(other)]
    Notice,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: u64,
    /// Unix milliseconds of the latest time it was said.
    pub at: i64,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub level: Level,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Where a click goes (NOTE-4), as `target::Target` writes it.
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub read: bool,
    /// How many times it was said, while unread, within `SAME_WITHIN_MS`.
    #[serde(default = "one")]
    pub count: u32,
}

fn one() -> u32 {
    1
}

/// A message about to be recorded.
#[derive(Clone, Debug)]
pub struct New {
    pub kind: Kind,
    pub level: Level,
    pub title: String,
    pub body: String,
    pub target: Option<String>,
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[derive(Default)]
struct Log {
    /// Newest first.
    items: VecDeque<Message>,
    next_id: u64,
}

impl Log {
    fn record(&mut self, new: New, now: i64) -> u64 {
        let title = clip(&new.title, TITLE_MAX);
        let body = clip(&new.body, BODY_MAX);
        let same = self.items.iter().position(|m| {
            !m.read
                && now - m.at < SAME_WITHIN_MS
                && m.kind == new.kind
                && m.title == title
                && m.body == body
                && m.target == new.target
        });
        if let Some(at) = same.and_then(|i| self.items.remove(i)) {
            let mut m = at;
            m.count = m.count.saturating_add(1);
            m.at = now;
            m.level = new.level;
            let id = m.id;
            self.items.push_front(m);
            return id;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.items.push_front(Message {
            id,
            at: now,
            kind: new.kind,
            level: new.level,
            title,
            body,
            target: new.target,
            read: false,
            count: 1,
        });
        self.items.truncate(CAP);
        id
    }

    /// Mark these read, or all of them. Whether anything changed.
    fn mark_read(&mut self, ids: Option<&[u64]>) -> bool {
        let mut changed = false;
        for m in self.items.iter_mut() {
            if !m.read && ids.is_none_or(|ids| ids.contains(&m.id)) {
                m.read = true;
                changed = true;
            }
        }
        changed
    }

    fn to_json(&self) -> Result<Vec<u8>> {
        let items: Vec<&Message> = self.items.iter().collect();
        Ok(serde_json::to_vec_pretty(&serde_json::json!({
            "version": VERSION,
            "messages": items,
        }))?)
    }

    /// A log read back. `None` when the file is not a log at all; a single
    /// entry this build cannot read is dropped on its own.
    fn parse(raw: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let entries = value.get("messages")?.as_array()?;
        let mut items: VecDeque<Message> = entries
            .iter()
            .filter_map(|e| serde_json::from_value(e.clone()).ok())
            .collect();
        items.truncate(CAP);
        let next_id = items.iter().map(|m| m.id).max().unwrap_or(0);
        Some(Self { items, next_id })
    }
}

pub struct Messages {
    path: PathBuf,
    log: parking_lot::Mutex<Log>,
    /// Tells the writer the log changed.
    dirty: mpsc::Sender<()>,
    /// Held while a write is out, so two never interleave on the temp file.
    disk: parking_lot::Mutex<()>,
}

impl Messages {
    /// The log in `dir`, and the receiving end the writer thread waits on.
    /// A missing file is an empty log. One that cannot be read is kept
    /// beside it, since the next write replaces it.
    pub fn load(dir: &Path) -> (Self, mpsc::Receiver<()>) {
        let path = dir.join("messages.json");
        let log = match std::fs::read_to_string(&path) {
            Ok(raw) => Log::parse(&raw).unwrap_or_else(|| {
                let kept = path.with_extension("json.unreadable");
                let _ = std::fs::copy(&path, &kept);
                eprintln!("messages.json could not be read; starting empty, the old one is at {}", kept.display());
                Log::default()
            }),
            Err(_) => Log::default(),
        };
        let (dirty, rx) = mpsc::channel();
        let store = Self {
            path,
            log: parking_lot::Mutex::new(log),
            dirty,
            disk: parking_lot::Mutex::new(()),
        };
        (store, rx)
    }

    /// Record one, in memory, and wake the writer. The caller announces it:
    /// `record_all` does both.
    pub fn record(&self, new: New) -> u64 {
        let id = self.log.lock().record(new, chrono::Utc::now().timestamp_millis());
        let _ = self.dirty.send(());
        id
    }

    pub fn list(&self) -> Vec<Message> {
        self.log.lock().items.iter().cloned().collect()
    }

    pub fn mark_read(&self, ids: Option<&[u64]>) -> bool {
        let changed = self.log.lock().mark_read(ids);
        if changed {
            let _ = self.dirty.send(());
        }
        changed
    }

    pub fn clear(&self) -> bool {
        let mut log = self.log.lock();
        if log.items.is_empty() {
            return false;
        }
        // Ids keep rising: a click on a message already gone from the list
        // must not mark a new one that happened to get its number.
        log.items.clear();
        drop(log);
        let _ = self.dirty.send(());
        true
    }

    /// Write the whole log now. The writer thread's, and quitting's.
    pub fn flush(&self) -> Result<()> {
        let _disk = self.disk.lock();
        let bytes = self.log.lock().to_json()?;
        crate::config::write_whole(&self.path, &bytes)
    }

    #[cfg(test)]
    pub fn for_tests(path: PathBuf) -> Self {
        let (dirty, _) = mpsc::channel();
        Self { path, log: Default::default(), dirty, disk: parking_lot::Mutex::new(()) }
    }
}

/// Say the log changed, so the bell and the list follow.
pub fn changed(app: &AppHandle) {
    let _ = app.emit("messages:changed", ());
}

/// Record each, then announce once.
pub fn record_all(app: &AppHandle, news: Vec<New>) {
    if news.is_empty() {
        return;
    }
    let state = app.state::<AppState>();
    for n in news {
        state.messages.record(n);
    }
    changed(app);
}

/// Write the log whenever it changes, for as long as the app runs.
pub fn spawn_writer(app: AppHandle, dirty: mpsc::Receiver<()>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("messages".into())
        .spawn(move || {
            while dirty.recv().is_ok() {
                std::thread::sleep(SETTLE);
                while dirty.try_recv().is_ok() {}
                if let Err(e) = app.state::<AppState>().messages.flush() {
                    eprintln!("messages.json could not be written: {e}");
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(title: &str) -> New {
        New {
            kind: Kind::Error,
            level: Level::Error,
            title: title.into(),
            body: String::new(),
            target: None,
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-messages-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn the_oldest_message_goes_once_there_are_two_hundred() {
        let mut log = Log::default();
        for i in 0..CAP + 5 {
            log.record(new(&format!("m{i}")), i as i64);
        }
        assert_eq!(log.items.len(), CAP);
        assert_eq!(log.items.front().map(|m| m.title.as_str()), Some("m204"));
        assert_eq!(log.items.back().map(|m| m.title.as_str()), Some("m5"));
    }

    #[test]
    fn the_same_unread_message_again_is_counted_not_repeated() {
        let mut log = Log::default();
        let first = log.record(new("Jira is down"), 0);
        log.record(new("something else"), 1);
        let again = log.record(new("Jira is down"), 2);
        assert_eq!(first, again);
        assert_eq!(log.items.len(), 2);
        assert_eq!(log.items[0].count, 2, "and it moves to the top");
        assert_eq!(log.items[0].at, 2);
        // Long after, it is news again.
        log.record(new("Jira is down"), 2 + SAME_WITHIN_MS);
        assert_eq!(log.items.len(), 3);
    }

    #[test]
    fn the_same_message_after_it_was_read_is_news_again() {
        let mut log = Log::default();
        log.record(new("Jira is down"), 0);
        log.mark_read(None);
        log.record(new("Jira is down"), 1);
        assert_eq!(log.items.len(), 2);
        assert!(!log.items[0].read);
    }

    #[test]
    fn marking_some_read_leaves_the_rest_unread() {
        let mut log = Log::default();
        let a = log.record(new("a"), 0);
        log.record(new("b"), 1);
        assert!(log.mark_read(Some(&[a])));
        assert!(!log.mark_read(Some(&[a])), "nothing changed the second time");
        let unread: Vec<&str> = log.items.iter().filter(|m| !m.read).map(|m| m.title.as_str()).collect();
        assert_eq!(unread, ["b"]);
    }

    #[test]
    fn a_long_error_is_clipped() {
        let mut log = Log::default();
        let long = "é".repeat(BODY_MAX + 50);
        log.record(New { body: long, ..new("x") }, 0);
        assert_eq!(log.items[0].body.chars().count(), BODY_MAX + 1);
        assert!(log.items[0].body.ends_with('…'));
    }

    #[test]
    fn the_log_comes_back_after_a_restart() {
        let dir = temp_dir();
        let (store, _rx) = Messages::load(&dir);
        store.record(New { target: Some("pane:t1:p1".into()), ..new("Claude Code finished") });
        store.flush().expect("written");

        let (back, _rx) = Messages::load(&dir);
        let list = back.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Claude Code finished");
        assert_eq!(list[0].target.as_deref(), Some("pane:t1:p1"));
    }

    #[test]
    fn ids_keep_rising_after_a_restart() {
        let dir = temp_dir();
        let (store, _rx) = Messages::load(&dir);
        let first = store.record(new("a"));
        store.flush().expect("written");
        let (back, _rx) = Messages::load(&dir);
        back.clear();
        assert!(back.record(new("b")) > first);
    }

    #[test]
    fn an_unreadable_log_is_kept_aside_and_the_app_starts_empty() {
        let dir = temp_dir();
        std::fs::write(dir.join("messages.json"), "{ not json").expect("written");
        let (store, _rx) = Messages::load(&dir);
        assert!(store.list().is_empty());
        let kept = std::fs::read_to_string(dir.join("messages.json.unreadable")).expect("kept");
        assert_eq!(kept, "{ not json");
    }

    #[test]
    fn one_bad_entry_does_not_lose_the_rest() {
        let raw = r#"{"version":1,"messages":[
            {"id":2,"at":5,"kind":"agent","level":"success","title":"good","read":true},
            {"id":"three","title":7},
            {"id":1,"at":4,"kind":"from-a-newer-build","title":"odd"}
        ]}"#;
        let log = Log::parse(raw).expect("a log");
        let titles: Vec<&str> = log.items.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, ["good", "odd"]);
        assert_eq!(log.items[1].kind, Kind::Notice);
        assert_eq!(log.items[0].count, 1);
        assert_eq!(log.next_id, 2);
    }
}
