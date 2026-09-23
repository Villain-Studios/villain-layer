//! Keeping registered repositories in shape, for the Repos view: how each
//! is doing (REPO-5), Locate (REPO-6) and Sync (REPO-7). Clean up (REPO-8)
//! is `cleanup.rs`.
//!
//! Written after a day of doing all of it by hand: a clone cloned again
//! under seven tasks, a repo moved to another folder, and task branches
//! left in clones that no longer needed them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::config::Project;
use crate::error::{Error, Result};
use crate::git::{self, Forwarded};

use super::projects::{ensure_store, walk_for_repos};
use super::AppState;

pub(super) fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// For comparing paths: the temp and home folders both have spellings that
/// differ from where they really are.
pub(super) fn canon(p: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(p.as_ref()).unwrap_or_else(|_| p.as_ref().to_path_buf())
}

/// A few repositories at a time: each is a handful of git processes, and a
/// fetch is mostly waiting on the network.
fn in_parallel<T: Send>(projects: &[Project], f: impl Fn(&Project) -> T + Sync) -> Vec<T> {
    let mut out = Vec::with_capacity(projects.len());
    std::thread::scope(|scope| {
        for chunk in projects.chunks(8) {
            let handles: Vec<_> = chunk.iter().map(|p| scope.spawn(|| f(p))).collect();
            out.extend(handles.into_iter().filter_map(|h| h.join().ok()));
        }
    });
    out
}

/// The app's copy of a project, if it is there.
pub(super) fn store_of(project: &Project) -> Option<&Path> {
    project.store.as_deref().map(Path::new).filter(|s| s.is_dir())
}

// ------------------------------------------------------------------ health

#[derive(Debug, Serialize)]
pub struct RepoHealth {
    pub project_id: String,
    /// `ok`; `missing`, nothing at its path; `not_repo`, a folder that is
    /// not a git repository; `other`, a different repository from the one
    /// the app's copy was made of.
    pub clone: &'static str,
    /// Where it fetches from.
    pub origin: Option<String>,
    /// The app's copy, when there is one.
    pub store: Option<String>,
    /// When a Sync last reached origin, in seconds since the epoch.
    pub synced_at: Option<u64>,
    /// The clone's default branch against origin's.
    pub behind: Option<usize>,
    pub ahead: Option<usize>,
    /// A folder that looks like this repository, when the clone is missing.
    pub found: Option<String>,
    /// How its history says the team updates branches, and why (UPD-7).
    /// What the repo is set to, `Project.update_by`, comes first.
    pub update_guess: Option<git::UpdateBy>,
    pub update_reason: Option<String>,
}

/// How every registered repository is doing (REPO-5).
///
/// Off the command thread: two or three git processes per repository, and
/// a missing clone walks the disk near where it was.
#[tauri::command]
pub async fn repo_health(app: AppHandle) -> Result<Vec<RepoHealth>> {
    super::blocking(app, |state| {
        let projects = state.config.read().projects;
        let registered: HashSet<PathBuf> = projects.iter().map(|p| canon(&p.path)).collect();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut rows = in_parallel(&projects, |p| health(p, &registered, home.as_deref()));
        follow_group(&projects, &mut rows);
        Ok(rows)
    })
    .await
}

fn health(project: &Project, registered: &HashSet<PathBuf>, home: Option<&Path>) -> RepoHealth {
    let clone = Path::new(&project.path);
    let store = store_of(project);
    let state = if !clone.is_dir() {
        "missing"
    } else if !clone.join(".git").exists() {
        "not_repo"
    } else if store.is_some_and(|s| !git::is_store_of(s, clone)) {
        "other"
    } else {
        "ok"
    };
    let standing = match (state, store) {
        ("ok", Some(s)) => git::standing(clone, s, &project.default_branch),
        _ => None,
    };
    let guess = store.and_then(|s| git::update_style(s, &project.default_branch));
    RepoHealth {
        project_id: project.id.clone(),
        clone: state,
        origin: store
            .and_then(git::origin_url)
            .or_else(|| if state == "ok" { git::origin_url(clone) } else { None }),
        store: store.map(|s| s.to_string_lossy().to_string()),
        synced_at: store.and_then(git::synced_at),
        behind: standing.map(|s| s.behind),
        ahead: standing.map(|s| s.ahead),
        found: match (state, home) {
            ("missing", Some(home)) => find_moved(project, store, registered, home),
            _ => None,
        },
        update_guess: guess.as_ref().map(|(by, _)| *by),
        update_reason: guess.map(|(_, why)| why),
    }
}

/// Set how Update from base updates branches in a repository (UPD-7), or
/// with None, go back to guessing from its history.
#[tauri::command]
pub fn set_project_update_by(
    state: State<AppState>,
    project_id: String,
    by: Option<git::UpdateBy>,
) -> Result<()> {
    state.config.update(|c| {
        if let Some(p) = c.projects.iter_mut().find(|p| p.id == project_id) {
            p.update_by = by;
        }
    })
}

/// A repo with too little history of its own goes the way its group does,
/// since a group is usually one team: admin, with one recent branch, is
/// guessed from web beside it. Only repos with evidence of their own count,
/// a choice or their own history, so one guess cannot spread through a
/// group by itself.
fn follow_group(projects: &[Project], rows: &mut [RepoHealth]) {
    let own: Vec<(&str, git::UpdateBy, &str)> = projects
        .iter()
        .filter_map(|p| {
            let by = p.update_by.or_else(|| rows.iter().find(|r| r.project_id == p.id)?.update_guess)?;
            Some((p.group.as_deref()?, by, p.name.as_str()))
        })
        .collect();
    for row in rows.iter_mut().filter(|r| r.update_guess.is_none()) {
        let Some(p) = projects.iter().find(|p| p.id == row.project_id) else { continue };
        let Some(group) = p.group.as_deref() else { continue };
        let peers: Vec<_> = own.iter().filter(|(g, _, name)| *g == group && *name != p.name).collect();
        let rebase = peers.iter().filter(|(_, by, _)| *by == git::UpdateBy::Rebase).count();
        let merge = peers.len() - rebase;
        if rebase == merge {
            continue;
        }
        let (by, word) = if rebase > merge { (git::UpdateBy::Rebase, "rebase") } else { (git::UpdateBy::Merge, "merge") };
        let names: Vec<&str> = peers.iter().filter(|(_, b, _)| *b == by).map(|(_, _, n)| *n).take(3).collect();
        row.update_guess = Some(by);
        row.update_reason = Some(format!("the other repos in {group} {word} ({})", names.join(", ")));
    }
}

/// A folder that is probably `project`'s clone, moved: the same folder
/// name, under the nearest folder still there above where it was, and the
/// same repository as the app's copy when there is one.
fn find_moved(
    project: &Project,
    store: Option<&Path>,
    registered: &HashSet<PathBuf>,
    home: &Path,
) -> Option<String> {
    let was = Path::new(&project.path);
    let name = was.file_name()?;
    let near = was.ancestors().skip(1).find(|d| d.is_dir())?;
    // Not the home folder or above it: three levels of that is a walk
    // through everything the user has.
    if !near.starts_with(home) || near == home {
        return None;
    }
    let mut found = Vec::new();
    walk_for_repos(near, 0, 3, &mut found);
    found
        .into_iter()
        .filter(|p| p.file_name() == Some(name) && !registered.contains(&canon(p)))
        .find(|p| store.is_none_or(|s| git::is_store_of(s, p)))
        .map(|p| p.to_string_lossy().to_string())
}

// ------------------------------------------------------------------ locate

/// Point a repository at the folder its clone is in now (REPO-6), keeping
/// it in every task. A repo whose clone was gone before copies came in has
/// none; it gets one now, in the background, as a newly added repo does.
#[tauri::command]
pub async fn locate_project(app: AppHandle, project_id: String, path: String) -> Result<Project> {
    let located = super::blocking(app.clone(), move |state| locate(state, &project_id, &path)).await?;
    let id = located.id.clone();
    std::thread::spawn(move || {
        use tauri::Manager;
        if let Err(e) = ensure_store(&app.state::<AppState>(), &id) {
            eprintln!("villain-layer: no private copy of {id} yet: {e}");
        }
    });
    Ok(located)
}

fn locate(state: &AppState, id: &str, path: &str) -> Result<Project> {
    let project = state.config.project(id)?;
    let root = git::repo_root(Path::new(path))?;
    let dir = PathBuf::from(&root);
    if let Some(store) = store_of(&project) {
        if !git::is_store_of(store, &dir) {
            return Err(Error::Other(format!(
                "{root} is a different repository: it fetches from {}, and {} from {}",
                git::origin_url(&dir).unwrap_or_else(|| "nowhere".into()),
                project.name,
                git::origin_url(store).unwrap_or_default(),
            )));
        }
        // What the user set in the clone, where they are working now.
        let _ = git::copy_local_config(&dir, store);
    }
    let default_branch = git::default_branch(&dir);
    state.config.update(|c| {
        if let Some(other) = c.projects.iter().find(|p| p.path == root && p.id != id) {
            return Err(Error::Other(format!("{root} is already registered, as {}", other.name)));
        }
        let p = c
            .projects
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::NotFound(format!("project {id}")))?;
        p.path = root.clone();
        p.default_branch = default_branch.clone();
        Ok(p.clone())
    })?
}

// -------------------------------------------------------------------- sync

#[derive(Debug, Serialize)]
pub struct Synced {
    pub project_id: String,
    pub repo: String,
    pub ok: bool,
    /// What moved, what was left alone and why.
    pub detail: String,
}

/// Sync the given repositories (REPO-7), a row each.
///
/// Off the command thread: a fetch per repository, over the network.
#[tauri::command]
pub async fn sync_repos(app: AppHandle, project_ids: Vec<String>) -> Result<Vec<Synced>> {
    super::blocking(app, move |state| {
        let projects: Vec<Project> =
            state.config.read().projects.into_iter().filter(|p| project_ids.contains(&p.id)).collect();
        Ok(in_parallel(&projects, |p| {
            let (ok, detail) = match sync(state, p) {
                Ok(d) => (true, d),
                Err(e) => (false, e.to_string()),
            };
            Synced { project_id: p.id.clone(), repo: p.name.clone(), ok, detail }
        }))
    })
    .await
}

fn sync(state: &AppState, project: &Project) -> Result<String> {
    let store = ensure_store(state, &project.id)?;
    git::fetch_store(&store)?;
    let clone = Path::new(&project.path);
    if !clone.is_dir() {
        return Ok(format!("Fetched. The clone is not at {} any more: Locate it.", project.path));
    }
    if !git::is_store_of(&store, clone) {
        return Ok(format!("Fetched. {} is a different repository now, so it was left alone.", project.path));
    }
    // Settings added to the clone since the copy was made.
    let _ = git::copy_local_config(clone, &store);
    let branch = &project.default_branch;
    Ok(match git::fast_forward(clone, &store, branch) {
        Ok(Forwarded::UpToDate) => format!("Fetched. {branch} is up to date."),
        Ok(Forwarded::Moved(n)) => format!("Fetched. {branch} moved forward {n} commit{}.", plural(n)),
        Ok(Forwarded::OwnCommits(n)) => {
            format!("Fetched. {branch} has {n} commit{} of its own, so it was left alone.", plural(n))
        }
        Ok(Forwarded::Busy) => {
            format!("Fetched. {branch} is checked out with uncommitted changes, so it was left alone.")
        }
        Ok(Forwarded::NoBranch) => format!("Fetched. The clone has no {branch} of its own."),
        Err(e) => return Err(Error::Other(format!("Fetched, but {branch} was not moved: {e}"))),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::config::{AppConfig, Checkout, ConfigStore, Task};

    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        crate::git::run_for_tests(dir, args).unwrap()
    }

    pub(crate) fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", file]);
    }

    /// A remote, and the user's clone of it registered as `api`, in a
    /// sandbox of their own.
    pub(crate) fn setup() -> (PathBuf, AppConfig) {
        let root = canon(std::env::temp_dir()).join(format!("vl-repos-{}", uuid::Uuid::new_v4()));
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "-b", "main", "--bare"]);
        let clone = root.join("code/api");
        std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
        git(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        git(&clone, &["config", "user.email", "t@villain.local"]);
        git(&clone, &["config", "user.name", "Test"]);
        commit(&clone, "a.txt");
        git(&clone, &["push", "-q", "-u", "origin", "main"]);
        let cfg = AppConfig {
            worktree_root: Some(root.join("tasks").to_string_lossy().to_string()),
            projects: vec![Project {
                id: "api".into(),
                name: "api".into(),
                path: clone.to_string_lossy().to_string(),
                default_branch: "main".into(),
                group: None,
                store: None,
                update_by: None,
            }],
            ..Default::default()
        };
        (root, cfg)
    }

    pub(crate) fn add_task(cfg: &mut AppConfig, root: &Path, name: &str) {
        let folder = root.join("tasks").join(name);
        cfg.tasks.push(Task {
            id: name.into(),
            name: name.into(),
            root: folder.to_string_lossy().to_string(),
            branch: name.into(),
            issue_key: None,
            issue_url: None,
            created_at: chrono::Utc::now(),
        });
        cfg.checkouts.push(Checkout {
            id: format!("{name}-api"),
            task_id: name.into(),
            project_id: "api".into(),
            path: folder.join("api").to_string_lossy().to_string(),
            base: "main".into(),
            base_commit: None,
            push_lease: None,
            point_before_update: None,
            last_head: None,
        });
    }

    /// Its config sits where the app's would, so a sibling folder can play
    /// the other build.
    pub(crate) fn state(root: &Path, cfg: AppConfig) -> AppState {
        let dir = root.join("support/app.test");
        std::fs::create_dir_all(&dir).unwrap();
        AppState {
            config: ConfigStore::for_tests(dir.join("config.json"), cfg),
            ptys: crate::pty::PtyManager::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
        }
    }

    #[test]
    fn a_repo_with_nothing_to_go_on_updates_the_way_its_group_does() {
        let (_root, cfg) = setup();
        let repo = |id: &str, group: Option<&str>, by: Option<git::UpdateBy>| Project {
            id: id.into(),
            name: id.into(),
            group: group.map(String::from),
            update_by: by,
            ..cfg.projects[0].clone()
        };
        let row = |id: &str, guess: Option<git::UpdateBy>| RepoHealth {
            project_id: id.into(),
            clone: "ok",
            origin: None,
            store: None,
            synced_at: None,
            behind: None,
            ahead: None,
            found: None,
            update_guess: guess,
            update_reason: guess.map(|_| "its own history".into()),
        };
        let rebase = Some(git::UpdateBy::Rebase);
        let projects = [
            repo("web", Some("frontend"), None),
            repo("admin", Some("frontend"), None),
            repo("design-system", Some("frontend"), rebase),
            repo("api", Some("backend"), None),
            repo("scratch", None, None),
        ];
        let mut rows = [
            row("web", rebase),
            row("admin", None),
            row("design-system", None),
            row("api", None),
            row("scratch", None),
        ];
        follow_group(&projects, &mut rows);
        assert_eq!(rows[1].update_guess, rebase);
        assert_eq!(rows[1].update_reason.as_deref(), Some("the other repos in frontend rebase (web, design-system)"));
        assert_eq!(rows[3].update_guess, None, "alone in its group, with nothing to go on");
        assert_eq!(rows[4].update_guess, None, "in no group");
        std::fs::remove_dir_all(&_root).ok();
    }

    #[test]
    fn a_moved_clone_is_found_and_located_without_leaving_its_tasks() {
        let (root, mut cfg) = setup();
        add_task(&mut cfg, &root, "T-1");
        let state = state(&root, cfg);
        ensure_store(&state, "api").unwrap();
        let clone = PathBuf::from(&state.config.project("api").unwrap().path);
        let moved = root.join("code/backend/api");
        std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
        std::fs::rename(&clone, &moved).unwrap();

        let none = HashSet::new();
        let seen = health(&state.config.project("api").unwrap(), &none, Some(&root));
        assert_eq!(seen.clone, "missing");
        assert_eq!(seen.found.map(canon), Some(canon(&moved)));

        // Another repository that happens to have the same name.
        let other = root.join("other.git");
        std::fs::create_dir_all(&other).unwrap();
        git(&other, &["init", "-q", "--bare"]);
        let impostor = root.join("elsewhere/api");
        git(&root, &["clone", "-q", other.to_str().unwrap(), impostor.to_str().unwrap()]);
        assert!(locate(&state, "api", impostor.to_str().unwrap()).is_err());

        let located = locate(&state, "api", moved.to_str().unwrap()).unwrap();
        assert_eq!(canon(&located.path), canon(&moved));
        assert_eq!(state.config.read().checkouts.len(), 1, "still in its task");
        assert_eq!(health(&located, &none, Some(&root)).clone, "ok");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sync_makes_a_missing_copy_and_brings_main_forward() {
        let (root, cfg) = setup();
        let clone = PathBuf::from(&cfg.projects[0].path);
        let state = state(&root, cfg);
        let mate = root.join("mate");
        let remote = root.join("remote.git");
        git(&root, &["clone", "-q", remote.to_str().unwrap(), mate.to_str().unwrap()]);
        git(&mate, &["config", "user.email", "t@villain.local"]);
        git(&mate, &["config", "user.name", "Mate"]);
        commit(&mate, "b.txt");
        git(&mate, &["push", "-q", "origin", "main"]);

        let project = state.config.project("api").unwrap();
        assert_eq!(sync(&state, &project).unwrap(), "Fetched. main moved forward 1 commit.");
        assert!(state.config.project("api").unwrap().store.is_some(), "the copy was made on the way");
        assert!(clone.join("b.txt").is_file());
        assert_eq!(sync(&state, &project).unwrap(), "Fetched. main is up to date.");
        std::fs::remove_dir_all(&root).ok();
    }
}
