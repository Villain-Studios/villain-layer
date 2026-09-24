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
    /// The commit the PR's branch pointed at on GitHub. For a merged PR, what
    /// landed: work after it is what still needs a PR.
    pub head_sha: String,
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
    pub fn new(cfg: &GithubConfig, token: &str) -> Result<Self> {
        super::require_https(&cfg.api_url, "The GitHub API URL")?;
        Ok(Self {
            api_url: cfg.api_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client: http_client(),
        })
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
        // As a query rather than pasted into the URL: `#` and `+` are legal in
        // a branch name, and pasted in they cut the filter short — no PR was
        // found, and opening one again failed with "already exists".
        let head = format!("{owner}:{branch}");
        let per_page = per_page.to_string();
        let v = self
            .json(
                self.req(reqwest::Method::GET, &format!("/repos/{owner}/{repo}/pulls"))
                    .query(&[("state", state), ("head", head.as_str()), ("per_page", per_page.as_str())]),
            )
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
        // Every page. They come oldest first, so stopping at the first cut
        // off the newest — and each inline reply is a review of its own, so a
        // busy PR passes a hundred quickly. The verdict then came from an old
        // "changes requested" with the approval after it out of sight.
        let mut all: Vec<Value> = Vec::new();
        for page in 1..=10 {
            let v = self
                .json(self.req(
                    reqwest::Method::GET,
                    &format!("/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100&page={page}"),
                ))
                .await?;
            let got = v.as_array().map(|a| a.len()).unwrap_or(0);
            if let Some(arr) = v.as_array() {
                all.extend(arr.iter().cloned());
            }
            if got < 100 {
                break;
            }
        }

        Ok(all
            .iter()
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
            .collect())
    }

    pub async fn checks(&self, owner: &str, repo: &str, git_ref: &str) -> Result<Vec<CheckRun>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                // The default page is thirty. A matrix build past that could
                // hide the failing run, and the panel went green.
                &format!("/repos/{owner}/{repo}/commits/{}/check-runs?per_page=100", path_segment(git_ref)),
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
                        url: c
                            .get("html_url")
                            .and_then(|x| x.as_str())
                            .map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// What reviewers have said on a pull request: its review threads, the
    /// bodies of submitted reviews, and the conversation.
    ///
    /// GraphQL, because REST cannot say whether a thread was resolved — and
    /// handing an agent every thread a reviewer already closed is asking it to
    /// redo work that was accepted.
    pub async fn pr_feedback(&self, owner: &str, repo: &str, number: u64) -> Result<PrFeedback> {
        // Threads come oldest first and resolved ones count towards a page,
        // so one page of 100 dropped the newest threads on a PR that a few
        // rounds of bot review had filled — and then said every thread was
        // resolved. Later pages ask for the threads alone.
        const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $after: String, $first: Boolean!) {
          repository(owner: $owner, name: $name) {
            pullRequest(number: $number) {
              author @include(if: $first) { login }
              reviewThreads(first: 100, after: $after) {
                pageInfo { hasNextPage endCursor }
                nodes {
                  isResolved isOutdated path line originalLine
                  comments(first: 100) { nodes { author { login __typename } body url createdAt } }
                }
              }
              reviews(last: 100) @include(if: $first) { nodes { author { login __typename } state body url submittedAt } }
              comments(last: 100) @include(if: $first) { nodes { author { login __typename } body url createdAt } }
            }
          }
        }";
        /// A PR nobody could read through is not one to hand an agent whole.
        const MAX_PAGES: usize = 10;

        let mut first: Option<Value> = None;
        let mut after: Option<String> = None;
        for page in 0..MAX_PAGES {
            let got = self
                .json(
                    self.client
                        .post(graphql_url(&self.api_url))
                        .header("Authorization", format!("Bearer {}", self.token))
                        .header("User-Agent", "villain-layer")
                        .json(&json!({
                            "query": QUERY,
                            "variables": {
                                "owner": owner, "name": repo, "number": number,
                                "after": after, "first": page == 0,
                            },
                        })),
                )
                .await;
            // A later page that failed is a shorter list, not no list.
            let v = match got {
                Ok(v) => v,
                Err(e) if page == 0 => return Err(e),
                Err(_) => break,
            };
            let threads = "/data/repository/pullRequest/reviewThreads";
            let more = v
                .pointer(&format!("{threads}/pageInfo/hasNextPage"))
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            after = v
                .pointer(&format!("{threads}/pageInfo/endCursor"))
                .and_then(|x| x.as_str())
                .map(str::to_string);
            match first.as_mut() {
                None => first = Some(v),
                Some(all) => {
                    let (Some(Value::Array(into)), Some(Value::Array(nodes))) = (
                        all.pointer_mut(&format!("{threads}/nodes")),
                        v.pointer(&format!("{threads}/nodes")),
                    ) else {
                        break;
                    };
                    into.extend(nodes.iter().cloned());
                }
            }
            if !more || after.is_none() {
                break;
            }
        }
        parse_feedback(&first.unwrap_or(Value::Null))
    }

    /// Check runs on a ref that failed, with what they said about it.
    ///
    /// Cancelled is left out: it is nearly always a newer push superseding the
    /// run, not something wrong with the code.
    pub async fn failed_checks(&self, owner: &str, repo: &str, git_ref: &str) -> Result<Vec<FailedCheck>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{repo}/commits/{}/check-runs?per_page=100", path_segment(git_ref)),
            ))
            .await?;
        let mut out: Vec<FailedCheck> = Vec::new();
        for c in v.get("check_runs").and_then(|c| c.as_array()).into_iter().flatten() {
            let conclusion = c.get("conclusion").and_then(|x| x.as_str()).unwrap_or("");
            if !matches!(conclusion, "failure" | "timed_out" | "action_required" | "startup_failure") {
                continue;
            }
            let name = s(c, "name");
            // The same workflow runs once for the push and again for the pull
            // request; one account of each failure is enough.
            if out.iter().any(|f| f.name == name) {
                continue;
            }
            let output = |k: &str| c.pointer(&format!("/output/{k}")).and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let summary = [output("title"), output("summary"), output("text")]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            out.push(FailedCheck {
                name,
                conclusion: conclusion.to_string(),
                url: c.get("html_url").and_then(|x| x.as_str()).map(str::to_string),
                summary: clip_tail(&summary, SUMMARY_BUDGET),
                log: None,
                job_id: (c.pointer("/app/slug").and_then(|x| x.as_str()) == Some("github-actions"))
                    .then(|| c.get("id").and_then(|x| x.as_u64()))
                    .flatten(),
            });
        }
        Ok(out)
    }

    /// The end of a GitHub Actions job's log, where the failure usually is.
    ///
    /// The log is a redirect to storage elsewhere; reqwest drops the token on
    /// the way, which that URL does not want anyway. Read a chunk at a time
    /// keeping only the tail, because a chatty job's log runs to tens of
    /// megabytes and only its last screenful says why it failed.
    pub async fn job_log_tail(&self, owner: &str, repo: &str, job_id: u64) -> Result<String> {
        let mut res = self
            .req(reqwest::Method::GET, &format!("/repos/{owner}/{repo}/actions/jobs/{job_id}/logs"))
            // The client's 30 seconds cover the body too, and a chatty job's
            // log over a VPN takes longer than that to read — which surfaced as
            // "No log could be read" for exactly the logs this is for.
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            return Err(Error::Other(format!("GitHub {status} reading the job log")));
        }
        let mut tail: Vec<u8> = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            tail.extend_from_slice(&chunk);
            if tail.len() > LOG_KEEP * 2 {
                tail.drain(..tail.len() - LOG_KEEP);
            }
        }
        Ok(log_excerpt(&String::from_utf8_lossy(&tail)))
    }

    /// Open pull requests matching a search query, newest activity first.
    ///
    /// One page. This feeds an inbox, and the search API is rate-limited
    /// separately from the rest of REST — a second page on every poll would
    /// spend that budget on rows nobody scrolls to. `more` is set when GitHub
    /// had further hits.
    pub async fn search_prs(&self, query: &str) -> Result<(Vec<ReviewRequest>, bool)> {
        let v = self
            .json(self.req(reqwest::Method::GET, "/search/issues").query(&[
                ("q", query),
                ("sort", "updated"),
                ("order", "desc"),
                ("per_page", "100"),
            ]))
            .await?;
        Ok(parse_search_page(&v))
    }

    /// Teams the token's user belongs to.
    ///
    /// Capped. A server that ignores `page` would otherwise be followed
    /// forever, and nobody belongs to enough teams for ten pages to matter.
    pub async fn user_teams(&self) -> Result<Vec<GhTeam>> {
        let mut out = Vec::new();
        for page in 1..=10 {
            let page_n = page.to_string();
            let v = self
                .json(
                    self.req(reqwest::Method::GET, "/user/teams")
                        .query(&[("per_page", "100"), ("page", page_n.as_str())]),
                )
                .await?;
            let Some(arr) = v.as_array() else {
                return Err(Error::Other(
                    "GitHub /user/teams did not return a list".into(),
                ));
            };
            if arr.is_empty() {
                break;
            }
            let n = arr.len();
            for t in arr {
                if let Some(team) = parse_team(t) {
                    out.push(team);
                }
            }
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    /// Turn a settings value into the team GitHub's search qualifier wants.
    ///
    /// `org/slug` is used as written, so a token without `read:org` still
    /// works. A bare slug has to be looked up, because the qualifier is
    /// `org/slug` and the org is not in the name the user types.
    pub async fn resolve_team(&self, raw: &str) -> Result<GhTeam> {
        let bare = raw.trim().trim_start_matches('@').trim();
        if bare.contains('/') {
            return pick_team(bare, &[]).map_err(Error::Other);
        }
        let teams = self.user_teams().await.map_err(|e| {
            Error::Other(format!(
                "could not look up @{bare} ({e}). Set the team as org/{bare} — a bare name needs the read:org scope."
            ))
        })?;
        pick_team(bare, &teams).map_err(Error::Other)
    }
}

/// A pull request someone is being asked to review. Search results, not the
/// pull endpoint: the queue is every repo on the install, not the ones this
/// app has checked out.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReviewRequest {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub draft: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhTeam {
    pub org: String,
    pub slug: String,
    pub name: String,
}

/// Reviews requested of the signed-in user, excluding ones they opened.
///
/// A pull request you authored is not waiting for your review, even if you
/// are also on the requested list.
pub(crate) fn mine_review_query() -> &'static str {
    "is:pr is:open review-requested:@me -author:@me"
}

/// Reviews requested of a team, excluding ones the signed-in user opened.
pub(crate) fn team_review_query(org: &str, slug: &str) -> String {
    format!("is:pr is:open team-review-requested:{org}/{slug} -author:@me")
}

/// What to store for the review team. Blank and a lone `@` are "no team".
pub(crate) fn normalize_review_team(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches('@').trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// `@fe` matched against the user's teams, or `org/fe` taken as given.
pub(crate) fn pick_team(raw: &str, teams: &[GhTeam]) -> std::result::Result<GhTeam, String> {
    let raw = raw.trim().trim_start_matches('@').trim();
    if raw.is_empty() {
        return Err("a review team needs a name".into());
    }
    if let Some((org, slug)) = raw.split_once('/') {
        let org = org.trim().trim_start_matches('@');
        let slug = slug.trim().trim_start_matches('@');
        if org.is_empty() || slug.is_empty() || slug.contains('/') {
            return Err("a review team looks like @fe or org/fe".into());
        }
        let name = teams
            .iter()
            .find(|t| t.org.eq_ignore_ascii_case(org) && t.slug.eq_ignore_ascii_case(slug))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| slug.to_string());
        return Ok(GhTeam {
            org: org.to_string(),
            slug: slug.to_string(),
            name,
        });
    }
    if raw.contains(char::is_whitespace) {
        return Err("a review team looks like @fe or org/fe".into());
    }
    let hits: Vec<&GhTeam> = teams
        .iter()
        .filter(|t| t.slug.eq_ignore_ascii_case(raw))
        .collect();
    match hits.as_slice() {
        [] => Err(format!("no team @{raw} on this account")),
        [one] => Ok((*one).clone()),
        many => Err(format!(
            "@{raw} is in more than one org ({}); set it as org/{raw}",
            many.iter()
                .map(|t| t.org.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Drop team rows that are already in the personal queue.
///
/// Requested of you and of the team at once would sit in both columns. The
/// personal one is the one that names you, so the team column keeps only what
/// you would otherwise miss.
pub(crate) fn drop_already_listed(team: &mut Vec<ReviewRequest>, mine: &[ReviewRequest]) {
    team.retain(|pr| !mine.iter().any(|m| same_pr(m, pr)));
}

fn same_pr(a: &ReviewRequest, b: &ReviewRequest) -> bool {
    if !a.repo.is_empty() && a.repo == b.repo && a.number == b.number {
        return true;
    }
    a.url == b.url
}

pub(crate) fn parse_search_page(v: &Value) -> (Vec<ReviewRequest>, bool) {
    let items = v.get("items").and_then(|i| i.as_array());
    // Compare against what GitHub returned, not what we could parse. A hit we
    // drop for having no number is not a further page.
    let returned = items.map(|a| a.len()).unwrap_or(0);
    let prs = items
        .map(|a| a.iter().filter_map(to_review_request).collect())
        .unwrap_or_default();
    let total = v
        .get("total_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(returned as u64);
    let incomplete = v
        .get("incomplete_results")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let more = incomplete || total > returned as u64;
    (prs, more)
}

fn to_review_request(v: &Value) -> Option<ReviewRequest> {
    let number = v
        .get("number")
        .and_then(|n| n.as_u64())
        .filter(|n| *n > 0)?;
    let url = v
        .pointer("/pull_request/html_url")
        .and_then(|u| u.as_str())
        .filter(|u| !u.is_empty())
        .or_else(|| {
            v.get("html_url")
                .and_then(|u| u.as_str())
                .filter(|u| !u.is_empty())
        })?;
    Some(ReviewRequest {
        repo: repo_slug(
            v.get("repository_url")
                .and_then(|u| u.as_str())
                .unwrap_or(""),
        ),
        number,
        title: s(v, "title"),
        url: url.to_string(),
        author: v
            .pointer("/user/login")
            .and_then(|l| l.as_str())
            .unwrap_or_default()
            .to_string(),
        draft: v.get("draft").and_then(|d| d.as_bool()).unwrap_or(false),
        updated_at: s(v, "updated_at"),
    })
}

/// `https://ghe.example.com/api/v3/repos/acme/web` → `acme/web`.
fn repo_slug(repository_url: &str) -> String {
    let Some(rest) = repository_url.split("/repos/").nth(1) else {
        return String::new();
    };
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some(owner), Some(name)) => format!("{owner}/{name}"),
        _ => String::new(),
    }
}

fn parse_team(v: &Value) -> Option<GhTeam> {
    let slug = v
        .get("slug")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let org = v
        .pointer("/organization/login")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    if slug.is_empty() || org.is_empty() {
        return None;
    }
    let name = v
        .get("name")
        .and_then(|s| s.as_str())
        .unwrap_or(&slug)
        .to_string();
    Some(GhTeam { org, slug, name })
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
        head_sha: v
            .pointer("/head/sha")
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
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}


/// Everything reviewers said on one pull request.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PrFeedback {
    /// Who opened it — their own replies are not feedback.
    pub author: String,
    pub threads: Vec<ReviewThread>,
    /// Submitted reviews that say something. A bare approval has no body and
    /// nothing to act on, so it is left out.
    pub reviews: Vec<Note>,
    pub comments: Vec<Note>,
}

/// An inline conversation on a line of the diff.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewThread {
    pub path: String,
    /// Null when the line is gone from the current diff.
    pub line: Option<u64>,
    pub resolved: bool,
    /// The code under it has changed since, so it may already be answered.
    pub outdated: bool,
    pub url: String,
    pub comments: Vec<Note>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub author: String,
    /// Written by an app — a coverage bot, a linter — not a person.
    pub bot: bool,
    /// For a review: APPROVED, CHANGES_REQUESTED or COMMENTED.
    pub state: Option<String>,
    pub body: String,
    pub url: String,
    pub at: Option<String>,
}

/// A check run that failed, and what it left to go on.
#[derive(Debug, Clone, Serialize)]
pub struct FailedCheck {
    pub name: String,
    pub conclusion: String,
    pub url: Option<String>,
    /// The run's own report — for an external CI, the only account there is.
    pub summary: String,
    /// The end of the job's log, when it ran on GitHub Actions.
    pub log: Option<String>,
    #[serde(skip)]
    pub job_id: Option<u64>,
}

/// Where the GraphQL endpoint is, from the REST base: `api.github.com` has
/// it at `/graphql`, and an Enterprise Server at `/api/graphql` beside
/// `/api/v3`.
pub(crate) fn graphql_url(api_url: &str) -> String {
    let base = api_url.trim_end_matches('/');
    format!("{}/graphql", base.strip_suffix("/v3").unwrap_or(base))
}

/// How much of a check's own report to keep.
const SUMMARY_BUDGET: usize = 3_000;
/// How much of a log to read to the end of.
const LOG_KEEP: usize = 2 * 1024 * 1024;
/// How many lines of log an agent is handed per failed job.
const LOG_LINES: usize = 80;
const LOG_BUDGET: usize = 8_000;

/// Keep the end of `text`, where a report's conclusion is, within `max` bytes.
fn clip_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &text[cut..])
}

/// The part of an Actions log that says why the job failed.
///
/// Every line carries a timestamp, and the job ends with cleanup steps that
/// say nothing: taken literally, the last eighty lines of a failed build were
/// "Post job cleanup" and a cache upload. The window ends at the last line
/// Actions marked as an error, or before the cleanup when nothing was marked.
pub(crate) fn log_excerpt(raw: &str) -> String {
    let lines: Vec<String> = crate::pty::strip_ansi(raw)
        .lines()
        .map(|l| {
            // Every line opens with when it was written: `2026-09-23T10:00:00.1234567Z `.
            let l = match l.split_once(' ') {
                Some((stamp, rest)) if stamp.len() >= 20 && stamp.ends_with('Z') && stamp.as_bytes()[4] == b'-' => rest,
                _ => l,
            };
            l.trim_end().to_string()
        })
        .filter(|l| !l.starts_with("##[endgroup]"))
        .map(|l| match l.strip_prefix("##[group]") {
            Some(rest) => format!("▸ {rest}"),
            None => l.replace("##[error]", "ERROR: ").replace("##[warning]", "warning: "),
        })
        .collect();

    let end = match lines.iter().rposition(|l| l.starts_with("ERROR: ")) {
        Some(at) => (at + 1).min(lines.len()),
        None => lines
            .iter()
            .position(|l| l.starts_with("▸ Post job cleanup") || l.starts_with("Post job cleanup"))
            .unwrap_or(lines.len()),
    };
    let start = end.saturating_sub(LOG_LINES);
    clip_tail(lines[start..end].join("\n").trim(), LOG_BUDGET)
}

fn parse_note(v: &Value) -> Note {
    Note {
        author: v.pointer("/author/login").and_then(|x| x.as_str()).unwrap_or("ghost").to_string(),
        bot: v.pointer("/author/__typename").and_then(|x| x.as_str()) == Some("Bot"),
        state: v.get("state").and_then(|x| x.as_str()).map(str::to_string),
        body: s(v, "body").trim().to_string(),
        url: s(v, "url"),
        at: v
            .get("createdAt")
            .or_else(|| v.get("submittedAt"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
    }
}

/// A ref as one path segment. A branch may hold `#`, `?`, `%` or a space,
/// which in a URL end the path or mean something else; its `/` is left alone,
/// since GitHub reads a ref across them.
fn path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn parse_feedback(v: &Value) -> Result<PrFeedback> {
    // GraphQL answers 200 with the failure inside: a PR that does not exist,
    // or a token without access, arrives as `errors` and a null.
    if let Some(msg) = v.pointer("/errors/0/message").and_then(|m| m.as_str()) {
        return Err(Error::Other(format!("GitHub: {msg}")));
    }
    let pr = v
        .pointer("/data/repository/pullRequest")
        .filter(|p| !p.is_null())
        .ok_or_else(|| Error::Other("GitHub did not return that pull request".into()))?;
    let nodes = |path: &str| {
        pr.pointer(path)
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default()
    };

    let threads = nodes("/reviewThreads/nodes")
        .iter()
        .map(|t| {
            let comments: Vec<Note> = t
                .pointer("/comments/nodes")
                .and_then(|n| n.as_array())
                .map(|a| a.iter().map(parse_note).collect())
                .unwrap_or_default();
            ReviewThread {
                path: s(t, "path"),
                line: t
                    .get("line")
                    .and_then(|x| x.as_u64())
                    .or_else(|| t.get("originalLine").and_then(|x| x.as_u64())),
                resolved: t.get("isResolved").and_then(|x| x.as_bool()).unwrap_or(false),
                outdated: t.get("isOutdated").and_then(|x| x.as_bool()).unwrap_or(false),
                url: comments.first().map(|c| c.url.clone()).unwrap_or_default(),
                comments,
            }
        })
        .filter(|t| !t.comments.is_empty())
        .collect();

    Ok(PrFeedback {
        author: pr.pointer("/author/login").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        threads,
        reviews: nodes("/reviews/nodes")
            .iter()
            .map(parse_note)
            .filter(|r| !r.body.is_empty())
            .collect(),
        comments: nodes("/comments/nodes")
            .iter()
            .map(parse_note)
            .filter(|c| !c.body.is_empty())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_ref_stays_one_path_segment() {
        assert_eq!(path_segment("villain/ACME-12-fix"), "villain/ACME-12-fix");
        assert_eq!(path_segment("fix#2 a+b%"), "fix%232%20a%2Bb%25");
        assert_eq!(path_segment("0a1b2c"), "0a1b2c");
    }

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
            verdict(&[
                review("ana", "CHANGES_REQUESTED"),
                review("ana", "APPROVED")
            ]),
            "approved"
        );
        assert_eq!(
            verdict(&[
                review("ana", "APPROVED"),
                review("ana", "CHANGES_REQUESTED")
            ]),
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
    fn a_search_hit_from_enterprise_keeps_the_repo() {
        let v = json!({
            "total_count": 2,
            "incomplete_results": false,
            "items": [{
                "number": 14,
                "title": "Fix the race",
                "draft": true,
                "html_url": "https://ghe.example.com/acme/web/issues/14",
                "updated_at": "2026-09-23T08:00:00Z",
                "user": { "login": "ada" },
                "repository_url": "https://ghe.example.com/api/v3/repos/acme/web",
                "pull_request": { "html_url": "https://ghe.example.com/acme/web/pull/14" }
            }, {
                "number": 0,
                "title": "not a pull"
            }]
        });
        let (prs, more) = parse_search_page(&v);
        assert!(!more);
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].repo, "acme/web");
        assert_eq!(prs[0].url, "https://ghe.example.com/acme/web/pull/14");
        assert!(prs[0].draft);
        assert_eq!(prs[0].author, "ada");
    }

    #[test]
    fn a_short_page_with_a_higher_total_is_not_the_whole_queue() {
        let v = json!({
            "total_count": 4,
            "items": [{
                "number": 1,
                "title": "One",
                "html_url": "https://github.com/acme/web/pull/1",
                "repository_url": "https://api.github.com/repos/acme/web"
            }]
        });
        let (prs, more) = parse_search_page(&v);
        assert_eq!(prs.len(), 1);
        assert!(more);
    }

    #[test]
    fn a_bare_slug_matches_the_one_team_you_are_on() {
        let teams = vec![
            GhTeam {
                org: "acme".into(),
                slug: "fe".into(),
                name: "Frontend".into(),
            },
            GhTeam {
                org: "acme".into(),
                slug: "be".into(),
                name: "Backend".into(),
            },
        ];
        let hit = pick_team("@fe", &teams).unwrap();
        assert_eq!(hit.org, "acme");
        assert_eq!(hit.slug, "fe");
        assert_eq!(hit.name, "Frontend");
        assert!(pick_team("missing", &teams).is_err());
    }

    #[test]
    fn org_and_slug_need_no_lookup() {
        let hit = pick_team("acme/fe", &[]).unwrap();
        assert_eq!(hit.org, "acme");
        assert_eq!(hit.slug, "fe");
        assert_eq!(
            team_review_query(&hit.org, &hit.slug),
            "is:pr is:open team-review-requested:acme/fe -author:@me"
        );
    }

    #[test]
    fn the_same_slug_in_two_orgs_asks_which() {
        let teams = vec![
            GhTeam {
                org: "acme".into(),
                slug: "fe".into(),
                name: "Frontend".into(),
            },
            GhTeam {
                org: "other".into(),
                slug: "fe".into(),
                name: "FE".into(),
            },
        ];
        let err = pick_team("fe", &teams).unwrap_err();
        assert!(err.contains("acme"));
        assert!(err.contains("other"));
    }

    #[test]
    fn blank_is_no_team_and_a_personal_row_leaves_the_team_list() {
        assert_eq!(normalize_review_team("  @fe "), Some("fe".into()));
        assert_eq!(normalize_review_team(" @ "), None);
        assert!(mine_review_query().contains("review-requested:@me"));
        assert!(mine_review_query().contains("-author:@me"));

        let mine = vec![req("acme/web", 3)];
        let mut team = vec![req("acme/web", 3), req("acme/web", 9)];
        drop_already_listed(&mut team, &mine);
        assert_eq!(team.len(), 1);
        assert_eq!(team[0].number, 9);
    }

    fn req(repo: &str, number: u64) -> ReviewRequest {
        ReviewRequest {
            repo: repo.into(),
            number,
            title: String::new(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            author: String::new(),
            draft: false,
            updated_at: String::new(),
        }
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
    #[test]
    fn graphql_sits_beside_rest_on_both_kinds_of_host() {
        assert_eq!(graphql_url("https://api.github.com"), "https://api.github.com/graphql");
        assert_eq!(graphql_url("https://ghe.example.com/api/v3/"), "https://ghe.example.com/api/graphql");
    }

    #[test]
    fn a_log_excerpt_ends_at_the_error_not_the_cleanup() {
        let mut raw = String::new();
        for i in 0..200 {
            raw.push_str(&format!("2026-09-23T10:00:00.1234567Z noise {i}\n"));
        }
        raw.push_str("2026-09-23T10:00:01.0000000Z ##[group]Run bun test\n");
        raw.push_str("2026-09-23T10:00:02.0000000Z \u{1b}[31mexpected 2, got 3\u{1b}[0m\n");
        raw.push_str("2026-09-23T10:00:02.0000000Z ##[endgroup]\n");
        raw.push_str("2026-09-23T10:00:03.0000000Z ##[error]Process completed with exit code 1.\n");
        raw.push_str("2026-09-23T10:00:04.0000000Z Post job cleanup.\n");
        raw.push_str("2026-09-23T10:00:04.0000000Z Cache saved\n");

        let out = log_excerpt(&raw);
        assert!(out.ends_with("ERROR: Process completed with exit code 1."), "{out}");
        assert!(out.contains("▸ Run bun test"));
        assert!(out.contains("expected 2, got 3"));
        assert!(!out.contains("2026-09-23T"));
        assert!(!out.contains("Cache saved"));
        assert!(!out.contains("noise 100"), "kept more than the window");
    }

    #[test]
    fn without_an_error_marker_the_excerpt_stops_before_cleanup() {
        let raw = "2026-09-23T10:00:00.1Z test failed\n2026-09-23T10:00:00.2Z Post job cleanup.\n2026-09-23T10:00:00.3Z done\n";
        assert_eq!(log_excerpt(raw), "test failed");
    }

    #[test]
    fn feedback_keeps_threads_with_their_state_and_drops_empty_reviews() {
        let v = json!({ "data": { "repository": { "pullRequest": {
            "author": { "login": "me" },
            "reviewThreads": { "nodes": [
                { "isResolved": false, "isOutdated": false, "path": "src/a.ts", "line": 12, "originalLine": 10,
                  "comments": { "nodes": [
                    { "author": { "login": "ana", "__typename": "User" }, "body": "Off by one?", "url": "u1", "createdAt": "t" },
                    { "author": { "login": "me", "__typename": "User" }, "body": "Looking", "url": "u2", "createdAt": "t" }
                  ] } },
                { "isResolved": true, "isOutdated": true, "path": "src/b.ts", "line": null, "originalLine": 4,
                  "comments": { "nodes": [
                    { "author": { "login": "cov", "__typename": "Bot" }, "body": "Uncovered", "url": "u3", "createdAt": "t" }
                  ] } }
            ] },
            "reviews": { "nodes": [
                { "author": { "login": "ana", "__typename": "User" }, "state": "APPROVED", "body": "", "url": "r1", "submittedAt": "t" },
                { "author": { "login": "bo", "__typename": "User" }, "state": "CHANGES_REQUESTED", "body": "Needs a test", "url": "r2", "submittedAt": "t" }
            ] },
            "comments": { "nodes": [] }
        } } } });
        let fb = parse_feedback(&v).unwrap();
        assert_eq!(fb.author, "me");
        assert_eq!(fb.threads.len(), 2);
        assert_eq!(fb.threads[0].line, Some(12));
        assert_eq!(fb.threads[0].url, "u1");
        assert_eq!(fb.threads[0].comments.len(), 2);
        assert!(fb.threads[1].resolved && fb.threads[1].outdated);
        // Gone from the diff: the line it was left on, not nothing.
        assert_eq!(fb.threads[1].line, Some(4));
        assert!(fb.threads[1].comments[0].bot);
        assert_eq!(fb.reviews.len(), 1);
        assert_eq!(fb.reviews[0].state.as_deref(), Some("CHANGES_REQUESTED"));
    }

    #[test]
    fn a_graphql_error_is_an_error() {
        let v = json!({ "data": { "repository": null }, "errors": [{ "message": "Could not resolve to a Repository" }] });
        assert!(parse_feedback(&v).unwrap_err().to_string().contains("Could not resolve"));
    }
}
