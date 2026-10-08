//! Clean up's task folders (REPO-8): folders no task uses, and worktrees in
//! a task's folder that are none of its checkouts.

use std::path::{Path, PathBuf};

use crate::config::AppConfig;
use crate::git;

use super::cleanup::{Action, CleanupItem, InUse, Planned, Verdict};
use super::panes::is_generated;
use super::repos::{canon, plural};

/// A worktree that can go through git without losing anything, and the
/// repository it belongs to; or why not.
fn removable_worktree(wt: &Path) -> std::result::Result<PathBuf, String> {
    if let Some(why) = git::unlinked(wt) {
        return Err(format!("{why}. Look at it by hand."));
    }
    let owner = git::owner(wt).ok_or("git cannot tell which repository it belongs to")?;
    let st = git::status(wt).map_err(|e| e.to_string())?;
    if st.dirty_files > 0 {
        return Err(format!("{} uncommitted change{}", st.dirty_files, plural(st.dirty_files as usize)));
    }
    if git::in_progress(wt).is_some() {
        return Err("in the middle of a merge or rebase".into());
    }
    // A branch keeps its commits when its worktree goes; a detached HEAD
    // keeps them only if some branch has them.
    if st.branch == "(detached)" && !git::holds(&owner, &st.head) {
        return Err("on a commit no branch has".into());
    }
    Ok(owner)
}

/// Folders under the task folder location that no task of any build uses.
pub(super) fn leftover_folders(root: &Path, used: &InUse, out: &mut Vec<Planned>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !dir.is_dir() || dir.is_symlink() || name.starts_with('.') || name == "_chat" {
            continue;
        }
        if used.roots.contains(&canon(&dir)) {
            continue;
        }
        let mut worktrees = Vec::new();
        let mut files = 0;
        let mut problems = Vec::new();
        let mut other = Other::default();
        for (rel, path) in contents(&dir) {
            if path.join(".git").is_file() {
                if used.checkouts.contains(&canon(&path)) {
                    problems.push(format!("{rel} is a task's worktree"));
                    continue;
                }
                match removable_worktree(&path) {
                    Ok(owner) => worktrees.push((owner, path)),
                    Err(why) => problems.push(format!("{rel}: {why}")),
                }
            } else if path.is_file() && is_generated(&rel) {
                files += 1;
            } else {
                other.look(rel, &path);
            }
        }
        problems.extend(other.problem());
        let (verdict, detail) = if problems.is_empty() && other.files > 0 {
            let worktrees = if worktrees.is_empty() { "" } else { " Its worktrees have no changes, and their branches stay." };
            (
                Verdict::Risky,
                format!(
                    "No task uses it, but it holds {} the app did not make ({}), none of them git's: {}.{worktrees}",
                    other.count(),
                    size(other.bytes),
                    other.listed(),
                ),
            )
        } else if problems.is_empty() {
            let mut held = Vec::new();
            if !worktrees.is_empty() {
                held.push(format!(
                    "{} worktree{} with no changes (the branch{} stay{} in the repository)",
                    worktrees.len(),
                    plural(worktrees.len()),
                    if worktrees.len() == 1 { "" } else { "es" },
                    if worktrees.len() == 1 { "s" } else { "" },
                ));
            }
            if files > 0 {
                held.push(format!("{files} file{} the app wrote", plural(files)));
            }
            if other.seen > 0 {
                held.push("empty folders".to_string());
            }
            let held = if held.is_empty() { "nothing".to_string() } else { held.join(" and ") };
            (Verdict::Safe, format!("No task uses it. It holds {held}."))
        } else {
            (Verdict::Blocked, format!("No task uses it, but {}.", problems.join("; ")))
        };
        // What it held when the list was made: a folder that gained files
        // since is not removed on the strength of an older list.
        let id = format!("folder:{}:{}:{}", dir.display(), other.files, other.bytes);
        let everything = other.seen > 0;
        out.push(Planned {
            item: CleanupItem { id, kind: "folder", repo: None, title: name, detail, verdict },
            action: Action::Folder(dir, worktrees, everything),
        });
    }
}

/// How much of what the app did not make Clean up looks through before it
/// leaves the folder to the user.
const LOOK_AT_MOST: usize = 5000;

/// What a leftover task folder holds that the app did not make.
///
/// Refusing all of it kept folders on disk for good over a build cache
/// written after the task's worktree went, or a log an agent CLI kept
/// there: nothing anyone would miss, and nothing Clean up would ever take.
/// Such files are offered now, named, but never pre-selected. Anything git
/// would know, a repository or worktree at any depth, still keeps the
/// folder: that can be work.
#[derive(Default)]
struct Other {
    /// Everything looked at, empty folders included.
    seen: usize,
    files: usize,
    bytes: u64,
    /// The first few files, by path in the task folder.
    first: Vec<String>,
    /// A folder that is a repository or worktree, by path in the task folder.
    repo: Option<String>,
    unreadable: Option<String>,
}

impl Other {
    fn look(&mut self, rel: String, path: &Path) {
        self.seen += 1;
        if self.seen > LOOK_AT_MOST || self.repo.is_some() || self.unreadable.is_some() {
            return;
        }
        if path.file_name().is_some_and(|n| n == ".git") {
            self.repo = Some(rel.rsplit_once('/').map_or(".", |(parent, _)| parent).to_string());
            return;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            self.unreadable = Some(rel);
            return;
        };
        if !meta.is_dir() {
            // A file, or a link, which is not followed.
            self.files += 1;
            self.bytes += meta.len();
            if self.first.len() < 3 {
                self.first.push(rel);
            }
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            self.unreadable = Some(rel);
            return;
        };
        for entry in entries.flatten() {
            self.look(format!("{rel}/{}", entry.file_name().to_string_lossy()), &entry.path());
        }
    }

    /// Why the folder stays, if something in here keeps it.
    fn problem(&self) -> Option<String> {
        if let Some(at) = &self.repo {
            return Some(if at == "." {
                "it is a git repository itself".to_string()
            } else {
                format!("{at} is a git repository or worktree that no app's copy lists")
            });
        }
        if let Some(rel) = &self.unreadable {
            return Some(format!("{rel} could not be read"));
        }
        (self.seen > LOOK_AT_MOST).then(|| format!("it holds more than {LOOK_AT_MOST} things the app did not make. Look at it by hand"))
    }

    fn count(&self) -> String {
        format!("{} file{}", self.files, plural(self.files))
    }

    fn listed(&self) -> String {
        let more = self.files - self.first.len();
        let first = self.first.join(", ");
        if more == 0 { first } else { format!("{first} and {more} more") }
    }
}

fn size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} bytes")
    } else if b < K * K {
        format!("{:.0} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.1} GB", b / (K * K * K))
    }
}

/// What a task folder holds, by its path there, looking one level into the
/// folders agent CLIs keep their settings in.
fn contents(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() && matches!(name.as_str(), ".claude" | ".gemini") {
            for inner in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                out.push((format!("{name}/{}", inner.file_name().to_string_lossy()), inner.path()));
            }
        } else {
            out.push((name, path));
        }
    }
    out
}

/// Worktrees inside a task's folder that are none of its checkouts: left
/// when a repo is removed from the app (REPO-3), which keeps them on disk.
pub(super) fn stray_worktrees(cfg: &AppConfig, used: &InUse, out: &mut Vec<Planned>) {
    for task in &cfg.tasks {
        let Ok(entries) = std::fs::read_dir(&task.root) else { continue };
        for entry in entries.flatten() {
            let wt = entry.path();
            if !wt.join(".git").is_file() || used.checkouts.contains(&canon(&wt)) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let (verdict, detail, owner) = match removable_worktree(&wt) {
                Ok(owner) => (
                    Verdict::Safe,
                    "In this task's folder, but not one of its repos. No changes; its branch stays in the repository.".to_string(),
                    owner,
                ),
                Err(why) => (Verdict::Blocked, format!("In this task's folder, but not one of its repos: {why}."), PathBuf::new()),
            };
            out.push(Planned {
                item: CleanupItem {
                    id: format!("worktree:{}", wt.display()),
                    kind: "worktree",
                    repo: Some(name.clone()),
                    title: format!("{}/{name}", task.name),
                    detail,
                    verdict,
                },
                action: Action::Worktree(owner, wt),
            });
        }
    }
}
