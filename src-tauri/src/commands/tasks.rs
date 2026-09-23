//! Task lifecycle: create, suggest repos, add/remove checkouts.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::config::{Checkout, ConfigStore, Project, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::pty::PaneKind;

use super::AppState;
use super::diff::RepoResult;
use super::panes::{agent_file_dir, remove_generated, write_task_context};

// ------------------------------------------------------------------- tasks

#[derive(Debug, Serialize)]
pub struct CheckoutView {
    #[serde(flatten)]
    pub checkout: Checkout,
    pub project_name: String,
    pub status: Option<git::WorktreeStatus>,
    pub exists: bool,
    /// The folder is there but git cannot read it, and why. Without this a
    /// worktree cut off from its repository showed as clean and empty.
    pub broken: Option<String>,
    /// Files with uncommitted changes — what the Diff tab lists by default.
    /// The status counts split the same work by staged, unstaged and
    /// untracked; this is the number of files across all three.
    pub changed: u32,
}

#[derive(Debug, Serialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    pub checkouts: Vec<CheckoutView>,
    pub pane_count: usize,
}

pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').chars().take(48).collect()
}

/// The Jira project a key belongs to: "ACME-123" -> "ACME". Used to remember which
/// repos a team's tickets usually touch.
pub(crate) fn issue_project(issue_key: Option<&str>) -> String {
    issue_key
        .and_then(|k| k.split_once('-').map(|(p, _)| p.to_string()))
        .unwrap_or_default()
}

/// How long a cold checkout may keep its last status before we ask git again.
const COLD_STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// How many worktrees to ask git about at once.
///
/// One `git status` after another is the whole cost of a cold `list_tasks`,
/// and at launch nothing is cached — so it is every worktree the user has ever
/// opened, in series, before the sidebar can draw. They are independent
/// processes mostly waiting on the disk, so they overlap well. The cap is
/// there because each one is a real fork: somebody with forty worktrees
/// should not start forty at once.
const STATUS_FANOUT: usize = 8;

/// What `git status` says about one worktree, that there is no worktree, or
/// why git cannot read the one that is there.
#[derive(Clone, Default)]
pub(super) struct Fresh {
    pub status: Option<git::WorktreeStatus>,
    pub changed: u32,
    pub exists: bool,
    pub broken: Option<String>,
}

fn fresh_status(dir: &Path) -> Fresh {
    if !dir.is_dir() {
        return Fresh::default();
    }
    if let Some(why) = git::unlinked(dir) {
        return Fresh { exists: true, broken: Some(why), ..Fresh::default() };
    }
    match git::status(dir) {
        // One `git status` already enumerated every dirty path. A second
        // `diff`/`ls-files` pass per checkout on every poll is what made the
        // app feel busy just for sitting open.
        Ok(s) => Fresh { changed: s.dirty_files, status: Some(s), exists: true, broken: None },
        Err(e) => Fresh { exists: true, broken: Some(e.to_string()), ..Fresh::default() },
    }
}

/// `fresh_status` for several worktrees, in the same order as the input.
///
/// Order matters more than it looks: the results are matched back to their
/// checkouts by position, so a worker that panics contributes placeholders
/// rather than a shorter list that would shift every status after it onto the
/// wrong repository.
pub(super) fn fresh_statuses(dirs: &[PathBuf]) -> Vec<Fresh> {
    if dirs.len() < 2 {
        return dirs.iter().map(|d| fresh_status(d)).collect();
    }
    let per = dirs.len().div_ceil(dirs.len().min(STATUS_FANOUT));
    std::thread::scope(|scope| {
        let handles: Vec<_> = dirs
            .chunks(per)
            .map(|chunk| {
                (
                    chunk.len(),
                    scope.spawn(move || chunk.iter().map(|d| fresh_status(d)).collect::<Vec<_>>()),
                )
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|(len, h)| h.join().unwrap_or_else(|_| vec![Fresh::default(); len]))
            .collect()
    })
}

/// Every task, with each checkout's git status.
///
/// Off the command thread: the sidebar polls this every few seconds and a
/// focused task re-runs `git status` in each of its worktrees, which is the
/// one blocking call in the app that happens whether or not anyone asked.
#[tauri::command]
pub async fn list_tasks(app: AppHandle, focus: Option<String>) -> Result<Vec<TaskView>> {
    super::blocking(app, move |state| Ok(list_tasks_inner(state, focus))).await
}

pub(crate) fn list_tasks_inner(state: &AppState, focus: Option<String>) -> Vec<TaskView> {
    let panes = state.ptys.list(None);
    let running: std::collections::HashSet<&str> = panes
        .iter()
        .filter(|p| p.running)
        .map(|p| p.task_id.as_str())
        .collect();
    let mut pane_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for p in &panes {
        *pane_counts.entry(p.task_id.as_str()).or_insert(0) += 1;
    }

    let cfg = state.config.read();
    // One pass over the projects rather than a lookup per checkout, which took
    // the config lock and cloned the whole project each time.
    let names: std::collections::HashMap<&str, &str> = cfg
        .projects
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    // Decide what each checkout needs before asking git anything, so the ones
    // that do need it can be asked together.
    let mut views: Vec<Vec<CheckoutView>> = Vec::with_capacity(cfg.tasks.len());
    let mut cold: Vec<(usize, usize, PathBuf)> = Vec::new();
    {
        let cache = state.status_cache.lock();
        for (ti, task) in cfg.tasks.iter().enumerate() {
            // Selected task and anything with a live agent stay fresh. The rest
            // reuse the cache so a poll is not N git-status processes forever.
            let hot = focus.as_deref() == Some(task.id.as_str())
                || running.contains(task.id.as_str());
            let mut row = Vec::new();
            for checkout in cfg.checkouts.iter().filter(|c| c.task_id == task.id) {
                let project_name = names
                    .get(checkout.project_id.as_str())
                    .map(|n| (*n).to_string())
                    .unwrap_or_else(|| "(unknown repo)".into());
                let hit = (!hot)
                    .then(|| cache.get(&checkout.id))
                    .flatten()
                    .filter(|h| h.at.elapsed() < COLD_STATUS_TTL);
                match hit {
                    Some(hit) => row.push(CheckoutView {
                        project_name,
                        status: hit.status.clone(),
                        changed: hit.changed,
                        exists: hit.exists,
                        broken: hit.broken.clone(),
                        checkout: checkout.clone(),
                    }),
                    None => {
                        cold.push((ti, row.len(), PathBuf::from(&checkout.path)));
                        // Filled in below. The placeholder is never returned:
                        // every slot pushed here gets a `cold` entry pointing
                        // at it.
                        row.push(CheckoutView {
                            project_name,
                            status: None,
                            changed: 0,
                            exists: false,
                            broken: None,
                            checkout: checkout.clone(),
                        });
                    }
                }
            }
            views.push(row);
        }
    }

    let dirs: Vec<PathBuf> = cold.iter().map(|(_, _, d)| d.clone()).collect();
    let statuses = fresh_statuses(&dirs);

    // Where each worktree was last seen, when that moved: what a folder cut
    // off from its repository is linked back at (`adopt_worktrees`).
    let mut seen_heads: std::collections::HashMap<String, String> = Default::default();
    let mut cache = state.status_cache.lock();
    for ((ti, ci, _), fresh) in cold.into_iter().zip(statuses) {
        let view = &mut views[ti][ci];
        if let Some(head) = fresh.status.as_ref().map(|s| &s.head).filter(|h| !h.is_empty()) {
            if view.checkout.last_head.as_ref() != Some(head) {
                seen_heads.insert(view.checkout.id.clone(), head.clone());
            }
        }
        cache.insert(
            view.checkout.id.clone(),
            super::CachedStatus {
                status: fresh.status.clone(),
                changed: fresh.changed,
                exists: fresh.exists,
                broken: fresh.broken.clone(),
                at: std::time::Instant::now(),
            },
        );
        view.status = fresh.status;
        view.changed = fresh.changed;
        view.exists = fresh.exists;
        view.broken = fresh.broken;
    }
    drop(cache);
    if !seen_heads.is_empty() {
        let _ = state.config.update(|c| {
            for ch in c.checkouts.iter_mut() {
                if let Some(head) = seen_heads.remove(&ch.id) {
                    ch.last_head = Some(head);
                }
            }
        });
    }

    cfg.tasks
        .iter()
        .zip(views)
        .map(|(t, checkouts)| TaskView {
            checkouts,
            pane_count: pane_counts.get(t.id.as_str()).copied().unwrap_or(0),
            task: t.clone(),
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct RepoSuggestion {
    pub project_ids: Vec<String>,
    /// Why these were preselected. Shown in the picker so it never feels like
    /// magic the user cannot audit or override.
    pub reason: Option<String>,
}

/// Which repositories to preselect for a ticket, in descending confidence:
/// what the last ticket in the same epic used, then the same Jira project,
/// then whatever was used last.
#[tauri::command]
pub fn suggest_repos(
    state: State<AppState>,
    issue_key: Option<String>,
    epic_key: Option<String>,
) -> RepoSuggestion {
    suggest_from(&state.config.read(), issue_key.as_deref(), epic_key.as_deref())
}

pub(crate) fn suggest_from(
    cfg: &crate::config::AppConfig,
    issue_key: Option<&str>,
    epic_key: Option<&str>,
) -> RepoSuggestion {
    let known = |ids: Vec<String>| -> Vec<String> {
        ids.into_iter()
            .filter(|id| cfg.projects.iter().any(|p| &p.id == id))
            .collect()
    };

    // 1. The same epic is a tighter signal than the same project.
    if let Some(epic) = epic_key.filter(|e| !e.is_empty()) {
        let ids = known(cfg.last_repos.get(&format!("epic:{epic}")).cloned().unwrap_or_default());
        if !ids.is_empty() {
            return RepoSuggestion {
                project_ids: ids,
                reason: Some(format!("last task under {epic}")),
            };
        }
    }

    // 2. The same Jira project. Bare keys are what v1 wrote.
    let project = issue_project(issue_key);
    if !project.is_empty() {
        for key in [format!("project:{project}"), project.clone()] {
            let ids = known(cfg.last_repos.get(&key).cloned().unwrap_or_default());
            if !ids.is_empty() {
                return RepoSuggestion {
                    project_ids: ids,
                    reason: Some(format!("last {project} ticket")),
                };
            }
        }
    }

    RepoSuggestion {
        project_ids: known(cfg.last_repos.get("").cloned().unwrap_or_default()),
        reason: None,
    }
}

/// The branch for a task.
///
/// A ticket's branch is its key, optionally with a suffix — `ACME-1234` or
/// `ACME-1234-some-feature`. The key is used verbatim rather than folded into a
/// slug of the task name, which would repeat it: task names from Jira already
/// begin with the key.
pub(crate) fn derive_branch(
    explicit: Option<&str>,
    issue_key: Option<&str>,
    suffix: Option<&str>,
    name: &str,
) -> String {
    if let Some(branch) = explicit.map(str::trim).filter(|b| !b.is_empty()) {
        return branch.to_string();
    }
    match issue_key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(key) => match suffix.map(slugify).filter(|s| !s.is_empty()) {
            Some(suffix) => format!("{key}-{suffix}"),
            None => key.to_string(),
        },
        // A name with nothing ASCII in it — "Корзина" — slugifies to nothing,
        // and `villain/` is not a branch git will make.
        None => match slugify(name) {
            slug if slug.is_empty() => {
                format!("villain/task-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
            }
            slug => format!("villain/{slug}"),
        },
    }
}

/// A task directory no other task owns and nothing occupies, so two tasks with
/// the same name never share a folder or clobber each other's worktrees.
pub(crate) fn unique_task_root(config: &ConfigStore, dir_name: &str) -> PathBuf {
    let base = config.worktree_root();
    let taken: Vec<String> = config.read().tasks.iter().map(|t| t.root.clone()).collect();

    let mut candidate = base.join(dir_name);
    let mut n = 2;
    while candidate.exists() || taken.iter().any(|t| Path::new(t) == candidate) {
        candidate = base.join(format!("{dir_name}-{n}"));
        n += 1;
    }
    candidate
}

/// `<task root>/<repo-name>`, deduped if two repos share a name.
pub(crate) fn checkout_path(root: &Path, project: &Project, taken: &[String]) -> PathBuf {
    let mut name = project.name.clone();
    let mut n = 2;
    while taken.iter().any(|t| t == &name) {
        name = format!("{}-{n}", project.name);
        n += 1;
    }
    root.join(name)
}

/// The branch a repo's worktree is cut from: the task's, or the repo's own.
fn base_for(project: &Project, base: Option<&str>) -> String {
    base.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(project.default_branch.as_str())
        .to_string()
}

pub(crate) fn create_checkout(
    state: &AppState,
    task: &Task,
    project: &Project,
    taken: &mut Vec<String>,
    // When set, every repo in the task is cut from this; otherwise each uses
    // its own default branch. Empty strings are treated as unset.
    base: Option<&str>,
    // The base was fetched already, with the other repos' — see `new_task`.
    fetched: bool,
) -> Result<Checkout> {
    let root = PathBuf::from(&task.root);
    let path = checkout_path(&root, project, taken);
    taken.push(
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    );

    let base = base_for(project, base);
    let repo = super::repo_for_branch(state, project, &task.branch);
    let base_commit = if fetched {
        git::add_worktree_fetched(&repo, &path, &task.branch, &base)?
    } else {
        git::add_worktree(&repo, &path, &task.branch, &base)?
    };

    let checkout = Checkout {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task.id.clone(),
        project_id: project.id.clone(),
        path: path.to_string_lossy().to_string(),
        base,
        base_commit: Some(base_commit).filter(|c| !c.is_empty()),
        push_lease: None,
        point_before_update: None,
        last_head: None,
    };
    state
        .config
        .update(|c| c.checkouts.push(checkout.clone()))?;
    Ok(checkout)
}

#[derive(Debug, Deserialize)]
pub struct NewTask {
    pub name: String,
    /// One worktree is created per repository, all on the same branch.
    pub project_ids: Vec<String>,
    /// A fully explicit branch name, overriding everything below.
    #[serde(default)]
    pub branch: Option<String>,
    /// Appended to the ticket key: ACME-1234 becomes ACME-1234-some-feature.
    #[serde(default)]
    pub branch_suffix: Option<String>,
    /// Branch every worktree is cut from. When omitted, each repository uses
    /// its own default branch.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub issue_key: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    /// Recorded only so the next ticket under the same epic can be prefilled.
    #[serde(default)]
    pub epic_key: Option<String>,
}

/// Off the command thread: one `git worktree add` per repository, and that is
/// the slowest git command there is on a large clone.
#[tauri::command]
pub async fn create_task(app: AppHandle, req: NewTask) -> Result<Task> {
    super::blocking(app, move |state| new_task(state, req)).await
}

pub(crate) fn new_task(state: &AppState, req: NewTask) -> Result<Task> {
    if req.project_ids.is_empty() {
        return Err(Error::Other("pick at least one repository".into()));
    }
    let projects: Vec<Project> = req
        .project_ids
        .iter()
        .map(|id| state.config.project(id))
        .collect::<Result<_>>()?;

    let branch = derive_branch(
        req.branch.as_deref(),
        req.issue_key.as_deref(),
        req.branch_suffix.as_deref(),
        &req.name,
    );

    // The folder is named after the branch, so the two are always findable
    // from each other. Slashes would nest it, so they become dashes.
    let root = unique_task_root(&state.config, &branch.replace('/', "-"));
    std::fs::create_dir_all(&root)?;

    let task = Task {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name,
        root: root.to_string_lossy().to_string(),
        branch,
        issue_key: req.issue_key.clone(),
        issue_url: req.issue_url,
        created_at: Utc::now(),
        ticket_stage: None,
    };
    state.config.update(|c| c.tasks.push(task.clone()))?;

    let base = req
        .base
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Every base at once, before any worktree: see `git::fetch_bases`.
    let targets: Vec<(PathBuf, String)> = projects
        .iter()
        .map(|p| (super::repo_for(state, p), base_for(p, base)))
        .collect();
    git::fetch_bases(&targets);

    let mut taken = Vec::new();
    let mut created = Vec::new();
    for project in &projects {
        // Asked first so that unwinding knows which branches are ours to take
        // back. Left behind, a retry with a different base found the branch
        // already there, checked it out as it was, and measured it against
        // the new base — the old base's commits then showed up in the diff.
        let repo = super::repo_for_branch(state, project, &task.branch);
        let fresh_branch = !git::branch_exists(&repo, &task.branch);
        match create_checkout(state, &task, project, &mut taken, base, true) {
            Ok(c) => created.push((c, fresh_branch)),
            Err(e) => {
                // Leave nothing half-built: unwind the worktrees we just made,
                // and the branches with them.
                if fresh_branch && git::branch_exists(&repo, &task.branch) {
                    let _ = git::delete_branch(&repo, &task.branch);
                }
                for (c, fresh) in &created {
                    if let Ok(p) = state.config.project(&c.project_id) {
                        let repo = super::owner_of(&p, &c.path);
                        let _ = git::remove_worktree(&repo, &c.path, true);
                        if *fresh {
                            let _ = git::delete_branch(&repo, &task.branch);
                        }
                    }
                }
                let task_id = task.id.clone();
                state.config.update(|c| {
                    c.tasks.retain(|t| t.id != task_id);
                    c.checkouts.retain(|ch| ch.task_id != task_id);
                })?;
                let _ = std::fs::remove_dir(&root);
                return Err(e);
            }
        }
    }

    // Remember the choice against both the epic and the Jira project, so the
    // next ticket gets the tightest signal available.
    let project = issue_project(req.issue_key.as_deref());
    let epic = req.epic_key.clone();
    let ids = req.project_ids.clone();
    state.config.update(|c| {
        if let Some(epic) = epic.filter(|e| !e.is_empty()) {
            c.last_repos.insert(format!("epic:{epic}"), ids.clone());
        }
        let key = if project.is_empty() {
            String::new()
        } else {
            format!("project:{project}")
        };
        c.last_repos.insert(key, ids);
    })?;

    // The folder describes itself from the moment it exists, so an agent
    // started by hand in it is no worse off than one the app launched.
    let _ = write_task_context(state, &task);

    Ok(task)
}

/// What a terminal has printed, as readable text.
///
/// Gated here rather than in the tool definition so nothing can route around
/// it: the scrollback of a shell is a record of everything that has passed
/// through it, which is useful to an agent and is also not automatically the
/// agent's business.
pub(crate) fn pane_output(state: &AppState, pane_id: &str, lines: usize) -> Result<String> {
    if !state.config.read().ui.agents_read_panes {
        return Err(Error::Other(
            "reading terminal output is switched off in the app's settings. Turn on \
             \"Let agents read terminal output\" there if you want this."
                .into(),
        ));
    }
    state.ptys.transcript(pane_id, lines.clamp(1, 2_000))
}

/// Tell the agents working on a task that it gained a repository.
///
/// A worktree appearing beside a running agent is invisible to it: nothing
/// prompts it to look at its surroundings again, so it carries on believing
/// the repo it needs is not there. The note is typed into the pane and
/// submitted — the one channel an agent is actually listening on. A CLI
/// queues it and picks it up when the current turn ends.
///
/// Returns how many were told, so the app can say so rather than doing it
/// silently.
pub(crate) fn tell_agents(state: &AppState, task_id: &str, text: &str) -> usize {
    let mut told = 0;
    for pane in state.ptys.list(Some(task_id)) {
        if pane.kind != PaneKind::Agent || !pane.running {
            continue;
        }
        if state.ptys.submit(&pane.id, text).is_ok() {
            told += 1;
        }
    }
    told
}

/// A repository added to a task, and how many running agents were told.
#[derive(Debug, Serialize)]
pub struct AddedRepo {
    #[serde(flatten)]
    pub checkout: Checkout,
    pub told: usize,
}

/// Add a repository to a task that is already in flight.
pub(crate) fn add_repo(state: &AppState, task_id: &str, project_id: &str) -> Result<AddedRepo> {
    let checkout = add_checkout_inner(state, task_id, project_id)?;
    let name = state
        .config
        .project(project_id)
        .map(|p| p.name)
        .unwrap_or_else(|_| "a repository".into());

    // The folder gained a sibling, so the description of it is now wrong.
    if let Ok(task) = state.config.task(task_id) {
        let _ = write_task_context(state, &task);
    }

    let told = tell_agents(
        state,
        task_id,
        &format!(
            "[villain-layer] {name} has just been added to this task, checked out on the \
             same branch at {}. It is there if the work needs it — read it before assuming \
             anything about what is in it.",
            checkout.path
        ),
    );
    Ok(AddedRepo { checkout, told })
}

#[tauri::command]
pub async fn add_checkout(
    app: AppHandle,
    task_id: String,
    project_id: String,
) -> Result<AddedRepo> {
    super::blocking(app, move |state| add_repo(state, &task_id, &project_id)).await
}

pub(crate) fn add_checkout_inner(
    state: &AppState,
    task_id: &str,
    project_id: &str,
) -> Result<Checkout> {
    let task_id = task_id.to_string();
    let project_id = project_id.to_string();
    let task = state.config.task(&task_id)?;
    let project = state.config.project(&project_id)?;

    let existing = state.config.checkouts_of(&task_id);
    if existing.iter().any(|c| c.project_id == project_id) {
        return Err(Error::Other(format!(
            "{} is already part of this task",
            project.name
        )));
    }

    // A task created before this repo joined may still point its root at a lone
    // worktree; give it a real task directory before adding a sibling.
    let root = PathBuf::from(&task.root);
    if existing.iter().any(|c| c.path == task.root) {
        let slug = match &task.issue_key {
            Some(key) => format!("{}-{}", key.to_uppercase(), slugify(&task.name)),
            None => slugify(&task.name),
        };
        let new_root = unique_task_root(&state.config, &slug);
        std::fs::create_dir_all(&new_root)?;
        let root_s = new_root.to_string_lossy().to_string();
        let id = task_id.clone();
        state.config.update(|c| {
            if let Some(t) = c.tasks.iter_mut().find(|t| t.id == id) {
                t.root = root_s;
            }
        })?;
    } else {
        std::fs::create_dir_all(&root)?;
    }

    let task = state.config.task(&task_id)?;
    let mut taken: Vec<String> = existing
        .iter()
        .filter_map(|c| {
            PathBuf::from(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .collect();
    // Match whatever the rest of the task was cut from when they agree; a
    // later repo joining work aimed at `develop` should not silently land on
    // `main` just because that is its own default.
    let shared = existing.first().map(|c| c.base.as_str()).filter(|&b| {
        existing.iter().all(|c| c.base == b)
    });
    create_checkout(state, &task, &project, &mut taken, shared, false)
}

/// Off the command thread for the same reason `delete_task` is: this stops
/// every pane rooted in the worktree, which waits up to two seconds on them,
/// and then runs the same `git worktree remove`.
#[tauri::command]
pub async fn remove_checkout(app: AppHandle, checkout_id: String, force: bool) -> Result<()> {
    super::blocking(app, move |state| remove_checkout_inner(state, checkout_id, force)).await
}

fn remove_checkout_inner(state: &AppState, checkout_id: String, force: bool) -> Result<()> {
    let checkout = state.config.checkout(&checkout_id)?;
    let project = state.config.project(&checkout.project_id)?;

    // As in `delete_task`: find out whether git will refuse before stopping
    // the agents working in there, not after.
    if !force {
        if let Ok(status) = git::status(Path::new(&checkout.path)) {
            if status.dirty_files > 0 {
                return Err(Error::Git(format!(
                    "{} has {} uncommitted change{}; nothing was removed",
                    project.name,
                    status.dirty_files,
                    if status.dirty_files == 1 { "" } else { "s" }
                )));
            }
        }
    }
    // Stopped with the repo, so not wanted back: a task-root agent stopped
    // here returned at the next launch.
    let closed = state.ptys.close_checkout(&checkout_id, &checkout.path);
    let _ = state.config.update(|c| c.saved_panes.retain(|p| !closed.contains(&p.id)));
    let repo = super::owner_of(&project, &checkout.path);
    if Path::new(&checkout.path).exists() {
        // A refusal — uncommitted work, without force — keeps the record too:
        // dropping it would leave the worktree on disk with nothing pointing
        // at it, which is the orphan delete_task already learned not to make.
        git::remove_worktree(&repo, &checkout.path, force)?;
    } else {
        // Already removed by hand; just tidy the admin files, wherever it
        // was registered.
        let _ = git::prune_worktrees(&project.repo());
        let _ = git::prune_worktrees(Path::new(&project.path));
    }

    state
        .config
        .update(|c| c.checkouts.retain(|ch| ch.id != checkout_id))?;
    state.status_cache.lock().remove(&checkout_id);

    // The folder lost a sibling, so the description of it is now wrong.
    if let Ok(task) = state.config.task(&checkout.task_id) {
        let _ = write_task_context(state, &task);
    }
    Ok(())
}

/// What finishing a task did.
#[derive(Debug, Serialize)]
pub struct Finished {
    /// One row per worktree. Any that failed means the task was kept, and
    /// nothing after it was done.
    pub repos: Vec<RepoResult>,
    /// One row per local branch.
    pub branches: Vec<RepoResult>,
    pub ticket_moved: bool,
    pub ticket_error: Option<String>,
}

/// Clear away a task whose work has landed: its agents, its worktrees, its
/// local branches, and — when a transition is given — its ticket.
///
/// In that order, and each only if the one before went. A worktree git will
/// not remove holds work nobody has seen yet, which keeps the task; and a
/// ticket marked done over work still on disk would be saying something
/// untrue.
///
/// Local branches only: the pull requests and what they landed are on
/// GitHub, and the remote branch is the repository's business — many delete
/// it on merge themselves. `landed` is each checkout's merged PR head; a
/// branch is deleted only when that head contains it.
#[tauri::command]
pub async fn finish_task(
    app: AppHandle,
    task_id: String,
    transition_id: Option<String>,
    landed: std::collections::HashMap<String, String>,
) -> Result<Finished> {
    let task = app.state::<AppState>().config.task(&task_id)?;
    let (repos, branches) = {
        let (task_id, branch) = (task_id.clone(), task.branch.clone());
        super::blocking(app.clone(), move |state| {
            // Where each branch lives, read before the task's record is gone.
            let homes: Vec<(String, String, PathBuf, String)> = state
                .config
                .checkouts_of(&task_id)
                .into_iter()
                .filter_map(|c| {
                    let p = state.config.project(&c.project_id).ok()?;
                    let home = super::owner_of(&p, &c.path);
                    Some((c.id, p.name, home, c.base))
                })
                .collect();
            let repos = delete_task_inner(state, task_id, false)?;
            if repos.iter().any(|r| !r.ok) {
                return Ok((repos, Vec::new()));
            }
            let branches = homes
                .into_iter()
                .map(|(checkout_id, repo, path, base)| {
                    let local = format!("refs/heads/{branch}");
                    // Only a branch its merged PR contains, or one with nothing
                    // of its own. "Merged" is said of the task once every PR
                    // has landed, and a commit made after one did — or in a
                    // repo that never had a PR — lives on this branch alone;
                    // deleting it deleted the work.
                    let covered = landed
                        .get(&checkout_id)
                        .is_some_and(|sha| git::is_ancestor(&path, &local, sha))
                        || git::is_ancestor(&path, &local, &format!("refs/remotes/origin/{base}"));
                    let (ok, detail) = if !git::branch_exists(&path, &branch) {
                        (true, "already gone".to_string())
                    } else if !covered {
                        (
                            false,
                            if landed.contains_key(&checkout_id) {
                                "kept: it has commits its merged pull request does not".to_string()
                            } else {
                                "kept: no merged pull request covers it".to_string()
                            },
                        )
                    } else {
                        // -D, not -d: a squash or rebase merge lands different
                        // commits, so git never sees this branch as merged.
                        match git::delete_branch(&path, &branch) {
                            Ok(()) => (true, "deleted".to_string()),
                            Err(e) => (false, e.to_string()),
                        }
                    };
                    RepoResult { checkout_id, repo, ok, detail }
                })
                .collect();
            Ok((repos, branches))
        })
        .await?
    };

    let mut finished = Finished { repos, branches, ticket_moved: false, ticket_error: None };
    if finished.repos.iter().any(|r| !r.ok) {
        return Ok(finished);
    }
    if let (Some(id), Some(key)) = (transition_id, task.issue_key.as_deref()) {
        let state = app.state::<AppState>();
        let moved = match super::jira::jira_client(&state) {
            Ok((client, _)) => client.transition(key, &id).await,
            Err(e) => Err(e),
        };
        match moved {
            Ok(()) => finished.ticket_moved = true,
            // The task is already gone, so this is the only place it can be said.
            Err(e) => finished.ticket_error = Some(e.to_string()),
        }
    }
    Ok(finished)
}

/// Delete a task and every worktree it owns.
///
/// Reports per repository rather than swallowing failures: `git worktree
/// remove` refuses while a worktree has uncommitted or untracked files, and
/// ignoring that left the worktree on disk with no task pointing at it — an
/// orphan the app could not see and the user had to clean up by hand.
#[tauri::command]
pub async fn delete_task(
    app: AppHandle,
    id: String,
    force: bool,
) -> Result<Vec<RepoResult>> {
    // git worktree remove and waiting on agents both sleep. Running them on
    // the command thread freezes every other invoke — including the ones that
    // keep the window painting — so the UI looks crashed until they finish.
    super::blocking(app, move |state| delete_task_inner(state, id, force)).await
}

fn delete_task_inner(state: &AppState, id: String, force: bool) -> Result<Vec<RepoResult>> {
    let task = state.config.task(&id)?;

    // Ask before stopping anything. git refuses to remove a dirty worktree
    // without force, and the task is then kept — but its agents had already
    // been stopped, so a refused delete still cost every conversation in it.
    // The sidebar's count that decides whether to force can be minutes old.
    if !force {
        let dirty: Vec<RepoResult> = state
            .config
            .checkouts_of(&id)
            .into_iter()
            .filter_map(|c| {
                let status = git::status(&PathBuf::from(&c.path)).ok()?;
                (status.dirty_files > 0).then(|| RepoResult {
                    repo: state
                        .config
                        .project(&c.project_id)
                        .map(|p| p.name)
                        .unwrap_or_else(|_| "(unknown)".into()),
                    checkout_id: c.id,
                    ok: false,
                    detail: format!(
                        "{} uncommitted change{}",
                        status.dirty_files,
                        if status.dirty_files == 1 { "" } else { "s" }
                    ),
                })
            })
            .collect();
        if !dirty.is_empty() {
            return Ok(dirty);
        }
    }
    let closed = state.ptys.close_task(&id);
    let _ = state.config.update(|c| c.saved_panes.retain(|p| !closed.contains(&p.id)));

    let mut results = Vec::new();
    for checkout in state.config.checkouts_of(&id) {
        let project = match state.config.project(&checkout.project_id) {
            Ok(p) => p,
            // The repo is gone from the app, so there is no worktree admin to
            // clean up; drop the record.
            Err(_) => continue,
        };

        let path = PathBuf::from(&checkout.path);
        let (ok, detail) = if !path.exists() {
            // Already removed by hand; just tidy the admin files, wherever
            // it was registered.
            let _ = git::prune_worktrees(&project.repo());
            let _ = git::prune_worktrees(Path::new(&project.path));
            (true, "already gone".to_string())
        } else {
            match git::remove_worktree(&super::owner_of(&project, &checkout.path), &checkout.path, force) {
                Ok(()) => (true, "removed".to_string()),
                Err(e) => (false, e.to_string()),
            }
        };

        results.push(RepoResult {
            checkout_id: checkout.id,
            repo: project.name,
            ok,
            detail,
        });
    }

    let stuck: Vec<String> = results
        .iter()
        .filter(|r| !r.ok)
        .map(|r| r.checkout_id.clone())
        .collect();

    if !stuck.is_empty() {
        // Keep the task so the worktrees stay reachable and the user can retry
        // with force, rather than stranding them.
        state.config.update(|c| {
            c.checkouts
                .retain(|ch| ch.task_id != id || stuck.contains(&ch.id));
        })?;
        return Ok(results);
    }

    // The app's own files go first, or the folder is never empty and lingers
    // as an orphan under the worktree root. Anything else in there is the
    // user's, so remove_dir refuses rather than taking it.
    if let Some(dir) = agent_file_dir(state, &task) {
        remove_generated(&dir);
    }
    let _ = std::fs::remove_dir(&task.root);

    state.config.update(|c| {
        c.tasks.retain(|t| t.id != id);
        c.checkouts.retain(|ch| ch.task_id != id);
    })?;
    // Stale status for a deleted checkout would otherwise linger until TTL.
    state.status_cache.lock().retain(|cid, _| {
        !results.iter().any(|r| r.checkout_id == *cid)
    });
    Ok(results)
}

