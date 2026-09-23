//! GitHub connect, PR listing, open/retarget.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::config::GithubConfig;
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::github::{self, GitHub};
use crate::secrets;

use super::diff::RepoResult;
use super::jira::{jira_client, stored_or};
use super::panes::agent_file_dir;
use super::slack::{record_post, slack_for};
use super::AppState;

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
    review_team: Option<String>,
) -> Result<String> {
    let cfg = GithubConfig {
        api_url: api_url.trim_end_matches('/').to_string(),
        web_url: web_url.trim_end_matches('/').to_string(),
        review_team: review_team
            .as_deref()
            .and_then(github::normalize_review_team),
    };
    let was = state.config.read().github.as_ref().map(|g| g.api_url.clone());
    let token = stored_or(secrets::GITHUB, &token, "GitHub", was.as_deref(), &cfg.api_url)?;
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
    /// Files changed on the branch — or, once its PR has merged, since what
    /// the PR landed.
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

/// Off the command thread: `branch_facts` is half a dozen git calls per
/// repository, and the Diff view asks for all of them again every time its
/// file list is rebuilt.
#[tauri::command]
pub async fn task_branch_facts(app: AppHandle, task_id: String) -> Result<Vec<RepoBranchFacts>> {
    super::blocking(app, move |state| task_branch_facts_inner(state, task_id)).await
}

fn task_branch_facts_inner(state: &AppState, task_id: String) -> Result<Vec<RepoBranchFacts>> {
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
pub async fn checkout_branches(app: AppHandle, checkout_id: String) -> Result<Vec<String>> {
    super::blocking(app, move |state| {
        let checkout = state.config.checkout(&checkout_id)?;
        git::remote_branches(&PathBuf::from(&checkout.path))
    })
    .await
}

/// Move the open pull request for a checkout onto its current base.
#[tauri::command]
pub async fn github_retarget_pr(state: State<'_, AppState>, checkout_id: String) -> Result<String> {
    let checkout = state.config.checkout(&checkout_id)?;
    let task = state.config.task(&checkout.task_id)?;
    let (client, _) = github_client(&state)?;

    let dir = PathBuf::from(&checkout.path);
    let (owner, name) = off_runtime(move || git::origin_slug(&dir)).await??;
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

    // A few tasks at a time rather than one after another: with every task's
    // repos in series a sweep of a dozen tasks took longer than the interval
    // between sweeps. Not all at once either — GitHub throttles a client that
    // opens dozens of requests together.
    use futures_util::stream::{self, StreamExt};
    let state = &*state;
    let client = &client;
    let out = stream::iter(ids)
        .map(|task_id| async move {
            task_prs(state, client, &task_id)
                .await
                .ok()
                .map(|rows| TaskPrs { task_id, rows })
        })
        .buffered(SWEEP_TASKS)
        .filter_map(|t| async move { t })
        .collect::<Vec<_>>()
        .await;
    Ok(out)
}

/// Blocking work — git, mostly — from an async command, on the blocking pool.
async fn off_runtime<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Other(format!("background work failed: {e}")))
}

/// How many tasks the PR sweep asks GitHub about at once.
const SWEEP_TASKS: usize = 4;

/// Pull requests waiting on the signed-in user, and on the configured team.
#[derive(Debug, Serialize)]
pub struct ReviewQueue {
    pub mine: Vec<github::ReviewRequest>,
    /// GitHub had more personal hits than the one page we asked for.
    pub mine_more: bool,
    /// Absent when no review team is configured. Present, with `error` set,
    /// when the team was configured but could not be resolved or searched —
    /// the personal list is still worth showing.
    pub team: Option<TeamReviews>,
}

#[derive(Debug, Serialize)]
pub struct TeamReviews {
    /// `org/slug`.
    pub slug: String,
    pub name: String,
    pub prs: Vec<github::ReviewRequest>,
    pub more: bool,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn github_review_queue(app: AppHandle, state: State<'_, AppState>) -> Result<ReviewQueue> {
    let queue = review_queue(&state).await?;
    crate::news::saw_reviews(&app, &queue);
    Ok(queue)
}

/// The queue itself, for the UI and for the watch that banners what is new.
pub(crate) async fn review_queue(state: &AppState) -> Result<ReviewQueue> {
    let (client, cfg) = github_client(state)?;
    // The two searches need nothing from each other, so they go together;
    // only the de-duplication waits for both.
    let team = async {
        match cfg.review_team.as_deref() {
            Some(raw) => Some(load_team_reviews(&client, &cfg.api_url, raw).await),
            None => None,
        }
    };
    let (mine, team) = tokio::join!(client.search_prs(github::mine_review_query()), team);
    let (mine, mine_more) = mine?;
    let team = team.map(|mut t| {
        github::drop_already_listed(&mut t.prs, &mine);
        t
    });
    Ok(ReviewQueue {
        mine,
        mine_more,
        team,
    })
}

/// The last review team resolved, keyed by the API and the setting it came
/// from.
///
/// A team given without its org is found by walking `/user/teams`, up to ten
/// pages, and that walk was repeated on every poll of the review queue for an
/// answer that changes when the setting does.
static RESOLVED_TEAM: parking_lot::Mutex<Option<(String, String, github::GhTeam)>> =
    parking_lot::Mutex::new(None);

/// The team column must not take the personal one down with it: a missing
/// `read:org` scope, or a slug that matches nothing, is a problem with that
/// column alone.
async fn load_team_reviews(client: &GitHub, api_url: &str, raw: &str) -> TeamReviews {
    let fallback = raw.trim_start_matches('@').to_string();
    let cached = RESOLVED_TEAM
        .lock()
        .as_ref()
        .filter(|(api, setting, _)| api == api_url && setting == raw)
        .map(|(_, _, team)| team.clone());
    let team = match cached {
        Some(team) => team,
        None => match client.resolve_team(raw).await {
            Ok(team) => {
                *RESOLVED_TEAM.lock() = Some((api_url.to_string(), raw.to_string(), team.clone()));
                team
            }
            Err(e) => {
                return TeamReviews {
                    slug: fallback.clone(),
                    name: fallback,
                    prs: Vec::new(),
                    more: false,
                    error: Some(e.to_string()),
                };
            }
        },
    };
    let slug = format!("{}/{}", team.org, team.slug);
    match client
        .search_prs(&github::team_review_query(&team.org, &team.slug))
        .await
    {
        Ok((prs, more)) => TeamReviews {
            slug,
            name: team.name,
            prs,
            more,
            error: None,
        },
        Err(e) => TeamReviews {
            slug,
            name: team.name,
            prs: Vec::new(),
            more: false,
            error: Some(e.to_string()),
        },
    }
}

pub(crate) async fn task_prs(
    state: &AppState,
    client: &GitHub,
    task_id: &str,
) -> Result<Vec<CheckoutPr>> {
    let task = state.config.task(task_id)?;
    let checkouts = state.config.checkouts_of(task_id);

    // What git knows, for every repo, first and off the async workers: these
    // are subprocesses, and on the runtime they held up the MCP server and
    // every other request sharing it.
    let local = {
        let checkouts = checkouts.clone();
        off_runtime(move || {
            checkouts
                .iter()
                .map(|c| {
                    let dir = PathBuf::from(&c.path);
                    // A count, not the list: this runs for every repository of
                    // every task on a timer, and building the list reads each
                    // untracked file.
                    let changed = git::changed_count(
                        &dir,
                        &c.base,
                        c.base_commit.as_deref(),
                        git::Scope::Branch,
                    );
                    (changed, git::origin_slug(&dir))
                })
                .collect::<Vec<_>>()
        })
        .await?
    };

    // Then GitHub, for every repo at once.
    let rows = checkouts.into_iter().zip(local).map(|(checkout, (changed, slug))| {
        let task = &task;
        async move {
            let repo = state
                .config
                .project(&checkout.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "(unknown)".into());

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
            match slug {
                Ok((owner, name)) => {
                    match client.pulls_for_branch(&owner, &name, &task.branch).await {
                        Ok(mut all) if !all.is_empty() => {
                            // The open one is the one still being decided. With
                            // none open, the newest says what became of the branch.
                            let at = all.iter().position(|p| p.state == "open").unwrap_or(0);
                            let found = all.remove(at);

                            if found.state == "open" {
                                // None of the three needs anything from the
                                // others, and this runs for every repository of
                                // every task on a timer — so they go together
                                // rather than in turn.
                                //
                                // The listing carries no comment counts, which is
                                // why the one being shown in full is fetched
                                // again rather than reported with those silently
                                // zeroed.
                                let (checks, reviews, full) = tokio::join!(
                                    // The commit, not the branch name: a name is
                                    // read by the URL, a sha is not.
                                    client.checks(
                                        &owner,
                                        &name,
                                        if found.head_sha.is_empty() { &task.branch } else { &found.head_sha },
                                    ),
                                    client.reviews(&owner, &name, found.number),
                                    client.pull(&owner, &name, found.number),
                                );

                                row.checks = checks.unwrap_or_default();
                                row.reviews = reviews.unwrap_or_default();
                                row.verdict = github::verdict(&row.reviews).to_string();
                                row.pr = Some(full.ok().unwrap_or(found));
                            } else {
                                // Closed is final: its checks and reviews will not
                                // change, and nothing on screen reads them. Three
                                // calls saved per finished repository, on every
                                // sweep, for as long as the task is kept — which
                                // is what keeps a dozen done tasks from eating
                                // the API budget the open ones need.
                                //
                                // A merged one covers everything up to what it
                                // landed. Counted from the branch point, a repo
                                // whose PR had merged never went back to zero, so
                                // a task with one repo merged and another in
                                // review read "partly up for review, one repo
                                // still without a PR".
                                if found.merged {
                                    let (dir, sha) =
                                        (PathBuf::from(&checkout.path), found.head_sha.clone());
                                    if let Ok(Some(n)) = off_runtime(move || {
                                        git::has_commit(&dir, &sha)
                                            .then(|| git::changed_count_from(&dir, &sha))
                                    })
                                    .await
                                    {
                                        row.changed = n;
                                    }
                                }
                                row.pr = Some(found);
                            }
                            row.past = all;
                        }
                        Ok(_) => {}
                        Err(e) => row.error = Some(e.to_string()),
                    }
                }
                Err(e) => row.error = Some(e.to_string()),
            }
            row
        }
    });
    Ok(futures_util::future::join_all(rows).await)
}

/// What reviewers and CI have said on one repository's open pull request.
#[derive(Debug, Serialize)]
pub struct RepoFeedback {
    pub checkout_id: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(flatten)]
    pub feedback: github::PrFeedback,
    pub checks: Vec<github::FailedCheck>,
    pub error: Option<String>,
}

/// How many failed jobs' logs are read per repository. A matrix build that
/// fails everywhere fails for one reason, and forty logs say it forty times.
const LOGS_PER_REPO: usize = 6;

/// Everything said on the task's open pull requests, for picking what to
/// hand the agent.
#[tauri::command]
pub async fn github_pr_feedback(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<RepoFeedback>> {
    let task = state.config.task(&task_id)?;
    let (client, _) = github_client(&state)?;
    let checkouts = state.config.checkouts_of(&task_id);

    let slugs = {
        let dirs: Vec<PathBuf> = checkouts.iter().map(|c| PathBuf::from(&c.path)).collect();
        off_runtime(move || dirs.iter().map(|d| git::origin_slug(d)).collect::<Vec<_>>()).await?
    };

    let rows = checkouts.into_iter().zip(slugs).map(|(checkout, slug)| {
        let (client, task, state) = (&client, &task, &*state);
        async move {
            let repo = state
                .config
                .project(&checkout.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "(unknown)".into());
            let found: Result<Option<RepoFeedback>> = async {
                let (owner, name) = slug?;
                let Some(pr) = client.pull_for_branch(&owner, &name, &task.branch).await? else {
                    return Ok(None);
                };
                // The commit GitHub has, not the local branch: what was checked
                // is what was pushed, and local commits since have no runs yet.
                let at = if pr.head_sha.is_empty() { task.branch.clone() } else { pr.head_sha.clone() };
                let (feedback, checks) = tokio::join!(
                    client.pr_feedback(&owner, &name, pr.number),
                    client.failed_checks(&owner, &name, &at),
                );
                let mut checks = checks.unwrap_or_default();
                // Counted among the checks that have a log to read: six failing
                // external checks listed first left the Actions job without one.
                let jobs: Vec<(usize, u64)> = checks
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.job_id.map(|id| (i, id)))
                    .take(LOGS_PER_REPO)
                    .collect();
                let logs = futures_util::future::join_all(jobs.iter().map(|&(_, id)| {
                    let (owner, name) = (&owner, &name);
                    async move { client.job_log_tail(owner, name, id).await.ok() }
                }))
                .await;
                for (&(i, _), log) in jobs.iter().zip(logs) {
                    checks[i].log = log.filter(|l| !l.is_empty());
                }
                Ok(Some(RepoFeedback {
                    checkout_id: checkout.id.clone(),
                    repo: repo.clone(),
                    number: pr.number,
                    title: pr.title,
                    url: pr.url,
                    feedback: feedback?,
                    checks,
                    error: None,
                }))
            }
            .await;
            match found {
                Ok(row) => row,
                // Said on the row, so one repository GitHub will not answer
                // for does not hide what the others' reviewers wrote.
                Err(e) => Some(RepoFeedback {
                    checkout_id: checkout.id,
                    repo,
                    number: 0,
                    title: String::new(),
                    url: String::new(),
                    feedback: Default::default(),
                    checks: Vec::new(),
                    error: Some(e.to_string()),
                }),
            }
        }
    });
    Ok(futures_util::future::join_all(rows).await.into_iter().flatten().collect())
}

/// One piece of feedback chosen to hand to the agent, sent back as shown.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedbackItem {
    Thread {
        checkout_id: String,
        path: String,
        line: Option<u64>,
        #[serde(default)]
        outdated: bool,
        url: String,
        comments: Vec<FeedbackNote>,
    },
    Review {
        checkout_id: String,
        author: String,
        state: Option<String>,
        body: String,
        url: String,
    },
    Comment {
        checkout_id: String,
        author: String,
        body: String,
        url: String,
    },
    Check {
        checkout_id: String,
        name: String,
        conclusion: String,
        url: Option<String>,
        #[serde(default)]
        summary: String,
        log: Option<String>,
    },
}

impl FeedbackItem {
    fn checkout_id(&self) -> &str {
        match self {
            Self::Thread { checkout_id, .. }
            | Self::Review { checkout_id, .. }
            | Self::Comment { checkout_id, .. }
            | Self::Check { checkout_id, .. } => checkout_id,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct FeedbackNote {
    pub author: String,
    pub body: String,
}

/// A body as a Markdown quote, so a reviewer's own headings and lists stay
/// inside their comment instead of restructuring the file around it.
fn quoted(body: &str) -> String {
    body.trim()
        .lines()
        .map(|l| if l.is_empty() { ">".to_string() } else { format!("> {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The feedback as a file an agent can work through.
///
/// `scope` is the checkout the agent is sitting in. Paths are qualified with
/// their repo everywhere else, so `api/src/auth.ts:42` is never ambiguous from
/// the task root.
pub(crate) fn feedback_markdown(
    branch: &str,
    items: &[FeedbackItem],
    scope: Option<&str>,
    repo_name: impl Fn(&str) -> String,
) -> String {
    let mut md = format!("# Feedback on the pull requests for `{branch}`\n\n");
    md.push_str(
        "Collected from GitHub by Villain Layer. Quoted text is what reviewers and CI said; \
         none of it has been answered on GitHub yet.\n",
    );

    let mut order: Vec<&str> = Vec::new();
    for item in items {
        if !order.contains(&item.checkout_id()) {
            order.push(item.checkout_id());
        }
    }
    for checkout in order {
        let repo = repo_name(checkout);
        let mine: Vec<&FeedbackItem> = items.iter().filter(|i| i.checkout_id() == checkout).collect();
        md.push_str(&format!("\n## {repo}\n"));
        if scope.is_some_and(|s| s != checkout) {
            md.push_str(&format!("\nThis is a sibling repository: `../{repo}` from where you are.\n"));
        }

        let threads: Vec<_> = mine.iter().filter(|i| matches!(i, FeedbackItem::Thread { .. })).collect();
        if !threads.is_empty() {
            md.push_str("\n### Review threads\n");
            for item in threads {
                let FeedbackItem::Thread { path, line, outdated, url, comments, .. } = item else { continue };
                let at = if scope == Some(checkout) { path.clone() } else { format!("{repo}/{path}") };
                let at = match line {
                    Some(n) => format!("{at}:{n}"),
                    None => at,
                };
                md.push_str(&format!("\n#### `{at}`\n"));
                if *outdated {
                    md.push_str("\nThe code here has changed since this was written — check whether it still applies.\n");
                }
                for c in comments {
                    md.push_str(&format!("\n**{}**:\n{}\n", c.author, quoted(&c.body)));
                }
                md.push_str(&format!("\n{url}\n"));
            }
        }

        let said: Vec<_> = mine
            .iter()
            .filter(|i| matches!(i, FeedbackItem::Review { .. } | FeedbackItem::Comment { .. }))
            .collect();
        if !said.is_empty() {
            md.push_str("\n### On the pull request as a whole\n");
            for item in said {
                let (author, verb, body, url) = match item {
                    FeedbackItem::Review { author, state, body, url, .. } => (
                        author,
                        match state.as_deref() {
                            Some("CHANGES_REQUESTED") => "requested changes",
                            Some("APPROVED") => "approved, adding",
                            _ => "reviewed",
                        },
                        body,
                        url,
                    ),
                    FeedbackItem::Comment { author, body, url, .. } => (author, "commented", body, url),
                    _ => continue,
                };
                md.push_str(&format!("\n**{author}** {verb}:\n{}\n\n{url}\n", quoted(body)));
            }
        }

        let checks: Vec<_> = mine.iter().filter(|i| matches!(i, FeedbackItem::Check { .. })).collect();
        if !checks.is_empty() {
            md.push_str("\n### Failing checks\n");
            for item in checks {
                let FeedbackItem::Check { name, conclusion, url, summary, log, .. } = item else { continue };
                md.push_str(&format!("\n#### {name} — {}\n", conclusion.replace('_', " ")));
                if let Some(url) = url {
                    md.push_str(&format!("\n{url}\n"));
                }
                if !summary.trim().is_empty() {
                    md.push_str(&format!("\nWhat the check reported:\n\n```\n{}\n```\n", summary.trim()));
                }
                match log {
                    Some(log) if !log.trim().is_empty() => md.push_str(&format!(
                        "\nThe end of its log, up to where it failed:\n\n```\n{}\n```\n",
                        log.trim()
                    )),
                    _ if summary.trim().is_empty() => {
                        md.push_str("\nNo log could be read — open the link, or run the same step locally.\n")
                    }
                    _ => {}
                }
            }
        }
    }
    md
}

/// "3 review threads, 1 comment and 2 failing checks".
fn feedback_summary(items: &[FeedbackItem]) -> String {
    let count = |f: fn(&FeedbackItem) -> bool| items.iter().filter(|i| f(i)).count();
    let parts: Vec<String> = [
        (count(|i| matches!(i, FeedbackItem::Thread { .. })), "review thread"),
        (count(|i| matches!(i, FeedbackItem::Review { .. } | FeedbackItem::Comment { .. })), "comment"),
        (count(|i| matches!(i, FeedbackItem::Check { .. })), "failing check"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}{}", if n == 1 { "" } else { "s" }))
    .collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Hand the chosen feedback to an agent.
///
/// Written to a file beside the worktrees and pointed at, not typed in: a
/// failing build's log is a hundred lines, and pasted into a TUI it arrives as
/// "[Pasted text +140 lines]" at best and as a hundred submissions at worst.
///
/// With `pane_id`, the prompt is typed into that agent. Without, it is only
/// returned, for starting a new agent with it — `scope` then says where that
/// agent will run.
#[tauri::command]
pub fn send_pr_feedback(
    state: State<AppState>,
    task_id: String,
    pane_id: Option<String>,
    scope: Option<String>,
    items: Vec<FeedbackItem>,
) -> Result<String> {
    if items.is_empty() {
        return Err(Error::Other("nothing chosen to send".into()));
    }
    let task = state.config.task(&task_id)?;
    let scope = match &pane_id {
        Some(id) => state.ptys.info(id)?.checkout_id,
        None => scope,
    };
    let md = feedback_markdown(&task.branch, &items, scope.as_deref(), |id| {
        state
            .config
            .checkout(id)
            .and_then(|c| state.config.project(&c.project_id))
            .map(|p| p.name)
            .unwrap_or_else(|_| "(unknown repo)".into())
    });

    let ask = "Work through every point. Fix what is right; where you think a reviewer is \
               wrong, say so and why instead of quietly skipping it. For a failing check, \
               find the cause before changing anything, and reproduce it locally if you can. \
               Do not reply on GitHub or resolve threads yourself. When you are done, go \
               through the points one by one and say what you did about each.";
    let head = format!(
        "Reviewers and CI have come back on the pull requests for `{}`: {}.",
        task.branch,
        feedback_summary(&items),
    );
    let prompt = match agent_file_dir(&state, &task) {
        Some(dir) => {
            let path = dir.join(FEEDBACK_FILE);
            std::fs::write(&path, &md)?;
            format!("{head} It is all in:\n{}\n\nRead that file. {ask}", path.display())
        }
        // A task from the one-repo layout has no folder outside its worktree,
        // and a file in the worktree is one the agent might commit.
        None => format!("{head}\n\n{md}\n\n{ask}"),
    };

    if let Some(id) = &pane_id {
        state.ptys.submit(id, &prompt)?;
    }
    Ok(prompt)
}

pub(crate) const FEEDBACK_FILE: &str = "PR_FEEDBACK.md";

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

        let (changed, commits) = {
            let (dir, base, point, branch) =
                (dir.clone(), checkout.base.clone(), checkout.base_commit.clone(), task.branch.clone());
            off_runtime(move || {
                (
                    git::changed_count(&dir, &base, point.as_deref(), git::Scope::Branch),
                    git::branch_facts(&dir, &branch, &base, point.as_deref()).commits,
                )
            })
            .await?
        };
        if changed == 0 {
            results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: true,
                detail: "no changes, skipped".into(),
            });
            continue;
        }
        // Changes, but none committed: pushed anyway, GitHub refused the PR
        // with a bare 422 "No commits between".
        if commits == 0 {
            results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: false,
                detail: "only uncommitted changes — commit them first".into(),
            });
            continue;
        }

        // Whether the PR came out of this call or was already there. Only the
        // new ones are announced: an existing PR was posted to the ticket and
        // the channel when it was opened, and saying so again on every press
        // of the button is noise that makes the real announcements look alike.
        let outcome: Result<(OpenedPr, bool)> = async {
            // A push is a network round trip with no timeout of its own; on
            // the async workers it held up the MCP server until git gave up.
            let (owner, name) = {
                let (dir, branch, lease) = (dir.clone(), task.branch.clone(), checkout.push_lease.clone());
                let slug = off_runtime(move || -> Result<Result<(String, String)>> {
                    git::push(&dir, &branch, lease.as_deref())?;
                    Ok(git::origin_slug(&dir))
                })
                .await??;
                // Spent by the push itself, whatever comes after it. Kept
                // because the slug could not be read, it was sent with every
                // later push and refused each one as "someone else pushed".
                super::diff::pushed(&state, &checkout.id);
                slug?
            };

            let (pr, new) = match client.pull_for_branch(&owner, &name, &task.branch).await? {
                Some(existing) => (existing, false),
                None => (
                    client
                        .create_pull(
                            &owner,
                            &name,
                            &title,
                            &body,
                            &task.branch,
                            &checkout.base,
                            draft,
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
                // Said, not swallowed: this only runs for PRs opened by this
                // call, so a retry finds them already open and never posts —
                // the ticket that is meant to be the hub would just not know.
                if let Err(e) = jira.comment(key, &text).await {
                    results.push(RepoResult {
                        checkout_id: key.clone(),
                        repo: key.clone(),
                        ok: false,
                        detail: format!(
                            "the PRs are open, but the comment linking them failed ({e}); add them to the ticket by hand"
                        ),
                    });
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(checkout: &str, path: &str, line: Option<u64>) -> FeedbackItem {
        FeedbackItem::Thread {
            checkout_id: checkout.into(),
            path: path.into(),
            line,
            outdated: false,
            url: "https://gh/t".into(),
            comments: vec![FeedbackNote { author: "ana".into(), body: "Off by one?\n\n- really".into() }],
        }
    }

    fn check(checkout: &str) -> FeedbackItem {
        FeedbackItem::Check {
            checkout_id: checkout.into(),
            name: "test".into(),
            conclusion: "timed_out".into(),
            url: None,
            summary: String::new(),
            log: Some("ERROR: boom".into()),
        }
    }

    fn name(id: &str) -> String {
        if id == "c1" { "api".into() } else { "web".into() }
    }

    #[test]
    fn paths_are_bare_only_inside_their_own_repo() {
        let items = vec![thread("c1", "src/a.ts", Some(42)), thread("c2", "src/b.ts", None)];

        let from_root = feedback_markdown("ACME-1", &items, None, name);
        assert!(from_root.contains("`api/src/a.ts:42`"));
        assert!(from_root.contains("`web/src/b.ts`"));

        let from_api = feedback_markdown("ACME-1", &items, Some("c1"), name);
        assert!(from_api.contains("`src/a.ts:42`"));
        assert!(from_api.contains("`web/src/b.ts`"));
        assert!(from_api.contains("`../web` from where you are"));
    }

    #[test]
    fn a_reviewers_markdown_stays_inside_the_quote() {
        let md = feedback_markdown("ACME-1", &[thread("c1", "a", Some(1))], None, name);
        assert!(md.contains("> Off by one?\n>\n> - really"));
    }

    #[test]
    fn a_check_says_what_it_reported_and_where_it_failed() {
        let md = feedback_markdown("ACME-1", &[check("c1")], None, name);
        assert!(md.contains("#### test — timed out"));
        assert!(md.contains("```\nERROR: boom\n```"));
    }

    #[test]
    fn the_summary_counts_in_words() {
        assert_eq!(feedback_summary(&[check("c1")]), "1 failing check");
        assert_eq!(
            feedback_summary(&[thread("c1", "a", None), thread("c1", "b", None), check("c1")]),
            "2 review threads and 1 failing check"
        );
    }

    #[test]
    fn items_arrive_tagged_by_kind() {
        let items: Vec<FeedbackItem> = serde_json::from_value(serde_json::json!([
            { "kind": "comment", "checkout_id": "c1", "author": "bo", "body": "hi", "url": "u" },
            { "kind": "check", "checkout_id": "c1", "name": "lint", "conclusion": "failure", "url": null, "log": null }
        ]))
        .unwrap();
        assert!(matches!(items[0], FeedbackItem::Comment { .. }));
        assert!(matches!(items[1], FeedbackItem::Check { .. }));
    }
}
