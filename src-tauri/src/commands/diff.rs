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
        // nothing to add, and git would refuse with "nothing to commit". Asked
        // of `git status`, not the file list, which reads every untracked
        // file to count its lines just to learn whether there are any.
        if git::status(&dir).map(|s| s.dirty_files == 0).unwrap_or(true) {
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


/// What bringing one repository up to date with its base came to.
#[derive(Debug, Serialize)]
pub struct RepoUpdate {
    pub checkout_id: String,
    pub repo: String,
    pub base: String,
    /// "up_to_date", "merged", "conflicts" or "failed".
    pub outcome: &'static str,
    /// Commits the base had that the branch did not.
    pub commits: u32,
    pub conflicts: Vec<String>,
    pub detail: String,
}

/// Merge each repository's base into the task branch.
///
/// Fetched first, all together, so "up to date" means with the remote and
/// not with whatever this clone last heard. Off the command thread: a fetch
/// per repository and a merge that runs the repository's hooks.
#[tauri::command]
pub async fn update_from_base(
    app: AppHandle,
    task_id: String,
    checkout_ids: Option<Vec<String>>,
) -> Result<Vec<RepoUpdate>> {
    super::blocking(app, move |state| update_from_base_inner(state, task_id, checkout_ids)).await
}

fn update_from_base_inner(
    state: &AppState,
    task_id: String,
    checkout_ids: Option<Vec<String>>,
) -> Result<Vec<RepoUpdate>> {
    let checkouts: Vec<_> = state
        .config
        .checkouts_of(&task_id)
        .into_iter()
        .filter(|c| checkout_ids.as_ref().is_none_or(|ids| ids.contains(&c.id)))
        .filter(|c| std::path::Path::new(&c.path).is_dir())
        .collect();
    git::fetch_bases(
        &checkouts
            .iter()
            .map(|c| (PathBuf::from(&c.path), c.base.clone()))
            .collect::<Vec<_>>(),
    );

    let mut out = Vec::new();
    for checkout in checkouts {
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown)".into());
        let mut row = RepoUpdate {
            checkout_id: checkout.id.clone(),
            repo,
            base: checkout.base.clone(),
            outcome: "failed",
            commits: 0,
            conflicts: Vec::new(),
            detail: String::new(),
        };
        match git::update_from_base(&PathBuf::from(&checkout.path), &checkout.base) {
            Ok((updated, target)) => {
                match updated {
                    git::Updated::UpToDate => {
                        row.outcome = "up_to_date";
                        row.detail = "already up to date".into();
                    }
                    git::Updated::Merged { commits } => {
                        row.outcome = "merged";
                        row.commits = commits;
                        row.detail = format!("merged {commits} commit{}", if commits == 1 { "" } else { "s" });
                    }
                    git::Updated::Conflicts(files) => {
                        row.outcome = "conflicts";
                        row.detail = format!(
                            "{} conflicted file{}",
                            files.len(),
                            if files.len() == 1 { "" } else { "s" }
                        );
                        row.conflicts = files;
                    }
                }
                // Recorded even for a conflict: it is where the branch is
                // heading, and `baseline` passes over a point the branch has
                // not reached — so while the merge is unfinished, or if it is
                // abandoned, the diff is measured as it was before.
                if row.outcome != "up_to_date" {
                    let id = checkout.id.clone();
                    state.config.update(|c| {
                        if let Some(found) = c.checkouts.iter_mut().find(|c| c.id == id) {
                            found.base_commit = Some(target.clone());
                        }
                    })?;
                }
            }
            Err(e) => row.detail = e.to_string(),
        }
        // The sidebar's counts are about to be wrong, and would stay wrong
        // until the cache ran out.
        state.status_cache.lock().remove(&checkout.id);
        out.push(row);
    }
    Ok(out)
}

/// Abandon a conflicted update in one repository.
#[tauri::command]
pub async fn abort_merge(app: AppHandle, checkout_id: String) -> Result<()> {
    super::blocking(app, move |state| {
        let checkout = state.config.checkout(&checkout_id)?;
        let done = git::abort_merge(&PathBuf::from(&checkout.path));
        state.status_cache.lock().remove(&checkout_id);
        done
    })
    .await
}

/// One repository's unfinished merge, as the prompt describes it.
pub(crate) struct Conflicted {
    pub repo: String,
    /// The agent is sitting in this repository, so its paths go bare.
    pub here: bool,
    pub base: String,
    pub files: Vec<String>,
}

pub(crate) fn conflict_prompt(branch: &str, repos: &[Conflicted]) -> String {
    let mut out = format!(
        "Bringing `{branch}` up to date with its base branch stopped on merge conflicts. \
         The merge is still in progress in {} — do not start it again or abort it.\n\n",
        if repos.len() == 1 { "that worktree" } else { "each of these worktrees" },
    );
    for r in repos {
        out.push_str(&format!("{} (merging origin/{}):\n", r.repo, r.base));
        for f in &r.files {
            if r.here {
                out.push_str(&format!("- {f}\n"));
            } else {
                out.push_str(&format!("- {}/{f}\n", r.repo));
            }
        }
        out.push('\n');
    }
    out.push_str(
        "For each file, work out what both sides were for and keep both where you can — \
         the base's change is someone else's finished work, and this branch's is ours. \
         Then build and run the tests, `git add` the files and finish with \
         `git commit --no-edit`. If two changes cannot both be kept, stop and tell me \
         which, and why, before choosing.",
    );
    out
}

/// Hand every unfinished merge in the task to an agent.
///
/// With `pane_id`, typed into that agent. Without, returned for starting one,
/// with `scope` saying where it will run.
#[tauri::command]
pub async fn send_merge_conflicts(
    app: AppHandle,
    task_id: String,
    pane_id: Option<String>,
    scope: Option<String>,
) -> Result<String> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        let scope = match &pane_id {
            Some(id) => state.ptys.info(id)?.checkout_id,
            None => scope,
        };
        let repos: Vec<Conflicted> = state
            .config
            .checkouts_of(&task_id)
            .into_iter()
            .filter_map(|c| {
                let dir = PathBuf::from(&c.path);
                if !git::merge_in_progress(&dir) {
                    return None;
                }
                Some(Conflicted {
                    repo: state
                        .config
                        .project(&c.project_id)
                        .map(|p| p.name)
                        .unwrap_or_else(|_| "(unknown)".into()),
                    here: scope.as_deref() == Some(c.id.as_str()),
                    base: c.base,
                    files: git::conflicted_files(&dir),
                })
            })
            .filter(|r| !r.files.is_empty())
            .collect();
        if repos.is_empty() {
            return Err(Error::Other("no repository in this task has conflicts left to resolve".into()));
        }
        let prompt = conflict_prompt(&task.branch, &repos);
        if let Some(id) = &pane_id {
            state.ptys.submit(id, &prompt)?;
        }
        Ok(prompt)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_are_named_from_where_the_agent_stands() {
        let prompt = conflict_prompt(
            "ACME-1",
            &[
                Conflicted { repo: "api".into(), here: true, base: "main".into(), files: vec!["src/a.ts".into()] },
                Conflicted { repo: "web".into(), here: false, base: "develop".into(), files: vec!["b.ts".into()] },
            ],
        );
        assert!(prompt.contains("api (merging origin/main):\n- src/a.ts\n"));
        assert!(prompt.contains("web (merging origin/develop):\n- web/b.ts\n"));
        assert!(prompt.contains("each of these worktrees"));
        assert!(prompt.contains("git commit --no-edit"));
    }
}
