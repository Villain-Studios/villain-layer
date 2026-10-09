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
/// `skip` names checkouts the user left out: not pushed, not opened.
#[tauri::command]
pub async fn github_open_prs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
    title: String,
    body: String,
    draft: bool,
    skip: Vec<String>,
) -> Result<Vec<RepoResult>> {
    let task = state.config.task(&task_id)?;
    super::pr_task::not_a_review(&task, "pushed or opened as a pull request")?;
    let (client, _) = github_client(&state)?;

    let mut results = Vec::new();
    let mut opened: Vec<github::Sibling> = Vec::new();
    // Every open PR of the task, new or not: one opened earlier does not know
    // about a repo added since.
    let mut set: Vec<github::Sibling> = Vec::new();

    for checkout in state.config.checkouts_of(&task_id) {
        if skip.contains(&checkout.id) {
            continue;
        }
        let dir = PathBuf::from(&checkout.path);
        let project = state.config.project(&checkout.project_id)?;
        let repo = project.name.clone();
        // Steps agents ticked since the spec was approved go to review with
        // the code (SPEC-13).
        if let Err(e) = {
            let (task, id) = (task.clone(), checkout.id.clone());
            super::blocking(app.clone(), move |state| super::spec::commit_ticks(state, &task, &id)).await
        } {
            results.push(RepoResult { checkout_id: checkout.id, repo, ok: false, detail: format!("the spec's ticked steps could not be committed: {e}") });
            continue;
        }

        let measure = |point: Option<String>| {
            let (dir, base) = (dir.clone(), checkout.base.clone());
            off_runtime(move || {
                (
                    git::changed_count(&dir, &base, point.as_deref(), git::Scope::Branch),
                    committed_since(&dir, &base, point.as_deref()),
                )
            })
        };
        let slug = {
            let dir = dir.clone();
            off_runtime(move || git::origin_slug(&dir)).await?
        };
        let (mut changed, mut committed) = measure(checkout.base_commit.clone()).await?;

        // What the branch has had on GitHub, asked only of a repo with work to
        // open. A merged one moves the branch point up to what it landed
        // (UPD-6): counted from where the branch was cut, a repo merged while
        // another was still in review had its finished work opened again.
        let mut open = None;
        let mut merged = None;
        if changed > 0 && committed {
            if let Ok((owner, name)) = &slug {
                let prs = match client.pulls_for_branch(owner, name, &task.branch).await {
                    Ok(prs) => prs,
                    Err(e) => {
                        results.push(RepoResult { checkout_id: checkout.id, repo, ok: false, detail: e.to_string() });
                        continue;
                    }
                };
                open = prs.iter().find(|p| p.state == "open").cloned();
                if let Some(pr) = super::landed(&prs) {
                    if super::advance(&state, &checkout, &pr.head_sha).await? {
                        (changed, committed) = measure(Some(pr.head_sha.clone())).await?;
                        merged = Some(pr.number);
                    }
                }
            }
        }
        if changed == 0 {
            results.push(RepoResult {
                checkout_id: checkout.id,
                repo,
                ok: true,
                detail: match merged {
                    Some(n) => format!("#{n} merged, nothing since — skipped"),
                    None => "no changes, skipped".into(),
                },
            });
            continue;
        }
        // Changes, but none committed: pushed anyway, GitHub refused the PR
        // with a bare 422 "No commits between".
        if !committed {
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


/// Whether the branch has a commit since `from`, or its branch point.
fn committed_since(dir: &Path, base: &str, from: Option<&str>) -> bool {
    git::commits_since(dir, base, from).is_ok_and(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::super::repos::tests::{commit, git};
    use super::*;

    #[test]
    fn only_committed_work_counts_as_committed() {
        let dir = std::env::temp_dir().join(format!("vl-open-prs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "test@villain.local"]);
        git(&dir, &["config", "user.name", "Test"]);
        commit(&dir, "a.txt");
        git(&dir, &["checkout", "-q", "-b", "task"]);
        let point = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();

        std::fs::write(dir.join("draft.txt"), "draft\n").unwrap();
        assert!(!committed_since(&dir, "main", Some(&point)));
        commit(&dir, "infra.tf");
        assert!(committed_since(&dir, "main", Some(&point)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
