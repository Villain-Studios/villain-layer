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
                &format!("/repos/{owner}/{repo}/commits/{git_ref}/check-runs?per_page=100"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
