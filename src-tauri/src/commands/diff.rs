//! Diff, review notes, commit and push across a task.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::error::{Error, Result};
use crate::git;

use super::AppState;

// -------------------------------------------------------------------- diff

#[derive(Debug, Serialize)]
pub struct ChangedFileView {
    #[serde(flatten)]
    pub file: git::ChangedFile,
    pub checkout_id: String,
    pub repo: String,
}

/// Every change across every repository in the task, tagged with its repo.
///
/// When `commit` is set, the list is that one commit's files in the checkout
/// that contains it — the Diff view's commit picker — rather than a working
/// tree comparison. `scope` is ignored in that case.
#[tauri::command]
pub async fn diff_files(
    app: AppHandle,
    task_id: String,
    scope: Option<git::Scope>,
    commit: Option<String>,
    checkout_id: Option<String>,
) -> Result<Vec<ChangedFileView>> {
    super::blocking(app, move |state| {
        diff_files_inner(state, task_id, scope, commit, checkout_id)
    })
    .await
}

pub(crate) fn diff_files_inner(
    state: &AppState,
    task_id: String,
    scope: Option<git::Scope>,
    commit: Option<String>,
    checkout_id: Option<String>,
) -> Result<Vec<ChangedFileView>> {
    if let (Some(sha), Some(checkout_id)) = (commit.as_deref(), checkout_id.as_deref()) {
        return commit_files_view(state, checkout_id, sha);
    }

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

fn commit_files_view(
    state: &AppState,
    checkout_id: &str,
    sha: &str,
) -> Result<Vec<ChangedFileView>> {
    let checkout = state.config.checkout(checkout_id)?;
    let dir = PathBuf::from(&checkout.path);
    let repo = state
        .config
        .project(&checkout.project_id)
        .map(|p| p.name)
        .unwrap_or_else(|_| "(unknown)".into());
    Ok(git::commit_files(&dir, sha)?
        .into_iter()
        .map(|file| ChangedFileView {
            file,
            checkout_id: checkout.id.clone(),
            repo: repo.clone(),
        })
        .collect())
}

#[tauri::command]
pub async fn diff_file(
    app: AppHandle,
    checkout_id: String,
    path: String,
    scope: Option<git::Scope>,
    commit: Option<String>,
) -> Result<String> {
    super::blocking(app, move |state| diff_file_inner(state, checkout_id, path, scope, commit)).await
}

fn diff_file_inner(
    state: &AppState,
    checkout_id: String,
    path: String,
    scope: Option<git::Scope>,
    commit: Option<String>,
) -> Result<String> {
    let checkout = state.config.checkout(&checkout_id)?;
    let dir = PathBuf::from(&checkout.path);
    if let Some(sha) = commit.as_deref().filter(|s| !s.is_empty()) {
        return git::commit_file_diff(&dir, sha, &path);
    }
    git::file_diff(
        &dir,
        &checkout.base,
        checkout.base_commit.as_deref(),
        scope.unwrap_or_default(),
        &path,
    )
}

#[derive(Debug, Serialize)]
pub struct RepoCommits {
    pub checkout_id: String,
    pub repo: String,
    pub commits: Vec<git::CommitInfo>,
}

/// Commits on each checkout since its branch point, newest first.
///
/// Feeds the Diff view's commit picker. Empty repos are kept in the list so
/// the UI can still name them; they just have nothing to pick.
#[tauri::command]
pub async fn task_commits(app: AppHandle, task_id: String) -> Result<Vec<RepoCommits>> {
    super::blocking(app, move |state| task_commits_inner(state, task_id)).await
}

fn task_commits_inner(state: &AppState, task_id: String) -> Result<Vec<RepoCommits>> {
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
        let commits =
            git::commits_since(&dir, &checkout.base, checkout.base_commit.as_deref())
                .unwrap_or_default();
        out.push(RepoCommits {
            checkout_id: checkout.id,
            repo,
            commits,
        });
    }
    Ok(out)
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

    state.ptys.submit(&pane_id, &prompt)?;
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
///
/// Off the command thread: a commit runs the repository's own pre-commit
/// hooks, which can be a full lint-and-test pass and are under nobody's
/// control here.
#[tauri::command]
pub async fn commit_task(
    app: AppHandle,
    task_id: String,
    message: String,
) -> Result<Vec<RepoResult>> {
    super::blocking(app, move |state| commit_task_inner(state, task_id, message)).await
}

fn commit_task_inner(
    state: &AppState,
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

/// Off the command thread: this is a network round trip per repository, with
/// no timeout of its own. A push to a remote that has gone away used to hold
/// the whole app until git gave up.
#[tauri::command]
pub async fn push_task(app: AppHandle, task_id: String) -> Result<Vec<RepoResult>> {
    super::blocking(app, move |state| push_task_inner(state, task_id)).await
}

fn push_task_inner(state: &AppState, task_id: String) -> Result<Vec<RepoResult>> {
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

