//! GitHub connect, PR listing, open/retarget.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::config::GithubConfig;
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::github::{self, GitHub};
use crate::secrets;

use super::AppState;
use super::diff::RepoResult;
use super::jira::{jira_client, stored_or};
use super::slack::{record_post, slack_for};

// ------------------------------------------------------------------ github

pub(crate) fn github_client(state: &AppState) -> Result<(GitHub, GithubConfig)> {
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

pub(crate) async fn task_prs(state: &AppState, client: &GitHub, task_id: &str) -> Result<Vec<CheckoutPr>> {
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

