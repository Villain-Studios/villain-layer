//! Pull requests waiting on your review, or your team's, and what each one
//! asks of you (REV-9).
//!
//! The REST search this replaced gave a title and an author, so every card
//! looked the same: a one-line fix and a 3,000-line rewrite, one you had
//! never opened and one you had approved before its author pushed again.
//! One GraphQL search answers all of that, as it does for your own pull
//! requests (`authored.rs`).

use serde::Serialize;
use serde_json::{json, Value};

use super::authored::rollup;
use super::GitHub;
use crate::error::{Error, Result};

/// How many are read: the most recently updated. The REST search read one
/// page of 100, and a team queue can be that long.
const REQUESTED_LIMIT: usize = 100;

/// A pull request someone is being asked to review. A search, not the pull
/// endpoint: the queue is every repo on the install, not the ones this app
/// has checked out.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct ReviewRequest {
    /// `owner/name` of the repository it merges into.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub draft: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Its branch, and the one it merges into.
    pub head: String,
    pub base: String,
    /// The commit at its head now.
    pub head_sha: String,
    /// It comes from a fork: its branch is not on the base repository's
    /// origin, and only `refs/pull/<n>/head` there has its commits.
    pub cross_repo: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// "passing", "failing", "pending", or "none" when nothing reports.
    pub checks: String,
    /// Your own latest review of it: "approved", "changes_requested",
    /// "commented", "dismissed", or "none" when you have given none.
    pub my_review: String,
    /// Its head has moved past the commit your latest review was of.
    pub new_since_review: bool,
}

impl GitHub {
    /// Open pull requests matching a review search, most recently updated
    /// first, and whether GitHub had more than were read.
    pub async fn requested_prs(&self, query: &str) -> Result<(Vec<ReviewRequest>, bool)> {
        // Only fields every GitHub Enterprise Server this app talks to has:
        // one unknown field fails the whole query, not just that field.
        const QUERY: &str = "query($q: String!, $first: Int!) {
          search(query: $q, type: ISSUE, first: $first) {
            issueCount
            nodes {
              ... on PullRequest {
                number title url isDraft createdAt updatedAt
                headRefName baseRefName headRefOid isCrossRepository
                additions deletions changedFiles
                author { login }
                repository { nameWithOwner }
                commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
                viewerLatestReview { state commit { oid } }
              }
            }
          }
        }";
        let v = self
            .graphql(QUERY, json!({ "q": format!("{query} archived:false sort:updated-desc"), "first": REQUESTED_LIMIT }))
            .await?;
        parse_requested(&v)
    }
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

pub(crate) fn parse_requested(v: &Value) -> Result<(Vec<ReviewRequest>, bool)> {
    // GraphQL answers 200 with the failure inside.
    if let Some(msg) = v.pointer("/errors/0/message").and_then(|m| m.as_str()) {
        return Err(Error::Other(format!("GitHub: {msg}")));
    }
    let search = v
        .pointer("/data/search")
        .filter(|s| !s.is_null())
        .ok_or_else(|| Error::Other("GitHub did not answer the search".into()))?;
    let nodes = search.get("nodes").and_then(|n| n.as_array()).cloned().unwrap_or_default();
    let prs = nodes.iter().filter_map(to_requested).collect();
    // Against what came back, not what parsed: a node dropped for having no
    // number is not a further page.
    let total = search.get("issueCount").and_then(|n| n.as_u64()).unwrap_or(0);
    Ok((prs, total > nodes.len() as u64))
}

fn to_requested(v: &Value) -> Option<ReviewRequest> {
    let number = v.get("number").and_then(|n| n.as_u64()).filter(|n| *n > 0)?;
    let url = v.get("url").and_then(|u| u.as_str()).filter(|u| !u.is_empty())?;
    let text = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let count = |k: &str| v.get(k).and_then(|n| n.as_u64()).unwrap_or(0);
    let head_sha = text("/headRefOid");

    let mine = v.get("viewerLatestReview").filter(|r| !r.is_null());
    let my_review = match mine.and_then(|r| r.get("state")).and_then(|s| s.as_str()) {
        Some("APPROVED") => "approved",
        Some("CHANGES_REQUESTED") => "changes_requested",
        Some("COMMENTED") => "commented",
        Some("DISMISSED") => "dismissed",
        // A pending review is a draft only you can see: not a review given.
        _ => "none",
    };
    let reviewed_at = mine.and_then(|r| r.pointer("/commit/oid")).and_then(|o| o.as_str());
    let new_since_review = my_review != "none"
        && matches!(reviewed_at, Some(oid) if !head_sha.is_empty() && oid != head_sha);

    Some(ReviewRequest {
        repo: text("/repository/nameWithOwner"),
        number,
        title: text("/title"),
        url: url.to_string(),
        // A deleted account comes back as a null author.
        author: text("/author/login"),
        draft: v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false),
        created_at: text("/createdAt"),
        updated_at: text("/updatedAt"),
        head: text("/headRefName"),
        base: text("/baseRefName"),
        head_sha,
        cross_repo: v.get("isCrossRepository").and_then(|c| c.as_bool()).unwrap_or(false),
        additions: count("additions"),
        deletions: count("deletions"),
        changed_files: count("changedFiles"),
        checks: rollup(v).into(),
        my_review: my_review.into(),
        new_since_review,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(extra: Value) -> Value {
        let mut base = json!({
            "number": 14, "title": "Fix the race", "url": "https://ghe.example.com/acme/web/pull/14",
            "isDraft": false, "createdAt": "2026-09-20T08:00:00Z", "updatedAt": "2026-09-23T08:00:00Z",
            "headRefName": "ACME-14-race", "baseRefName": "main", "headRefOid": "bbb",
            "isCrossRepository": false, "additions": 120, "deletions": 30, "changedFiles": 4,
            "author": { "login": "ada" }, "repository": { "nameWithOwner": "acme/web" },
            "commits": { "nodes": [] }, "viewerLatestReview": null,
        });
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        base
    }

    fn one(extra: Value) -> ReviewRequest {
        let v = json!({ "data": { "search": { "issueCount": 1, "nodes": [node(extra)] } } });
        parse_requested(&v).unwrap().0.remove(0)
    }

    #[test]
    fn a_request_says_how_big_it_is_where_it_comes_from_and_how_its_checks_stand() {
        let pr = one(json!({
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "FAILURE" } } }] },
        }));
        assert_eq!(pr.repo, "acme/web");
        assert_eq!(pr.author, "ada");
        assert_eq!((pr.head.as_str(), pr.base.as_str(), pr.head_sha.as_str()), ("ACME-14-race", "main", "bbb"));
        assert_eq!((pr.additions, pr.deletions, pr.changed_files), (120, 30, 4));
        assert_eq!(pr.checks, "failing");
        assert!(!pr.cross_repo);
        assert!(one(json!({ "isCrossRepository": true })).cross_repo);
        assert_eq!(one(json!({ "author": null })).author, "", "a deleted account is no author, not no row");
    }

    #[test]
    fn your_review_is_stale_once_the_head_moves_past_the_commit_it_was_of() {
        let reviewed = |state: &str, oid: &str| one(json!({ "viewerLatestReview": { "state": state, "commit": { "oid": oid } } }));
        let fresh = one(json!({}));
        assert_eq!(fresh.my_review, "none");
        assert!(!fresh.new_since_review, "nothing to be new since");

        let current = reviewed("APPROVED", "bbb");
        assert_eq!(current.my_review, "approved");
        assert!(!current.new_since_review);

        let stale = reviewed("CHANGES_REQUESTED", "aaa");
        assert_eq!(stale.my_review, "changes_requested");
        assert!(stale.new_since_review);

        assert_eq!(reviewed("COMMENTED", "bbb").my_review, "commented");
        let pending = reviewed("PENDING", "aaa");
        assert_eq!(pending.my_review, "none", "a draft review is not one given");
        assert!(!pending.new_since_review);
    }

    #[test]
    fn more_is_said_when_github_had_more_than_was_read() {
        let v = json!({ "data": { "search": { "issueCount": 300, "nodes": [node(json!({}))] } } });
        assert!(parse_requested(&v).unwrap().1);
        let v = json!({ "data": { "search": { "issueCount": 2, "nodes": [node(json!({})), json!({})] } } });
        let (prs, more) = parse_requested(&v).unwrap();
        assert_eq!(prs.len(), 1, "a node that is not a pull request is skipped");
        assert!(!more, "and is not a further page");
    }

    #[test]
    fn a_graphql_error_is_an_error_not_an_empty_queue() {
        let v = json!({ "data": null, "errors": [{ "message": "Field 'viewerLatestReview' doesn't exist" }] });
        assert!(parse_requested(&v).unwrap_err().to_string().contains("viewerLatestReview"));
    }

    #[test]
    fn a_personal_row_leaves_the_team_list() {
        assert!(mine_review_query().contains("review-requested:@me"));
        assert!(mine_review_query().contains("-author:@me"));
        assert_eq!(team_review_query("acme", "fe"), "is:pr is:open team-review-requested:acme/fe -author:@me");

        let req = |number: u64| ReviewRequest {
            repo: "acme/web".into(),
            number,
            url: format!("https://github.com/acme/web/pull/{number}"),
            ..Default::default()
        };
        let mine = vec![req(3)];
        let mut team = vec![req(3), req(9)];
        drop_already_listed(&mut team, &mine);
        assert_eq!(team.len(), 1);
        assert_eq!(team[0].number, 9);
    }
}
