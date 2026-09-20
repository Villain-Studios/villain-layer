//! Everything the frontend can call. Thin orchestration over git, PTYs and the
//! three integrations.
//!
//! The unit of work is a **task**: one ticket, N repositories. Most commands
//! take a task id and fan out over its checkouts.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agents;
use crate::config::{
    Checkout, ConfigStore, GithubConfig, JiraConfig, Project, SavedPane, SlackConfig, Task,
    UiPrefs,
};
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::{github, github::GitHub, jira, jira::Jira, slack::Slack};
use crate::pty::{PaneInfo, PaneKind, PtyManager, SpawnOptions};
use crate::secrets;
use crate::shellenv;

pub struct AppState {
    pub config: ConfigStore,
    pub ptys: PtyManager,
    /// Issue types are per-site and change about never, but each icon is a
    /// separate authenticated fetch, so they are pulled once per run.
    pub jira_types: parking_lot::Mutex<Option<Vec<jira::IssueType>>>,
}

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
const SKIP: &[&str] = &[
    "node_modules", "target", "dist", "build", "vendor", "Pods",
    "DerivedData", "__pycache__", "Library", "Applications",
];

fn walk_for_repos(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
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

fn register_project(state: &AppState, path: &str, group: Option<&str>) -> Result<Project> {
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

// ------------------------------------------------------------------- tasks

#[derive(Debug, Serialize)]
pub struct CheckoutView {
    #[serde(flatten)]
    pub checkout: Checkout,
    pub project_name: String,
    pub status: Option<git::WorktreeStatus>,
    pub exists: bool,
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

fn slugify(s: &str) -> String {
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
fn issue_project(issue_key: Option<&str>) -> String {
    issue_key
        .and_then(|k| k.split_once('-').map(|(p, _)| p.to_string()))
        .unwrap_or_default()
}

fn view_checkout(state: &AppState, checkout: Checkout) -> CheckoutView {
    let dir = PathBuf::from(&checkout.path);
    let exists = dir.is_dir();
    CheckoutView {
        project_name: state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown repo)".into()),
        status: exists.then(|| git::status(&dir).ok()).flatten(),
        changed: if exists {
            git::changed_count(
                &dir,
                &checkout.base,
                checkout.base_commit.as_deref(),
                git::Scope::Uncommitted,
            )
        } else {
            0
        },
        exists,
        checkout,
    }
}

#[tauri::command]
pub fn list_tasks(state: State<AppState>) -> Vec<TaskView> {
    let cfg = state.config.read();
    cfg.tasks
        .iter()
        .map(|t| TaskView {
            checkouts: cfg
                .checkouts
                .iter()
                .filter(|c| c.task_id == t.id)
                .cloned()
                .map(|c| view_checkout(&state, c))
                .collect(),
            pane_count: state.ptys.list(Some(&t.id)).len(),
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

fn suggest_from(
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
fn derive_branch(
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
        None => format!("villain/{}", slugify(name)),
    }
}

/// A task directory no other task owns and nothing occupies, so two tasks with
/// the same name never share a folder or clobber each other's worktrees.
fn unique_task_root(config: &ConfigStore, dir_name: &str) -> PathBuf {
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
fn checkout_path(root: &Path, project: &Project, taken: &[String]) -> PathBuf {
    let mut name = project.name.clone();
    let mut n = 2;
    while taken.iter().any(|t| t == &name) {
        name = format!("{}-{n}", project.name);
        n += 1;
    }
    root.join(name)
}

fn create_checkout(
    state: &AppState,
    task: &Task,
    project: &Project,
    taken: &mut Vec<String>,
) -> Result<Checkout> {
    let root = PathBuf::from(&task.root);
    let path = checkout_path(&root, project, taken);
    taken.push(
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    );

    let base_commit = git::add_worktree(
        &PathBuf::from(&project.path),
        &path,
        &task.branch,
        &project.default_branch,
    )?;

    let checkout = Checkout {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task.id.clone(),
        project_id: project.id.clone(),
        path: path.to_string_lossy().to_string(),
        base: project.default_branch.clone(),
        base_commit: Some(base_commit).filter(|c| !c.is_empty()),
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
    #[serde(default)]
    pub issue_key: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    /// Recorded only so the next ticket under the same epic can be prefilled.
    #[serde(default)]
    pub epic_key: Option<String>,
}

#[tauri::command]
pub fn create_task(state: State<AppState>, req: NewTask) -> Result<Task> {
    new_task(&state, req)
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
    };
    state.config.update(|c| c.tasks.push(task.clone()))?;

    let mut taken = Vec::new();
    let mut created = Vec::new();
    for project in &projects {
        match create_checkout(state, &task, project, &mut taken) {
            Ok(c) => created.push(c),
            Err(e) => {
                // Leave nothing half-built: unwind the worktrees we just made.
                for c in &created {
                    if let Ok(p) = state.config.project(&c.project_id) {
                        let _ = git::remove_worktree(&PathBuf::from(&p.path), &c.path, true);
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
/// the repo it needs is not there. The note goes into the pane's input, which
/// is the one channel an agent is actually listening on — a CLI queues it and
/// picks it up when the current turn ends.
///
/// Returns how many were told, so the app can say so rather than doing it
/// silently.
fn tell_agents(state: &AppState, task_id: &str, text: &str) -> usize {
    let mut told = 0;
    for pane in state.ptys.list(Some(task_id)) {
        if pane.kind != PaneKind::Agent || !pane.running {
            continue;
        }
        if state.ptys.write(&pane.id, text).is_ok() && state.ptys.write(&pane.id, "\r").is_ok() {
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
pub fn add_checkout(
    state: State<AppState>,
    task_id: String,
    project_id: String,
) -> Result<AddedRepo> {
    add_repo(&state, &task_id, &project_id)
}

fn add_checkout_inner(
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
    create_checkout(state, &task, &project, &mut taken)
}

#[tauri::command]
pub fn remove_checkout(state: State<AppState>, checkout_id: String, force: bool) -> Result<()> {
    let checkout = state.config.checkout(&checkout_id)?;
    let project = state.config.project(&checkout.project_id)?;

    state.ptys.close_checkout(&checkout_id);
    let repo = PathBuf::from(&project.path);
    if Path::new(&checkout.path).exists() {
        // A refusal — uncommitted work, without force — keeps the record too:
        // dropping it would leave the worktree on disk with nothing pointing
        // at it, which is the orphan delete_task already learned not to make.
        git::remove_worktree(&repo, &checkout.path, force)?;
    } else {
        // Already removed by hand; just tidy the admin files.
        let _ = git::run(&repo, &["worktree", "prune"]);
    }

    state
        .config
        .update(|c| c.checkouts.retain(|ch| ch.id != checkout_id))?;

    // The folder lost a sibling, so the description of it is now wrong.
    if let Ok(task) = state.config.task(&checkout.task_id) {
        let _ = write_task_context(&state, &task);
    }
    Ok(())
}

/// Delete a task and every worktree it owns.
///
/// Reports per repository rather than swallowing failures: `git worktree
/// remove` refuses while a worktree has uncommitted or untracked files, and
/// ignoring that left the worktree on disk with no task pointing at it — an
/// orphan the app could not see and the user had to clean up by hand.
#[tauri::command]
pub fn delete_task(state: State<AppState>, id: String, force: bool) -> Result<Vec<RepoResult>> {
    let task = state.config.task(&id)?;
    state.ptys.close_task(&id);

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
            // Already removed by hand; just tidy the admin files.
            let _ = git::run(&PathBuf::from(&project.path), &["worktree", "prune"]);
            (true, "already gone".to_string())
        } else {
            match git::remove_worktree(&PathBuf::from(&project.path), &checkout.path, force) {
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
    if let Some(dir) = agent_file_dir(&state, &task) {
        for name in GENERATED_FILES {
            let _ = std::fs::remove_file(dir.join(name));
        }
    }
    let _ = std::fs::remove_dir(&task.root);

    state.config.update(|c| {
        c.tasks.retain(|t| t.id != id);
        c.checkouts.retain(|ch| ch.task_id != id);
    })?;
    Ok(results)
}

// ------------------------------------------------------------------- panes

#[tauri::command]
pub fn list_agents() -> Vec<agents::AgentStatus> {
    agents::available()
}

#[tauri::command]
pub fn list_panes(state: State<AppState>, task_id: Option<String>) -> Vec<PaneInfo> {
    state.ptys.list(task_id.as_deref())
}

/// Where a pane should start, and what to call it.
///
/// An explicit checkout wins. Otherwise everything starts at the task root,
/// where each repository is a sibling folder — including a task that has only
/// one today. Starting inside the single repo read better at the time, but it
/// puts the agent in a directory that cannot grow: a repository added later
/// lands outside its working directory, where it needs permission to look and
/// no reason to think of looking. The root is the same place before and after,
/// which is what lets an agent be told "there is another repo now" and simply
/// carry on.
fn resolve_scope(
    state: &AppState,
    task: &Task,
    checkout_id: Option<&str>,
) -> Result<(String, String, Option<String>)> {
    if let Some(id) = checkout_id {
        let checkout = state.config.checkout(id)?;
        let name = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "repo".into());
        return Ok((checkout.path, name, Some(id.to_string())));
    }

    let checkouts = state.config.checkouts_of(&task.id);
    let name = match checkouts.as_slice() {
        [] => return Err(Error::Other("this task has no repositories".into())),
        [only] => state
            .config
            .project(&only.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "repo".into()),
        many => format!("{} repos", many.len()),
    };
    Ok((task.root.clone(), name, None))
}

/// Conversations that could be picked up for this task, wherever they live.
///
/// Merged across the task root and every worktree, because the directory an
/// agent was started in has changed: a session saved under a worktree is still
/// a session, and refusing to offer it would quietly strand work.
fn resumable_for(state: &AppState, task: &Task, cwd: &str) -> Vec<agents::Resumable> {
    let mut found: Vec<agents::Resumable> = agents::resumable(cwd);
    for checkout in state.config.checkouts_of(&task.id) {
        if checkout.path == cwd {
            continue;
        }
        for r in agents::resumable(&checkout.path) {
            match found.iter_mut().find(|f| f.agent_id == r.agent_id) {
                Some(existing) => {
                    existing.sessions += r.sessions;
                    existing.last_active = existing.last_active.max(r.last_active);
                }
                None => found.push(r),
            }
        }
    }
    found
}

/// Where an agent's saved conversation for this task actually lives.
///
/// Sessions are keyed by working directory, and tasks started before agents
/// moved to the task root have theirs under the worktree. Looking in both
/// means existing work still resumes rather than starting over.
fn resume_dir(state: &AppState, task: &Task, agent_id: &str, cwd: &str) -> String {
    if agents::resumable(cwd).iter().any(|r| r.agent_id == agent_id) {
        return cwd.to_string();
    }
    state
        .config
        .checkouts_of(&task.id)
        .into_iter()
        .find(|c| agents::resumable(&c.path).iter().any(|r| r.agent_id == agent_id))
        .map(|c| c.path)
        .unwrap_or_else(|| cwd.to_string())
}

#[tauri::command]
pub fn spawn_shell(
    app: AppHandle,
    state: State<AppState>,
    task_id: String,
    checkout_id: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    open_shell(&app, &state, task_id, checkout_id, rows, cols)
}

fn open_shell(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    checkout_id: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let (cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Shell,
            title: format!("shell · {scope}"),
            program: shellenv::login_shell(),
            args: vec!["-l".into()],
            agent_id: None,
            rows,
            cols,
            initial_input: None,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn spawn_agent(
    app: AppHandle,
    state: State<AppState>,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: Option<bool>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    start_agent(
        &app, &state, task_id, agent_id, checkout_id, prompt,
        resume.unwrap_or(false), rows, cols,
    )
}

/// Conversations that could be picked up again in a task's working directory.
///
/// Panes do not survive the app closing, but the agent CLIs keep their own
/// transcripts per directory — so the work can continue even though the
/// process cannot.
#[tauri::command]
pub fn resumable_agents(
    state: State<AppState>,
    task_id: String,
    checkout_id: Option<String>,
) -> Result<Vec<agents::Resumable>> {
    let task = state.config.task(&task_id)?;
    let (cwd, _, _) = resolve_scope(&state, &task, checkout_id.as_deref())?;
    Ok(if checkout_id.is_some() {
        agents::resumable(&cwd)
    } else {
        resumable_for(&state, &task, &cwd)
    })
}

/// Answer the trust dialog for a folder this app created, if that is wanted.
///
/// Scoped on purpose: only directories under the app's own worktree root are
/// ever pre-trusted, so opening an agent somewhere else still asks.
fn pretrust_own_dir(state: &AppState, agent_id: &str, cwd: &str) {
    if !state.config.read().ui.trust_agent_dirs {
        return;
    }
    let dir = Path::new(cwd);
    if !dir.starts_with(state.config.worktree_root()) {
        return;
    }
    agents::pretrust(agent_id, dir);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_agent(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: bool,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    let (mut cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;
    if resume && checkout_id.is_none() {
        cwd = resume_dir(state, &task, &agent_id, &cwd);
    }

    // Resuming means handing the conversation back to the CLI, so an opening
    // prompt would only talk over it.
    let (mut args, initial_input) = if resume {
        let flags = def.resume_args.ok_or_else(|| {
            Error::Other(format!("{} cannot resume a previous session", def.name))
        })?;
        (flags.iter().map(|f| f.to_string()).collect::<Vec<_>>(), None)
    } else {
        agents::launch_args(def, prompt.as_deref())
    };

    // Give the agent the app's own MCP tools. When its cwd is the task root it
    // picks .mcp.json up by itself; when the cwd is a worktree the file has to
    // live elsewhere and be pointed at explicitly.
    // Best effort: neither is a reason to refuse to start the agent.
    let _ = write_task_context(state, &task);
    if let Some(dir) = agent_file_dir(state, &task) {
        let _ = crate::mcp::write_config(&dir);
        if Path::new(&cwd) != dir {
            if let Some(flag) = def.mcp_config_flag {
                args.push(flag.to_string());
                args.push(dir.join(".mcp.json").to_string_lossy().to_string());
            }
        }
    }

    pretrust_own_dir(state, &agent_id, &cwd);

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Agent,
            title: format!("{} · {scope}{}", def.name, if resume { " (resumed)" } else { "" }),
            program,
            args,
            agent_id: Some(agent_id),
            rows,
            cols,
            initial_input,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

/// Panes that belong to the standing chat rather than to any task.
pub const CHAT_TASK_ID: &str = "chat";

/// The chat agent's working directory: a scratch folder it can keep notes and
/// drafts in, deliberately outside any repository.
pub(crate) fn chat_dir(state: &AppState) -> Result<PathBuf> {
    let dir = state.config.worktree_root().join("_chat");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// One chat's own folder under the chat root.
///
/// The agent CLIs key their saved conversations by working directory, so two
/// chats sharing a folder would both pick up whichever ran last. The folder is
/// what gives a chat its identity across a restart.
fn chat_room(state: &AppState, id: &str) -> Result<PathBuf> {
    let dir = chat_dir(state)?.join(id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Everything the app itself writes into a task folder.
const GENERATED_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md", ".mcp.json", "PR_DESCRIPTION.md"];

/// Where generated agent files (`.mcp.json`, context) may safely be written.
///
/// Never inside a worktree: a generated file there is an untracked change that
/// shows up in review and can be committed by accident. A task root is only
/// safe when it is not itself a checkout, which it is for tasks migrated from
/// the one-repo-per-task layout.
fn agent_file_dir(state: &AppState, task: &Task) -> Option<PathBuf> {
    let root = PathBuf::from(&task.root);
    let is_a_checkout = state
        .config
        .checkouts_of(&task.id)
        .iter()
        .any(|c| Path::new(&c.path) == root);

    (!is_a_checkout && root.is_dir()).then_some(root)
}

/// What the app knows, written where a coding agent will read it.
///
/// Agents pick up `CLAUDE.md` (and `AGENTS.md`) from their working directory,
/// so the chat scratch folder is a safe place to leave this: it is not a
/// repository, so nothing here can pollute a diff or get committed by mistake.
pub(crate) fn write_chat_context(state: &AppState, dir: &Path) -> Result<()> {
    let cfg = state.config.read();
    let mut md = String::from(concat!(
        "# Villain Layer — session context\n\n",
        "Regenerated by Villain Layer every time a chat agent starts. You are in a ",
        "scratch directory, not a repository; use it freely for notes and drafts.\n\n",
    ));

    md.push_str("## Integrations the app holds credentials for\n\n");
    match &cfg.jira {
        Some(j) => md.push_str(&format!(
            "- **Jira** — {} (project {})\n",
            j.base_url,
            j.project_key.as_deref().unwrap_or("any"),
        )),
        None => md.push_str("- Jira — not connected\n"),
    }
    match &cfg.github {
        Some(g) => md.push_str(&format!("- **GitHub** — {}\n", g.api_url)),
        None => md.push_str("- GitHub — not connected\n"),
    }
    match &cfg.slack {
        Some(sl) => md.push_str(&format!("- **Slack** — {}\n", sl.channel)),
        None => md.push_str("- Slack — not connected\n"),
    }
    md.push_str(concat!(
        "\nYou can act on all of these through the `villain-layer` MCP server, which ",
        "this app hosts and which uses the credentials above — you need none of your ",
        "own. Its tools include `jira_search`, `jira_get_issue`, `jira_create_issue`, ",
        "`jira_comment`, `jira_transition`, `slack_post`, `list_tasks`, `list_repos`, ",
        "`task_diff`, `start_work` and `open_prs`.\n\n",
        "These act immediately and are not confirmed: filing a ticket or posting to ",
        "Slack happens for real. Check with the user before anything others will see.\n\n",
    ));

    if !cfg.projects.is_empty() {
        md.push_str("## Repositories\n\n| repo | group | path |\n|---|---|---|\n");
        for p in &cfg.projects {
            md.push_str(&format!(
                "| {} | {} | `{}` |\n",
                p.name,
                p.group.as_deref().unwrap_or("—"),
                p.path,
            ));
        }
        md.push('\n');
    }

    if cfg.tasks.is_empty() {
        md.push_str("## Tasks in flight\n\nNone right now.\n");
    } else {
        md.push_str("## Tasks in flight\n\n");
        for task in &cfg.tasks {
            md.push_str(&format!("### {} — `{}`\n", task.name, task.branch));
            if let Some(url) = &task.issue_url {
                md.push_str(&format!("{url}\n"));
            }
            for c in cfg.checkouts.iter().filter(|c| c.task_id == task.id) {
                let repo = cfg
                    .projects
                    .iter()
                    .find(|p| p.id == c.project_id)
                    .map(|p| p.name.as_str())
                    .unwrap_or("unknown");
                md.push_str(&format!("- {repo}: `{}`\n", c.path));
            }
            md.push('\n');
        }
    }

    std::fs::write(dir.join("CLAUDE.md"), &md)?;
    // Agents that look for AGENTS.md instead should see the same thing.
    std::fs::write(dir.join("AGENTS.md"), &md)?;
    Ok(())
}

/// The layout of a task, written into its folder for the agent standing in it.
///
/// The opening prompt says all of this too, but a prompt is said once: it
/// scrolls away, and a resumed conversation never hears it at all. A file in
/// the working directory is read every time, which is what an agent needs when
/// the task gains a repository weeks after it started.
///
/// Never written into a worktree — a generated file there is an untracked
/// change that turns up in review.
fn write_task_context(state: &AppState, task: &Task) -> Result<()> {
    let Some(dir) = agent_file_dir(state, task) else {
        return Ok(());
    };
    let checkouts = state.config.checkouts_of(&task.id);

    let mut md = format!(
        "# {}\n\nWritten by Villain Layer. You are in the task folder, not inside a \
         repository: each repository below is checked out as a folder here, all on \
         branch `{}`.\n\n",
        task.name, task.branch,
    );
    if let Some(url) = &task.issue_url {
        md.push_str(&format!("Ticket: {url}\n\n"));
    }

    md.push_str("## Repositories in this task\n\n");
    if checkouts.is_empty() {
        md.push_str("None yet.\n\n");
    } else {
        md.push_str("| folder | clone it came from |\n|---|---|\n");
        for c in &checkouts {
            let origin = state
                .config
                .project(&c.project_id)
                .map(|p| p.path)
                .unwrap_or_default();
            let folder = PathBuf::from(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            md.push_str(&format!("| `{folder}/` | `{origin}` |\n"));
        }
        md.push('\n');
    }

    md.push_str(concat!(
        "Run git, and each repository's own tests, from inside its folder. A ",
        "repository's own CLAUDE.md or AGENTS.md lives in that folder and applies ",
        "there.\n\n",
        "## If the work needs a repository that is not here\n\n",
        "Call `add_repo` on the `villain-layer` MCP server with this task's id and the ",
        "repository's name, and it is checked out here on the same branch. Do that ",
        "rather than reading or editing the original clone: that one is on its own ",
        "branch and is not yours to change. `list_repos` shows what is available and ",
        "`list_tasks` gives the task id.\n",
    ));

    std::fs::write(dir.join("CLAUDE.md"), &md)?;
    // Agents that look for AGENTS.md instead should see the same thing.
    std::fs::write(dir.join("AGENTS.md"), &md)?;
    Ok(())
}

/// A standing agent with no worktree, for questions, ticket drafting and
/// whatever MCP servers the user has configured for their own CLI.
#[tauri::command]
pub fn spawn_chat(
    app: AppHandle,
    state: State<AppState>,
    agent_id: String,
    prompt: Option<String>,
) -> Result<PaneInfo> {
    open_chat(&app, &state, agent_id, prompt, None, false)
}

fn open_chat(
    app: &AppHandle,
    state: &AppState,
    agent_id: String,
    prompt: Option<String>,
    // An existing chat folder to reopen, or None to start a new one.
    room: Option<PathBuf>,
    resume: bool,
) -> Result<PaneInfo> {
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    let dir = match room {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            dir
        }
        None => chat_room(state, &uuid::Uuid::new_v4().to_string()[..8])?,
    };
    // Best effort: a context file that cannot be written is not a reason to
    // refuse to start the agent.
    let _ = write_chat_context(state, &dir);
    let _ = crate::mcp::write_config(&dir);

    // Resuming hands the conversation back to the CLI, so an opening prompt
    // would only talk over it.
    let (args, initial_input) = if resume {
        let flags = def.resume_args.ok_or_else(|| {
            Error::Other(format!("{} cannot resume a previous session", def.name))
        })?;
        (flags.iter().map(|f| f.to_string()).collect::<Vec<_>>(), None)
    } else {
        agents::launch_args(def, prompt.as_deref())
    };

    pretrust_own_dir(state, &agent_id, &dir.to_string_lossy());

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id: CHAT_TASK_ID.to_string(),
            checkout_id: None,
            cwd: dir.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: format!("{}{}", def.name, if resume { " (resumed)" } else { "" }),
            program,
            args,
            agent_id: Some(agent_id),
            rows: None,
            cols: None,
            initial_input,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

#[tauri::command]
pub fn pty_write(state: State<AppState>, pane_id: String, data: String) -> Result<()> {
    state.ptys.write(&pane_id, &data)
}

#[tauri::command]
pub fn pty_resize(state: State<AppState>, pane_id: String, rows: u16, cols: u16) -> Result<()> {
    state.ptys.resize(&pane_id, rows, cols)
}

#[tauri::command]
pub fn pty_scrollback(state: State<AppState>, pane_id: String) -> Result<String> {
    state.ptys.scrollback(&pane_id)
}

/// Remember a pane so it can be put back next launch.
///
/// Called where the pane is actually created rather than by each caller: an
/// agent started from a ticket went through `start_agent` directly and so was
/// never recorded, and vanished for good at the next restart.
fn remember_pane(state: &AppState, pane: &PaneInfo) {
    let saved = SavedPane {
        id: pane.id.clone(),
        task_id: pane.task_id.clone(),
        checkout_id: pane.checkout_id.clone(),
        kind: match pane.kind {
            PaneKind::Agent => "agent".into(),
            PaneKind::Shell => "shell".into(),
        },
        agent_id: pane.agent_id.clone(),
        cwd: Some(pane.cwd.clone()),
    };
    let _ = state.config.update(|c| {
        // Replace rather than append: recording the same pane twice is how a
        // restore that also recorded what it restored doubled this list on
        // every launch.
        c.saved_panes.retain(|p| p.id != saved.id);
        c.saved_panes.push(saved);
        // Panes that exited are never removed from here — only closing one on
        // purpose does that — so without a bound this grows for as long as the
        // app is used. Oldest first, since the newest are what you had open.
        let over = c.saved_panes.len().saturating_sub(SAVED_PANE_LIMIT);
        if over > 0 {
            c.saved_panes.drain(..over);
        }
    });
}

/// How many panes are remembered between launches.
///
/// Larger than what a restore will actually open, so the record survives a
/// session with a lot of churn, and still bounded: this file is written every
/// time a pane starts.
const SAVED_PANE_LIMIT: usize = 40;

/// The most panes a restore will ever open.
///
/// A ceiling, not a preference. Restoring is the one path that turns a number
/// in a config file into that many processes, so it must not be able to take
/// the machine down however the file got that way.
const RESTORE_LIMIT: usize = 12;

// Raising the restore limit past what the manager will open would make a
// restore fail part-way through, which is the confusing version of the bug
// rather than the dangerous one. Checked at compile time, so it cannot drift.
const _: () = assert!(RESTORE_LIMIT < crate::pty::MAX_PANES);
const _: () = assert!(RESTORE_LIMIT <= SAVED_PANE_LIMIT);

/// What to put back: each remembered pane once, and never more than the machine
/// should be asked to start at once.
///
/// Identity is the pane's id, not what it looks like. Three shells in one
/// folder are three shells someone wanted; collapsing them because they share a
/// directory throws away work rather than protecting anything. The doubling
/// this guards against wrote the *same* pane twice, which the id catches.
fn panes_to_restore(saved: Vec<SavedPane>, limit: usize) -> Vec<SavedPane> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for pane in saved {
        if !seen.insert(pane.id.clone()) {
            continue;
        }
        out.push(pane);
        if out.len() == limit {
            break;
        }
    }
    out
}

#[tauri::command]
pub fn close_pane(state: State<AppState>, pane_id: String) -> Result<()> {
    // Closing a pane on purpose means not wanting it back.
    let _ = state.config.update(|c| c.saved_panes.retain(|p| p.id != pane_id));
    state.ptys.close(&pane_id)
}

/// Put back what was open when the app last closed.
///
/// Agents are resumed rather than restarted where their CLI can do it, so the
/// conversation continues instead of beginning again. Anything whose worktree
/// or repository has since gone is dropped rather than failing the restore.
pub fn restore_panes(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.config.read().ui.restore_panes {
        return;
    }

    let saved = state.config.read().saved_panes;
    let wanted = saved.len();
    let saved = panes_to_restore(saved, RESTORE_LIMIT);
    if saved.is_empty() {
        return;
    }
    if wanted > saved.len() {
        eprintln!(
            "restoring {} of {wanted} saved panes (duplicates dropped, {RESTORE_LIMIT} at most)",
            saved.len(),
        );
    }
    // The list is rebuilt as each pane comes back with a new id, and only then
    // written. Clearing it up front meant a restore that failed — or an app
    // killed part-way through one — lost the record of what had been open.
    let _ = state.config.update(|c| c.saved_panes.clear());

    for pane in saved {
        // Chats belong to no task: they live in their own folder, so they are
        // restored on the agent's own transcript rather than a worktree.
        if pane.task_id == CHAT_TASK_ID {
            let Some(agent_id) = pane.agent_id.clone() else {
                continue;
            };
            let room = pane.cwd.clone().map(PathBuf::from);
            // Only resume where there is a conversation to resume: a chat
            // saved before folders were per-chat has nothing of its own.
            let resume = room
                .as_deref()
                .map(|d| {
                    agents::resumable(&d.to_string_lossy())
                        .iter()
                        .any(|r| r.agent_id == agent_id)
                })
                .unwrap_or(false);
            // No remember_pane here: spawning records the pane itself.
            if let Err(e) = open_chat(app, &state, agent_id, None, room, resume) {
                eprintln!("could not restore a chat: {e}");
            }
            continue;
        }
        if state.config.task(&pane.task_id).is_err() {
            continue;
        }
        let restored = match pane.kind.as_str() {
            "agent" => {
                let Some(agent_id) = pane.agent_id.clone() else {
                    continue;
                };
                // Only resume if there is a conversation to resume.
                let Ok(task) = state.config.task(&pane.task_id) else {
                    continue;
                };
                let resume = resolve_scope(&state, &task, pane.checkout_id.as_deref())
                    .map(|(cwd, _, _)| {
                        resumable_for(&state, &task, &cwd)
                            .iter()
                            .any(|r| r.agent_id == agent_id)
                    })
                    .unwrap_or(false);

                start_agent(
                    app,
                    &state,
                    pane.task_id.clone(),
                    agent_id,
                    pane.checkout_id.clone(),
                    None,
                    resume,
                    None,
                    None,
                )
            }
            _ => open_shell(app, &state, pane.task_id.clone(), pane.checkout_id.clone(), None, None),
        };

        // Likewise: the spawn recorded it, so recording it again here is what
        // made every restart double the list.
        if let Err(e) = restored {
            eprintln!("could not restore a pane: {e}");
        }
    }
}

/// Stop the process but keep the pane and its scrollback on screen.
#[tauri::command]
pub fn kill_pane(state: State<AppState>, pane_id: String) -> Result<()> {
    state.ptys.kill(&pane_id)
}

// -------------------------------------------------------------------- diff

#[derive(Debug, Serialize)]
pub struct ChangedFileView {
    #[serde(flatten)]
    pub file: git::ChangedFile,
    pub checkout_id: String,
    pub repo: String,
}

/// Every change across every repository in the task, tagged with its repo.
#[tauri::command]
pub fn diff_files(
    state: State<AppState>,
    task_id: String,
    scope: Option<git::Scope>,
) -> Result<Vec<ChangedFileView>> {
    let scope = scope.unwrap_or_default();
    let mut out = Vec::new();
    for checkout in state.config.checkouts_of(&task_id) {
        let dir = PathBuf::from(&checkout.path);
        if !dir.is_dir() {
            continue;
        }
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());

        // One broken repo should not hide the others' changes.
        for file in git::changed_files(&dir, &checkout.base, checkout.base_commit.as_deref(), scope)
            .unwrap_or_default()
        {
            out.push(ChangedFileView {
                file,
                checkout_id: checkout.id.clone(),
                repo: repo.clone(),
            });
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn diff_file(
    state: State<AppState>,
    checkout_id: String,
    path: String,
    scope: Option<git::Scope>,
) -> Result<String> {
    let checkout = state.config.checkout(&checkout_id)?;
    git::file_diff(
        &PathBuf::from(&checkout.path),
        &checkout.base,
        checkout.base_commit.as_deref(),
        scope.unwrap_or_default(),
        &path,
    )
}

#[derive(Debug, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: u32,
    pub body: String,
    #[serde(default)]
    pub code: Option<String>,
    /// Repository the file belongs to; included so paths stay unambiguous when
    /// the agent is sitting at the task root.
    #[serde(default)]
    pub repo: Option<String>,
}

/// Batch review notes into one prompt and type it straight into the agent pane.
#[tauri::command]
pub fn send_review(
    state: State<AppState>,
    pane_id: String,
    comments: Vec<ReviewComment>,
) -> Result<String> {
    if comments.is_empty() {
        return Err(Error::Other("no comments to send".into()));
    }

    let mut prompt = String::from("Review feedback on your changes. Please address each point:\n\n");
    for c in &comments {
        let path = match c.repo.as_ref().filter(|r| !r.is_empty()) {
            Some(repo) => format!("{repo}/{}", c.path),
            None => c.path.clone(),
        };
        prompt.push_str(&format!("- {path}:{} — {}\n", c.line, c.body.trim()));
        if let Some(code) = c.code.as_ref().filter(|s| !s.trim().is_empty()) {
            prompt.push_str(&format!("    (line reads: `{}`)\n", code.trim()));
        }
    }

    state.ptys.write(&pane_id, &prompt)?;
    state.ptys.write(&pane_id, "\r")?;
    Ok(prompt)
}

#[derive(Debug, Serialize)]
pub struct RepoResult {
    pub checkout_id: String,
    pub repo: String,
    pub ok: bool,
    pub detail: String,
}

/// Commit every repository that has changes, under one message.
#[tauri::command]
pub fn commit_task(
    state: State<AppState>,
    task_id: String,
    message: String,
) -> Result<Vec<RepoResult>> {
    let mut results = Vec::new();
    for checkout in state.config.checkouts_of(&task_id) {
        let dir = PathBuf::from(&checkout.path);
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());

        if !dir.is_dir() {
            continue;
        }
        // Uncommitted, not branch: a repo whose work is already committed has
        // nothing to add, and git would refuse with "nothing to commit".
        if git::changed_files(
            &dir,
            &checkout.base,
            checkout.base_commit.as_deref(),
            git::Scope::Uncommitted,
        )
            .map(|f| f.is_empty())
            .unwrap_or(true)
        {
            continue;
        }

        let (ok, detail) = match git::commit_all(&dir, &message) {
            Ok(sha) => (true, sha.chars().take(8).collect()),
            Err(e) => (false, e.to_string()),
        };
        results.push(RepoResult {
            checkout_id: checkout.id,
            repo,
            ok,
            detail,
        });
    }
    Ok(results)
}

#[tauri::command]
pub fn push_task(state: State<AppState>, task_id: String) -> Result<Vec<RepoResult>> {
    let task = state.config.task(&task_id)?;
    let mut results = Vec::new();

    for checkout in state.config.checkouts_of(&task_id) {
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());
        let (ok, detail) = match git::push(&PathBuf::from(&checkout.path), &task.branch) {
            Ok(_) => (true, "pushed".into()),
            Err(e) => (false, e.to_string()),
        };
        results.push(RepoResult {
            checkout_id: checkout.id,
            repo,
            ok,
            detail,
        });
    }
    Ok(results)
}

// -------------------------------------------------------------------- jira

/// The token as typed, or the one already in the keychain when the field was
/// left blank — which is what the settings form says a blank field means.
fn stored_or(key: &str, typed: &str, what: &'static str) -> Result<String> {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Ok(typed.to_string());
    }
    secrets::get(key)?.ok_or(Error::NotConfigured(what))
}

pub(crate) fn jira_client(state: &AppState) -> Result<(Jira, JiraConfig)> {
    let cfg = state
        .config
        .read()
        .jira
        .ok_or(Error::NotConfigured("Jira"))?;
    let token = secrets::get(secrets::JIRA)?.ok_or(Error::NotConfigured("Jira"))?;
    Ok((Jira::new(&cfg, &token), cfg))
}

#[tauri::command]
pub async fn jira_connect(
    state: State<'_, AppState>,
    base_url: String,
    email: String,
    token: String,
    project_key: Option<String>,
    jql: Option<String>,
) -> Result<String> {
    let mut cfg = JiraConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        email,
        project_key,
        jql,
        epic_field: None,
    };
    // Changing the project key or JQL should not need the token typed again.
    let token = stored_or(secrets::JIRA, &token, "Jira")?;
    // Verify before persisting, so a typo never looks like a working setup.
    let client = Jira::new(&cfg, &token);
    let who = client.myself().await?;
    // Asked once, here, because the id differs on every site.
    cfg.epic_field = client.epic_link_field().await;
    secrets::set(secrets::JIRA, &token)?;
    state.config.update(|c| c.jira = Some(cfg))?;
    Ok(who.display_name)
}

/// Every epic on a project, not just the ones already carrying work.
///
/// The New task dialog offered whatever epics happened to appear in the user's
/// own issue list, which is the epics that already have tickets on them — the
/// least useful set when the point of the dialog is to file the first one.
/// Asked of Jira directly instead, by the hierarchy level that means "epic"
/// everywhere rather than by a name that means it only here.
#[tauri::command]
pub async fn jira_epics(
    state: State<'_, AppState>,
    project_key: String,
) -> Result<Vec<jira::Issue>> {
    let types = jira_issue_types_inner(&state, false).await?;
    let names: Vec<String> = types
        .iter()
        .filter(|t| t.hierarchy_level >= 1)
        .map(|t| jira::jql_string(&t.name))
        .collect();
    if names.is_empty() || project_key.trim().is_empty() {
        return Ok(Vec::new());
    }

    let (client, _) = jira_client(&state)?;
    let jql = format!(
        "project = {} AND issuetype in ({}) AND statusCategory != Done ORDER BY key DESC",
        jira::jql_string(project_key.trim()),
        names.join(", "),
    );
    Ok(client.search(&jql, 200).await?.issues)
}

/// Every issue type this Jira defines, with its own icon. Nothing about types
/// is hardcoded — a site with custom types renders exactly as it does in Jira.
#[tauri::command]
pub async fn jira_issue_types(
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> Result<Vec<jira::IssueType>> {
    jira_issue_types_inner(&state, refresh == Some(true)).await
}

pub(crate) async fn jira_issue_types_inner(
    state: &AppState,
    refresh: bool,
) -> Result<Vec<jira::IssueType>> {
    if !refresh {
        if let Some(cached) = state.jira_types.lock().clone() {
            return Ok(cached);
        }
    }
    let (client, _) = jira_client(state)?;
    let types = client.issue_types().await?;
    *state.jira_types.lock() = Some(types.clone());
    Ok(types)
}

#[tauri::command]
pub async fn jira_issues(state: State<'_, AppState>) -> Result<jira::Page> {
    learn_epic_field(&state).await;
    let (client, cfg) = jira_client(&state)?;
    let jql = cfg
        .jql
        .clone()
        .unwrap_or_else(|| jira::default_jql(cfg.project_key.as_deref()));
    // Your own queue should be all of it: this is the list the app groups into
    // epics and reasons about, and a silent cut makes that grouping wrong.
    client.search(&jql, 500).await
}

/// Find this site's Epic Link field once, for a connection made before the app
/// knew to ask. Sites that have no such field are asked again next time, which
/// is one cheap request and keeps the config free of "we looked and found
/// nothing" bookkeeping.
async fn learn_epic_field(state: &AppState) {
    let Ok((client, cfg)) = jira_client(state) else {
        return;
    };
    if cfg.epic_field.is_some() {
        return;
    }
    if let Some(field) = client.epic_link_field().await {
        let _ = state.config.update(|c| {
            if let Some(j) = c.jira.as_mut() {
                j.epic_field = Some(field.clone());
            }
        });
    }
}

/// Look past your own queue: unassigned work, or anyone else's.
///
/// The JQL is built here rather than in the UI so what is typed stays a search
/// term. Results are capped: this is for finding a ticket, not for paging
/// through a backlog.
#[tauri::command]
pub async fn jira_browse(
    state: State<'_, AppState>,
    text: Option<String>,
    whose: String,
    include_done: Option<bool>,
    types: Option<Vec<String>>,
) -> Result<jira::Page> {
    let (client, cfg) = jira_client(&state)?;
    let jql = jira::browse_jql(
        cfg.project_key.as_deref(),
        text.as_deref(),
        jira::Whose::parse(&whose),
        include_done.unwrap_or(false),
        &types.unwrap_or_default(),
    );
    // A search is refined rather than read end to end, so it stays capped —
    // but it now says when there was more.
    client.search(&jql, 100).await
}

#[tauri::command]
pub async fn jira_transitions(
    state: State<'_, AppState>,
    key: String,
) -> Result<Vec<jira::Transition>> {
    let (client, _) = jira_client(&state)?;
    client.transitions(&key).await
}

#[tauri::command]
pub async fn jira_transition(
    state: State<'_, AppState>,
    key: String,
    transition_id: String,
) -> Result<()> {
    let (client, _) = jira_client(&state)?;
    client.transition(&key, &transition_id).await
}

/// The opening prompt. For a multi-repo task it also spells out the layout, so
/// an agent at the task root knows what the sibling folders are.
fn ticket_prompt(issue: &jira::Issue, task: &Task, repos: &[(String, String)]) -> String {
    let description = if issue.description.trim().is_empty() {
        "(no description on the ticket)"
    } else {
        issue.description.trim()
    };

    let mut prompt = format!(
        "You are working on Jira issue {} ({}).\n\nTitle: {}\n\nDescription:\n{description}\n\n",
        issue.key, issue.url, issue.summary,
    );

    if !repos.is_empty() {
        prompt.push_str(&format!(
            "You are in the task folder, not in a repository. {} checked out as {} inside it, on branch `{}`:\n",
            if repos.len() == 1 { "One repository is" } else { "Each repository is" },
            if repos.len() == 1 { "a folder" } else { "sibling folders" },
            task.branch,
        ));
        for (folder, origin) in repos {
            prompt.push_str(&format!("  {folder}/  — {origin}\n"));
        }
        prompt.push_str(
            "\nRun git and each repository's own tests from inside its folder. More \
             repositories can be added to the task later, with the add_repo tool if you \
             find you need one, and they appear here as further folders.\n\n",
        );
    }

    prompt.push_str(
        "Start by exploring the relevant code, then implement the change. Ask before making sweeping refactors.",
    );
    prompt
}

/// The repos in a task, as (folder, origin path) pairs for the prompt.
fn task_repos(state: &AppState, task: &Task) -> Vec<(String, String)> {
    state
        .config
        .checkouts_of(&task.id)
        .iter()
        .map(|c| {
            let folder = PathBuf::from(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let origin = state
                .config
                .project(&c.project_id)
                .map(|p| p.path)
                .unwrap_or_default();
            (folder, origin)
        })
        .collect()
}

/// The opening prompt for an agent started on an existing task.
///
/// Used to prefill the launch dialog, so the text on screen is the text that
/// gets sent. When the task came from a ticket this refetches it, so an agent
/// started later gets the same briefing as the first one rather than just the
/// task's title.
#[tauri::command]
pub async fn task_prompt(state: State<'_, AppState>, task_id: String) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let repos = task_repos(&state, &task);

    if let Some(key) = task.issue_key.as_deref() {
        if let Ok((client, _)) = jira_client(&state) {
            if let Ok(issue) = client.issue(key).await {
                return Ok(ticket_prompt(&issue, &task, &repos));
            }
        }
    }

    // No ticket, or Jira unreachable: describe what we do know.
    let mut prompt = format!("Task: {}\n\n", task.name);
    if let Some(url) = &task.issue_url {
        prompt.push_str(&format!("Ticket: {url}\n\n"));
    }
    if repos.len() > 1 {
        prompt.push_str(&format!(
            "This task spans {} repositories, checked out as sibling folders in your \
             working directory, all on branch `{}`:\n",
            repos.len(),
            task.branch,
        ));
        for (folder, origin) in &repos {
            prompt.push_str(&format!("  {folder}/  — {origin}\n"));
        }
        prompt.push('\n');
    }
    prompt.push_str(
        "Start by exploring the relevant code, then implement the change. Ask before \
         making sweeping refactors.",
    );
    Ok(prompt)
}

/// Where an agent is asked to leave a drafted pull request description.
///
/// In the task folder, never a worktree: a generated file inside a checkout
/// would show up in the very diff the description is about.
fn pr_draft_path(state: &AppState, task: &Task) -> Option<PathBuf> {
    agent_file_dir(state, task).map(|d| d.join("PR_DESCRIPTION.md"))
}

/// Generated files that are noise at review time: machine-written, enormous,
/// and never what a reviewer is asked to look at. They stay in the summary of
/// changed files but are kept out of the diff body, so the model spends its
/// attention — and the request its time — on real code.
const GENERATED: &[&str] = &[
    ":!*package-lock.json",
    ":!*yarn.lock",
    ":!*pnpm-lock.yaml",
    ":!*bun.lockb",
    ":!*bun.lock",
    ":!*Cargo.lock",
    ":!*composer.lock",
    ":!*Gemfile.lock",
    ":!*poetry.lock",
    ":!*go.sum",
];

/// How much diff to send. Past this a description stops getting better and the
/// request only gets slower, so the tail is dropped and the model is told.
const DIFF_BUDGET: usize = 60_000;

/// One repository's place in a task, resolved from config before any git runs.
struct Reviewed {
    repo: String,
    dir: PathBuf,
    base: String,
    base_commit: Option<String>,
}

fn reviewed_repos(state: &AppState, task: &Task) -> Vec<Reviewed> {
    state
        .config
        .checkouts_of(&task.id)
        .into_iter()
        .map(|c| Reviewed {
            repo: state
                .config
                .project(&c.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "repo".into()),
            dir: PathBuf::from(&c.path),
            base: c.base,
            base_commit: c.base_commit,
        })
        .collect()
}

/// What the work looks like from outside: commit subjects, the file summary,
/// and as much of the reviewable diff as fits in the budget.
///
/// Takes resolved repositories rather than the config, so the git calls — a
/// handful of subprocesses, and a slow one on a large tree — can be made off
/// the async runtime instead of holding one of its workers.
fn review_context(repos: &[Reviewed]) -> String {
    let mut out = String::new();
    let mut diff = String::new();

    for checkout in repos {
        let dir = checkout.dir.clone();
        let repo = checkout.repo.clone();
        // The recorded branch point where there is one, so the description
        // covers what this branch did and not what its base has done since.
        let merge_base = git::baseline(&dir, &checkout.base, checkout.base_commit.as_deref());

        out.push_str(&format!("\n## {repo}\n\n"));
        if let Ok(log) = git::run(&dir, &["log", "--no-color", "--format=- %s", &format!("{merge_base}..HEAD")]) {
            if !log.trim().is_empty() {
                out.push_str("Commits:\n");
                out.push_str(log.trim());
                out.push('\n');
            }
        }
        if let Ok(stat) = git::run(&dir, &["diff", "--no-color", "--stat", &merge_base]) {
            if !stat.trim().is_empty() {
                out.push_str("\nFiles changed:\n```\n");
                out.push_str(stat.trim());
                out.push_str("\n```\n");
            }
        }

        let mut args = vec!["diff", "--no-color", &merge_base, "--", "."];
        args.extend_from_slice(GENERATED);
        let mut patch = git::run(&dir, &args).unwrap_or_default();
        if patch.trim().is_empty() {
            // A dependency bump is all generated files. Filtering them out
            // would leave nothing to describe, so in that case they are the
            // change and the whole diff goes through.
            patch = git::run(&dir, &["diff", "--no-color", &merge_base]).unwrap_or_default();
        }
        if !patch.trim().is_empty() {
            diff.push_str(&format!("\n### {repo}\n\n```diff\n{}\n```\n", patch.trim()));
        }
    }

    out.push_str("\n# Diff\n");
    if diff.len() > DIFF_BUDGET {
        // Back off to a character boundary first — slicing into the middle of
        // a multi-byte character panics, and a diff is full of them — then to
        // a line boundary, so the last hunk shown is readable.
        let mut end = DIFF_BUDGET;
        while end > 0 && !diff.is_char_boundary(end) {
            end -= 1;
        }
        let cut = diff[..end].rfind('\n').unwrap_or(end);
        out.push_str(&diff[..cut]);
        out.push_str(
            "\n\n(The diff was longer than fits here and is cut off. Describe what you\
             \ncan see, and say that you only reviewed part of it.)\n",
        );
    } else {
        out.push_str(&diff);
    }
    out
}

#[derive(Clone, Serialize)]
struct DraftChunk<'a> {
    task_id: &'a str,
    text: &'a str,
}

/// Draft the pull request description without disturbing the working agent.
///
/// The obvious implementation — type the request into the agent that did the
/// work — is slow and fragile: that session carries a large context, it may be
/// mid-turn or sitting on a permission prompt, and the answer has to travel
/// back through a file. This asks a fresh, cheap, one-shot model instead and
/// hands it the diff directly, so nothing has to be discovered and no tools
/// have to run. `request_pr_description` remains for the case where the agent's
/// own account of the work is worth the wait.
#[tauri::command]
pub async fn draft_pr_description(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let program = shellenv::which("claude")
        .ok_or_else(|| Error::NotFound("claude is not on your PATH".into()))?;

    let repos = reviewed_repos(&state, &task);
    let branch = task.branch.clone();
    let issue_key = task.issue_key.clone();

    // The same directory an interactive agent for this task would get, from
    // the helper that decides it, rather than a second opinion that will not
    // follow when that one changes its mind.
    let cwd = PathBuf::from(resolve_scope(&state, &task, None)?.0);

    let env = shellenv::user_env().clone();
    let out = tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};

        let prompt = format!(
        concat!(
            "Write the body of a pull request description for the work on branch ",
            "`{branch}`{key}.\n\n",
            "It is for a reviewer who has not seen this work and was not part of the ",
            "conversation that produced it. Say what changed and why, call out anything ",
            "risky or worth a closer look, and note what is not covered. Do not walk ",
            "through the diff file by file, and do not pad it — a few short sections is ",
            "right for most changes.\n\n",
            "Reply with the Markdown body and nothing else: no preamble, no closing ",
            "remark, no code fence around the whole thing.\n\n",
            "# The change\n{context}\n",
        ),
        branch = branch,
        key = issue_key
            .as_ref()
            .map(|k| format!(" for {k}"))
            .unwrap_or_default(),
        context = review_context(&repos),
        );

        let mut child = Command::new(&program)
            .args([
                "-p",
                // Cheap and fast: this is a summarising job, not a reasoning one.
                "--model",
                "haiku",
                // Connecting to MCP servers is the single largest part of a cold
                // start, and this run needs none of them.
                "--strict-mcp-config",
                // Streamed, so the description appears as it is written rather
                // than all at once at the end.
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
                // Everything it needs is in the prompt. Without this it may go
                // reading the repository and turn seconds into minutes.
                "--disallowed-tools",
                "Bash",
                "Read",
                "Edit",
                "Write",
                "Glob",
                "Grep",
                "WebFetch",
                "WebSearch",
            ])
            .current_dir(&cwd)
            .envs(&env)
            // Nothing here is worth thinking about first, and the thinking block
            // is dead time the reader spends watching a spinner.
            .env("MAX_THINKING_TOKENS", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Other(format!("could not run claude: {e}")))?;

        // Both pipes are pumped on their own threads. The prompt carries a whole
        // diff, which is larger than a pipe buffer, so writing it inline would
        // block before claude has said anything — and each side would then be
        // waiting on the other.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Other("claude took no input".into()))?;
        std::thread::spawn(move || {
            let _ = stdin.write_all(prompt.as_bytes());
        });

        let errors = child.stderr.take().map(|e| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                let _ = std::io::Read::read_to_string(&mut BufReader::new(e), &mut buf);
                buf
            })
        });

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("claude produced no output".into()))?;

        let mut streamed = String::new();
        let mut result = None;
        let mut reported = None;
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("stream_event") => {
                    let delta = &v["event"]["delta"];
                    if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                        continue;
                    }
                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                        streamed.push_str(text);
                        let _ = app.emit(
                            "pr:draft",
                            DraftChunk { task_id: &task_id, text },
                        );
                    }
                }
                Some("result") => {
                    let text = v.get("result").and_then(Value::as_str).unwrap_or_default();
                    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                        reported = Some(text.to_string());
                    } else {
                        result = Some(text.to_string());
                    }
                }
                _ => {}
            }
        }

        let status = child
            .wait()
            .map_err(|e| Error::Other(format!("claude did not finish: {e}")))?;
        let stderr = errors
            .and_then(|h| h.join().ok())
            .unwrap_or_default();

        if let Some(why) = reported {
            return Err(Error::Other(format!("claude: {}", why.trim())));
        }
        if !status.success() {
            let why = stderr.trim();
            return Err(Error::Other(if why.is_empty() {
                "claude could not draft the description".into()
            } else {
                format!("claude: {why}")
            }));
        }
        // The final message is authoritative; the deltas are what was shown.
        Ok(result.unwrap_or(streamed).trim().to_string())
    })
    .await
    .map_err(|e| Error::Other(format!("drafting was interrupted: {e}")))??;

    if out.is_empty() {
        return Err(Error::Other("claude returned an empty description".into()));
    }
    Ok(out)
}

/// Ask a running agent to write the pull request description.
///
/// The agent has just done the work, so it knows things the diff does not:
/// what it tried, what it deliberately left out, where a reviewer should look
/// hardest. It writes to a file rather than the terminal so the app can pick
/// the text up cleanly.
#[tauri::command]
pub fn request_pr_description(
    state: State<AppState>,
    task_id: String,
    pane_id: String,
) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let path = pr_draft_path(&state, &task).ok_or_else(|| {
        Error::Other(
            "this task has no folder outside its worktrees to write the draft into".into(),
        )
    })?;

    // A stale draft would look like an instant answer.
    let _ = std::fs::remove_file(&path);

    let repos: Vec<String> = state
        .config
        .checkouts_of(&task_id)
        .iter()
        .filter_map(|c| state.config.project(&c.project_id).ok().map(|p| p.name))
        .collect();

    let prompt = format!(
        "Write the pull request description for the work on branch `{}`{}.\n\n         Write it for a reviewer who has not seen any of this and was not in the          conversation. Cover: what changed and why, anything you decided against or          left unfinished, and where review effort is best spent. Mention what you          verified and what you did not. Do not pad it, and do not restate the diff          file by file.\n\n         Base it on the actual diff{}. Save it as Markdown to:\n{}\n\n         Write only that file, change nothing else, and tell me when it is saved.",
        task.branch,
        task.issue_key
            .as_ref()
            .map(|k| format!(" for {k}"))
            .unwrap_or_default(),
        if repos.len() > 1 {
            format!(" across {}", repos.join(", "))
        } else {
            String::new()
        },
        path.display(),
    );

    state.ptys.write(&pane_id, &prompt)?;
    state.ptys.write(&pane_id, "\r")?;
    Ok(path.to_string_lossy().to_string())
}

/// The drafted description, if the agent has saved it yet. Reading it takes it.
#[tauri::command]
pub fn take_pr_description(state: State<AppState>, task_id: String) -> Result<Option<String>> {
    let task = state.config.task(&task_id)?;
    let Some(path) = pr_draft_path(&state, &task) else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let _ = std::fs::remove_file(&path);
            Ok(Some(text))
        }
        Err(_) => Ok(None),
    }
}

/// Everything a different agent needs to pick this work up.
///
/// There is no portable session format between the agent CLIs, so nothing can
/// truly "resume" across them. What travels is the state of the work: the
/// ticket, what has changed, and what the outgoing agent was last doing. The
/// last of those comes from the pane's own scrollback, which matters because
/// running out of budget is exactly when an agent cannot summarise itself.
#[tauri::command]
pub async fn handoff_prompt(state: State<'_, AppState>, pane_id: String) -> Result<String> {
    let pane = state.ptys.info(&pane_id)?;
    let task = state.config.task(&pane.task_id)?;

    let mut out =
        String::from("You are taking over work another agent started and could not finish.\n\n");

    // The original briefing, refetched so the ticket is current.
    out.push_str(&task_prompt(state.clone(), task.id.clone()).await?);
    out.push_str("\n\n---\n\n## What has happened so far\n\n");

    let mut any_change = false;
    for checkout in state.config.checkouts_of(&task.id) {
        let dir = PathBuf::from(&checkout.path);
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "unknown".into());

        // From the branch point, like the file list under it: measured against
        // the base branch itself, every commit merged in from elsewhere since
        // would be listed as this branch's own work.
        let point = git::baseline(&dir, &checkout.base, checkout.base_commit.as_deref());
        let commits = git::run(
            &dir,
            &["log", "--oneline", "--no-decorate", &format!("{point}..HEAD")],
        )
        .unwrap_or_default();
        let files = git::changed_files(
            &dir,
            &checkout.base,
            checkout.base_commit.as_deref(),
            git::Scope::Branch,
        ).unwrap_or_default();

        if commits.trim().is_empty() && files.is_empty() {
            continue;
        }
        any_change = true;
        out.push_str(&format!("### {repo}\n"));
        if !commits.trim().is_empty() {
            out.push_str("\nCommits on this branch:\n");
            for line in commits.lines() {
                out.push_str(&format!("- {line}\n"));
            }
        }
        if !files.is_empty() {
            out.push_str("\nWorking tree against the base branch:\n");
            for f in &files {
                out.push_str(&format!("- {} (+{} -{})\n", f.path, f.additions, f.deletions));
            }
        }
        out.push('\n');
    }
    if !any_change {
        out.push_str("Nothing has been committed or changed yet.\n\n");
    }

    // The outgoing agent's own words, as far as they got.
    if let Ok(tail) = state.ptys.transcript(&pane_id, 120) {
        if !tail.trim().is_empty() {
            out.push_str(&format!(
                "## The previous agent's terminal, most recent last\n\n                 This is raw output from {}, not a summary, and may be truncated                  mid-thought.\n\n```\n{tail}\n```\n\n",
                pane.title,
            ));
        }
    }

    out.push_str(concat!(
        "## What to do\n\n",
        "Work out from the diff and the transcript above where the previous agent got ",
        "to, then carry on. Verify its work rather than trusting it — it may have left ",
        "something half-finished. Say what you think was already done before you start ",
        "changing anything.",
    ));
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct NewIssue {
    pub summary: String,
    #[serde(default)]
    pub description: String,
    pub issue_type: String,
    /// Falls back to the parent's project, then to the one in Settings.
    #[serde(default)]
    pub project_key: Option<String>,
    #[serde(default)]
    pub parent_key: Option<String>,
    /// Anything else this project requires, already shaped the way Jira wants
    /// it by the caller, which is the side that has the metadata.
    #[serde(default)]
    pub fields: Option<Value>,
}

/// The project a key belongs to: everything before the first dash.
fn project_of(key: &str) -> Option<String> {
    let (project, number) = key.split_once('-')?;
    (!project.is_empty() && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .then(|| project.to_uppercase())
}

/// Whatever this site calls an ordinary issue.
///
/// Not "Task": that is one site's name for it. Hierarchy level is Jira's own
/// and means the same everywhere — 0 is a standard issue — so the plain type
/// is found rather than assumed, preferring one actually called Task when
/// there is one.
pub(crate) async fn default_issue_type(state: &AppState) -> Result<String> {
    let types = jira_issue_types_inner(state, false).await?;
    let plain: Vec<&jira::IssueType> = types
        .iter()
        .filter(|t| t.hierarchy_level == 0 && !t.subtask)
        .collect();
    plain
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case("task"))
        .or_else(|| plain.first())
        .map(|t| t.name.clone())
        .ok_or_else(|| Error::Other("this Jira defines no ordinary issue type".into()))
}

/// What a project demands before it will accept a new issue of this type.
#[tauri::command]
pub async fn jira_create_fields(
    state: State<'_, AppState>,
    project_key: String,
    issue_type_id: String,
) -> Result<Vec<jira::CreateField>> {
    let (client, _) = jira_client(&state)?;
    client.create_fields(&project_key, &issue_type_id).await
}

/// File a ticket without starting work on it.
///
/// Separate from `jira_create_task` because filing under an epic and opening
/// worktrees are different intentions: adding a ticket to a plan is not saying
/// you will start it now.
#[tauri::command]
pub async fn jira_create_issue(state: State<'_, AppState>, req: NewIssue) -> Result<jira::Issue> {
    let summary = req.summary.trim().to_string();
    if summary.is_empty() {
        return Err(Error::Other("the ticket needs a summary".into()));
    }
    let (client, cfg) = jira_client(&state)?;

    // A sub-task has to be filed in its parent's project, and the parent's key
    // says which that is — so an epic found by searching needs nothing else.
    let project_key = req
        .project_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .or_else(|| req.parent_key.as_deref().and_then(project_of))
        .or_else(|| cfg.project_key.clone())
        .ok_or_else(|| {
            Error::Other("no Jira project to file into — set a project key in Settings".into())
        })?;

    let key = client
        .create_issue(
            &project_key,
            &summary,
            &req.description,
            &req.issue_type,
            req.parent_key.as_deref(),
            &req.fields.clone().unwrap_or(Value::Null),
        )
        .await?;
    client.issue(&key).await
}

#[derive(Debug, Deserialize)]
pub struct NewJiraTask {
    pub summary: String,
    #[serde(default)]
    pub description: String,
    pub issue_type: String,
    /// Falls back to the project configured in Settings.
    #[serde(default)]
    pub project_key: Option<String>,
    /// The epic, or any parent the issue type allows.
    #[serde(default)]
    pub parent_key: Option<String>,
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub branch_suffix: Option<String>,
    #[serde(default)]
    pub fields: Option<Value>,
}

/// File the ticket and open the worktrees in one step.
///
/// The ticket goes first because the key it comes back with is what names the
/// branch, the folder and the task. If the worktrees then fail, the ticket is
/// left standing — deleting someone's issue to tidy up after a git error would
/// be worse than the orphan — and the error says so, naming the key, so the
/// work can be picked up with "start work" once the repositories are sorted.
#[tauri::command]
pub async fn jira_create_task(state: State<'_, AppState>, req: NewJiraTask) -> Result<Started> {
    if req.project_ids.is_empty() {
        return Err(Error::Other("pick at least one repository".into()));
    }
    let summary = req.summary.trim().to_string();
    if summary.is_empty() {
        return Err(Error::Other("the ticket needs a summary".into()));
    }

    let (client, cfg) = jira_client(&state)?;
    let project_key = req
        .project_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .or_else(|| req.parent_key.as_deref().and_then(project_of))
        .or_else(|| cfg.project_key.clone())
        .ok_or_else(|| {
            Error::Other("no Jira project to file into — set a project key in Settings".into())
        })?;

    let key = client
        .create_issue(
            &project_key,
            &summary,
            &req.description,
            &req.issue_type,
            req.parent_key.as_deref(),
            &req.fields.clone().unwrap_or(Value::Null),
        )
        .await?;
    let url = format!("{}/browse/{key}", cfg.base_url.trim_end_matches('/'));

    let task = new_task(
        &state,
        NewTask {
            name: format!("{key} {summary}"),
            project_ids: req.project_ids,
            branch: None,
            branch_suffix: req.branch_suffix,
            issue_key: Some(key.clone()),
            issue_url: Some(url),
            epic_key: req.parent_key.clone(),
        },
    )
    .map_err(|e| {
        Error::Other(format!(
            "{key} was filed in Jira, but its worktrees were not created: {e}. The ticket \
             is still there — start work on it from Tickets once that is sorted."
        ))
    })?;

    let moved = sync_started(&state, &key).await;
    Ok(Started { task, moved })
}

/// Move a ticket into progress alongside the worktrees, if that is wanted.
///
/// Starting work in two places and telling Jira about neither is how a board
/// ends up disagreeing with the app: the ticket reads Open while a branch,
/// a worktree and an agent are all running against it. Best effort — a
/// workflow that will not allow the move, or an account that may not make it,
/// is not a reason to undo a task that was created successfully.
async fn sync_started(state: &AppState, key: &str) -> Option<String> {
    if !state.config.read().ui.sync_jira_status {
        return None;
    }
    let (client, _) = jira_client(state).ok()?;
    match client.start_progress(key).await {
        Ok(moved) => moved,
        Err(e) => {
            eprintln!("could not move {key} into progress: {e}");
            None
        }
    }
}

/// Move a ticket into progress on request, for one that fell out of step.
///
/// Tasks created before this app moved tickets — or while the setting was off,
/// or when the workflow refused — leave a board saying Open next to a branch
/// that is clearly being worked on. This is the one-click way back into step.
#[tauri::command]
pub async fn jira_sync_status(state: State<'_, AppState>, key: String) -> Result<Option<String>> {
    let (client, _) = jira_client(&state)?;
    client.start_progress(&key).await
}

/// A task, and the status its ticket was moved to on the way.
#[derive(Debug, Serialize)]
pub struct Started {
    #[serde(flatten)]
    pub task: Task,
    /// The status Jira was moved to, when it was moved.
    pub moved: Option<String>,
}

/// The one-click path: ticket -> worktree per repo -> agent primed with both
/// the ticket and the layout.
#[tauri::command]
pub async fn jira_start_work(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    project_ids: Vec<String>,
    agent_id: Option<String>,
    branch_suffix: Option<String>,
) -> Result<Started> {
    // A second task for the same ticket would try to check the same branch out
    // twice and fail deep inside git. Unless a suffix asks for a distinct
    // branch, point at what already exists.
    if branch_suffix.as_deref().unwrap_or_default().trim().is_empty() {
        if let Some(existing) = state
            .config
            .read()
            .tasks
            .iter()
            .find(|t| t.issue_key.as_deref() == Some(key.as_str()))
        {
            return Err(Error::Other(format!(
                "{key} already has a task on branch {}. Open that one, or pass a \
                 branch_suffix to work on the ticket a second time.",
                existing.branch
            )));
        }
    }

    let (client, _) = jira_client(&state)?;
    let issue = client.issue(&key).await?;

    let task = new_task(
        &state,
        NewTask {
            name: format!("{} {}", issue.key, issue.summary),
            project_ids,
            branch: None,
            branch_suffix,
            issue_key: Some(issue.key.clone()),
            issue_url: Some(issue.url.clone()),
            epic_key: issue.epic_key.clone(),
        },
    )?;

    if let Some(agent_id) = agent_id {
        let repos = task_repos(&state, &task);

        start_agent(
            &app,
            &state,
            task.id.clone(),
            agent_id,
            None,
            Some(ticket_prompt(&issue, &task, &repos)),
            false,
            None,
            None,
        )?;
    }

    let moved = sync_started(&state, &issue.key).await;
    Ok(Started { task, moved })
}

// ------------------------------------------------------------------ github

fn github_client(state: &AppState) -> Result<(GitHub, GithubConfig)> {
    let cfg = state
        .config
        .read()
        .github
        .ok_or(Error::NotConfigured("GitHub"))?;
    let token = secrets::get(secrets::GITHUB)?.ok_or(Error::NotConfigured("GitHub"))?;
    Ok((GitHub::new(&cfg, &token), cfg))
}

#[tauri::command]
pub async fn github_connect(
    state: State<'_, AppState>,
    api_url: String,
    web_url: String,
    token: String,
) -> Result<String> {
    let cfg = GithubConfig {
        api_url: api_url.trim_end_matches('/').to_string(),
        web_url: web_url.trim_end_matches('/').to_string(),
    };
    let token = stored_or(secrets::GITHUB, &token, "GitHub")?;
    let login = GitHub::new(&cfg, &token).login().await?;
    secrets::set(secrets::GITHUB, &token)?;
    state.config.update(|c| c.github = Some(cfg))?;
    Ok(login)
}

#[derive(Debug, Serialize)]
pub struct CheckoutPr {
    pub checkout_id: String,
    pub repo: String,
    pub pr: Option<crate::integrations::github::PullRequest>,
    pub checks: Vec<crate::integrations::github::CheckRun>,
    /// Every review submitted on `pr`, so the panel can name who said what.
    pub reviews: Vec<crate::integrations::github::Review>,
    /// Earlier pull requests from this same branch, newest first. A branch
    /// abandoned once and retried has them, and losing them off the screen
    /// loses the record of what was already tried.
    pub past: Vec<crate::integrations::github::PullRequest>,
    /// The decision those reviews add up to: "approved", "changes_requested",
    /// "commented" or "none".
    pub verdict: String,
    /// Where this repository's next PR will be opened against. An open PR
    /// keeps whatever base it was created with until it is retargeted, so
    /// this and `pr.base` can differ, and the panel says so when they do.
    pub base: String,
    pub changed: usize,
    pub error: Option<String>,
}

/// Point a checkout's pull requests at a different branch.
///
/// The base is chosen when the worktree is made, from the repository's own
/// default branch, and there was no way to say otherwise — work meant for a
/// long-lived integration branch had to be retargeted by hand on GitHub every
/// time. This decides where the *next* PR is opened; one already open keeps
/// its base until `github_retarget_pr` moves it.
#[tauri::command]
pub async fn set_checkout_base(
    state: State<'_, AppState>,
    checkout_id: String,
    base: String,
) -> Result<()> {
    let base = base.trim().to_string();
    if base.is_empty() {
        return Err(Error::Other("a pull request needs a base branch".into()));
    }
    state.config.update(|c| {
        let Some(found) = c.checkouts.iter_mut().find(|c| c.id == checkout_id) else {
            return Err(Error::NotFound(format!("checkout {checkout_id}")));
        };
        found.base = base.clone();
        // The recorded branch point belongs to the base it was taken from, and
        // `baseline` uses it as-is — keeping it would measure this branch
        // against where it left a branch it is no longer going to. Dropped, so
        // the diff falls back to the merge base with whatever the base is now.
        found.base_commit = None;
        Ok(())
    })?
}

/// What each of a task's branches is measured against, one row per repository.
#[derive(Debug, Serialize)]
pub struct RepoBranchFacts {
    pub checkout_id: String,
    pub repo: String,
    pub base: String,
    #[serde(flatten)]
    pub facts: git::BranchFacts,
}

#[tauri::command]
pub fn task_branch_facts(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<RepoBranchFacts>> {
    let task = state.config.task(&task_id)?;
    Ok(state
        .config
        .checkouts_of(&task_id)
        .into_iter()
        .map(|c| RepoBranchFacts {
            repo: state
                .config
                .project(&c.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "(unknown)".into()),
            facts: git::branch_facts(
                &PathBuf::from(&c.path),
                &task.branch,
                &c.base,
                c.base_commit.as_deref(),
            ),
            base: c.base,
            checkout_id: c.id,
        })
        .collect())
}

/// What this repository could open a pull request against.
#[tauri::command]
pub fn checkout_branches(state: State<'_, AppState>, checkout_id: String) -> Result<Vec<String>> {
    let checkout = state.config.checkout(&checkout_id)?;
    git::remote_branches(&PathBuf::from(&checkout.path))
}

/// Move the open pull request for a checkout onto its current base.
#[tauri::command]
pub async fn github_retarget_pr(
    state: State<'_, AppState>,
    checkout_id: String,
) -> Result<String> {
    let checkout = state.config.checkout(&checkout_id)?;
    let task = state.config.task(&checkout.task_id)?;
    let (client, _) = github_client(&state)?;

    let dir = PathBuf::from(&checkout.path);
    let (owner, name) = git::origin_slug(&dir)?;
    let pr = client
        .pull_for_branch(&owner, &name, &task.branch)
        .await?
        .ok_or_else(|| Error::NotFound(format!("an open PR for {}", task.branch)))?;

    client
        .set_pull_base(&owner, &name, pr.number, &checkout.base)
        .await?;
    Ok(format!("#{} now targets {}", pr.number, checkout.base))
}

/// Every task's rows in one sweep, for the watch that polls in the background.
#[derive(Debug, Serialize)]
pub struct TaskPrs {
    pub task_id: String,
    pub rows: Vec<CheckoutPr>,
}

/// PR state for every repository in the task, one row each.
#[tauri::command]
pub async fn github_task_prs(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<CheckoutPr>> {
    let (client, _) = github_client(&state)?;
    task_prs(&state, &client, &task_id).await
}

/// The same for every task, over one client.
///
/// A task whose rows cannot be read is left out rather than failing the sweep:
/// the watch runs unattended, and one unreadable repo must not blind the rest.
#[tauri::command]
pub async fn github_all_prs(state: State<'_, AppState>) -> Result<Vec<TaskPrs>> {
    let (client, _) = github_client(&state)?;
    let ids: Vec<String> = state
        .config
        .read()
        .tasks
        .iter()
        .map(|t| t.id.clone())
        .collect();

    let mut out = Vec::new();
    for task_id in ids {
        if let Ok(rows) = task_prs(&state, &client, &task_id).await {
            out.push(TaskPrs { task_id, rows });
        }
    }
    Ok(out)
}

async fn task_prs(state: &AppState, client: &GitHub, task_id: &str) -> Result<Vec<CheckoutPr>> {
    let task = state.config.task(task_id)?;
    let mut out = Vec::new();

    for checkout in state.config.checkouts_of(task_id) {
        let dir = PathBuf::from(&checkout.path);
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());
        let changed = git::changed_files(
            &dir,
            &checkout.base,
            checkout.base_commit.as_deref(),
            git::Scope::Branch,
        )
            .map(|f| f.len())
            .unwrap_or(0);

        let mut row = CheckoutPr {
            checkout_id: checkout.id.clone(),
            repo,
            pr: None,
            checks: Vec::new(),
            reviews: Vec::new(),
            past: Vec::new(),
            verdict: "none".into(),
            base: checkout.base.clone(),
            changed,
            error: None,
        };

        // One repo without a GitHub remote should not fail the whole view.
        match git::origin_slug(&dir) {
            Ok((owner, name)) => {
                match client.pulls_for_branch(&owner, &name, &task.branch).await {
                    Ok(mut all) if !all.is_empty() => {
                        // The open one is the one still being decided. With
                        // none open, the newest says what became of the branch.
                        let at = all.iter().position(|p| p.state == "open").unwrap_or(0);
                        let found = all.remove(at);

                        // None of the three needs anything from the others,
                        // and this runs for every repository of every task on
                        // a timer — so they go together rather than in turn.
                        //
                        // The listing carries no comment counts and no merged
                        // flag, which is why the one being shown in full is
                        // fetched again rather than reported with those
                        // silently zeroed.
                        let (checks, reviews, full) = tokio::join!(
                            client.checks(&owner, &name, &task.branch),
                            client.reviews(&owner, &name, found.number),
                            client.pull(&owner, &name, found.number),
                        );

                        row.checks = checks.unwrap_or_default();
                        row.reviews = reviews.unwrap_or_default();
                        row.verdict = github::verdict(&row.reviews).to_string();
                        row.pr = Some(full.ok().unwrap_or(found));
                        row.past = all;
                    }
                    Ok(_) => {}
                    Err(e) => row.error = Some(e.to_string()),
                }
            }
            Err(e) => row.error = Some(e.to_string()),
        }
        out.push(row);
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct OpenedPr {
    pub repo: String,
    pub url: String,
    pub number: u64,
}

/// Push and open a PR in every repository that has changes, then post the whole
/// set back to the Jira ticket and Slack. The ticket is the hub: sibling PRs are
/// linked through it rather than to each other.
#[tauri::command]
pub async fn github_open_prs(
    state: State<'_, AppState>,
    task_id: String,
    title: String,
    body: String,
    draft: bool,
) -> Result<Vec<RepoResult>> {
    let task = state.config.task(&task_id)?;
    let (client, _) = github_client(&state)?;

    let mut results = Vec::new();
    let mut opened: Vec<OpenedPr> = Vec::new();

    for checkout in state.config.checkouts_of(&task_id) {
        let dir = PathBuf::from(&checkout.path);
        let project = state.config.project(&checkout.project_id)?;
        let repo = project.name.clone();

        let changed = git::changed_files(
            &dir,
            &checkout.base,
            checkout.base_commit.as_deref(),
            git::Scope::Branch,
        )
            .map(|f| f.len())
            .unwrap_or(0);
        if changed == 0 {
            results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: true,
                detail: "no changes, skipped".into(),
            });
            continue;
        }

        // Whether the PR came out of this call or was already there. Only the
        // new ones are announced: an existing PR was posted to the ticket and
        // the channel when it was opened, and saying so again on every press
        // of the button is noise that makes the real announcements look alike.
        let outcome: Result<(OpenedPr, bool)> = async {
            git::push(&dir, &task.branch)?;
            let (owner, name) = git::origin_slug(&dir)?;

            let (pr, new) = match client.pull_for_branch(&owner, &name, &task.branch).await? {
                Some(existing) => (existing, false),
                None => (
                    client
                        .create_pull(
                            &owner, &name, &title, &body, &task.branch,
                            &checkout.base, draft,
                        )
                        .await?,
                    true,
                ),
            };
            Ok((
                OpenedPr {
                    repo: repo.clone(),
                    url: pr.url,
                    number: pr.number,
                },
                new,
            ))
        }
        .await;

        match outcome {
            Ok((pr, new)) => {
                results.push(RepoResult {
                    checkout_id: checkout.id,
                    repo,
                    ok: true,
                    detail: if new {
                        format!("#{}", pr.number)
                    } else {
                        format!("#{} already open, pushed", pr.number)
                    },
                });
                if new {
                    opened.push(pr);
                }
            }
            Err(e) => results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: false,
                detail: e.to_string(),
            }),
        }
    }

    if !opened.is_empty() {
        let lines: Vec<String> = opened
            .iter()
            .map(|p| format!("{}: {}", p.repo, p.url))
            .collect();

        if let Some(key) = task.issue_key.as_ref() {
            if let Ok((jira, _)) = jira_client(&state) {
                let text = format!(
                    "Pull request{} for this ticket:\n{}",
                    if opened.len() == 1 { "" } else { "s" },
                    lines.join("\n"),
                );
                let _ = jira.comment(key, &text).await;
            }
        }

        if let Ok(Some((client, cfg))) = slack_for(&state, "prs") {
            let text = format!(
                "*{title}* — {} PR{} opened",
                opened.len(),
                if opened.len() == 1 { "" } else { "s" },
            );
            let context = opened
                .iter()
                .map(|p| format!("<{}|{} #{}>", p.url, p.repo, p.number))
                .collect::<Vec<_>>()
                .join("  ·  ");
            if let Ok(posted) = client.post(&cfg.channel, &text, Some(&context)).await {
                record_post(&state, posted);
            }
        }
    }

    Ok(results)
}

// ------------------------------------------------------------------- slack

/// Remember a posted message so it can be deleted later, newest last.
fn record_post(state: &AppState, posted: Option<crate::integrations::slack::Posted>) {
    let Some(posted) = posted.filter(|p| !p.ts.is_empty()) else {
        return;
    };
    let _ = state.config.update(|c| {
        c.slack_posted.push(posted);
        // Unbounded history would grow the config file forever.
        let len = c.slack_posted.len();
        if len > 100 {
            c.slack_posted.drain(..len - 100);
        }
    });
}

/// What a Slack message is for, so it can be muted on its own.
fn slack_allows(cfg: &SlackConfig, kind: &str) -> bool {
    cfg.enabled
        && match kind {
            "agent_done" => cfg.notify_on_done,
            "prs" => cfg.notify_on_prs,
            "agent_tool" => cfg.allow_agent_posts,
            // Explicit user actions, such as the connection test, are never muted.
            _ => true,
        }
}

/// The client, or None when this kind of message is switched off. Gating lives
/// here rather than in the UI so nothing can route around it.
fn slack_for(state: &AppState, kind: &str) -> Result<Option<(Slack, SlackConfig)>> {
    let (client, cfg) = slack_client(state)?;
    Ok(slack_allows(&cfg, kind).then_some((client, cfg)))
}

fn slack_client(state: &AppState) -> Result<(Slack, SlackConfig)> {
    let cfg = state
        .config
        .read()
        .slack
        .ok_or(Error::NotConfigured("Slack"))?;
    let secret = secrets::get(secrets::SLACK)?.ok_or(Error::NotConfigured("Slack"))?;
    Ok((Slack::new(&secret), cfg))
}

/// Delete messages the app posted. With no `links`, deletes everything it has
/// recorded; otherwise deletes the Slack permalinks given, which is the only
/// way to reach messages posted before the app started recording them.
#[tauri::command]
pub async fn slack_delete_posted(
    state: State<'_, AppState>,
    links: Option<Vec<String>>,
) -> Result<Vec<RepoResult>> {
    let (client, _) = slack_client(&state)?;

    let targets: Vec<crate::integrations::slack::Posted> = match links {
        Some(links) if !links.is_empty() => links
            .iter()
            .filter_map(|l| {
                crate::integrations::slack::parse_permalink(l).or_else(|| {
                    // Bare "channel ts" pairs are accepted too.
                    let (c, t) = l.split_once(char::is_whitespace)?;
                    Some(crate::integrations::slack::Posted {
                        channel: c.trim().to_string(),
                        ts: t.trim().to_string(),
                    })
                })
            })
            .collect(),
        _ => state.config.read().slack_posted,
    };

    if targets.is_empty() {
        return Err(Error::Other(
            "nothing to delete: no recorded messages, and no usable links given".into(),
        ));
    }

    let mut results = Vec::new();
    for t in &targets {
        let (ok, detail) = match client.delete(&t.channel, &t.ts).await {
            Ok(()) => (true, "deleted".to_string()),
            Err(e) => (false, e.to_string()),
        };
        results.push(RepoResult {
            checkout_id: t.ts.clone(),
            repo: t.channel.clone(),
            ok,
            detail,
        });
    }

    // Forget whatever is now gone, so a retry does not report it again.
    let gone: Vec<String> = results
        .iter()
        .filter(|r| r.ok || r.detail == "already gone")
        .map(|r| r.checkout_id.clone())
        .collect();
    state
        .config
        .update(|c| c.slack_posted.retain(|p| !gone.contains(&p.ts)))?;

    Ok(results)
}

/// What the app can see and do in Slack, for when a cleanup is refused.
#[tauri::command]
pub async fn slack_diagnose(state: State<'_, AppState>) -> Result<Value> {
    let (client, cfg) = slack_client(&state)?;
    let (bot_id, scopes) = client.scopes().await?;
    Ok(serde_json::json!({
        "channel": cfg.channel,
        "bot_id": bot_id,
        "scopes": scopes.split(',').map(str::trim).collect::<Vec<_>>(),
        "recorded": state.config.read().slack_posted.len(),
    }))
}

/// Find and delete every message this app posted to its channel, including
/// ones sent before the app started recording them.
#[tauri::command]
pub async fn slack_cleanup(state: State<'_, AppState>, dry_run: bool) -> Result<Value> {
    let (client, cfg) = slack_client(&state)?;
    let (bot_id, scopes) = client.scopes().await?;
    if bot_id.is_empty() {
        return Err(Error::Other(
            "this token is not a bot token, so it has no messages of its own".into(),
        ));
    }

    let channel_id = client.channel_id(&cfg.channel).await.map_err(|e| {
        Error::Other(format!(
            "{e}. The app needs the channels:read scope to find the channel by name \
             (it has: {scopes})"
        ))
    })?;
    let found = client.own_recent(&channel_id, &bot_id).await.map_err(|e| {
        Error::Other(format!(
            "{e}. The app needs the channels:history scope to see its own messages \
             (it has: {scopes})"
        ))
    })?;

    if dry_run {
        return Ok(serde_json::json!({ "would_delete": found.len(), "channel": channel_id }));
    }

    let mut deleted = 0;
    let mut failures = Vec::new();
    for m in &found {
        match client.delete(&m.channel, &m.ts).await {
            Ok(()) => deleted += 1,
            Err(e) => failures.push(format!("{}: {e}", m.ts)),
        }
    }
    state.config.update(|c| c.slack_posted.clear())?;
    Ok(serde_json::json!({ "deleted": deleted, "failed": failures }))
}

#[tauri::command]
pub async fn slack_connect(
    state: State<'_, AppState>,
    secret: String,
    channel: String,
) -> Result<()> {
    // Reconnecting is how the channel or token gets changed; the switches
    // under it were chosen separately and should not have to be chosen again.
    let cfg = SlackConfig {
        channel: channel.clone(),
        ..state.config.read().slack.unwrap_or_default()
    };
    let posted = Slack::new(&secret)
        .post(&channel, "Villain Layer connected. :white_check_mark:", None)
        .await?;
    secrets::set(secrets::SLACK, &secret)?;
    state.config.update(|c| c.slack = Some(cfg))?;
    record_post(&state, posted);
    Ok(())
}

#[tauri::command]
pub async fn slack_notify(
    state: State<'_, AppState>,
    text: String,
    context: Option<String>,
    kind: Option<String>,
) -> Result<bool> {
    // Muting is not an error: the caller carries on, it just stays quiet.
    let Some((client, cfg)) = slack_for(&state, kind.as_deref().unwrap_or("manual"))? else {
        return Ok(false);
    };
    let posted = client.post(&cfg.channel, &text, context.as_deref()).await?;
    record_post(&state, posted);
    Ok(true)
}

#[tauri::command]
pub fn set_slack_prefs(state: State<AppState>, prefs: SlackConfig) -> Result<()> {
    state.config.update(|c| {
        if let Some(existing) = c.slack.as_mut() {
            // The channel is changed through the connect form, not here.
            existing.enabled = prefs.enabled;
            existing.notify_on_done = prefs.notify_on_done;
            existing.notify_on_prs = prefs.notify_on_prs;
            existing.allow_agent_posts = prefs.allow_agent_posts;
        }
    })
}

// ---------------------------------------------------------------- settings

#[derive(Debug, Serialize)]
pub struct Settings {
    pub ui: UiPrefs,
    pub jira: Option<JiraConfig>,
    pub github: Option<GithubConfig>,
    pub slack: Option<SlackConfig>,
    pub worktree_root: String,
    pub worktree_root_is_default: bool,
    pub jira_connected: bool,
    pub github_connected: bool,
    pub slack_connected: bool,
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    let c = state.config.read();
    Settings {
        ui: c.ui.clone(),
        // These configs are only persisted after the token verified, so their
        // presence is the connection state. No keychain read, no password prompt.
        jira_connected: c.jira.is_some(),
        github_connected: c.github.is_some(),
        slack_connected: c.slack.is_some(),
        worktree_root: state.config.worktree_root().to_string_lossy().to_string(),
        worktree_root_is_default: c.worktree_root.is_none(),
        jira: c.jira,
        github: c.github,
        slack: c.slack,
    }
}

#[tauri::command]
pub fn set_ui_prefs(state: State<AppState>, ui: UiPrefs) -> Result<()> {
    // Clamp rather than reject: the UI sends slider values.
    let ui = UiPrefs {
        scale: ui.scale.clamp(0.8, 1.6),
        terminal_font_size: ui.terminal_font_size.clamp(9, 24),
        restore_panes: ui.restore_panes,
        agents_read_panes: ui.agents_read_panes,
        trust_agent_dirs: ui.trust_agent_dirs,
        sync_jira_status: ui.sync_jira_status,
    };
    state.config.update(|c| c.ui = ui)
}

#[tauri::command]
pub fn set_worktree_root(state: State<AppState>, path: Option<String>) -> Result<()> {
    state
        .config
        .update(|c| c.worktree_root = path.filter(|p| !p.trim().is_empty()))
}

#[tauri::command]
pub fn disconnect(state: State<AppState>, which: String) -> Result<()> {
    match which.as_str() {
        "jira" => {
            secrets::delete(secrets::JIRA)?;
            state.config.update(|c| c.jira = None)
        }
        "github" => {
            secrets::delete(secrets::GITHUB)?;
            state.config.update(|c| c.github = None)
        }
        "slack" => {
            secrets::delete(secrets::SLACK)?;
            state.config.update(|c| c.slack = None)
        }
        other => Err(Error::NotFound(format!("integration {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn project(id: &str, group: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            path: format!("/repos/{id}"),
            default_branch: "main".into(),
            group: group.map(str::to_string),
        }
    }

    fn cfg() -> AppConfig {
        AppConfig {
            projects: vec![
                project("web", Some("frontend")),
                project("admin", Some("frontend")),
                project("api-orders", Some("backend")),
                project("api-billing", Some("backend")),
            ],
            ..Default::default()
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn epic_memory_beats_project_memory() {
        let mut c = cfg();
        c.last_repos.insert("epic:ACME-100".into(), strs(&["api-orders", "api-billing"]));
        c.last_repos.insert("project:ACME".into(), strs(&["web"]));

        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-100"));
        assert_eq!(s.project_ids, strs(&["api-orders", "api-billing"]));
        assert_eq!(s.reason.as_deref(), Some("last task under ACME-100"));

        // A ticket in the project but under no known epic falls through.
        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-999"));
        assert_eq!(s.project_ids, strs(&["web"]));
        assert_eq!(s.reason.as_deref(), Some("last ACME ticket"));
    }

    #[test]
    fn falls_back_to_v1_bare_project_keys() {
        let mut c = cfg();
        c.last_repos.insert("ACME".into(), strs(&["web"]));
        let s = suggest_from(&c, Some("ACME-3"), None);
        assert_eq!(s.project_ids, strs(&["web"]));
    }

    #[test]
    fn never_suggests_a_repo_that_was_removed() {
        let mut c = cfg();
        c.last_repos.insert("epic:ACME-100".into(), strs(&["deleted-repo"]));
        c.last_repos.insert("project:ACME".into(), strs(&["deleted-repo", "web"]));

        // The epic remembers a repository that is gone, so that tier resolves
        // to nothing and the cascade continues rather than preselecting an id
        // that matches no repository the user still has.
        let s = suggest_from(&c, Some("ACME-1"), Some("ACME-100"));
        assert_eq!(s.project_ids, strs(&["web"]));
    }

    #[test]
    fn ticket_branches_are_the_key_plus_an_optional_suffix() {
        // Jira task names already start with the key, so folding the name into
        // the branch used to repeat it: acme-1234-acme-1234-fix-the-thing.
        let name = "ACME-1234 Fix the thing";

        assert_eq!(derive_branch(None, Some("ACME-1234"), None, name), "ACME-1234");
        assert_eq!(
            derive_branch(None, Some("ACME-1234"), Some("some feature"), name),
            "ACME-1234-some-feature",
        );
        // The UI sends the suffix exactly as typed; slugifying happens here.
        assert_eq!(
            derive_branch(None, Some("ACME-21215"), Some("Locale Switch!!"), name),
            "ACME-21215-locale-switch",
        );
        // A blank or punctuation-only suffix is the same as none.
        assert_eq!(derive_branch(None, Some("ACME-1234"), Some("   "), name), "ACME-1234");
        assert_eq!(derive_branch(None, Some("ACME-1234"), Some("--"), name), "ACME-1234");
    }

    #[test]
    fn explicit_branch_wins_and_ticketless_tasks_use_the_name() {
        assert_eq!(
            derive_branch(Some("hotfix/prod"), Some("ACME-1"), Some("ignored"), "whatever"),
            "hotfix/prod",
        );
        assert_eq!(derive_branch(Some("  "), Some("ACME-1"), None, "n"), "ACME-1");
        assert_eq!(
            derive_branch(None, None, None, "fix checkout rounding"),
            "villain/fix-checkout-rounding",
        );
    }

    #[test]
    fn slack_switches_gate_each_kind_of_message() {
        let all_on = SlackConfig::default();
        assert!(slack_allows(&all_on, "agent_done"));
        assert!(slack_allows(&all_on, "prs"));
        assert!(slack_allows(&all_on, "agent_tool"));

        // The master switch silences everything, including explicit actions.
        let muted = SlackConfig { enabled: false, ..SlackConfig::default() };
        for kind in ["agent_done", "prs", "agent_tool", "manual"] {
            assert!(!slack_allows(&muted, kind), "{kind} should be muted");
        }

        // Each switch is independent of the others.
        let no_agents = SlackConfig { allow_agent_posts: false, ..SlackConfig::default() };
        assert!(!slack_allows(&no_agents, "agent_tool"));
        assert!(slack_allows(&no_agents, "agent_done"));
        assert!(slack_allows(&no_agents, "prs"));

        let no_prs = SlackConfig { notify_on_prs: false, ..SlackConfig::default() };
        assert!(!slack_allows(&no_prs, "prs"));
        assert!(slack_allows(&no_prs, "agent_done"));

        // A connection test is an explicit user action, never an event.
        assert!(slack_allows(&no_prs, "manual"));
    }

    #[test]
    fn suggests_nothing_when_there_is_no_signal() {
        let s = suggest_from(&cfg(), Some("ACME-1"), None);
        assert!(s.project_ids.is_empty());
        assert!(s.reason.is_none());
    }
    fn saved(id: &str, task: &str, kind: &str, cwd: &str) -> SavedPane {
        SavedPane {
            id: id.into(),
            task_id: task.into(),
            checkout_id: None,
            kind: kind.into(),
            agent_id: if kind == "agent" { Some("claude".into()) } else { None },
            cwd: Some(cwd.into()),
        }
    }

    #[test]
    fn a_key_says_which_project_it_belongs_to() {
        // An epic found by searching carries its project in its key, which is
        // what lets a ticket be filed under it with nothing else configured.
        assert_eq!(project_of("ACME-19335").as_deref(), Some("ACME"));
        assert_eq!(project_of("sre-1189").as_deref(), Some("SRE"));
        assert_eq!(project_of("ACME-19335-suffix"), None);
        assert_eq!(project_of("ACME-"), None);
        assert_eq!(project_of("-1"), None);
        assert_eq!(project_of("nodash"), None);
    }

    #[test]
    fn the_same_pane_listed_twice_is_only_restored_once() {
        // The doubling wrote each pane under its own id, twice.
        let doubled = vec![
            saved("1", "t", "agent", "/w"),
            saved("1", "t", "agent", "/w"),
            saved("2", "t", "shell", "/w"),
            saved("2", "t", "shell", "/w"),
        ];
        assert_eq!(panes_to_restore(doubled, RESTORE_LIMIT).len(), 2);
    }

    #[test]
    fn several_shells_in_one_folder_all_come_back() {
        // Three shells in the same worktree are three shells someone opened on
        // purpose. Treating them as duplicates of each other silently threw two
        // of them away at every launch.
        let shells = vec![
            saved("1", "t", "shell", "/w"),
            saved("2", "t", "shell", "/w"),
            saved("3", "t", "shell", "/w"),
        ];
        assert_eq!(panes_to_restore(shells, RESTORE_LIMIT).len(), 3);
    }

    #[test]
    fn restoring_is_capped_however_the_config_got_that_way() {
        // The ceiling is the thing that keeps a bad config from being able to
        // take the machine down, so it holds even when every entry is distinct.
        let many: Vec<SavedPane> = (0..500)
            .map(|i| saved(&i.to_string(), &format!("task{i}"), "shell", &format!("/w{i}")))
            .collect();
        assert_eq!(panes_to_restore(many, RESTORE_LIMIT).len(), RESTORE_LIMIT);
    }

    #[test]
    fn panes_in_different_places_are_all_kept() {
        let distinct = vec![
            saved("1", "t", "agent", "/a"),
            saved("2", "t", "agent", "/b"),
            saved("3", "chat", "agent", "/c"),
        ];
        assert_eq!(panes_to_restore(distinct, RESTORE_LIMIT).len(), 3);
    }

    #[test]
    fn a_dropped_pane_is_only_ever_a_repeat_or_over_the_cap() {
        // Nothing else may be dropped: the message the user sees says
        // "duplicates dropped", and it has to be true.
        let many: Vec<SavedPane> = (0..RESTORE_LIMIT)
            .map(|i| saved(&i.to_string(), "t", "shell", "/same"))
            .collect();
        assert_eq!(panes_to_restore(many, RESTORE_LIMIT).len(), RESTORE_LIMIT);
    }
}
