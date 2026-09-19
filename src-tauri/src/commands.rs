//! Everything the frontend can call. Thin orchestration over git, PTYs and the
//! three integrations.
//!
//! The unit of work is a **task**: one ticket, N repositories. Most commands
//! take a task id and fan out over its checkouts.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, State};

use crate::agents;
use crate::config::{
    Checkout, ConfigStore, GithubConfig, JiraConfig, MatchKind, Project, RepoRule,
    RepoSet, SlackConfig, Task, UiPrefs,
};
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::{github::GitHub, jira, jira::Jira, slack::Slack};
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

#[tauri::command]
pub fn add_project(state: State<AppState>, path: String, group: Option<String>) -> Result<Project> {
    register_project(&state, &path, group.as_deref())
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

// ------------------------------------------------- repo sets and Jira rules

#[tauri::command]
pub fn list_repo_sets(state: State<AppState>) -> Vec<RepoSet> {
    state.config.read().repo_sets
}

#[tauri::command]
pub fn save_repo_set(
    state: State<AppState>,
    id: Option<String>,
    name: String,
    project_ids: Vec<String>,
) -> Result<RepoSet> {
    if name.trim().is_empty() {
        return Err(Error::Other("a set needs a name".into()));
    }
    if project_ids.is_empty() {
        return Err(Error::Other("a set needs at least one repository".into()));
    }
    let set = RepoSet {
        id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: name.trim().to_string(),
        project_ids,
    };
    state.config.update(|c| {
        match c.repo_sets.iter_mut().find(|s| s.id == set.id) {
            Some(existing) => *existing = set.clone(),
            None => c.repo_sets.push(set.clone()),
        }
        set.clone()
    })
}

#[tauri::command]
pub fn delete_repo_set(state: State<AppState>, id: String) -> Result<()> {
    state.config.update(|c| c.repo_sets.retain(|s| s.id != id))
}

#[tauri::command]
pub fn list_repo_rules(state: State<AppState>) -> Vec<RepoRule> {
    state.config.read().repo_rules
}

#[tauri::command]
pub fn save_repo_rule(
    state: State<AppState>,
    id: Option<String>,
    kind: MatchKind,
    value: String,
    project_ids: Vec<String>,
) -> Result<RepoRule> {
    if value.trim().is_empty() {
        return Err(Error::Other("a rule needs a component or label".into()));
    }
    let rule = RepoRule {
        id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        kind,
        value: value.trim().to_string(),
        project_ids,
    };
    state.config.update(|c| {
        match c.repo_rules.iter_mut().find(|r| r.id == rule.id) {
            Some(existing) => *existing = rule.clone(),
            None => c.repo_rules.push(rule.clone()),
        }
        rule.clone()
    })
}

#[tauri::command]
pub fn delete_repo_rule(state: State<AppState>, id: String) -> Result<()> {
    state.config.update(|c| c.repo_rules.retain(|r| r.id != id))
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
/// the user's own component/label rules, then what the last ticket in the same
/// epic used, then the same Jira project, then nothing.
#[tauri::command]
pub fn suggest_repos(
    state: State<AppState>,
    issue_key: Option<String>,
    epic_key: Option<String>,
    components: Vec<String>,
    labels: Vec<String>,
) -> RepoSuggestion {
    suggest_from(
        &state.config.read(),
        issue_key.as_deref(),
        epic_key.as_deref(),
        &components,
        &labels,
    )
}

fn suggest_from(
    cfg: &crate::config::AppConfig,
    issue_key: Option<&str>,
    epic_key: Option<&str>,
    components: &[String],
    labels: &[String],
) -> RepoSuggestion {
    let known = |ids: Vec<String>| -> Vec<String> {
        ids.into_iter()
            .filter(|id| cfg.projects.iter().any(|p| &p.id == id))
            .collect()
    };

    // 1. Explicit rules. Several can match; take the union.
    let mut matched: Vec<String> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for rule in &cfg.repo_rules {
        let haystack = match rule.kind {
            MatchKind::Component => components,
            MatchKind::Label => labels,
        };
        if haystack.iter().any(|v| v.eq_ignore_ascii_case(&rule.value)) {
            matched.push(rule.value.clone());
            for id in &rule.project_ids {
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
        }
    }
    let ids = known(ids);
    if !ids.is_empty() {
        return RepoSuggestion {
            project_ids: ids,
            reason: Some(format!("rule for {}", matched.join(", "))),
        };
    }

    // 2. The same epic is a tighter signal than the same project.
    if let Some(epic) = epic_key.filter(|e| !e.is_empty()) {
        let ids = known(cfg.last_repos.get(&format!("epic:{epic}")).cloned().unwrap_or_default());
        if !ids.is_empty() {
            return RepoSuggestion {
                project_ids: ids,
                reason: Some(format!("last task under {epic}")),
            };
        }
    }

    // 3. The same Jira project. Bare keys are what v1 wrote.
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

/// `<worktree_root>/<task-slug>/<repo-name>`, deduped if two repos share a name.
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

    git::add_worktree(
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

    Ok(task)
}

/// Add a repository to a task that is already in flight.
#[tauri::command]
pub fn add_checkout(
    state: State<AppState>,
    task_id: String,
    project_id: String,
) -> Result<Checkout> {
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
    create_checkout(&state, &task, &project, &mut taken)
}

#[tauri::command]
pub fn remove_checkout(state: State<AppState>, checkout_id: String, force: bool) -> Result<()> {
    let checkout = state.config.checkout(&checkout_id)?;
    let project = state.config.project(&checkout.project_id)?;

    state.ptys.close_checkout(&checkout_id);
    let _ = git::remove_worktree(&PathBuf::from(&project.path), &checkout.path, force);

    state
        .config
        .update(|c| c.checkouts.retain(|ch| ch.id != checkout_id))
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

    // Only tidies the task directory if nothing else lives there.
    let _ = std::fs::remove_dir(&task.root);

    state.config.update(|c| {
        c.tasks.retain(|t| t.id != id);
        c.checkouts.retain(|ch| ch.task_id != id);
    })?;
    Ok(results)
}

/// Worktrees that exist on disk but are not part of any task yet.
#[tauri::command]
pub fn scan_worktrees(
    state: State<AppState>,
    project_id: String,
) -> Result<Vec<git::WorktreeEntry>> {
    let project = state.config.project(&project_id)?;
    let known: Vec<String> = state
        .config
        .read()
        .checkouts
        .iter()
        .map(|c| c.path.clone())
        .collect();

    Ok(git::list_worktrees(&PathBuf::from(&project.path))?
        .into_iter()
        .filter(|e| e.path != project.path && !known.contains(&e.path))
        .collect())
}

/// Register an existing worktree as a single-repo task, branch untouched.
#[tauri::command]
pub fn adopt_worktree(
    state: State<AppState>,
    project_id: String,
    path: String,
    name: Option<String>,
) -> Result<Task> {
    let project = state.config.project(&project_id)?;
    let dir = PathBuf::from(&path);
    let branch = git::current_branch(&dir)?;

    let task = Task {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.unwrap_or_else(|| branch.clone()),
        root: path.clone(),
        branch,
        issue_key: None,
        issue_url: None,
        created_at: Utc::now(),
    };
    let checkout = Checkout {
        id: uuid::Uuid::new_v4().to_string(),
        task_id: task.id.clone(),
        project_id: project.id,
        path,
        base: project.default_branch,
    };

    state.config.update(|c| {
        c.tasks.push(task.clone());
        c.checkouts.push(checkout);
    })?;
    Ok(task)
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
/// An explicit checkout wins. Otherwise a single-repo task starts inside its
/// repo (so the agent keeps its git awareness) and a multi-repo task starts at
/// the task root, where every repo is a sibling directory.
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
    match checkouts.as_slice() {
        [] => Err(Error::Other("this task has no repositories".into())),
        [only] => {
            let name = state
                .config
                .project(&only.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "repo".into());
            Ok((only.path.clone(), name, Some(only.id.clone())))
        }
        many => Ok((
            task.root.clone(),
            format!("{} repos", many.len()),
            None,
        )),
    }
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
    let task = state.config.task(&task_id)?;
    let (cwd, scope, checkout_id) = resolve_scope(&state, &task, checkout_id.as_deref())?;

    state.ptys.spawn(
        &app,
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
    )
}

#[tauri::command]
pub fn spawn_agent(
    app: AppHandle,
    state: State<AppState>,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    start_agent(&app, &state, task_id, agent_id, checkout_id, prompt, rows, cols)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_agent(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    let (cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;
    let (mut args, initial_input) = agents::launch_args(def, prompt.as_deref());

    // Give the agent the app's own MCP tools. When its cwd is the task root it
    // picks .mcp.json up by itself; when the cwd is a worktree the file has to
    // live elsewhere and be pointed at explicitly.
    if let Some(dir) = agent_file_dir(state, &task) {
        let _ = crate::mcp::write_config(&dir);
        if Path::new(&cwd) != dir {
            if let Some(flag) = def.mcp_config_flag {
                args.push(flag.to_string());
                args.push(dir.join(".mcp.json").to_string_lossy().to_string());
            }
        }
    }

    state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Agent,
            title: format!("{} · {scope}", def.name),
            program,
            args,
            agent_id: Some(agent_id),
            rows,
            cols,
            initial_input,
        },
    )
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

/// A standing agent with no worktree, for questions, ticket drafting and
/// whatever MCP servers the user has configured for their own CLI.
#[tauri::command]
pub fn spawn_chat(
    app: AppHandle,
    state: State<AppState>,
    agent_id: String,
    prompt: Option<String>,
) -> Result<PaneInfo> {
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;
    let (args, initial_input) = agents::launch_args(def, prompt.as_deref());

    state.ptys.spawn(
        &app,
        SpawnOptions {
            task_id: CHAT_TASK_ID.to_string(),
            checkout_id: None,
            cwd: {
                let dir = chat_dir(&state)?;
                // Best effort: a context file that cannot be written is not a
                // reason to refuse to start the agent.
                let _ = write_chat_context(&state, &dir);
                let _ = crate::mcp::write_config(&dir);
                dir.to_string_lossy().to_string()
            },
            kind: PaneKind::Agent,
            title: def.name.to_string(),
            program,
            args,
            agent_id: Some(agent_id),
            rows: None,
            cols: None,
            initial_input,
        },
    )
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

#[tauri::command]
pub fn close_pane(state: State<AppState>, pane_id: String) -> Result<()> {
    state.ptys.close(&pane_id)
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
pub fn diff_files(state: State<AppState>, task_id: String) -> Result<Vec<ChangedFileView>> {
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
        for file in git::changed_files(&dir, &checkout.base).unwrap_or_default() {
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
pub fn diff_file(state: State<AppState>, checkout_id: String, path: String) -> Result<String> {
    let checkout = state.config.checkout(&checkout_id)?;
    git::file_diff(&PathBuf::from(&checkout.path), &checkout.base, &path)
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
        if git::changed_files(&dir, &checkout.base)
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
    let cfg = JiraConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        email,
        project_key,
        jql,
    };
    // Verify before persisting, so a typo never looks like a working setup.
    let who = Jira::new(&cfg, &token).myself().await?;
    secrets::set(secrets::JIRA, &token)?;
    state.config.update(|c| c.jira = Some(cfg))?;
    Ok(who.display_name)
}

/// Every issue type this Jira defines, with its own icon. Nothing about types
/// is hardcoded — a site with custom types renders exactly as it does in Jira.
#[tauri::command]
pub async fn jira_issue_types(
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> Result<Vec<jira::IssueType>> {
    if refresh != Some(true) {
        if let Some(cached) = state.jira_types.lock().clone() {
            return Ok(cached);
        }
    }
    let (client, _) = jira_client(&state)?;
    let types = client.issue_types().await?;
    *state.jira_types.lock() = Some(types.clone());
    Ok(types)
}

#[tauri::command]
pub async fn jira_issues(state: State<'_, AppState>) -> Result<Vec<jira::Issue>> {
    let (client, cfg) = jira_client(&state)?;
    let jql = cfg
        .jql
        .clone()
        .unwrap_or_else(|| jira::default_jql(cfg.project_key.as_deref()));
    client.search(&jql, 50).await
}

#[tauri::command]
pub async fn jira_issue(state: State<'_, AppState>, key: String) -> Result<jira::Issue> {
    let (client, _) = jira_client(&state)?;
    client.issue(&key).await
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

#[tauri::command]
pub async fn jira_comment(state: State<'_, AppState>, key: String, text: String) -> Result<()> {
    let (client, _) = jira_client(&state)?;
    client.comment(&key, &text).await
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

    if repos.len() > 1 {
        prompt.push_str(&format!(
            "This task spans {} repositories, checked out as sibling folders in your working directory, all on branch `{}`:\n",
            repos.len(),
            task.branch,
        ));
        for (folder, origin) in repos {
            prompt.push_str(&format!("  {folder}/  — {origin}\n"));
        }
        prompt.push_str(
            "\nChanges are expected to span more than one of them, so check how they fit together before editing. Run each repository's own tests from inside its folder.\n\n",
        );
    }

    prompt.push_str(
        "Start by exploring the relevant code, then implement the change. Ask before making sweeping refactors.",
    );
    prompt
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
) -> Result<Task> {
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
        let repos: Vec<(String, String)> = state
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
            .collect();

        start_agent(
            &app,
            &state,
            task.id.clone(),
            agent_id,
            None,
            Some(ticket_prompt(&issue, &task, &repos)),
            None,
            None,
        )?;
    }

    Ok(task)
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
    pub changed: usize,
    pub error: Option<String>,
}

/// PR state for every repository in the task, one row each.
#[tauri::command]
pub async fn github_task_prs(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<CheckoutPr>> {
    let task = state.config.task(&task_id)?;
    let (client, _) = github_client(&state)?;
    let mut out = Vec::new();

    for checkout in state.config.checkouts_of(&task_id) {
        let dir = PathBuf::from(&checkout.path);
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());
        let changed = git::changed_files(&dir, &checkout.base)
            .map(|f| f.len())
            .unwrap_or(0);

        let mut row = CheckoutPr {
            checkout_id: checkout.id.clone(),
            repo,
            pr: None,
            checks: Vec::new(),
            changed,
            error: None,
        };

        // One repo without a GitHub remote should not fail the whole view.
        match git::origin_slug(&dir) {
            Ok((owner, name)) => {
                match client.pull_for_branch(&owner, &name, &task.branch).await {
                    Ok(pr) => {
                        if pr.is_some() {
                            row.checks = client
                                .checks(&owner, &name, &task.branch)
                                .await
                                .unwrap_or_default();
                        }
                        row.pr = pr;
                    }
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

        let changed = git::changed_files(&dir, &checkout.base)
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

        let outcome: Result<OpenedPr> = async {
            git::push(&dir, &task.branch)?;
            let (owner, name) = git::origin_slug(&dir)?;

            let pr = match client.pull_for_branch(&owner, &name, &task.branch).await? {
                Some(existing) => existing,
                None => {
                    client
                        .create_pull(
                            &owner, &name, &title, &body, &task.branch,
                            &checkout.base, draft,
                        )
                        .await?
                }
            };
            Ok(OpenedPr {
                repo: repo.clone(),
                url: pr.url,
                number: pr.number,
            })
        }
        .await;

        match outcome {
            Ok(pr) => {
                results.push(RepoResult {
                    checkout_id: checkout.id,
                    repo,
                    ok: true,
                    detail: format!("#{}", pr.number),
                });
                opened.push(pr);
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

/// Messages the app has posted and can still take back.
#[tauri::command]
pub fn slack_posted_messages(
    state: State<AppState>,
) -> Vec<crate::integrations::slack::Posted> {
    state.config.read().slack_posted
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
    let cfg = SlackConfig {
        channel: channel.clone(),
        ..SlackConfig::default()
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
    fn rules_outrank_history_and_union_across_matches() {
        let mut c = cfg();
        c.repo_rules = vec![
            RepoRule {
                id: "r1".into(),
                kind: MatchKind::Component,
                value: "Payments".into(),
                project_ids: strs(&["api-billing"]),
            },
            RepoRule {
                id: "r2".into(),
                kind: MatchKind::Label,
                value: "frontend".into(),
                project_ids: strs(&["web"]),
            },
        ];
        c.last_repos.insert("project:ACME".into(), strs(&["admin"]));

        // Case-insensitive on both sides, and both matches contribute.
        let s = suggest_from(&c, Some("ACME-1"), None, &strs(&["payments"]), &strs(&["FRONTEND"]));
        assert_eq!(s.project_ids, strs(&["api-billing", "web"]));
        assert!(s.reason.unwrap().contains("Payments"));
    }

    #[test]
    fn epic_memory_beats_project_memory() {
        let mut c = cfg();
        c.last_repos.insert("epic:ACME-100".into(), strs(&["api-orders", "api-billing"]));
        c.last_repos.insert("project:ACME".into(), strs(&["web"]));

        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-100"), &[], &[]);
        assert_eq!(s.project_ids, strs(&["api-orders", "api-billing"]));
        assert_eq!(s.reason.as_deref(), Some("last task under ACME-100"));

        // A ticket in the project but under no known epic falls through.
        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-999"), &[], &[]);
        assert_eq!(s.project_ids, strs(&["web"]));
        assert_eq!(s.reason.as_deref(), Some("last ACME ticket"));
    }

    #[test]
    fn falls_back_to_v1_bare_project_keys() {
        let mut c = cfg();
        c.last_repos.insert("ACME".into(), strs(&["web"]));
        let s = suggest_from(&c, Some("ACME-3"), None, &[], &[]);
        assert_eq!(s.project_ids, strs(&["web"]));
    }

    #[test]
    fn never_suggests_a_repo_that_was_removed() {
        let mut c = cfg();
        c.repo_rules = vec![RepoRule {
            id: "r1".into(),
            kind: MatchKind::Component,
            value: "Gone".into(),
            project_ids: strs(&["deleted-repo"]),
        }];
        c.last_repos.insert("project:ACME".into(), strs(&["deleted-repo", "web"]));

        // The rule matches but resolves to nothing, so the cascade continues.
        let s = suggest_from(&c, Some("ACME-1"), None, &strs(&["Gone"]), &[]);
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
    fn suggests_nothing_when_there_is_no_signal() {
        let s = suggest_from(&cfg(), Some("ACME-1"), None, &[], &[]);
        assert!(s.project_ids.is_empty());
        assert!(s.reason.is_none());
    }
}
