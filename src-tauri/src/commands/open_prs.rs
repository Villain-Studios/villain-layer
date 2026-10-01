//! Opening a task's pull requests (PR-1, PR-2): push each repo with work no
//! pull request has landed, open or reuse its PR, and announce the new ones.

use std::path::{Path, PathBuf};

use tauri::State;

use crate::error::Result;
use crate::git;
use crate::integrations::github;

use super::diff::RepoResult;
use super::github::github_client;
use super::jira::jira_client;
use super::slack::{record_post, slack_for};
use super::{off_runtime, AppState};

/// Push and open a PR in every repository that has changes, then post the whole
/// set back to the Jira ticket and Slack, and list in each PR the others.
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
    let mut opened: Vec<github::Sibling> = Vec::new();
    // Every open PR of the task, new or not: one opened earlier does not know
    // about a repo added since.
    let mut set: Vec<github::Sibling> = Vec::new();

    for checkout in state.config.checkouts_of(&task_id) {
        let dir = PathBuf::from(&checkout.path);
        let project = state.config.project(&checkout.project_id)?;
        let repo = project.name.clone();

        let slug = {
            let dir = dir.clone();
            off_runtime(move || git::origin_slug(&dir)).await?
        };

        // What the branch has had on GitHub. With none open, the newest merged
        // one has landed everything up to its head: counted from the branch
        // point instead, a repo merged while another was still in review had
        // its finished work opened again as a new PR.
        let prs = match &slug {
            Ok((owner, name)) => match client.pulls_for_branch(owner, name, &task.branch).await {
                Ok(prs) => prs,
                Err(e) => {
                    results.push(RepoResult { checkout_id: checkout.id, repo, ok: false, detail: e.to_string() });
                    continue;
                }
            },
            // Said after the push, as it always was: the push needs no slug.
            Err(_) => Vec::new(),
        };
        let open = prs.iter().find(|p| p.state == "open").cloned();
        let landed = if open.is_some() { None } else { prs.iter().find(|p| p.merged) };

        let (changed, commits) = {
            let (dir, base, point, branch) =
                (dir.clone(), checkout.base.clone(), checkout.base_commit.clone(), task.branch.clone());
            let head = landed.map(|p| p.head_sha.clone());
            off_runtime(move || unlanded(&dir, &branch, &base, point.as_deref(), head.as_deref())).await?
        };
        if changed == 0 {
            results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: true,
                detail: match landed {
                    Some(p) => format!("#{} merged, nothing since — skipped", p.number),
                    None => "no changes, skipped".into(),
                },
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
        let outcome: Result<(github::Sibling, bool)> = async {
            // A push is a network round trip with no timeout of its own; on
            // the async workers it held up the MCP server until git gave up.
            let (owner, name) = {
                let (dir, branch, lease) = (dir.clone(), task.branch.clone(), checkout.push_lease.clone());
                off_runtime(move || git::push(&dir, &branch, lease.as_deref())).await??;
                // Spent by the push itself, whatever comes after it. Kept
                // because the slug could not be read, it was sent with every
                // later push and refused each one as "someone else pushed".
                super::diff::pushed(&state, &checkout.id);
                slug?
            };

            let (pr, new) = match open {
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
                github::Sibling {
                    repo: repo.clone(),
                    owner,
                    name,
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
                set.push(pr.clone());
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

    if !opened.is_empty() && set.len() > 1 {
        for (repo, e) in client.link_siblings(&set, task.issue_key.as_deref()).await {
            results.push(RepoResult {
                checkout_id: repo.clone(),
                repo,
                ok: false,
                detail: format!("the PR is open, but listing the others in it failed ({e})"),
            });
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


/// Files and commits on the branch that no merged pull request has landed:
/// counted from the branch point, or from `landed`, the merged head, if this
/// repository has it — whichever leaves less.
///
/// Either count holds all the branch's unlanded work, so the smaller is the
/// truer one. The branch point overcounts once a PR has merged; the merged
/// head overcounts once the base, with that PR in it, is merged back in.
pub(crate) fn unlanded(
    dir: &Path,
    branch: &str,
    base: &str,
    point: Option<&str>,
    landed: Option<&str>,
) -> (usize, u32) {
    let measure = |from: Option<&str>| {
        (
            git::changed_count(dir, base, from, git::Scope::Branch),
            git::branch_facts(dir, branch, base, from).commits,
        )
    };
    let (changed, commits) = measure(point);
    match landed.filter(|sha| git::has_commit(dir, sha)) {
        Some(sha) => {
            let (since, after) = measure(Some(sha));
            (changed.min(since), commits.min(after))
        }
        None => (changed, commits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        crate::git::run_for_tests(dir, args).expect("git")
    }

    /// A repo on `task` cut from `main`, returning it and the branch point.
    fn branch() -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("vl-open-prs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "test@villain.local"]);
        git(&dir, &["config", "user.name", "Test"]);
        commit(&dir, "a.txt", "one\n");
        git(&dir, &["checkout", "-q", "-b", "task"]);
        let point = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();
        (dir, point)
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", file]);
        git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    #[test]
    fn a_branch_whose_pull_request_merged_has_nothing_left_to_open() {
        let (dir, point) = branch();
        let head = commit(&dir, "infra.tf", "bucket\n");

        assert_eq!(unlanded(&dir, "task", "main", Some(&point), None), (1, 1));
        assert_eq!(unlanded(&dir, "task", "main", Some(&point), Some(&head)), (0, 0));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn work_after_a_merged_pull_request_is_still_opened() {
        let (dir, point) = branch();
        let head = commit(&dir, "infra.tf", "bucket\n");
        commit(&dir, "more.tf", "queue\n");

        assert_eq!(unlanded(&dir, "task", "main", Some(&point), Some(&head)), (1, 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_merged_head_this_repository_never_saw_counts_from_the_branch_point() {
        let (dir, point) = branch();
        commit(&dir, "infra.tf", "bucket\n");

        let unknown = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(unlanded(&dir, "task", "main", Some(&point), Some(unknown)), (1, 1));
        std::fs::remove_dir_all(&dir).ok();
    }
}
