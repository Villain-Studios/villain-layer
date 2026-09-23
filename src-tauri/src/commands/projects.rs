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
        git::remote_branches(&project.repo())
    })
    .await
}

/// Register several repositories at once, skipping any that fail rather than
/// failing the whole batch.
///
/// Off the command thread, and one config write rather than one per repo: a
/// folder of twenty clones meant twenty full serialise-and-rename cycles on
/// top of three git calls each.
///
/// Each gets the app's own copy of its repository afterwards, in the
/// background: hard-linked, but still a clone per repo, and a scanned
/// folder of twenty should not hold the dialog open for it. A task started
/// before its copy is made makes it then.
#[tauri::command]
pub async fn add_projects(
    app: AppHandle,
    paths: Vec<String>,
    group: Option<String>,
) -> Result<Vec<Project>> {
    let added = super::blocking(app.clone(), move |state| {
        let group = group.as_deref();
        let described: Vec<Project> = paths
            .iter()
            .filter_map(|p| describe_project(p, group).ok())
            .collect();
        state.config.update(|c| {
            described
                .into_iter()
                .map(|project| merge_project(c, project))
                .collect::<Vec<_>>()
        })
    })
    .await?;
    let ids: Vec<String> = added.iter().filter(|p| p.store.is_none()).map(|p| p.id.clone()).collect();
    std::thread::spawn(move || {
        use tauri::Manager;
        let state = app.state::<AppState>();
        for id in ids {
            if let Err(e) = ensure_store(&state, &id) {
                eprintln!("villain-layer: no private copy of {id} yet: {e}");
            }
        }
    });
    Ok(added)
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
        store: None,
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

/// Off the command thread: a task this leaves with nothing has its agents
/// stopped, and stopping waits for them.
#[tauri::command]
pub async fn remove_project(app: tauri::AppHandle, id: String) -> Result<()> {
    super::blocking(app, move |state| remove_project_inner(state, &id)).await
}

pub(crate) fn remove_project_inner(state: &AppState, id: &str) -> Result<()> {
    let (gone, dropped): (Vec<String>, Vec<String>) = state.config.update(|c| {
        c.projects.retain(|p| p.id != id);
        let gone: Vec<String> = c
            .checkouts
            .iter()
            .filter(|ch| ch.project_id == id)
            .map(|ch| ch.id.clone())
            .collect();
        // The tasks this removal leaves with nothing, and only those. Taking
        // every task with no repositories also took ones that were already
        // empty — a task whose last repo was removed to be replaced — which
        // vanished when some unrelated repository was forgotten.
        let touched: std::collections::HashSet<String> = c
            .checkouts
            .iter()
            .filter(|ch| ch.project_id == id)
            .map(|ch| ch.task_id.clone())
            .collect();
        c.checkouts.retain(|ch| ch.project_id != id);
        let live: std::collections::HashSet<&str> =
            c.checkouts.iter().map(|ch| ch.task_id.as_str()).collect();
        let dropped: Vec<String> = c
            .tasks
            .iter()
            .filter(|t| touched.contains(&t.id) && !live.contains(t.id.as_str()))
            .map(|t| t.id.clone())
            .collect();
        c.tasks.retain(|t| !dropped.contains(&t.id));
        (gone, dropped)
    })?;
    state.status_cache.lock().retain(|cid, _| !gone.contains(cid));
    // A task that is gone has nowhere to show its terminals. Its agents ran
    // on out of sight, holding places under the pane cap. The folders stay,
    // as the removal promises; only the processes the app started go.
    for task in &dropped {
        let closed = state.ptys.close_task(task);
        let _ = state.config.update(|c| c.saved_panes.retain(|p| !closed.contains(&p.id)));
    }
    Ok(())
}


// ------------------------------------------------------ the app's own copies

/// One copy made at a time: a task started while the copy its repo is
/// getting in the background would otherwise race it to the same folder.
static MAKING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The app's copy of a project's repository, made now if it is not there.
/// It lives in `.repos/` under the task folder location. A folder there that
/// is already a copy of the same repository, made by the other build or
/// before the repo was last removed, is taken over rather than duplicated.
pub(crate) fn ensure_store(state: &AppState, project_id: &str) -> Result<PathBuf> {
    let _one = MAKING.lock();
    let project = state.config.project(project_id)?;
    if let Some(store) = project.store.as_deref().filter(|s| Path::new(s).is_dir()) {
        return Ok(PathBuf::from(store));
    }
    let source = PathBuf::from(&project.path);
    if !source.is_dir() {
        return Err(Error::NotFound(format!(
            "{} is gone, and the app has no copy of it to work from",
            project.path
        )));
    }
    let root = state.config.worktree_root().join(".repos");
    let mut store = root.join(format!("{}.git", project.name));
    let mut n = 2;
    while store.exists() && !git::is_store_of(&store, &source) {
        store = root.join(format!("{}-{n}.git", project.name));
        n += 1;
    }
    if !store.exists() {
        git::create_store(&source, &store)?;
    }
    let path = store.to_string_lossy().to_string();
    state.config.update(|c| {
        if let Some(p) = c.projects.iter_mut().find(|p| p.id == project_id) {
            p.store = Some(path.clone());
        }
    })?;
    Ok(store)
}

/// Where new worktrees for `project` come from: its copy, made if need be.
/// Falls back to the user's clone, as before, when no copy can be made —
/// a task that cannot start is worse than one that shares a clone.
pub(crate) fn repo_for(state: &AppState, project: &Project) -> PathBuf {
    ensure_store(state, &project.id).unwrap_or_else(|e| {
        eprintln!("villain-layer: using {} directly: {e}", project.path);
        PathBuf::from(&project.path)
    })
}

/// The repository an existing worktree is registered in, which may not be
/// the project's copy yet: one that could not be moved still belongs to the
/// user's clone, and git only removes a worktree through its own repository.
pub(crate) fn owner_of(project: &Project, worktree: &str) -> PathBuf {
    git::owner(Path::new(worktree)).unwrap_or_else(|| project.repo())
}

/// Move every task's worktrees onto the app's copies, making the copies as
/// needed; a folder whose link is gone is linked back at its last seen
/// commit where the copy has it. Returns how many moved, and what could
/// not be. Run before any pane comes back, so no agent is working in a
/// folder while its link is swapped; after the first launch there is
/// nothing to do but check.
pub fn adopt_worktrees(state: &AppState) -> (usize, Vec<String>) {
    let cfg = state.config.read();
    let mut moved = 0;
    let mut problems = Vec::new();
    for project in cfg.projects.iter().filter(|p| cfg.checkouts.iter().any(|c| c.project_id == p.id)) {
        let store = match ensure_store(state, &project.id) {
            Ok(s) => s,
            Err(e) => {
                problems.push(format!("{}: {e}", project.name));
                continue;
            }
        };
        for checkout in cfg.checkouts.iter().filter(|c| c.project_id == project.id) {
            let wt = Path::new(&checkout.path);
            if !wt.is_dir() || git::belongs_to(wt, &store) {
                continue;
            }
            let task = cfg.tasks.iter().find(|t| t.id == checkout.task_id);
            let result = match (git::unlinked(wt), task, checkout.last_head.as_deref()) {
                (None, _, _) => git::adopt_worktree(&store, wt),
                (Some(_), Some(task), Some(head)) => git::relink_worktree(&store, wt, &task.branch, head),
                (Some(why), _, _) => Err(Error::Git(why)),
            };
            match result {
                Ok(()) => {
                    moved += 1;
                    state.status_cache.lock().remove(&checkout.id);
                }
                Err(e) => problems.push(format!(
                    "{} in {}: {e}",
                    project.name,
                    task.map(|t| t.name.as_str()).unwrap_or("a task"),
                )),
            }
        }
    }
    (moved, problems)
}

/// Copies for the repositories no task uses yet, so the first task on one
/// does not wait for it. After the panes are back: nothing needs these now.
pub fn make_missing_stores(state: &AppState) {
    let ids: Vec<String> = state
        .config
        .read()
        .projects
        .iter()
        .filter(|p| p.store.as_deref().is_none_or(|s| !Path::new(s).is_dir()))
        .map(|p| p.id.clone())
        .collect();
    for id in ids {
        if let Err(e) = ensure_store(state, &id) {
            eprintln!("villain-layer: no private copy of {id}: {e}");
        }
    }
}
