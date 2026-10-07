//! Working on a pull request from the Reviews view: one you opened (REV-7),
//! or one you are asked to review (REV-10).
//!
//! "Opened by you" showed where each pull request stood and then offered
//! nothing to do about it but open GitHub. The work view is where feedback
//! goes to an agent, so one of your pull requests leads to its task, and one
//! with no task yet gets one on its own branch. "To review" did the same:
//! a review meant reading the diff on GitHub, where no agent can help and
//! nothing can be run.

use std::path::Path;

use tauri::AppHandle;

use crate::config::{Project, ReviewOf, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::jira::looks_like_a_key;

use super::task_context::write_task_context;
use super::tasks::{add_repo, new_task, NewTask};
use super::AppState;

/// The task for one of your pull requests: the one already on its branch, or
/// a new one on it. `repo` is `owner/name`; `head` and `base` are the pull
/// request's branches, as GitHub named them.
#[tauri::command]
pub async fn task_for_pr(
    app: AppHandle,
    repo: String,
    head: String,
    base: String,
    title: String,
) -> Result<Task> {
    super::blocking(app, move |state| task_for_pr_inner(state, &repo, &head, &base, &title)).await
}

pub(crate) fn task_for_pr_inner(state: &AppState, repo: &str, head: &str, base: &str, title: &str) -> Result<Task> {
    let project = project_for(state, repo)?;

    // Already worked on: that task, with this repo added if it lacked it. A
    // second task on one branch would be a second worktree git refuses.
    let cfg = state.config.read();
    if let Some(task) = cfg.tasks.iter().find(|t| t.branch == head).cloned() {
        let has_repo = cfg.checkouts.iter().any(|c| c.task_id == task.id && c.project_id == project.id);
        drop(cfg);
        if !has_repo {
            add_repo(state, &task.id, &project.id)?;
        }
        return Ok(task);
    }
    drop(cfg);

    let (issue_key, issue_url) = ticket_for(state, head, title);
    new_task(
        state,
        NewTask {
            name: title.trim().to_string(),
            project_ids: vec![project.id],
            // TASK-3: it goes on from the pull request's commits, taken from
            // GitHub when neither the copy nor the clone has them.
            branch: Some(head.to_string()),
            branch_suffix: None,
            base: Some(base.to_string()),
            issue_key,
            issue_url,
            epic_key: None,
        },
    )
}

/// The pull request a review task is asked for: what "To review" knows of it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ReviewPr {
    /// `owner/name` of the repository it merges into.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: String,
    /// Its own branch, read only for a ticket key: its commits come from
    /// `refs/pull/<n>/head`.
    pub head: String,
    pub base: String,
}

/// The task for reviewing someone else's pull request (REV-10): the one
/// already reviewing it, or a new one on its head.
#[tauri::command]
pub async fn task_for_review(app: AppHandle, pr: ReviewPr) -> Result<Task> {
    super::blocking(app, move |state| task_for_review_inner(state, &pr)).await
}

pub(crate) fn task_for_review_inner(state: &AppState, pr: &ReviewPr) -> Result<Task> {
    let project = project_for(state, &pr.repo)?;
    let reviewing = |t: &Task| {
        t.review.as_ref().is_some_and(|r| r.repo.eq_ignore_ascii_case(&pr.repo) && r.number == pr.number)
    };
    if let Some(task) = state.config.read().tasks.iter().find(|t| reviewing(t)).cloned() {
        return Ok(task);
    }

    // The app's own name for it, never pushed: its author's branch may be on
    // a fork, or be a different branch of the same name on origin. The
    // repo is in it so two repos' #12 are two branches in a folder listing.
    let branch = format!("review/{}-{}", super::tasks::slugify(&project.name), pr.number);
    let head_sha = git::take_pull(&super::repo_for(state, &project), pr.number, &branch)?;
    let (issue_key, issue_url) = ticket_for(state, &pr.head, &pr.title);
    let task = new_task(
        state,
        NewTask {
            name: format!("Review: {}", pr.title.trim()),
            project_ids: vec![project.id],
            branch: Some(branch),
            branch_suffix: None,
            base: Some(pr.base.clone()),
            issue_key,
            issue_url,
            epic_key: None,
        },
    )?;
    let review = ReviewOf {
        repo: pr.repo.clone(),
        number: pr.number,
        url: pr.url.clone(),
        author: pr.author.clone(),
        head_sha,
    };
    state.config.update(|c| {
        if let Some(t) = c.tasks.iter_mut().find(|t| t.id == task.id) {
            t.review = Some(review.clone());
        }
    })?;
    let task = Task { review: Some(review), ..task };
    // Written once already by `new_task`, before it was a review.
    let _ = write_task_context(state, &task);
    Ok(task)
}

/// Move a review task to its pull request's head now, as its author pushed
/// it (REV-10), and measure it from where that leaves the base.
#[tauri::command]
pub async fn review_take_latest(app: AppHandle, task_id: String) -> Result<Task> {
    super::blocking(app, move |state| review_take_latest_inner(state, &task_id)).await
}

pub(crate) fn review_take_latest_inner(state: &AppState, task_id: &str) -> Result<Task> {
    let task = state.config.task(task_id)?;
    let review = task
        .review
        .clone()
        .ok_or_else(|| Error::Other(format!("{} is not a review of a pull request", task.name)))?;
    let checkout = state
        .config
        .checkouts_of(task_id)
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound(format!("{} has no worktree", task.name)))?;
    let (head_sha, point) =
        git::follow_pull(Path::new(&checkout.path), review.number, &checkout.base, &review.head_sha)?;
    state.config.update(|c| {
        if let Some(t) = c.tasks.iter_mut().find(|t| t.id == task_id) {
            if let Some(r) = t.review.as_mut() {
                r.head_sha = head_sha.clone();
            }
        }
        if let Some(found) = c.checkouts.iter_mut().find(|x| x.id == checkout.id) {
            found.base_commit = Some(point);
        }
    })?;
    let task = state.config.task(task_id)?;
    // An agent already here reads which commit the review is of from it.
    let _ = write_task_context(state, &task);
    Ok(task)
}

/// Refuse what would publish a review task's branch. It is the app's own
/// name for someone else's work: pushed, it would be a second copy of their
/// pull request under a branch nobody asked for.
pub(crate) fn not_a_review(task: &Task, what: &str) -> Result<()> {
    match &task.review {
        Some(r) => Err(Error::Other(format!(
            "{} is a review of {}#{}; it is never {what}",
            task.name, r.repo, r.number
        ))),
        None => Ok(()),
    }
}

/// The ticket a pull request is for, from its branch or title, linked only
/// where Jira is connected, so that the link opens something.
fn ticket_for(state: &AppState, head: &str, title: &str) -> (Option<String>, Option<String>) {
    let issue_key = ticket_key_in(head).or_else(|| ticket_key_in(title));
    let issue_url = issue_key.as_ref().and_then(|key| {
        let jira = state.config.read().jira?;
        Some(format!("{}/browse/{key}", jira.base_url.trim_end_matches('/')))
    });
    (issue_key.filter(|_| issue_url.is_some()), issue_url)
}

/// The registered repository whose origin is `owner/name` on GitHub.
fn project_for(state: &AppState, repo: &str) -> Result<Project> {
    let projects = state.config.read().projects;
    projects
        .into_iter()
        // Read from the copy where there is one: no clone is made, or fetched,
        // just to be asked where it came from.
        .find(|p| {
            git::origin_slug(&p.repo())
                .is_ok_and(|(owner, name)| format!("{owner}/{name}").eq_ignore_ascii_case(repo))
        })
        .ok_or_else(|| {
            Error::NotFound(format!(
                "{repo} is not one of your repositories. Add it under Repos to work on it here."
            ))
        })
}

/// The first Jira key in a branch name or a title: `ACME-4821` in
/// `feature/ACME-4821-new-flow` or `feat(ACME-4821): new flow`.
pub(crate) fn ticket_key_in(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .filter_map(|word| {
            let mut parts = word.splitn(3, '-');
            let key = format!("{}-{}", parts.next()?, parts.next()?);
            looks_like_a_key(&key).then_some(key)
        })
        .next()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::{AppConfig, ConfigStore};

    fn git(dir: &Path, args: &[&str]) -> String {
        crate::git::run_for_tests(dir, args).unwrap().trim().to_string()
    }

    /// GitHub as a bare repo at `…/acme/api.git`, so its origin reads as
    /// `acme/api`, and the user's clone of it with `main` pushed.
    fn github_and_clone() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("vl-pr-task-{}", uuid::Uuid::new_v4()));
        let remote = root.join("acme/api.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        let clone = root.join("clone");
        git(&root, &["clone", "-q", &format!("file://{}", remote.display()), clone.to_str().unwrap()]);
        git(&clone, &["config", "user.email", "me@work.example"]);
        git(&clone, &["config", "user.name", "Me"]);
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-qm", "init"]);
        git(&clone, &["push", "-q", "-u", "origin", "main"]);
        (root, remote, clone)
    }

    fn state(root: &Path, clone: &Path) -> AppState {
        let mut cfg = AppConfig {
            worktree_root: Some(root.join("worktrees").to_string_lossy().to_string()),
            ..Default::default()
        };
        cfg.projects.push(Project {
            id: "api".into(),
            name: "api".into(),
            path: clone.to_string_lossy().to_string(),
            default_branch: "main".into(),
            group: None,
            store: None,
            update_by: None,
        });
        AppState {
            config: ConfigStore::for_tests(root.join("config.json"), cfg),
            ptys: crate::pty::PtyManager::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
            messages: crate::messages::Messages::for_tests(root.join("messages.json")),
            notes: crate::notes::Notes::load(root),
            browser: Default::default(),
        }
    }

    /// The pull request was pushed from another machine: neither the app's
    /// copy nor the user's clone has its branch, only GitHub.
    #[test]
    fn a_pull_request_only_github_has_becomes_a_task_on_its_own_commits() {
        let (root, _remote, clone) = github_and_clone();
        git(&clone, &["switch", "-q", "-c", "ACME-7-retry"]);
        std::fs::write(clone.join("a.txt"), "retried\n").unwrap();
        git(&clone, &["commit", "-qam", "retry"]);
        git(&clone, &["push", "-q", "origin", "ACME-7-retry"]);
        let pushed = git(&clone, &["rev-parse", "HEAD"]);
        git(&clone, &["switch", "-q", "main"]);
        git(&clone, &["branch", "-qD", "ACME-7-retry"]);
        let state = state(&root, &clone);

        let task = task_for_pr_inner(&state, "Acme/API", "ACME-7-retry", "main", "Retry the login").unwrap();
        assert_eq!(task.branch, "ACME-7-retry");
        assert_eq!(task.name, "Retry the login");
        assert_eq!(task.issue_key, None, "no Jira connected, so no ticket to link");
        let checkout = &state.config.checkouts_of(&task.id)[0];
        assert_eq!(git(Path::new(&checkout.path), &["rev-parse", "HEAD"]), pushed, "not cut again from main");
        assert_eq!(std::fs::read_to_string(Path::new(&checkout.path).join("a.txt")).unwrap(), "retried\n");

        let again = task_for_pr_inner(&state, "acme/api", "ACME-7-retry", "main", "Retry the login").unwrap();
        assert_eq!(again.id, task.id, "the second click opens the same task");
        assert_eq!(state.config.read().tasks.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_pull_request_in_a_repo_that_is_not_registered_says_so() {
        let (root, _remote, clone) = github_and_clone();
        let state = state(&root, &clone);
        let err = task_for_pr_inner(&state, "acme/web", "x", "main", "t").unwrap_err().to_string();
        assert!(err.contains("acme/web is not one of your repositories"), "{err}");
        assert!(state.config.read().tasks.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    fn review_of(number: u64) -> ReviewPr {
        ReviewPr {
            repo: "acme/api".into(),
            number,
            title: "Speed up the search".into(),
            url: format!("https://github.com/acme/api/pull/{number}"),
            author: "bo".into(),
            head: "patch-1".into(),
            base: "main".into(),
        }
    }

    /// From a fork: GitHub has its commits only as `refs/pull/5/head`, and
    /// a `patch-1` on origin, if there were one, would be someone else's.
    #[test]
    fn someone_elses_pull_request_is_reviewed_on_its_own_commits_and_never_published() {
        let (root, remote, clone) = github_and_clone();
        git(&clone, &["switch", "-q", "-c", "patch-1"]);
        std::fs::write(clone.join("a.txt"), "faster\n").unwrap();
        git(&clone, &["commit", "-qam", "faster"]);
        git(&clone, &["push", "-q", "origin", "HEAD:refs/pull/5/head"]);
        let theirs = git(&clone, &["rev-parse", "HEAD"]);
        let state = state(&root, &clone);

        let task = task_for_review_inner(&state, &review_of(5)).unwrap();
        assert_eq!(task.branch, "review/api-5");
        assert_eq!(task.name, "Review: Speed up the search");
        assert_eq!(task.review.as_ref().map(|r| r.head_sha.as_str()), Some(theirs.as_str()));
        let checkout = &state.config.checkouts_of(&task.id)[0];
        let wt = Path::new(&checkout.path);
        assert_eq!(git(wt, &["rev-parse", "HEAD"]), theirs);
        let main = git(&clone, &["rev-parse", "main"]);
        assert_eq!(checkout.base_commit.as_deref(), Some(main.as_str()), "measured from where it left main");
        assert_eq!(task_for_review_inner(&state, &review_of(5)).unwrap().id, task.id, "one task per review");
        let context = std::fs::read_to_string(Path::new(&task.root).join("CLAUDE.md")).unwrap();
        assert!(context.contains("reviews pull request acme/api#5"), "an agent here is told it is a review");

        let stored = state.config.task(&task.id).unwrap();
        assert!(not_a_review(&stored, "pushed").unwrap_err().to_string().contains("acme/api#5"));

        // The author pushes again, and the review follows.
        std::fs::write(clone.join("b.txt"), "more\n").unwrap();
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-qm", "more"]);
        git(&clone, &["push", "-q", "origin", "HEAD:refs/pull/5/head"]);
        let newer = git(&clone, &["rev-parse", "HEAD"]);
        let moved = review_take_latest_inner(&state, &task.id).unwrap();
        assert_eq!(moved.review.map(|r| r.head_sha), Some(newer.clone()));
        assert_eq!(git(wt, &["rev-parse", "HEAD"]), newer);
        assert!(
            crate::git::run_for_tests(&remote, &["rev-parse", "--verify", "--quiet", "refs/heads/review/api-5"]).is_err(),
            "nothing of the review reached GitHub"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_ticket_key_is_read_from_a_branch_or_a_title() {
        assert_eq!(ticket_key_in("feature/ACME-4821-new-flow").as_deref(), Some("ACME-4821"));
        assert_eq!(ticket_key_in("feat(ACME-4810): drop the old flag").as_deref(), Some("ACME-4810"));
        assert_eq!(ticket_key_in("ACME-4821 [FE] tidy the header").as_deref(), Some("ACME-4821"));
        assert_eq!(ticket_key_in("Dt 19415 gbv from rebase"), None);
        assert_eq!(ticket_key_in("villain/some-task"), None);
    }
}
