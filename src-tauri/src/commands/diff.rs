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

    let task = state.config.task(&state.ptys.info(&pane_id)?.task_id)?;
    super::hand_over(&state, &task, &pane_id, "REVIEW_COMMENTS.md", &prompt)?;
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
    // Every repository at once: each is a round trip to the remote, and
    // nothing in one waits on another.
    let results = std::thread::scope(|scope| {
        let pushes: Vec<_> = state
            .config
            .checkouts_of(&task_id)
            .into_iter()
            .map(|checkout| {
                let branch = &task.branch;
                scope.spawn(move || {
                    let repo = state
                        .config
                        .project(&checkout.project_id)
                        .map(|p| p.name)
                        .unwrap_or_else(|_| "(unknown)".into());
                    let lease = checkout.push_lease.as_deref();
                    let (ok, detail) = match git::push(&PathBuf::from(&checkout.path), branch, lease) {
                        Ok(replaced) => {
                            pushed(state, &checkout.id);
                            let how = if replaced { "pushed, replacing the pre-rebase branch" } else { "pushed" };
                            (true, how.to_string())
                        }
                        Err(e) => (false, e.to_string()),
                    };
                    RepoResult { checkout_id: checkout.id, repo, ok, detail }
                })
            })
            .collect();
        pushes.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    Ok(results)
}


/// What bringing one repository up to date with its base came to.
#[derive(Debug, Serialize)]
pub struct RepoUpdate {
    pub checkout_id: String,
    pub repo: String,
    pub base: String,
    /// "up_to_date", "updated", "conflicts" or "failed".
    pub outcome: &'static str,
    /// Commits the base had that the branch did not.
    pub commits: u32,
    pub conflicts: Vec<String>,
    pub detail: String,
}

/// A push went through, so a lease taken at the last rebase has been spent.
pub(crate) fn pushed(state: &AppState, checkout_id: &str) {
    let _ = state.config.update(|c| {
        if let Some(found) = c.checkouts.iter_mut().find(|c| c.id == checkout_id) {
            found.push_lease = None;
        }
    });
}

/// One repository to update, and how: each repo its team's way (UPD-7).
#[derive(Debug, Deserialize)]
pub struct UpdatePick {
    pub checkout_id: String,
    pub by: git::UpdateBy,
}

/// Bring each repository's base into the task branch, merging or rebasing.
///
/// Fetched first, all together, so "up to date" means with the remote and
/// not with whatever this clone last heard. A rebase fetches the task branch
/// too: it has to know what is on the remote branch before it rewrites it.
/// Off the command thread: fetches, and an update that runs the repository's
/// hooks.
#[tauri::command]
pub async fn update_from_base(app: AppHandle, task_id: String, picks: Vec<UpdatePick>) -> Result<Vec<RepoUpdate>> {
    super::blocking(app, move |state| update_from_base_inner(state, task_id, picks)).await
}

fn update_from_base_inner(state: &AppState, task_id: String, picks: Vec<UpdatePick>) -> Result<Vec<RepoUpdate>> {
    let task = state.config.task(&task_id)?;
    let checkouts: Vec<_> = state
        .config
        .checkouts_of(&task_id)
        .into_iter()
        .filter(|c| std::path::Path::new(&c.path).is_dir())
        .filter_map(|c| picks.iter().find(|p| p.checkout_id == c.id).map(|p| (c, p.by)))
        .collect();
    // Offered first next time in each repo: a team that rebases rebases
    // every time. One app-wide choice was wrong wherever teams differ.
    state.config.update(|cfg| {
        for (c, by) in &checkouts {
            if let Some(p) = cfg.projects.iter_mut().find(|p| p.id == c.project_id) {
                p.update_by = Some(*by);
            }
        }
    })?;
    // Every repository at once, but base then branch within each: two
    // fetches at once in one repository race for its FETCH_HEAD.
    std::thread::scope(|scope| {
        for (c, by) in &checkouts {
            let (dir, branch) = (PathBuf::from(&c.path), &task.branch);
            scope.spawn(move || {
                git::fetch_bases(&[(dir.clone(), c.base.clone())]);
                if *by == git::UpdateBy::Rebase {
                    git::fetch_branch(&dir, branch);
                }
            });
        }
    });

    let mut out = Vec::new();
    for (checkout, by) in checkouts {
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
        let dir = PathBuf::from(&checkout.path);
        // Read before the rebase: afterwards the local branch no longer
        // follows on from it, and it is the one commit a push may replace.
        let remote = git::remote_tip(&dir, &task.branch);
        match git::update_from_base(&dir, &task.branch, &checkout.base, by, checkout.push_lease.as_deref()) {
            Ok((updated, target)) => {
                match updated {
                    git::Updated::UpToDate => {
                        row.outcome = "up_to_date";
                        row.detail = "already up to date".into();
                    }
                    git::Updated::Applied { commits } => {
                        row.outcome = "updated";
                        row.commits = commits;
                        let s = if commits == 1 { "" } else { "s" };
                        row.detail = match by {
                            git::UpdateBy::Merge => format!("merged {commits} commit{s}"),
                            git::UpdateBy::Rebase => format!("rebased onto {commits} new commit{s}"),
                        };
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
                // not reached — so while the update is unfinished, or if it is
                // abandoned, the diff is measured as it was before.
                if row.outcome != "up_to_date" {
                    let (id, rebased) = (checkout.id.clone(), by == git::UpdateBy::Rebase);
                    let stopped = row.outcome == "conflicts";
                    state.config.update(|c| {
                        if let Some(found) = c.checkouts.iter_mut().find(|c| c.id == id) {
                            found.point_before_update =
                                if stopped { found.base_commit.clone() } else { None };
                            found.base_commit = Some(target.clone());
                            // What the remote held when this rebase began —
                            // the rebase went ahead only if the branch had all
                            // of it, or it was still the last rebase's lease.
                            // Kept from before, one taken at an earlier rebase
                            // went stale the moment anyone pushed after it.
                            // None when the remote no longer has the branch.
                            if rebased {
                                found.push_lease = remote.clone();
                            }
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

/// Abandon a conflicted merge or rebase in one repository.
#[tauri::command]
pub async fn abort_update(app: AppHandle, checkout_id: String) -> Result<()> {
    super::blocking(app, move |state| {
        let checkout = state.config.checkout(&checkout_id)?;
        let done = git::abort_update(&PathBuf::from(&checkout.path));
        state.status_cache.lock().remove(&checkout_id);
        done?;
        // Back where it was measured from before the update began.
        if let Some(before) = checkout.point_before_update {
            state.config.update(|c| {
                if let Some(found) = c.checkouts.iter_mut().find(|c| c.id == checkout_id) {
                    found.base_commit = Some(before.clone());
                    found.point_before_update = None;
                }
            })?;
        }
        Ok(())
    })
    .await
}

/// One repository's unfinished update, as the prompt describes it.
pub(crate) struct Conflicted {
    pub repo: String,
    /// The agent is sitting in this repository, so its paths go bare.
    pub here: bool,
    pub base: String,
    pub by: git::UpdateBy,
    pub files: Vec<String>,
}

pub(crate) fn conflict_prompt(branch: &str, repos: &[Conflicted]) -> String {
    let rebasing = repos.iter().any(|r| r.by == git::UpdateBy::Rebase);
    let merging = repos.iter().any(|r| r.by == git::UpdateBy::Merge);
    let what = match (merging, rebasing) {
        (true, true) => "merge or rebase",
        (false, true) => "rebase",
        _ => "merge",
    };
    let mut out = format!(
        "Bringing `{branch}` up to date with its base branch stopped on conflicts. \
         The {what} is still in progress in {} — do not start it again or abort it.\n\n",
        if repos.len() == 1 { "that worktree" } else { "each of these worktrees" },
    );
    for r in repos {
        let verb = match r.by {
            git::UpdateBy::Merge => "merging",
            git::UpdateBy::Rebase => "rebasing onto",
        };
        out.push_str(&format!("{} ({verb} origin/{}):\n", r.repo, r.base));
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
         Then build and run the tests and `git add` the files. ",
    );
    if merging {
        out.push_str("Finish a merge with `git commit --no-edit`. ");
    }
    if rebasing {
        // The two words git uses are backwards from what they mean in a
        // merge, and an agent that reaches for `--ours` gets the base.
        out.push_str(
            "Continue a rebase with `GIT_EDITOR=true git rebase --continue`; it replays one \
             commit at a time, so it can stop again on a later one — resolve each the same way \
             until it finishes. During a rebase `--ours` is the base and `--theirs` is this \
             branch's commit. Do not push: the history has been rewritten, and the app pushes it \
             only over the commit it was rebased from. ",
        );
    }
    out.push_str(
        "If two changes cannot both be kept, stop and tell me which, and why, before choosing.",
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
                let by = git::in_progress(&dir)?;
                Some(Conflicted {
                    repo: state
                        .config
                        .project(&c.project_id)
                        .map(|p| p.name)
                        .unwrap_or_else(|_| "(unknown)".into()),
                    here: scope.as_deref() == Some(c.id.as_str()),
                    base: c.base,
                    by,
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
            super::hand_over(state, &task, id, "CONFLICTS.md", &prompt)?;
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
                Conflicted { repo: "api".into(), here: true, base: "main".into(), by: git::UpdateBy::Merge, files: vec!["src/a.ts".into()] },
                Conflicted { repo: "web".into(), here: false, base: "develop".into(), by: git::UpdateBy::Merge, files: vec!["b.ts".into()] },
            ],
        );
        assert!(prompt.contains("api (merging origin/main):\n- src/a.ts\n"));
        assert!(prompt.contains("web (merging origin/develop):\n- web/b.ts\n"));
        assert!(prompt.contains("each of these worktrees"));
        assert!(prompt.contains("git commit --no-edit"));
        assert!(!prompt.contains("rebase --continue"));
    }

    #[test]
    fn a_rebase_prompt_says_how_to_continue_and_not_to_push() {
        let prompt = conflict_prompt(
            "ACME-1",
            &[Conflicted { repo: "api".into(), here: false, base: "main".into(), by: git::UpdateBy::Rebase, files: vec!["a.ts".into()] }],
        );
        assert!(prompt.contains("api (rebasing onto origin/main):\n- api/a.ts\n"));
        assert!(prompt.contains("The rebase is still in progress"));
        assert!(prompt.contains("GIT_EDITOR=true git rebase --continue"));
        assert!(prompt.contains("`--ours` is the base"));
        assert!(prompt.contains("Do not push"));
        assert!(!prompt.contains("commit --no-edit"));
    }
}
