//! Clean up (REPO-8): what tasks left behind, found, judged and removed.
//!
//! Each thing found is safe (it loses nothing, and starts ticked), risky
//! (it may lose what the detail says) or blocked (not the app's to remove).
//! Nothing is removed from the list the user saw: the list is made again,
//! and what changed since is judged afresh or not found at all.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::AppHandle;

use crate::config::{AppConfig, Project};
use crate::error::{Error, Result};
use crate::git;

use super::panes::{is_generated, remove_generated};
use super::repos::{canon, plural, store_of};
use super::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Loses nothing: pre-selected.
    Safe,
    /// May lose what the detail says: offered, never pre-selected.
    Risky,
    /// Not removable here; the detail says why.
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupItem {
    /// What the item is and, for a branch, where it pointed: one that moved
    /// since the list was made no longer matches, and is not removed.
    pub id: String,
    /// `worktree`, `folder`, `clone_branch`, `store_branch`, `records` or
    /// `store`, which is also the order they are removed in.
    pub kind: &'static str,
    pub repo: Option<String>,
    pub title: String,
    pub detail: String,
    pub verdict: Verdict,
}

enum Action {
    /// The worktrees in it, each with the repository it belongs to; then
    /// the files the app made; then the folder, once empty.
    Folder(PathBuf, Vec<(PathBuf, PathBuf)>),
    Worktree(PathBuf, PathBuf),
    /// A branch in a repository, and the tip it was judged at.
    Branch(PathBuf, String, String),
    Records(PathBuf),
    Store(PathBuf),
}

struct Planned {
    item: CleanupItem,
    action: Action,
}

const ORDER: [&str; 6] = ["worktree", "folder", "clone_branch", "store_branch", "records", "store"];

#[derive(Debug, Serialize)]
pub struct Cleaned {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// What Clean up would remove, and why (REPO-8). Removes nothing.
#[tauri::command]
pub async fn cleanup_plan(app: AppHandle) -> Result<Vec<CleanupItem>> {
    super::blocking(app, |state| Ok(plan(state).into_iter().map(|p| p.item).collect())).await
}

/// Remove the items picked from the plan. Everything is judged again first:
/// what became unsafe since is refused, and what changed is not found.
#[tauri::command]
pub async fn cleanup_apply(app: AppHandle, ids: Vec<String>) -> Result<Vec<Cleaned>> {
    super::blocking(app, move |state| Ok(apply(state, &ids))).await
}

/// What some build of the app still uses.
#[derive(Default)]
struct InUse {
    roots: HashSet<PathBuf>,
    checkouts: HashSet<PathBuf>,
    stores: HashSet<PathBuf>,
    branches: HashSet<String>,
}

fn in_use(state: &AppState, cfg: &AppConfig) -> InUse {
    let mut used = InUse::default();
    for t in &cfg.tasks {
        used.roots.insert(canon(&t.root));
        used.branches.insert(t.branch.clone());
    }
    used.checkouts.extend(cfg.checkouts.iter().map(|c| canon(&c.path)));
    used.stores.extend(cfg.projects.iter().filter_map(|p| p.store.as_deref()).map(canon));
    for other in state.config.other_builds() {
        let each = |list: &str, field: &str| -> Vec<String> {
            other[list]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v[field].as_str().map(String::from))
                .collect()
        };
        used.roots.extend(each("tasks", "root").iter().map(canon));
        used.branches.extend(each("tasks", "branch"));
        used.checkouts.extend(each("checkouts", "path").iter().map(canon));
        used.stores.extend(each("projects", "store").iter().map(canon));
    }
    used
}

fn plan(state: &AppState) -> Vec<Planned> {
    let cfg = state.config.read();
    let used = in_use(state, &cfg);
    let root = state.config.worktree_root();
    let mut out = Vec::new();
    leftover_folders(&root, &used, &mut out);
    stray_worktrees(&cfg, &used, &mut out);
    for project in &cfg.projects {
        let Some(store) = store_of(project) else { continue };
        let clone = Path::new(&project.path);
        if clone.is_dir() && git::is_store_of(store, clone) {
            clone_branches(project, clone, store, &cfg, &mut out);
        }
        store_branches(project, store, &used, &mut out);
        records(project, store, &mut out);
    }
    unused_stores(&root.join(".repos"), &used, &mut out);
    out
}

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
fn leftover_folders(root: &Path, used: &InUse, out: &mut Vec<Planned>) {
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
fn stray_worktrees(cfg: &AppConfig, used: &InUse, out: &mut Vec<Planned>) {
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

/// Branches checked out in any worktree of `repo`, or everything, as far as
/// a failure to list them goes: a branch in use is never offered.
fn checked_out(repo: &Path) -> Option<HashSet<String>> {
    let mut out: HashSet<String> = git::list_worktrees(repo).ok()?.into_iter().filter_map(|w| w.branch).collect();
    // A bare copy's HEAD names a branch that no worktree lists.
    out.extend(git::current_branch(repo).ok());
    Some(out)
}

/// Task branches left in the user's clone from before worktrees came from
/// the app's copy (REPO-4).
fn clone_branches(project: &Project, clone: &Path, store: &Path, cfg: &AppConfig, out: &mut Vec<Planned>) {
    let tasks: HashSet<&str> = cfg
        .tasks
        .iter()
        .filter(|t| cfg.checkouts.iter().any(|c| c.task_id == t.id && c.project_id == project.id))
        .map(|t| t.branch.as_str())
        .collect();
    let (Ok(tips), Some(busy)) = (git::branch_tips(clone), checked_out(clone)) else { return };
    for (branch, tip) in tips.into_iter().filter(|(b, _)| tasks.contains(b.as_str())) {
        let (verdict, detail) = if busy.contains(&branch) {
            (Verdict::Blocked, "Checked out in your clone.".to_string())
        } else if git::holds(store, &tip) {
            (
                Verdict::Safe,
                "A task works on this branch in the app's copy, which has every commit on it. The task does not need this one.".to_string(),
            )
        } else {
            (Verdict::Blocked, "It has commits the app's copy does not. Push them, or delete it yourself.".to_string())
        };
        out.push(Planned {
            item: CleanupItem {
                id: format!("clone_branch:{}:{branch}:{tip}", clone.display()),
                kind: "clone_branch",
                repo: Some(project.name.clone()),
                title: branch.clone(),
                detail,
                verdict,
            },
            action: Action::Branch(clone.to_path_buf(), branch, tip),
        });
    }
}

/// Branches in the app's copy that no task of any build is on.
fn store_branches(project: &Project, store: &Path, used: &InUse, out: &mut Vec<Planned>) {
    let (Ok(tips), Some(busy)) = (git::branch_tips(store), checked_out(store)) else { return };
    let clone = Path::new(&project.path);
    for (branch, tip) in tips {
        if branch == project.default_branch || busy.contains(&branch) || used.branches.contains(&branch) {
            continue;
        }
        let (verdict, detail) = match git::only_here(store, &tip) {
            Ok(0) => (Verdict::Safe, "No task uses it, and every commit on it is on origin.".to_string()),
            // Copied from the clone when the copy was made, and still there.
            Ok(_) if clone.is_dir() && git::holds(clone, &tip) => {
                (Verdict::Safe, "No task uses it, and your clone has every commit on it.".to_string())
            }
            Ok(n) => (
                Verdict::Risky,
                format!(
                    "No task uses it, but {n} commit{} on it {} on no origin branch. A branch squash-merged and then deleted on origin looks like this too.",
                    plural(n),
                    if n == 1 { "is" } else { "are" },
                ),
            ),
            Err(e) => (Verdict::Blocked, e.to_string()),
        };
        out.push(Planned {
            item: CleanupItem {
                id: format!("store_branch:{}:{branch}:{tip}", store.display()),
                kind: "store_branch",
                repo: Some(project.name.clone()),
                title: branch.clone(),
                detail,
                verdict,
            },
            action: Action::Branch(store.to_path_buf(), branch, tip),
        });
    }
}

/// Git's records, in the app's copy, of worktrees whose folders are gone,
/// and what an interrupted move onto the copy (TASK-12) left behind.
fn records(project: &Project, store: &Path, out: &mut Vec<Planned>) {
    let Ok(wts) = git::list_worktrees(store) else { return };
    let gone = wts.iter().filter(|w| w.prunable && !w.locked).count();
    let staging = store.join("villain-adopting").is_dir();
    if gone == 0 && !staging {
        return;
    }
    let mut detail = Vec::new();
    if gone > 0 {
        detail.push(format!("Git still lists {gone} worktree{} whose folder{} gone.", plural(gone), if gone == 1 { " is" } else { "s are" }));
    }
    if staging {
        detail.push("A move onto the copy was interrupted and left its staging folder.".to_string());
    }
    out.push(Planned {
        item: CleanupItem {
            id: format!("records:{}", store.display()),
            kind: "records",
            repo: Some(project.name.clone()),
            title: "Records of deleted worktrees".into(),
            detail: detail.join(" "),
            verdict: Verdict::Safe,
        },
        action: Action::Records(store.to_path_buf()),
    });
}

/// Copies in `.repos/` that no repo of any build uses.
fn unused_stores(repos: &Path, used: &InUse, out: &mut Vec<Planned>) {
    let Ok(entries) = std::fs::read_dir(repos) else { return };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || dir.is_symlink() || used.stores.contains(&canon(&dir)) || !git::is_bare(&dir) {
            continue;
        }
        let live = git::list_worktrees(&dir)
            .map(|w| w.iter().skip(1).filter(|w| !w.prunable).count())
            .unwrap_or(usize::MAX);
        let unpushed = git::branch_tips(&dir)
            .map(|tips| tips.iter().filter(|(_, tip)| git::only_here(&dir, tip).map_or(true, |n| n > 0)).count())
            .unwrap_or(usize::MAX);
        let (verdict, detail) = if live == usize::MAX || unpushed == usize::MAX {
            (Verdict::Blocked, "Git cannot read it.".to_string())
        } else if live > 0 {
            (Verdict::Blocked, format!("No repo uses it, but {live} worktree{} still belong{} to it.", plural(live), if live == 1 { "s" } else { "" }))
        } else if unpushed > 0 {
            (
                Verdict::Risky,
                format!("No repo uses it, but {unpushed} branch{} in it {} commits on no origin branch.", if unpushed == 1 { "" } else { "es" }, if unpushed == 1 { "has" } else { "have" }),
            )
        } else {
            (Verdict::Safe, "No repo uses it, and origin has everything in it.".to_string())
        };
        out.push(Planned {
            item: CleanupItem {
                id: format!("store:{}", dir.display()),
                kind: "store",
                repo: None,
                title: entry.file_name().to_string_lossy().to_string(),
                detail,
                verdict,
            },
            action: Action::Store(dir),
        });
    }
}

fn apply(state: &AppState, ids: &[String]) -> Vec<Cleaned> {
    let planned = plan(state);
    let mut results: Vec<Cleaned> = ids
        .iter()
        .filter(|id| !planned.iter().any(|p| &p.item.id == *id))
        .map(|id| Cleaned { id: id.clone(), ok: false, detail: "Changed since the list was made. Look again.".into() })
        .collect();
    for kind in ORDER {
        for p in planned.iter().filter(|p| p.item.kind == kind && ids.contains(&p.item.id)) {
            let done = if p.item.verdict == Verdict::Blocked {
                Err(Error::Other(p.item.detail.clone()))
            } else {
                remove(&p.action)
            };
            results.push(Cleaned {
                id: p.item.id.clone(),
                ok: done.is_ok(),
                detail: done.err().map(|e| e.to_string()).unwrap_or_default(),
            });
        }
    }
    results
}

fn remove(action: &Action) -> Result<()> {
    match action {
        Action::Folder(dir, worktrees) => {
            for (owner, wt) in worktrees {
                git::remove_worktree(owner, &wt.to_string_lossy(), false)?;
            }
            remove_generated(dir);
            std::fs::remove_dir(dir)
                .map_err(|e| Error::Other(format!("{} still holds something: {e}", dir.display())))
        }
        Action::Worktree(owner, wt) => git::remove_worktree(owner, &wt.to_string_lossy(), false),
        Action::Branch(repo, branch, tip) => git::delete_branch_at(repo, branch, tip),
        Action::Records(store) => {
            let staging = store.join("villain-adopting");
            // Each an empty folder with a `.git` link, never checked out;
            // anything else in there stops the removal.
            for entry in std::fs::read_dir(&staging).into_iter().flatten().flatten() {
                let _ = std::fs::remove_file(entry.path().join(".git"));
                std::fs::remove_dir(entry.path())?;
            }
            let _ = std::fs::remove_dir(&staging);
            git::prune_worktrees(store)
        }
        Action::Store(dir) => Ok(std::fs::remove_dir_all(dir)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::projects::ensure_store;
    use crate::commands::repos::tests::{add_task, commit, git, setup, state};

    fn items(state: &AppState) -> Vec<CleanupItem> {
        plan(state).into_iter().map(|p| p.item).collect()
    }

    fn verdict(items: &[CleanupItem], kind: &str, title: &str) -> Option<Verdict> {
        items.iter().find(|i| i.kind == kind && i.title == title).map(|i| i.verdict)
    }

    fn succeeded(done: &[Cleaned], items: &[CleanupItem], kind: &str, title: &str) -> bool {
        let item = items.iter().find(|i| i.kind == kind && i.title == title).unwrap();
        done.iter().find(|d| d.id == item.id).unwrap().ok
    }

    #[test]
    fn clean_up_takes_what_deleted_tasks_left_and_nothing_that_holds_work() {
        let (root, cfg) = setup();
        let state = state(&root, cfg);
        let store = ensure_store(&state, "api").unwrap();
        let tasks = root.join("tasks");

        // Deleted by an older build: the worktree went, a settings file kept the folder.
        let old = tasks.join("OLD-1");
        std::fs::create_dir_all(old.join(".claude")).unwrap();
        std::fs::write(old.join(".claude/settings.local.json"), "{}").unwrap();
        std::fs::write(old.join("AGENTS.md"), "context").unwrap();
        std::fs::write(old.join(".DS_Store"), "").unwrap();
        // A worktree with a commit nobody pushed, and nothing uncommitted.
        let clean = tasks.join("OLD-2");
        crate::git::add_worktree(&store, &clean.join("api"), "OLD-2", "main").unwrap();
        commit(&clean.join("api"), "work.txt");
        // A worktree with a file nobody committed.
        let dirty = tasks.join("OLD-3");
        crate::git::add_worktree(&store, &dirty.join("api"), "OLD-3", "main").unwrap();
        std::fs::write(dirty.join("api/new.txt"), "uncommitted\n").unwrap();
        // A note the user left.
        let noted = tasks.join("OLD-4");
        std::fs::create_dir_all(&noted).unwrap();
        std::fs::write(noted.join("notes.txt"), "mine").unwrap();
        // The dev build's task, in the same task folder location.
        let theirs = tasks.join("DEV-1");
        crate::git::add_worktree(&store, &theirs.join("api"), "DEV-1", "main").unwrap();
        let dev = root.join("support/app.test.dev");
        std::fs::create_dir_all(&dev).unwrap();
        let dev_config = serde_json::json!({
            "tasks": [{ "root": theirs, "branch": "DEV-1" }],
            "checkouts": [{ "path": theirs.join("api") }],
        });
        std::fs::write(dev.join("config.json"), dev_config.to_string()).unwrap();

        let found = items(&state);
        assert_eq!(verdict(&found, "folder", "OLD-1"), Some(Verdict::Safe));
        assert_eq!(verdict(&found, "folder", "OLD-2"), Some(Verdict::Safe));
        assert_eq!(verdict(&found, "folder", "OLD-3"), Some(Verdict::Blocked));
        assert_eq!(verdict(&found, "folder", "OLD-4"), Some(Verdict::Blocked));
        assert!(found.iter().all(|i| !i.title.contains("DEV-1")), "the other build's task is not offered");

        let ids: Vec<String> = found.iter().map(|i| i.id.clone()).collect();
        let done = apply(&state, &ids);
        assert!(succeeded(&done, &found, "folder", "OLD-1") && succeeded(&done, &found, "folder", "OLD-2"));
        assert!(!succeeded(&done, &found, "folder", "OLD-3") && !succeeded(&done, &found, "folder", "OLD-4"));
        assert!(!old.exists() && !clean.exists());
        assert!(dirty.join("api/new.txt").is_file() && noted.join("notes.txt").is_file());
        assert!(theirs.join("api/a.txt").is_file());

        // The removed worktree's branch stayed, with its commit, and is offered
        // on its own now, but not pre-selected: origin has never seen it.
        assert_eq!(verdict(&items(&state), "store_branch", "OLD-2"), Some(Verdict::Risky));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_task_branch_left_in_the_clone_goes_only_when_the_copy_has_every_commit() {
        let (root, mut cfg) = setup();
        let clone = PathBuf::from(&cfg.projects[0].path);
        for name in ["T-1", "T-2", "T-3"] {
            add_task(&mut cfg, &root, name);
        }
        // Made in the clone by the old layout, before there was a copy.
        git(&clone, &["branch", "T-1"]);
        git(&clone, &["branch", "T-3"]);
        let state = state(&root, cfg);
        let store = ensure_store(&state, "api").unwrap();
        // Made after the copy, with a commit only the clone has.
        git(&clone, &["switch", "-q", "-c", "T-2"]);
        commit(&clone, "only-here.txt");
        // Checked out by the user.
        git(&clone, &["switch", "-q", "T-3"]);

        let found = items(&state);
        assert_eq!(verdict(&found, "clone_branch", "T-1"), Some(Verdict::Safe));
        assert_eq!(verdict(&found, "clone_branch", "T-2"), Some(Verdict::Blocked));
        assert_eq!(verdict(&found, "clone_branch", "T-3"), Some(Verdict::Blocked));
        assert_eq!(verdict(&found, "store_branch", "T-1"), None, "a task is on it");

        let ids: Vec<String> = found.iter().map(|i| i.id.clone()).collect();
        let done = apply(&state, &ids);
        assert!(succeeded(&done, &found, "clone_branch", "T-1"));
        assert!(!crate::git::branch_exists(&clone, "T-1"));
        assert!(crate::git::branch_exists(&store, "T-1"), "the task's own copy stays");
        assert!(crate::git::branch_exists(&clone, "T-2") && crate::git::branch_exists(&clone, "T-3"));
        std::fs::remove_dir_all(&root).ok();
    }
}
