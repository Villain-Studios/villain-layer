//! Your pull requests in one repository, by the branch each came from: what
//! Clean up (REPO-8) needs to tell a branch on origin that is done with from
//! one that is not.

use serde_json::{json, Value};

use super::GitHub;
use crate::error::{Error, Result};

/// How many of your pull requests in a repository are read: the most
/// recently created.
pub const MY_PULLS_LIMIT: usize = 300;
const PAGE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BranchPr {
    pub number: u64,
    /// The branch it came from, in the repository it was opened in.
    pub head: String,
    /// Where that branch pointed when the pull request last saw it: for a
    /// merged one, what was merged.
    pub head_sha: String,
    pub state: PrState,
}

impl GitHub {
    /// Your pull requests in `owner/repo` from its own branches, newest
    /// first, and whether there were more than [`MY_PULLS_LIMIT`].
    pub async fn my_pulls(&self, owner: &str, repo: &str) -> Result<(Vec<BranchPr>, bool)> {
        const QUERY: &str = "query($q: String!, $first: Int!, $after: String) {
          search(query: $q, type: ISSUE, first: $first, after: $after) {
            pageInfo { hasNextPage endCursor }
            nodes { ... on PullRequest { number state headRefName headRefOid isCrossRepository } }
          }
        }";
        // Spliced into a search, where a space or a colon would be another
        // qualifier: `repo:acme/api is:open` reads only the open ones.
        let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if !plain(owner) || !plain(repo) {
            return Err(Error::Other(format!("{owner}/{repo} is not a GitHub repository name")));
        }
        let q = format!("is:pr author:@me repo:{owner}/{repo} sort:created-desc");
        let mut prs = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let v = self
                .graphql(QUERY, json!({ "q": q, "first": PAGE, "after": after }))
                .await?;
            // A page that failed is an error, not a shorter list: a branch
            // whose pull request went unread would be judged without it.
            let (page, next) = parse_my_pulls(&v)?;
            prs.extend(page);
            match next {
                Some(cursor) if prs.len() < MY_PULLS_LIMIT => after = Some(cursor),
                Some(_) => return Ok((prs, true)),
                None => return Ok((prs, false)),
            }
        }
    }
}

/// One page: the pull requests from the repository's own branches, and the
/// cursor of the next page, if there is one.
pub(crate) fn parse_my_pulls(v: &Value) -> Result<(Vec<BranchPr>, Option<String>)> {
    // GraphQL answers 200 with the failure inside.
    if let Some(msg) = v.pointer("/errors/0/message").and_then(|m| m.as_str()) {
        return Err(Error::Other(format!("GitHub: {msg}")));
    }
    let search = v
        .pointer("/data/search")
        .filter(|s| !s.is_null())
        .ok_or_else(|| Error::Other("GitHub did not answer the search".into()))?;
    let prs = search
        .get("nodes")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
        // A fork's branch of the same name is not this repository's.
        .filter(|n| n.get("isCrossRepository").and_then(|c| c.as_bool()) == Some(false))
        .filter_map(|n| {
            let text = |k: &str| n.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
            Some(BranchPr {
                number: n.get("number").and_then(|x| x.as_u64())?,
                head: text("headRefName")?,
                head_sha: text("headRefOid")?,
                state: match n.get("state").and_then(|s| s.as_str())? {
                    "OPEN" => PrState::Open,
                    "MERGED" => PrState::Merged,
                    "CLOSED" => PrState::Closed,
                    _ => return None,
                },
            })
        })
        .collect();
    let next = search
        .pointer("/pageInfo/hasNextPage")
        .and_then(|m| m.as_bool())
        .filter(|more| *more)
        .and_then(|_| search.pointer("/pageInfo/endCursor").and_then(|c| c.as_str()))
        .map(str::to_string);
    Ok((prs, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_reads_each_pull_requests_branch_and_what_became_of_it() {
        let v = json!({ "data": { "search": {
            "pageInfo": { "hasNextPage": true, "endCursor": "Y3Vyc29yOjEwMA==" },
            "nodes": [
                { "number": 12, "state": "MERGED", "headRefName": "ACME-12", "headRefOid": "a1b2c3", "isCrossRepository": false },
                { "number": 11, "state": "CLOSED", "headRefName": "ACME-11", "headRefOid": "d4e5f6", "isCrossRepository": false },
                { "number": 10, "state": "OPEN", "headRefName": "ACME-10", "headRefOid": "0a0b0c", "isCrossRepository": false },
                { "number": 9, "state": "MERGED", "headRefName": "main", "headRefOid": "999999", "isCrossRepository": true },
                {},
            ],
        } } });
        let (prs, next) = parse_my_pulls(&v).unwrap();
        assert_eq!(
            prs.iter().map(|p| (p.number, p.head.as_str(), p.state)).collect::<Vec<_>>(),
            vec![(12, "ACME-12", PrState::Merged), (11, "ACME-11", PrState::Closed), (10, "ACME-10", PrState::Open)],
            "a fork's branch, and a node that is not a pull request, are left out",
        );
        assert_eq!(prs[0].head_sha, "a1b2c3");
        assert_eq!(next.as_deref(), Some("Y3Vyc29yOjEwMA=="));
    }

    #[test]
    fn the_last_page_has_no_next_and_an_error_is_not_an_empty_list() {
        let v = json!({ "data": { "search": { "pageInfo": { "hasNextPage": false, "endCursor": "x" }, "nodes": [] } } });
        assert_eq!(parse_my_pulls(&v).unwrap(), (vec![], None));
        let v = json!({ "data": null, "errors": [{ "message": "Field 'headRefOid' doesn't exist" }] });
        assert!(parse_my_pulls(&v).unwrap_err().to_string().contains("headRefOid"));
    }
}
