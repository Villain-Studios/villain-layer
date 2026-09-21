//! Registered repositories and folder scans.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::config::Project;
use crate::error::{Error, Result};
use crate::git;

use super::AppState;

// ---------------------------------------------------------------- projects

#[tauri::command]
pub fn list_projects(state: State<AppState>) -> Vec<Project> {
    state.config.read().projects
}

/// Branches a new worktree in this repository could be cut from.
///
/// Same shape as `checkout_branches` — remote-tracking names without the
/// `origin/` prefix — so the start-work picker can offer them before any
/// worktree exists yet.
/// Off the command thread: the create-task and start-work dialogs ask for
/// every picked repository at once, and on the main thread those "parallel"
/// calls simply queued behind each other.
#[tauri::command]
pub async fn project_branches(app: AppHandle, project_id: String) -> Result<Vec<String>> {
    super::blocking(app, move |state| {
        let project = state.config.project(&project_id)?;
        git::remote_branches(&PathBuf::from(&project.path))
    })
    .await
}

/// Register several repositories at once, skipping any that fail rather than
/// failing the whole batch.
///
/// Off the command thread, and one config write rather than one per repo: a
/// folder of twenty clones meant twenty full serialise-and-rename cycles on
/// top of three git calls each.
#[tauri::command]
pub async fn add_projects(
    app: AppHandle,
    paths: Vec<String>,
    group: Option<String>,
) -> Result<Vec<Project>> {
    super::blocking(app, move |state| {
        let group = group.as_deref();
        let described: Vec<Project> = paths
            .iter()
            .filter_map(|p| describe_project(p, group).ok())
            .collect();
        state.config.update(|c| {
            described
                .into_iter()
                .map(|project| merge_project(c, project))
                .collect()
        })
    })
    .await
}

#[tauri::command]
pub fn set_project_group(
    state: State<AppState>,
    project_ids: Vec<String>,
    group: Option<String>,
) -> Result<()> {
    let group = group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty());
    state.config.update(|c| {
        for p in c.projects.iter_mut().filter(|p| project_ids.contains(&p.id)) {
            p.group = group.clone();
        }
    })
}

#[derive(Debug, Serialize)]
pub struct FoundRepo {
    pub path: String,
    pub name: String,
    pub branch: String,
    /// Already in the sidebar, so the picker can grey it out.
    pub registered: bool,
}

/// Directories that are never worth descending into when hunting for repos.
pub(crate) const SKIP: &[&str] = &[
    "node_modules", "target", "dist", "build", "vendor", "Pods",
    "DerivedData", "__pycache__", "Library", "Applications",
];

pub(crate) fn walk_for_repos(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= 500 {
        return;
    }
    let git = dir.join(".git");
    if git.is_dir() {
        out.push(dir.to_path_buf());
        // A repo's own subdirectories are not separate projects.
        return;
    }
    if git.is_file() {
        // `.git` as a file means a linked worktree or a submodule — including
        // the worktrees this app creates. The real repository is registered
        // separately; descending further would only find more of the same.
        return;
    }
    if depth >= max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || path.is_symlink() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || SKIP.contains(&name) {
            continue;
        }
        walk_for_repos(&path, depth + 1, max_depth, out);
    }
}

/// Every git repository under `root`, so a folder of repos can be added in one go.
///
/// Off the command thread: this walks the disk three levels deep and then runs
/// `git rev-parse` in every repository it found.
#[tauri::command]
pub async fn scan_repos(
    app: AppHandle,
    root: String,
    max_depth: Option<usize>,
) -> Result<Vec<FoundRepo>> {
    super::blocking(app, move |state| scan_repos_inner(state, root, max_depth)).await
}

fn scan_repos_inner(
    state: &AppState,
    root: String,
    max_depth: Option<usize>,
) -> Result<Vec<FoundRepo>> {
    let dir = PathBuf::from(&root);
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("{root} is not a directory")));
    }

    let mut found = Vec::new();
    walk_for_repos(&dir, 0, max_depth.unwrap_or(3), &mut found);

    let known: Vec<String> = state.config.read().projects.iter().map(|p| p.path.clone()).collect();

    let mut repos: Vec<FoundRepo> = found
        .into_iter()
        .map(|p| {
            let path = p.to_string_lossy().to_string();
            FoundRepo {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone()),
                branch: git::current_branch(&p).unwrap_or_default(),
                registered: known.contains(&path),
                path,
            }
        })
        .collect();

    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(repos)
}

/// What a path on disk would become as a project. Git only — no config lock
/// held, so a batch can ask git about every path before it writes once.
fn describe_project(path: &str, group: Option<&str>) -> Result<Project> {
    let dir = PathBuf::from(path);
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("{path} is not a directory")));
    }
    let root = git::repo_root(&dir)?;
    let root_path = PathBuf::from(&root);

    let name = root_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.clone());

    Ok(Project {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        path: root,
        default_branch: git::default_branch(&root_path),
        group: group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty()),
    })
}

/// Fold one described project into the config. Re-adding a known repo is a
/// no-op, except that it may now name a group.
fn merge_project(c: &mut crate::config::AppConfig, project: Project) -> Project {
    if let Some(existing) = c.projects.iter_mut().find(|p| p.path == project.path) {
        if project.group.is_some() {
            existing.group = project.group.clone();
        }
        return existing.clone();
    }
    c.projects.push(project.clone());
    project
}

#[tauri::command]
pub fn remove_project(state: State<AppState>, id: String) -> Result<()> {
    let gone: Vec<String> = state.config.update(|c| {
        c.projects.retain(|p| p.id != id);
        let gone: Vec<String> = c
            .checkouts
            .iter()
            .filter(|ch| ch.project_id == id)
            .map(|ch| ch.id.clone())
            .collect();
        c.checkouts.retain(|ch| ch.project_id != id);
        // Drop tasks that have no repositories left.
        let live: Vec<String> = c.checkouts.iter().map(|ch| ch.task_id.clone()).collect();
        c.tasks.retain(|t| live.contains(&t.id));
        gone
    })?;
    state.status_cache.lock().retain(|cid, _| !gone.contains(cid));
    Ok(())
}

