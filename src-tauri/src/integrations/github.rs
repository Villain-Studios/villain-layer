//! GitHub REST, pointed at github.com or a GitHub Enterprise Server install by
//! configuring `api_url` (`https://ghe.example.com/api/v3`).

use serde::Serialize;
use serde_json::{json, Value};

use super::http_client;
use crate::config::GithubConfig;
use crate::error::{Error, Result};

pub struct GitHub {
    api_url: String,
    token: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub head: String,
    pub base: String,
    pub url: String,
    pub mergeable_state: Option<String>,
    pub merged: bool,
    /// Conversation comments and inline review comments. Both are zero on the
    /// list endpoint, which does not carry them — only `pull` fills them in.
    pub comments: u64,
    pub review_comments: u64,
}

/// One submitted review. A reviewer may leave several; only the latest
/// decisive one counts, which is what [`verdict`] works out.
#[derive(Debug, Clone, Serialize)]
pub struct Review {
    pub author: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED or DISMISSED.
    pub state: String,
    pub submitted_at: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckRun {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: Option<String>,
}

impl GitHub {
    pub fn new(cfg: &GithubConfig, token: &str) -> Self {
        Self {
            api_url: cfg.api_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client: http_client(),
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.api_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "villain-layer")
    }

    async fn json(&self, rb: reqwest::RequestBuilder) -> Result<Value> {
        let res = rb.send().await?;
        let status = res.status();
        let body = res.text().await?;
        if !status.is_success() {
            return Err(Error::Other(format!(
                "GitHub {status}: {}",
                body.chars().take(400).collect::<String>()
            )));
        }
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn login(&self) -> Result<String> {
        let v = self.json(self.req(reqwest::Method::GET, "/user")).await?;
        Ok(v.get("login")
            .and_then(|l| l.as_str())
            .unwrap_or_default()
            .to_string())
    }

    /// The open PR for a branch, if there is one.
    pub async fn pull_for_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<PullRequest>> {
        Ok(self
            .list_pulls(owner, repo, branch, "open", 1)
            .await?
            .into_iter()
            .next())
    }

    /// Pull requests whose head is this branch, newest first.
    async fn list_pulls(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        state: &str,
        per_page: u32,
    ) -> Result<Vec<PullRequest>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!(
                    "/repos/{owner}/{repo}/pulls?state={state}&head={owner}:{branch}&per_page={per_page}"
                ),
            ))
            .await?;
        Ok(v.as_array()
            .map(|a| a.iter().map(to_pr).collect())
            .unwrap_or_default())
    }

    /// Every pull request a branch has had, newest first.
    ///
    /// [`pull_for_branch`](Self::pull_for_branch) asks only for open ones,
    /// which is right when deciding whether to open another but useless for
    /// looking back: GitHub drops a PR from that listing the moment it closes,
    /// so a panel built on it watches history disappear. A branch abandoned
    /// once and retried has two, and both are worth keeping on screen.
    pub async fn pulls_for_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>> {
        self.list_pulls(owner, repo, branch, "all", 20).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_pull(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<PullRequest> {
        let v = self
            .json(
                self.req(
                    reqwest::Method::POST,
                    &format!("/repos/{owner}/{repo}/pulls"),
                )
                .json(&json!({
                    "title": title,
                    "body": body,
                    "head": head,
                    "base": base,
                    "draft": draft,
                })),
            )
            .await?;
        Ok(to_pr(&v))
    }

    /// One pull request in full.
    ///
    /// The list endpoint that [`pull_for_branch`](Self::pull_for_branch) uses
    /// omits `merged`, the comment counts and `mergeable_state`, so anything
    /// that reads those has to come back here for them.
    pub async fn pull(&self, owner: &str, repo: &str, number: u64) -> Result<PullRequest> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{repo}/pulls/{number}"),
            ))
            .await?;
        Ok(to_pr(&v))
    }

    /// Move an open pull request onto another base branch.
    ///
    /// GitHub allows this on an open PR and recomputes the diff itself, which
    /// is the whole reason it is worth offering: the alternative is closing
    /// the PR and opening another, losing its review history with it.
    pub async fn set_pull_base(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        base: &str,
    ) -> Result<PullRequest> {
        let v = self
            .json(
                self.req(
                    reqwest::Method::PATCH,
                    &format!("/repos/{owner}/{repo}/pulls/{number}"),
                )
                .json(&json!({ "base": base })),
            )
            .await?;
        Ok(to_pr(&v))
    }

    /// Every review submitted on a pull request, oldest first.
    pub async fn reviews(&self, owner: &str, repo: &str, number: u64) -> Result<Vec<Review>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100"),
            ))
            .await?;

        Ok(v.as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| Review {
                        author: r
                            .pointer("/user/login")
                            .and_then(|l| l.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        state: s(r, "state"),
                        submitted_at: r
                            .get("submitted_at")
                            .and_then(|x| x.as_str())
                            .map(str::to_string),
                        url: s(r, "html_url"),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn checks(&self, owner: &str, repo: &str, git_ref: &str) -> Result<Vec<CheckRun>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{repo}/commits/{git_ref}/check-runs"),
            ))
            .await?;

        Ok(v.get("check_runs")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|c| CheckRun {
                        name: s(c, "name"),
                        status: s(c, "status"),
                        conclusion: c
                            .get("conclusion")
                            .and_then(|x| x.as_str())
                            .map(str::to_string),
                        url: c.get("html_url").and_then(|x| x.as_str()).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}

fn to_pr(v: &Value) -> PullRequest {
    PullRequest {
        number: v.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
        title: s(v, "title"),
        state: s(v, "state"),
        draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
        author: v
            .pointer("/user/login")
            .and_then(|l| l.as_str())
            .unwrap_or_default()
            .to_string(),
        head: v
            .pointer("/head/ref")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string(),
        base: v
            .pointer("/base/ref")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string(),
        url: s(v, "html_url"),
        mergeable_state: v
            .get("mergeable_state")
            .and_then(|m| m.as_str())
            .map(str::to_string),
        // The list endpoint has no `merged`, only `merged_at`; the detail
        // endpoint has both. Reading either means a listing is enough to say
        // what became of a closed PR, without a second call per repository.
        merged: v
            .get("merged")
            .and_then(|m| m.as_bool())
            .unwrap_or_else(|| v.get("merged_at").and_then(|m| m.as_str()).is_some()),
        comments: n(v, "comments"),
        review_comments: n(v, "review_comments"),
    }
}

/// What GitHub itself would show as the review decision.
///
/// A reviewer may submit any number of reviews, and only their latest decisive
/// one counts: COMMENTED leaves the previous verdict standing, and DISMISSED
/// clears it. One outstanding "changes requested" outranks any number of
/// approvals, because that is the one that still needs answering.
pub fn verdict(reviews: &[Review]) -> &'static str {
    let mut decided: Vec<(&str, &str)> = Vec::new();
    for r in reviews {
        let decisive = match r.state.as_str() {
            "APPROVED" | "CHANGES_REQUESTED" => true,
            "DISMISSED" => false,
            // COMMENTED and PENDING say nothing about the decision.
            _ => continue,
        };
        decided.retain(|(who, _)| *who != r.author.as_str());
        if decisive {
            decided.push((&r.author, &r.state));
        }
    }

    if decided.iter().any(|(_, st)| *st == "CHANGES_REQUESTED") {
        "changes_requested"
    } else if decided.iter().any(|(_, st)| *st == "APPROVED") {
        "approved"
    } else if reviews.iter().any(|r| r.state == "COMMENTED") {
        "commented"
    } else {
        "none"
    }
}

fn n(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(author: &str, state: &str) -> Review {
        Review {
            author: author.into(),
            state: state.into(),
            submitted_at: None,
            url: String::new(),
        }
    }

    #[test]
    fn no_reviews_decide_nothing() {
        assert_eq!(verdict(&[]), "none");
        assert_eq!(verdict(&[review("ana", "PENDING")]), "none");
    }

    #[test]
    fn a_reviewer_may_change_their_mind() {
        // Only the latest decisive review from each person counts, in either
        // direction: coming back to approve clears the block, and coming back
        // to block clears the approval.
        assert_eq!(
            verdict(&[review("ana", "CHANGES_REQUESTED"), review("ana", "APPROVED")]),
            "approved"
        );
        assert_eq!(
            verdict(&[review("ana", "APPROVED"), review("ana", "CHANGES_REQUESTED")]),
            "changes_requested"
        );
    }

    #[test]
    fn one_block_outranks_any_number_of_approvals() {
        assert_eq!(
            verdict(&[
                review("ana", "APPROVED"),
                review("bo", "APPROVED"),
                review("cy", "CHANGES_REQUESTED"),
            ]),
            "changes_requested"
        );
    }

    #[test]
    fn commenting_leaves_an_earlier_verdict_standing() {
        assert_eq!(
            verdict(&[review("ana", "APPROVED"), review("ana", "COMMENTED")]),
            "approved"
        );
        assert_eq!(verdict(&[review("ana", "COMMENTED")]), "commented");
    }

    #[test]
    fn dismissing_clears_the_verdict_it_dismissed() {
        assert_eq!(
            verdict(&[review("ana", "APPROVED"), review("ana", "DISMISSED")]),
            "none"
        );
        assert_eq!(
            verdict(&[
                review("ana", "CHANGES_REQUESTED"),
                review("ana", "DISMISSED"),
                review("bo", "APPROVED"),
            ]),
            "approved"
        );
    }
}
