//! Registered repositories and folder scans.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::config::Project;
use crate::error::{Error, Result};
use crate::git;

use super::AppState;

// ---------------------------------------------------------------- projects

#[tauri::command]
pub fn list_projects(state: State<AppState>) -> Vec<Project> {
    state.config.read().projects
}

/// Register several repositories at once, skipping any that fail rather than
/// failing the whole batch.
#[tauri::command]
pub fn add_projects(
    state: State<AppState>,
    paths: Vec<String>,
    group: Option<String>,
) -> Result<Vec<Project>> {
    Ok(paths
        .iter()
        .filter_map(|p| register_project(&state, p, group.as_deref()).ok())
        .collect())
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
#[tauri::command]
pub fn scan_repos(
    state: State<AppState>,
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

pub(crate) fn register_project(state: &AppState, path: &str, group: Option<&str>) -> Result<Project> {
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

    let project = Project {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        path: root.clone(),
        default_branch: git::default_branch(&root_path),
        group: group.map(|g| g.trim().to_string()).filter(|g| !g.is_empty()),
    };

    state.config.update(|c| {
        // Re-adding a known repo is a no-op, except that it may now name a group.
        if let Some(existing) = c.projects.iter_mut().find(|p| p.path == root) {
            if project.group.is_some() {
                existing.group = project.group.clone();
            }
            return existing.clone();
        }
        c.projects.push(project.clone());
        project.clone()
    })
}

#[tauri::command]
pub fn remove_project(state: State<AppState>, id: String) -> Result<()> {
    state.config.update(|c| {
        c.projects.retain(|p| p.id != id);
        c.checkouts.retain(|ch| ch.project_id != id);
        // Drop tasks that have no repositories left.
        let live: Vec<String> = c.checkouts.iter().map(|ch| ch.task_id.clone()).collect();
        c.tasks.retain(|t| live.contains(&t.id));
    })
}

