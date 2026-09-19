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
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{repo}/pulls?state=open&head={owner}:{branch}"),
            ))
            .await?;
        Ok(v.as_array().and_then(|a| a.first()).map(to_pr))
    }

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
    }
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}
