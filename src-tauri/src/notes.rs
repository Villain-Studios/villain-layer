//! Repo notes (NOTE-1…7): what agents learned about a repository that holds
//! beyond one task, kept for the next task on it.
//!
//! Every task started cold, so each agent relearned the same repository:
//! how its tests run, where things live, what bit the one before. The
//! app is the one thing that outlives a task, so it keeps what they learned.
//!
//! A note that is wrong is worse than none, since it is read as
//! instructions. So each one carries the commit it was last checked at and
//! the paths it is about, and whoever reads it is told what changed since.
//!
//! In `notes.json`, beside `config.json` and never in it. Notes change a
//! few times a week, always from blocking work, so each change is written
//! whole straight away rather than by a writer thread as `messages.rs` does.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A repository with more than this many notes has stopped curating them.
pub const PER_REPO: usize = 50;
/// In characters. A note is a fact, not a document.
pub const NOTE_MAX: usize = 500;
const PATHS_MAX: usize = 20;
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: String,
    /// The repository, as `git::remote_key` spells where it fetches from.
    pub repo: String,
    pub text: String,
    /// Files or folders it is about, relative to the repository's root.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Unix milliseconds.
    pub written_at: i64,
    /// Where it came from: "Claude Code in ACME-12 Refunds…", or "you".
    #[serde(default)]
    pub source: String,
    /// The default branch's commit when it was written.
    #[serde(default)]
    pub written_commit: Option<String>,
    /// When someone last said it still holds. Writing and editing count.
    pub checked_at: i64,
    #[serde(default)]
    pub checked_commit: Option<String>,
}

/// A note about to be written.
#[derive(Clone, Debug, Default)]
pub struct New {
    pub repo: String,
    pub text: String,
    pub paths: Vec<String>,
    pub source: String,
    pub commit: Option<String>,
}

/// The note's text as it will be kept, or why it will not be.
fn clean_text(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Other("a note needs some text".into()));
    }
    let n = text.chars().count();
    if n > NOTE_MAX {
        return Err(Error::Other(format!(
            "a note is at most {NOTE_MAX} characters and this one is {n}; keep it to the one fact"
        )));
    }
    Ok(text.to_string())
}

/// Paths as git will be given them: relative, inside the repository, one
/// spelling each.
fn clean_paths(paths: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        let p = raw.trim().trim_start_matches("./").trim_end_matches('/');
        if p.is_empty() {
            continue;
        }
        if p.starts_with('/') || p.starts_with('-') || p.split('/').any(|seg| seg == "..") {
            return Err(Error::Other(format!(
                "{raw:?} is not a path inside the repository; give it relative to the repository's root"
            )));
        }
        if !out.iter().any(|q| q == p) {
            out.push(p.to_string());
        }
    }
    if out.len() > PATHS_MAX {
        return Err(Error::Other(format!("a note is about at most {PATHS_MAX} paths")));
    }
    Ok(out)
}

#[derive(Default, Debug)]
struct Book {
    notes: Vec<Note>,
}

impl Book {
    fn add(&mut self, new: New, now: i64) -> Result<Note> {
        let text = clean_text(&new.text)?;
        let paths = clean_paths(&new.paths)?;
        if self.notes.iter().filter(|n| n.repo == new.repo).count() >= PER_REPO {
            return Err(Error::Other(format!(
                "this repository already has {PER_REPO} notes; forget one that no longer \
                 holds before remembering another"
            )));
        }
        let note = Note {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            repo: new.repo,
            text,
            paths,
            written_at: now,
            source: new.source,
            written_commit: new.commit.clone(),
            checked_at: now,
            checked_commit: new.commit,
        };
        self.notes.push(note.clone());
        Ok(note)
    }

    fn find(&mut self, id: &str) -> Result<&mut Note> {
        self.notes
            .iter_mut()
            .find(|n| n.id == id)
            .ok_or_else(|| Error::NotFound(format!("note {id}")))
    }

    fn check(&mut self, id: &str, commit: Option<String>, now: i64) -> Result<Note> {
        let note = self.find(id)?;
        note.checked_at = now;
        note.checked_commit = commit.or(note.checked_commit.take());
        Ok(note.clone())
    }

    fn forget(&mut self, id: &str) -> Result<Note> {
        let at = self
            .notes
            .iter()
            .position(|n| n.id == id)
            .ok_or_else(|| Error::NotFound(format!("note {id}")))?;
        Ok(self.notes.remove(at))
    }

    /// A repository's notes, the most recently checked first.
    fn of(&self, repo: &str) -> Vec<Note> {
        let mut out: Vec<Note> = self.notes.iter().filter(|n| n.repo == repo).cloned().collect();
        out.sort_by_key(|n| std::cmp::Reverse(n.checked_at));
        out
    }

    fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec_pretty(&serde_json::json!({
            "version": VERSION,
            "notes": self.notes,
        }))?)
    }

    /// `None` when the file is not a notebook at all; a single note this
    /// build cannot read is dropped on its own.
    fn parse(raw: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let notes = value
            .get("notes")?
            .as_array()?
            .iter()
            .filter_map(|n| serde_json::from_value(n.clone()).ok())
            .collect();
        Some(Self { notes })
    }
}

pub struct Notes {
    path: PathBuf,
    book: parking_lot::Mutex<Book>,
}

impl Notes {
    /// The notebook in `dir`. A missing file is an empty one. One that
    /// cannot be read is kept beside it, since the next write replaces it.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("notes.json");
        let book = match std::fs::read_to_string(&path) {
            Ok(raw) => Book::parse(&raw).unwrap_or_else(|| {
                let kept = path.with_extension("json.unreadable");
                let _ = std::fs::copy(&path, &kept);
                eprintln!("notes.json could not be read; starting empty, the old one is at {}", kept.display());
                Book::default()
            }),
            Err(_) => Book::default(),
        };
        Self { path, book: parking_lot::Mutex::new(book) }
    }

    pub fn of(&self, repo: &str) -> Vec<Note> {
        self.book.lock().of(repo)
    }

    pub fn get(&self, id: &str) -> Result<Note> {
        self.book.lock().find(id).map(|n| n.clone())
    }

    pub fn add(&self, new: New) -> Result<Note> {
        self.change(|book, now| book.add(new, now))
    }

    pub fn check(&self, id: &str, commit: Option<String>) -> Result<Note> {
        self.change(|book, now| book.check(id, commit, now))
    }

    pub fn forget(&self, id: &str) -> Result<Note> {
        self.change(|book, _| book.forget(id))
    }

    /// Make a change and write the whole notebook, under one lock so two
    /// changes never write over each other. Blocking: called only from
    /// `commands::blocking`.
    fn change<T>(&self, f: impl FnOnce(&mut Book, i64) -> Result<T>) -> Result<T> {
        let mut book = self.book.lock();
        let out = f(&mut book, chrono::Utc::now().timestamp_millis())?;
        crate::config::write_whole(&self.path, &book.to_json()?)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(repo: &str, text: &str) -> New {
        New { repo: repo.into(), text: text.into(), ..Default::default() }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-notes-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn a_repository_past_its_cap_is_told_to_forget_one_rather_than_losing_the_oldest() {
        let mut book = Book::default();
        for i in 0..PER_REPO {
            book.add(new("github.com/acme/api", &format!("fact {i}")), i as i64).expect("under the cap");
        }
        let refused = book.add(new("github.com/acme/api", "one more"), 99);
        assert!(refused.is_err_and(|e| e.to_string().contains("forget one")));
        assert_eq!(book.of("github.com/acme/api").len(), PER_REPO);
        assert!(book.add(new("github.com/acme/web", "another repo"), 99).is_ok());
    }

    #[test]
    fn a_long_note_is_refused_not_cut() {
        let mut book = Book::default();
        let long = "é".repeat(NOTE_MAX + 1);
        assert!(book.add(new("r", &long), 0).is_err());
        assert!(book.add(new("r", &"é".repeat(NOTE_MAX)), 0).is_ok());
    }

    #[test]
    fn paths_stay_inside_the_repository() {
        let mut book = Book::default();
        for bad in ["/etc/passwd", "../other", "src/../../x", "--output=x"] {
            let n = New { paths: vec![bad.into()], ..new("r", "t") };
            assert!(book.add(n, 0).is_err(), "{bad} was accepted");
        }
        let n = New { paths: vec!["./db/".into(), "db".into(), " ".into()], ..new("r", "t") };
        assert_eq!(book.add(n, 0).expect("fine").paths, ["db"]);
    }

    #[test]
    fn checking_a_note_moves_it_to_the_top_at_the_new_commit() {
        let mut book = Book::default();
        let old = book.add(New { commit: Some("aaa".into()), ..new("r", "old") }, 1).expect("added");
        book.add(new("r", "newer"), 2).expect("added");
        assert_eq!(book.of("r")[0].text, "newer");

        let checked = book.check(&old.id, Some("bbb".into()), 3).expect("checked");
        assert_eq!(checked.checked_commit.as_deref(), Some("bbb"));
        assert_eq!(checked.written_commit.as_deref(), Some("aaa"), "where it came from stays");
        assert_eq!(book.of("r")[0].text, "old");
    }

    #[test]
    fn a_check_with_no_commit_to_give_keeps_the_last_one() {
        let mut book = Book::default();
        let n = book.add(New { commit: Some("aaa".into()), ..new("r", "t") }, 1).expect("added");
        assert_eq!(book.check(&n.id, None, 2).expect("checked").checked_commit.as_deref(), Some("aaa"));
    }

    #[test]
    fn notes_come_back_after_a_restart_and_forgetting_one_sticks() {
        let dir = temp_dir();
        let notes = Notes::load(&dir);
        let keep = notes.add(new("github.com/acme/api", "tests need `make db` first")).expect("added");
        let gone = notes.add(new("github.com/acme/api", "no longer true")).expect("added");
        notes.forget(&gone.id).expect("forgotten");

        let back = Notes::load(&dir);
        let list = back.of("github.com/acme/api");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], keep);
        assert!(back.get(&gone.id).is_err());
    }

    #[test]
    fn an_unreadable_notebook_is_kept_aside_and_the_app_starts_empty() {
        let dir = temp_dir();
        std::fs::write(dir.join("notes.json"), "{ not json").expect("written");
        let notes = Notes::load(&dir);
        assert!(notes.of("r").is_empty());
        let kept = std::fs::read_to_string(dir.join("notes.json.unreadable")).expect("kept");
        assert_eq!(kept, "{ not json");
    }
}
