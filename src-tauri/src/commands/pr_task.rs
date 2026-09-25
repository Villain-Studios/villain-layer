//! Working on a pull request you opened, from the Reviews view (REV-7).
//!
//! "Opened by you" showed where each pull request stood and then offered
//! nothing to do about it but open GitHub. The work view is where feedback
//! goes to an agent, so one of your pull requests leads to its task, and one
//! with no task yet gets one on its own branch.

use tauri::AppHandle;

use crate::config::{Project, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::jira::looks_like_a_key;

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

    let issue_key = ticket_key_in(head).or_else(|| ticket_key_in(title));
    // Only linked where Jira is connected, so that the link opens something.
    let issue_url = issue_key.as_ref().and_then(|key| {
        let jira = state.config.read().jira?;
        Some(format!("{}/browse/{key}", jira.base_url.trim_end_matches('/')))
    });
    let issue_key = issue_key.filter(|_| issue_url.is_some());

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

/// The first Jira key in a branch name or a title: `DT-21299` in
/// `feature/DT-21299-new-flow` or `feat(DT-21299): new flow`.
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
        }
    }

    /// The pull request was pushed from another machine: neither the app's
    /// copy nor the user's clone has its branch, only GitHub.
    #[test]
    fn a_pull_request_only_github_has_becomes_a_task_on_its_own_commits() {
        let (root, _remote, clone) = github_and_clone();
        git(&clone, &["switch", "-q", "-c", "DT-7-retry"]);
        std::fs::write(clone.join("a.txt"), "retried\n").unwrap();
        git(&clone, &["commit", "-qam", "retry"]);
        git(&clone, &["push", "-q", "origin", "DT-7-retry"]);
        let pushed = git(&clone, &["rev-parse", "HEAD"]);
        git(&clone, &["switch", "-q", "main"]);
        git(&clone, &["branch", "-qD", "DT-7-retry"]);
        let state = state(&root, &clone);

        let task = task_for_pr_inner(&state, "Acme/API", "DT-7-retry", "main", "Retry the login").unwrap();
        assert_eq!(task.branch, "DT-7-retry");
        assert_eq!(task.name, "Retry the login");
        assert_eq!(task.issue_key, None, "no Jira connected, so no ticket to link");
        let checkout = &state.config.checkouts_of(&task.id)[0];
        assert_eq!(git(Path::new(&checkout.path), &["rev-parse", "HEAD"]), pushed, "not cut again from main");
        assert_eq!(std::fs::read_to_string(Path::new(&checkout.path).join("a.txt")).unwrap(), "retried\n");

        let again = task_for_pr_inner(&state, "acme/api", "DT-7-retry", "main", "Retry the login").unwrap();
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

    #[test]
    fn a_ticket_key_is_read_from_a_branch_or_a_title() {
        assert_eq!(ticket_key_in("feature/DT-21299-new-flow").as_deref(), Some("DT-21299"));
        assert_eq!(ticket_key_in("feat(DT-21155): remove gating").as_deref(), Some("DT-21155"));
        assert_eq!(ticket_key_in("DT-21299 [FE] improve the analytics").as_deref(), Some("DT-21299"));
        assert_eq!(ticket_key_in("Dt 19415 gbv from rebase"), None);
        assert_eq!(ticket_key_in("villain/some-task"), None);
    }
}
