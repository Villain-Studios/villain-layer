//! Your review of someone else's pull request, posted as one GitHub review
//! (REV-11): line comments, a body, and a verdict, all at once, as the
//! review form on GitHub sends them.

use serde_json::{json, Value};

use super::GitHub;
use crate::error::{Error, Result};

/// A comment on a line, or a range of lines, of one side of a file's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineComment {
    pub path: String,
    pub line: u32,
    /// The range's first line, on the same side, when it is one.
    pub start_line: Option<u32>,
    /// The old version of the file: a removed line.
    pub removed: bool,
    pub body: String,
}

impl LineComment {
    fn json(&self) -> Value {
        let side = if self.removed { "LEFT" } else { "RIGHT" };
        let mut v = json!({ "path": self.path, "line": self.line, "side": side, "body": self.body });
        if let Some(start) = self.start_line.filter(|s| *s < self.line) {
            v["start_line"] = json!(start);
            v["start_side"] = json!(side);
        }
        v
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Posted {
    /// The review's page.
    Review(String),
    /// GitHub would not anchor a line comment (422), and posted nothing.
    LinesRefused(String),
}

/// "COMMENT", "APPROVE" or "REQUEST_CHANGES", from how the app names them.
pub fn review_event(verdict: &str) -> Result<&'static str> {
    match verdict {
        "comment" => Ok("COMMENT"),
        "approve" => Ok("APPROVE"),
        "request_changes" => Ok("REQUEST_CHANGES"),
        other => Err(Error::Other(format!("{other:?} is not a review verdict"))),
    }
}

/// `owner` and `name` of `owner/name`, each one plain path segment: the
/// repository comes back from a search and goes into a URL path.
pub fn repo_parts(repo: &str) -> Result<(&str, &str)> {
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && s != "..";
    match repo.split_once('/') {
        Some((owner, name)) if plain(owner) && plain(name) => Ok((owner, name)),
        _ => Err(Error::Other(format!("{repo} is not a GitHub repository name"))),
    }
}

impl GitHub {
    /// Submit a review of pull request `number`, pinned to `commit_id`: the
    /// commit it was read at, so a line means what it meant then.
    pub async fn post_review(
        &self,
        repo: &str,
        number: u64,
        commit_id: &str,
        event: &str,
        body: &str,
        comments: &[LineComment],
    ) -> Result<Posted> {
        let (owner, name) = repo_parts(repo)?;
        let comments: Vec<Value> = comments.iter().map(LineComment::json).collect();
        let res = self
            .req(reqwest::Method::POST, &format!("/repos/{owner}/{name}/pulls/{number}/reviews"))
            .json(&json!({ "commit_id": commit_id, "event": event, "body": body, "comments": comments }))
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY && !comments.is_empty() {
            return Ok(Posted::LinesRefused(text.chars().take(400).collect()));
        }
        if !status.is_success() {
            return Err(Error::Other(format!("GitHub {status}: {}", text.chars().take(400).collect::<String>())));
        }
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(Posted::Review(v.get("html_url").and_then(|u| u.as_str()).unwrap_or_default().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_name_from_outside_stays_two_plain_segments() {
        assert_eq!(repo_parts("acme/web").unwrap(), ("acme", "web"));
        assert_eq!(repo_parts("acme/web.js").unwrap(), ("acme", "web.js"));
        for bad in ["acme", "acme/web/pulls", "../web", "acme/..", "acme/we b", "/web", "acme/"] {
            assert!(repo_parts(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_range_and_a_removed_line_are_sent_as_github_names_them() {
        let c = |line, start_line, removed| LineComment { path: "a.ts".into(), line, start_line, removed, body: "b".into() };
        assert_eq!(c(4, None, false).json(), json!({ "path": "a.ts", "line": 4, "side": "RIGHT", "body": "b" }));
        assert_eq!(
            c(6, Some(2), false).json(),
            json!({ "path": "a.ts", "line": 6, "side": "RIGHT", "start_line": 2, "start_side": "RIGHT", "body": "b" })
        );
        assert_eq!(c(9, None, true).json()["side"], "LEFT");
        assert!(c(5, Some(5), false).json().get("start_line").is_none(), "a range of one line is a line");
    }

    #[test]
    fn only_the_three_verdicts_github_knows_are_sent() {
        assert_eq!(review_event("approve").unwrap(), "APPROVE");
        assert_eq!(review_event("request_changes").unwrap(), "REQUEST_CHANGES");
        assert_eq!(review_event("comment").unwrap(), "COMMENT");
        assert!(review_event("PENDING").is_err(), "a pending review would sit unseen on GitHub");
    }
}
