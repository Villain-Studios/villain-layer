//! Jira Cloud REST v3. Auth is Basic <email:api-token>; the token lives in the
//! keychain. Descriptions arrive as ADF, so they get flattened for agent prompts.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::JiraConfig;
use crate::error::{Error, Result};

pub struct Jira {
    base_url: String,
    auth: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub key: String,
    pub summary: String,
    pub description: String,
    pub status: String,
    pub status_category: String,
    pub issue_type: String,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub components: Vec<String>,
    /// The epic (or other parent) this issue sits under, when it has one.
    pub epic_key: Option<String>,
    pub epic_summary: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_status: String,
}

#[derive(Debug, Deserialize)]
pub struct Myself {
    #[serde(rename = "displayName")]
    pub display_name: String,
}

impl Jira {
    pub fn new(cfg: &JiraConfig, token: &str) -> Self {
        let auth = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", cfg.email, token));
        Self {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            auth: format!("Basic {auth}"),
            client: reqwest::Client::new(),
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .header("Authorization", &self.auth)
            .header("Accept", "application/json")
    }

    async fn json(&self, rb: reqwest::RequestBuilder) -> Result<Value> {
        let res = rb.send().await?;
        let status = res.status();
        let body = res.text().await?;
        if !status.is_success() {
            return Err(Error::Other(format!("Jira {status}: {}", trim(&body))));
        }
        if body.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn myself(&self) -> Result<Myself> {
        let v = self.json(self.req(reqwest::Method::GET, "/rest/api/3/myself")).await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn search(&self, jql: &str, max: u32) -> Result<Vec<Issue>> {
        let fields = json!([
            "summary", "description", "status", "issuetype",
            "priority", "assignee", "labels", "components", "parent",
            // Company-managed projects may still expose the epic here.
            "customfield_10014"
        ]);

        // The modern endpoint; older Jira instances still serve /search.
        let body = json!({ "jql": jql, "maxResults": max, "fields": fields });
        let v = match self
            .json(
                self.req(reqwest::Method::POST, "/rest/api/3/search/jql")
                    .json(&body),
            )
            .await
        {
            Ok(v) => v,
            Err(_) => {
                self.json(
                    self.req(reqwest::Method::POST, "/rest/api/3/search")
                        .json(&body),
                )
                .await?
            }
        };

        let issues = v
            .get("issues")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(issues.iter().map(|i| self.to_issue(i)).collect())
    }

    pub async fn issue(&self, key: &str) -> Result<Issue> {
        let v = self
            .json(self.req(reqwest::Method::GET, &format!("/rest/api/3/issue/{key}")))
            .await?;
        Ok(self.to_issue(&v))
    }

    pub async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/rest/api/3/issue/{key}/transitions"),
            ))
            .await?;

        Ok(v.get("transitions")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|t| Transition {
                        id: str_at(t, "id"),
                        name: str_at(t, "name"),
                        to_status: t
                            .pointer("/to/name")
                            .and_then(|s| s.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn transition(&self, key: &str, transition_id: &str) -> Result<()> {
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/3/issue/{key}/transitions"),
            )
            .json(&json!({ "transition": { "id": transition_id } })),
        )
        .await?;
        Ok(())
    }

    pub async fn comment(&self, key: &str, text: &str) -> Result<()> {
        let body = json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": text }]
                }]
            }
        });
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/3/issue/{key}/comment"),
            )
            .json(&body),
        )
        .await?;
        Ok(())
    }

    fn to_issue(&self, v: &Value) -> Issue {
        let f = v.get("fields").cloned().unwrap_or(Value::Null);
        let key = str_at(v, "key");
        Issue {
            url: format!("{}/browse/{}", self.base_url, key),
            key,
            summary: str_at(&f, "summary"),
            description: adf_to_text(f.get("description")),
            status: f
                .pointer("/status/name")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            status_category: f
                .pointer("/status/statusCategory/key")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            issue_type: f
                .pointer("/issuetype/name")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            priority: f
                .pointer("/priority/name")
                .and_then(|s| s.as_str())
                .map(str::to_string),
            assignee: f
                .pointer("/assignee/displayName")
                .and_then(|s| s.as_str())
                .map(str::to_string),
            labels: f
                .get("labels")
                .and_then(|l| l.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            components: f
                .get("components")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            epic_key: epic_key(&f),
            epic_summary: f
                .pointer("/parent/fields/summary")
                .and_then(|s| s.as_str())
                .map(str::to_string),
        }
    }
}

/// Jira Cloud unified epics onto `parent`, but older company-managed projects
/// still carry the Epic Link in a custom field.
fn epic_key(fields: &Value) -> Option<String> {
    if let Some(key) = fields.pointer("/parent/key").and_then(|k| k.as_str()) {
        return Some(key.to_string());
    }
    fields
        .get("customfield_10014")
        .and_then(|v| v.as_str())
        .filter(|s| s.contains('-'))
        .map(str::to_string)
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

fn trim(s: &str) -> String {
    s.chars().take(400).collect()
}

/// Walk an Atlassian Document Format tree into readable plain text. Good enough
/// to hand an agent as context; not a faithful renderer.
pub fn adf_to_text(node: Option<&Value>) -> String {
    let Some(node) = node else {
        return String::new();
    };
    // Some endpoints return a plain string description.
    if let Some(s) = node.as_str() {
        return s.to_string();
    }

    let mut out = String::new();
    walk(node, &mut out);
    out.trim().to_string()
}

fn walk(node: &Value, out: &mut String) {
    match node.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            if let Some(t) = node.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
            }
        }
        Some("hardBreak") => out.push('\n'),
        Some("paragraph") | Some("heading") => {
            children(node, out);
            out.push_str("\n\n");
        }
        Some("listItem") => {
            out.push_str("- ");
            children(node, out);
        }
        Some("codeBlock") => {
            out.push_str("```\n");
            children(node, out);
            out.push_str("\n```\n\n");
        }
        Some("rule") => out.push_str("\n---\n"),
        Some("inlineCard") => {
            if let Some(url) = node.pointer("/attrs/url").and_then(|u| u.as_str()) {
                out.push_str(url);
            }
        }
        _ => children(node, out),
    }
}

fn children(node: &Value, out: &mut String) {
    if let Some(arr) = node.get("content").and_then(|c| c.as_array()) {
        for child in arr {
            walk(child, out);
        }
    }
}

/// The JQL used when the user has not written their own.
pub fn default_jql(project_key: Option<&str>) -> String {
    let scope = project_key
        .map(|k| format!("project = {k} AND "))
        .unwrap_or_default();
    format!("{scope}assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC")
}
