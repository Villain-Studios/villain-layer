//! Diff, review notes, commit and push across a task.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::State;

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

