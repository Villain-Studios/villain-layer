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
                problems.push(format!("{rel} is not something the app made"));
            }
        }
        let (verdict, detail) = if problems.is_empty() {
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
            let held = if held.is_empty() { "nothing".to_string() } else { held.join(" and ") };
            (Verdict::Safe, format!("No task uses it. It holds {held}."))
        } else {
            (Verdict::Blocked, format!("No task uses it, but {}.", problems.join("; ")))
        };
        out.push(Planned {
            item: CleanupItem {
                id: format!("folder:{}", dir.display()),
                kind: "folder",
                repo: None,
                title: name,
                detail,
                verdict,
            },
            action: Action::Folder(dir, worktrees),
        });
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
