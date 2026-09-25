//! The pull requests you opened, and where each one stands (REV-4).
//!
//! The review queue answers "what is waiting on me"; this answers the other
//! side, "what am I waiting on". One GraphQL search, because REST would take
//! a search and then three calls per pull request (checks, reviews, threads)
//! for the same picture.

use serde::Serialize;
use serde_json::{json, Value};

use super::{graphql_url, GitHub};
use crate::error::{Error, Result};

/// How many of your pull requests are read: the most recently updated.
pub const AUTHORED_LIMIT: usize = 50;
/// How many review threads of each are counted for "unresolved".
const THREAD_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuthoredPr {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    pub updated_at: String,
    /// "passing", "failing", "pending", or "none" when nothing reports.
    pub checks: String,
    /// "approved", "changes_requested", "review_required", or "none".
    pub review: String,
    pub approved_by: Vec<String>,
    pub changes_by: Vec<String>,
    /// Asked to review and not yet done so: logins, and teams as `@slug`.
    pub waiting_on: Vec<String>,
    pub unresolved: u32,
    /// More threads than were counted, so `unresolved` is a floor.
    pub unresolved_more: bool,
    /// GitHub found a conflict with the base. Unknown, while it is still
    /// working that out, is not one.
    pub conflicts: bool,
}

impl GitHub {
    /// Your open pull requests, most recently updated first, and whether
    /// GitHub had more than were read.
    pub async fn authored_prs(&self) -> Result<(Vec<AuthoredPr>, bool)> {
        // Only fields every GitHub Enterprise Server this app talks to has:
        // one unknown field fails the whole query, not just that field.
        // `mergeStateStatus` (behind, blocked) is left out for that reason.
        const QUERY: &str = "query($q: String!, $first: Int!, $threads: Int!) {
          search(query: $q, type: ISSUE, first: $first) {
            issueCount
            nodes {
              ... on PullRequest {
                number title url isDraft updatedAt mergeable reviewDecision
                repository { nameWithOwner }
                commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
                reviewRequests(first: 20) {
                  nodes { requestedReviewer { ... on User { login } ... on Team { slug } } }
                }
                latestOpinionatedReviews(first: 20) { nodes { state author { login } } }
                reviewThreads(first: $threads) { pageInfo { hasNextPage } nodes { isResolved } }
              }
            }
          }
        }";
        let v = self
            .json(
                self.client
                    .post(graphql_url(&self.api_url))
                    .header("Authorization", format!("Bearer {}", self.token))
                    .header("User-Agent", "villain-layer")
                    .json(&json!({
                        "query": QUERY,
                        "variables": {
                            "q": authored_query(),
                            "first": AUTHORED_LIMIT,
                            "threads": THREAD_LIMIT,
                        },
                    })),
            )
            .await?;
        parse_authored(&v)
    }
}

/// Open pull requests by the signed-in user, newest activity first.
/// Archived repositories are read-only; nothing there is waiting on anyone.
pub(crate) fn authored_query() -> &'static str {
    "is:pr is:open author:@me archived:false sort:updated-desc"
}

pub(crate) fn parse_authored(v: &Value) -> Result<(Vec<AuthoredPr>, bool)> {
    // GraphQL answers 200 with the failure inside.
    if let Some(msg) = v.pointer("/errors/0/message").and_then(|m| m.as_str()) {
        return Err(Error::Other(format!("GitHub: {msg}")));
    }
    let search = v
        .pointer("/data/search")
        .filter(|s| !s.is_null())
        .ok_or_else(|| Error::Other("GitHub did not answer the search".into()))?;
    let nodes = search.get("nodes").and_then(|n| n.as_array()).cloned().unwrap_or_default();
    let prs: Vec<AuthoredPr> = nodes.iter().filter_map(to_authored).collect();
    // Against what came back, not what parsed: a node dropped for having no
    // number is not a further page.
    let total = search.get("issueCount").and_then(|n| n.as_u64()).unwrap_or(0);
    Ok((prs, total > nodes.len() as u64))
}

fn to_authored(v: &Value) -> Option<AuthoredPr> {
    let number = v.get("number").and_then(|n| n.as_u64()).filter(|n| *n > 0)?;
    let text = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let list = |p: &str| v.pointer(p).and_then(|x| x.as_array()).cloned().unwrap_or_default();

    let checks = match v.pointer("/commits/nodes/0/commit/statusCheckRollup/state").and_then(|s| s.as_str()) {
        Some("SUCCESS") => "passing",
        Some("FAILURE" | "ERROR") => "failing",
        Some("PENDING" | "EXPECTED") => "pending",
        _ => "none",
    };

    let reviewers = |state: &str| -> Vec<String> {
        list("/latestOpinionatedReviews/nodes")
            .iter()
            .filter(|r| r.get("state").and_then(|s| s.as_str()) == Some(state))
            .filter_map(|r| r.pointer("/author/login").and_then(|l| l.as_str()).map(str::to_string))
            .collect()
    };
    let approved_by = reviewers("APPROVED");
    let changes_by = reviewers("CHANGES_REQUESTED");
    // The decision is null where branch protection asks for no review; the
    // reviews given still say something, and one request for changes
    // outranks any number of approvals, as PR-5 has it.
    let review = match v.get("reviewDecision").and_then(|d| d.as_str()) {
        Some("APPROVED") => "approved",
        Some("CHANGES_REQUESTED") => "changes_requested",
        Some("REVIEW_REQUIRED") => "review_required",
        _ if !changes_by.is_empty() => "changes_requested",
        _ if !approved_by.is_empty() => "approved",
        _ => "none",
    };

    let waiting_on = list("/reviewRequests/nodes")
        .iter()
        .filter_map(|r| {
            let who = r.get("requestedReviewer")?;
            who.get("login")
                .and_then(|l| l.as_str())
                .map(str::to_string)
                .or_else(|| who.get("slug").and_then(|s| s.as_str()).map(|s| format!("@{s}")))
        })
        .collect();

    let unresolved = list("/reviewThreads/nodes")
        .iter()
        .filter(|t| t.get("isResolved").and_then(|r| r.as_bool()) == Some(false))
        .count() as u32;

    Some(AuthoredPr {
        repo: text("/repository/nameWithOwner"),
        number,
        title: text("/title"),
        url: text("/url"),
        draft: v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false),
        updated_at: text("/updatedAt"),
        checks: checks.into(),
        review: review.into(),
        approved_by,
        changes_by,
        waiting_on,
        unresolved,
        unresolved_more: v
            .pointer("/reviewThreads/pageInfo/hasNextPage")
            .and_then(|m| m.as_bool())
            .unwrap_or(false),
        conflicts: v.get("mergeable").and_then(|m| m.as_str()) == Some("CONFLICTING"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(extra: Value) -> Value {
        let mut base = json!({
            "number": 7, "title": "Retry the login", "url": "https://github.com/acme/api/pull/7",
            "isDraft": false, "updatedAt": "2026-09-25T10:00:00Z", "mergeable": "MERGEABLE",
            "reviewDecision": null, "repository": { "nameWithOwner": "acme/api" },
            "commits": { "nodes": [] },
            "reviewRequests": { "nodes": [] },
            "latestOpinionatedReviews": { "nodes": [] },
            "reviewThreads": { "pageInfo": { "hasNextPage": false }, "nodes": [] },
        });
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        base
    }

    fn one(extra: Value) -> AuthoredPr {
        let v = json!({ "data": { "search": { "issueCount": 1, "nodes": [node(extra)] } } });
        parse_authored(&v).unwrap().0.remove(0)
    }

    #[test]
    fn a_pull_request_says_what_it_is_waiting_on() {
        let pr = one(json!({
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": "FAILURE" } } }] },
            "reviewDecision": "REVIEW_REQUIRED",
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "ana" } },
                { "requestedReviewer": { "slug": "fe" } },
            ] },
            "reviewThreads": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "isResolved": false }, { "isResolved": true }, { "isResolved": false },
            ] },
            "mergeable": "CONFLICTING",
        }));
        assert_eq!(pr.repo, "acme/api");
        assert_eq!(pr.checks, "failing");
        assert_eq!(pr.review, "review_required");
        assert_eq!(pr.waiting_on, vec!["ana", "@fe"]);
        assert_eq!(pr.unresolved, 2);
        assert!(!pr.unresolved_more);
        assert!(pr.conflicts);
    }

    #[test]
    fn with_no_review_required_the_reviews_given_decide_and_changes_outrank_approvals() {
        let reviews = |states: &[(&str, &str)]| {
            json!({ "latestOpinionatedReviews": { "nodes": states.iter().map(|(who, state)| json!({ "state": state, "author": { "login": who } })).collect::<Vec<_>>() } })
        };
        let pr = one(reviews(&[("ana", "APPROVED"), ("bo", "CHANGES_REQUESTED")]));
        assert_eq!(pr.review, "changes_requested");
        assert_eq!(pr.approved_by, vec!["ana"]);
        assert_eq!(pr.changes_by, vec!["bo"]);
        assert_eq!(one(reviews(&[("ana", "APPROVED")])).review, "approved");
        assert_eq!(one(json!({})).review, "none");
        // What branch protection decided stands.
        let mut decided = reviews(&[("ana", "APPROVED")]);
        decided["reviewDecision"] = json!("REVIEW_REQUIRED");
        assert_eq!(one(decided).review, "review_required");
    }

    #[test]
    fn checks_read_as_github_rolls_them_up_and_unknown_mergeability_is_no_conflict() {
        let rollup = |s: &str| json!({ "commits": { "nodes": [{ "commit": { "statusCheckRollup": { "state": s } } }] } });
        assert_eq!(one(rollup("SUCCESS")).checks, "passing");
        assert_eq!(one(rollup("ERROR")).checks, "failing");
        assert_eq!(one(rollup("EXPECTED")).checks, "pending");
        assert_eq!(one(json!({})).checks, "none");
        assert!(!one(json!({ "mergeable": "UNKNOWN" })).conflicts);
    }

    #[test]
    fn more_is_said_when_github_had_more_than_was_read() {
        let v = json!({ "data": { "search": { "issueCount": 80, "nodes": [node(json!({}))] } } });
        assert!(parse_authored(&v).unwrap().1);
        let v = json!({ "data": { "search": { "issueCount": 1, "nodes": [node(json!({})), json!({})] } } });
        let (prs, more) = parse_authored(&v).unwrap();
        assert_eq!(prs.len(), 1, "a node that is not a pull request is skipped");
        assert!(!more, "and is not a further page");
        let threads = one(json!({ "reviewThreads": { "pageInfo": { "hasNextPage": true }, "nodes": [{ "isResolved": false }] } }));
        assert!(threads.unresolved_more);
    }

    #[test]
    fn a_graphql_error_is_an_error_not_an_empty_list() {
        let v = json!({ "data": null, "errors": [{ "message": "Field 'latestOpinionatedReviews' doesn't exist" }] });
        assert!(parse_authored(&v).unwrap_err().to_string().contains("latestOpinionatedReviews"));
    }
}
