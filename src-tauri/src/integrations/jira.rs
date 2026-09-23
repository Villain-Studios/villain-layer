//! Jira Cloud REST v3. Auth is Basic <email:api-token>; the token lives in the
//! keychain. Descriptions arrive as ADF, so they get flattened for agent prompts.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::http_client;
use crate::config::JiraConfig;
use crate::error::{Error, Result};

pub struct Jira {
    base_url: String,
    auth: String,
    client: reqwest::Client,
    /// This site's Epic Link field, when it has one. Ids are allocated per
    /// site, so the same field is a different number on every Jira — it has to
    /// be discovered rather than written down.
    epic_field: Option<String>,
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

/// An issue type exactly as this Jira defines it. Nothing about types is
/// assumed: the names, the icons and the hierarchy all come from the site, so
/// a project with custom types renders as correctly as a stock one.
#[derive(Debug, Clone, Serialize)]
pub struct IssueType {
    pub id: String,
    pub name: String,
    pub subtask: bool,
    /// 1 and above is epic-level, 0 is a standard issue, -1 is a sub-task.
    /// This is the one structural fact that holds across every Jira.
    pub hierarchy_level: i32,
    /// Jira's own icon as a data URI. Avatar URLs need authentication, so the
    /// image is fetched here rather than by an <img> tag in the web view.
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_status: String,
    /// The category the target status sits in: "new", "indeterminate" or
    /// "done". Status *names* differ between sites and workflows; the three
    /// categories are Jira's own and mean the same everywhere, which is what
    /// makes it safe to pick a transition without being told which one.
    pub to_category: String,
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
            client: http_client(),
            epic_field: cfg.epic_field.clone(),
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

    /// Every issue type this site defines, with its icon inlined.
    pub async fn issue_types(&self) -> Result<Vec<IssueType>> {
        let v = self
            .json(self.req(reqwest::Method::GET, "/rest/api/3/issuetype"))
            .await?;
        let raw = v.as_array().cloned().unwrap_or_default();

        let mut out: Vec<IssueType> = Vec::new();
        let mut icons: Vec<String> = Vec::new();
        for t in raw {
            let name = str_at(&t, "name");
            // A site repeats a type per project scheme; one entry each is enough.
            if name.is_empty() || out.iter().any(|e| e.name.eq_ignore_ascii_case(&name)) {
                continue;
            }
            icons.push(str_at(&t, "iconUrl"));
            out.push(IssueType {
                id: str_at(&t, "id"),
                subtask: t.get("subtask").and_then(|b| b.as_bool()).unwrap_or(false),
                hierarchy_level: t
                    .get("hierarchyLevel")
                    .and_then(|h| h.as_i64())
                    .unwrap_or(0) as i32,
                icon: None,
                name,
            });
        }
        // Together, not in turn: a site with thirty types was thirty round
        // trips before the Tickets view could draw its first badge.
        let fetched =
            futures_util::future::join_all(icons.iter().map(|url| self.fetch_icon(url))).await;
        for (t, icon) in out.iter_mut().zip(fetched) {
            t.icon = icon;
        }
        Ok(out)
    }

    /// Best-effort: a missing icon degrades to a generated badge, it is never
    /// worth failing the whole list over.
    async fn fetch_icon(&self, url: &str) -> Option<String> {
        if url.is_empty() {
            return None;
        }
        // Credentials go only to this site. An icon URL is whatever the
        // site says, and on some that is a CDN or another host entirely.
        let own = reqwest::Url::parse(url).ok().zip(reqwest::Url::parse(&self.base_url).ok())
            .is_some_and(|(icon, site)| icon.origin() == site.origin());
        let mut req = self.client.get(url);
        if own {
            req = req.header("Authorization", &self.auth);
        }
        let res = req.send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }

        let mime = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/png")
            .split(';')
            .next()
            .unwrap_or("image/png")
            .to_string();

        let bytes = res.bytes().await.ok()?;
        // Guard against anything that is obviously not an icon.
        if bytes.is_empty() || bytes.len() > 256 * 1024 {
            return None;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Some(format!("data:{mime};base64,{b64}"))
    }

    /// Issues matching `jql`, following Jira's paging up to `max`.
    ///
    /// Jira returns a page at a time whatever is asked for, so a single request
    /// quietly truncates: a queue of seventy issues came back as fifty, with
    /// nothing to say twenty were missing. `Page::more` says whether Jira still
    /// had results when the cap was reached, so a caller can tell the
    /// difference between "that is all of it" and "that is all you asked for".
    pub async fn search(&self, jql: &str, max: u32) -> Result<Page> {
        let mut fields = json!([
            "summary", "description", "status", "issuetype",
            "priority", "assignee", "labels", "components", "parent",
        ]);
        // Asked for only when this site has one; its id differs per site.
        if let Some(field) = &self.epic_field {
            if let Some(list) = fields.as_array_mut() {
                list.push(json!(field));
            }
        }

        let mut issues: Vec<Issue> = Vec::new();
        let mut token: Option<String> = None;
        let mut more = false;

        while issues.len() < max as usize {
            let want = (max as usize - issues.len()).min(100);
            let mut body = json!({ "jql": jql, "maxResults": want, "fields": fields });
            if let Some(t) = &token {
                body["nextPageToken"] = json!(t);
            }

            // The modern endpoint; older Jira instances still serve /search,
            // which has no page token — so those stop after one page, as before.
            // Only a missing endpoint falls back: a bad JQL or a rejected token
            // retried against the old endpoint would come back as *its* error,
            // hiding the one that says what is actually wrong.
            let v = match self
                .json(
                    self.req(reqwest::Method::POST, "/rest/api/3/search/jql")
                        .json(&body),
                )
                .await
            {
                Ok(v) => v,
                Err(Error::Other(msg)) if msg.starts_with("Jira 404") => {
                    self.json(
                        self.req(reqwest::Method::POST, "/rest/api/3/search")
                            .json(&body),
                    )
                    .await?
                }
                Err(e) => return Err(e),
            };

            let page = v
                .get("issues")
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            // A token with an empty page would otherwise loop for ever.
            if page.is_empty() {
                break;
            }
            issues.extend(page.iter().map(|i| self.to_issue(i)));

            token = v
                .get("nextPageToken")
                .and_then(|t| t.as_str())
                .map(str::to_string);
            if token.is_none() {
                break;
            }
            more = true;
        }

        // Only truthfully "more" if we stopped at the cap with a page to spare.
        Ok(Page {
            more: more && token.is_some() && issues.len() >= max as usize,
            issues,
        })
    }

    pub async fn issue(&self, key: &str) -> Result<Issue> {
        let v = self
            .json(self.req(reqwest::Method::GET, &format!("/rest/api/3/issue/{}", segment(key)?)))
            .await?;
        Ok(self.to_issue(&v))
    }

    pub async fn transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let v = self
            .json(self.req(
                reqwest::Method::GET,
                &format!("/rest/api/3/issue/{}/transitions", segment(key)?),
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
                        to_category: t
                            .pointer("/to/statusCategory/key")
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
                &format!("/rest/api/3/issue/{}/transitions", segment(key)?),
            )
            .json(&json!({ "transition": { "id": transition_id } })),
        )
        .await?;
        Ok(())
    }

    /// Create an issue. `description` is plain text; Jira wants ADF, so it is
    /// wrapped into one paragraph per line.
    pub async fn create_issue(
        &self,
        project_key: &str,
        summary: &str,
        description: &str,
        issue_type: &str,
        parent_key: Option<&str>,
        extra: &Value,
    ) -> Result<String> {
        let mut fields = json!({
            "project": { "key": project_key },
            "summary": summary,
            "issuetype": { "name": issue_type },
            "description": text_to_adf(description),
        });
        if let Some(parent) = parent_key.filter(|p| !p.is_empty()) {
            fields["parent"] = json!({ "key": parent });
        }
        // Whatever else this project demands. Shaped by the caller from the
        // metadata Jira itself reported, so nothing about any one site's
        // required fields is written down here.
        if let (Some(extra), Some(into)) = (extra.as_object(), fields.as_object_mut()) {
            for (k, v) in extra {
                into.insert(k.clone(), v.clone());
            }
        }

        let v = self
            .json(
                self.req(reqwest::Method::POST, "/rest/api/3/issue")
                    .json(&json!({ "fields": fields })),
            )
            .await?;
        Ok(str_at(&v, "key"))
    }

    /// The id this site uses for the Epic Link field, if it still has one.
    ///
    /// Matched on the field's `schema.custom`, which is the same string on
    /// every Jira; the numeric id after `customfield_` is not.
    ///
    /// `Ok(None)` is an answer — the site has no such field — and an error is
    /// not, so the two can be remembered differently.
    pub async fn epic_link_field(&self) -> Result<Option<String>> {
        const EPIC_LINK: &str = "com.pyxis.greenhopper.jira:gh-epic-link";
        let v = self
            .json(self.req(reqwest::Method::GET, "/rest/api/3/field"))
            .await?;
        Ok(v.as_array().and_then(|fields| {
            fields.iter().find_map(|f| {
                (f.pointer("/schema/custom").and_then(|c| c.as_str()) == Some(EPIC_LINK))
                    .then(|| str_at(f, "id"))
                    .filter(|id| !id.is_empty())
            })
        }))
    }

    /// What this project demands before it will create an issue of this type.
    ///
    /// Asked of Jira rather than assumed: one project makes components
    /// mandatory, another a custom field, most neither. The only way to build a
    /// form that works on every site is to be told what the form is.
    pub async fn create_fields(
        &self,
        project_key: &str,
        issue_type_id: &str,
    ) -> Result<Vec<CreateField>> {
        // Paged, fifty to a page by default. Reading the first page alone
        // lost whatever came after it — and a required field past the
        // fiftieth never reached the form, so creating failed with "X is
        // required" for a field nobody had been asked about.
        let path = format!(
            "/rest/api/3/issue/createmeta/{}/issuetypes/{}",
            segment(project_key)?,
            segment(issue_type_id)?
        );
        let mut fields: Vec<Value> = Vec::new();
        for _ in 0..20 {
            let at = fields.len().to_string();
            let v = self
                .json(
                    self.req(reqwest::Method::GET, &path)
                        .query(&[("startAt", at.as_str()), ("maxResults", "100")]),
                )
                .await?;
            let page = v
                .get("fields")
                .and_then(|f| f.as_array())
                .cloned()
                .unwrap_or_default();
            let total = v.get("total").and_then(Value::as_u64);
            let got = page.len();
            fields.extend(page);
            let done = match total {
                Some(total) => fields.len() as u64 >= total,
                None => true,
            };
            if got == 0 || done {
                break;
            }
        }

        Ok(fields
            .iter()
            .map(|f| CreateField {
                id: str_at(f, "fieldId"),
                name: str_at(f, "name"),
                required: f.get("required").and_then(Value::as_bool).unwrap_or(false),
                kind: f
                    .pointer("/schema/type")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string(),
                allowed: f
                    .get("allowedValues")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .map(|v| AllowedValue {
                                id: str_at(v, "id"),
                                // Components use `name`, many custom fields use
                                // `value`, versions use `name` too.
                                name: match v.get("name").and_then(Value::as_str) {
                                    Some(n) => n.to_string(),
                                    None => str_at(v, "value"),
                                },
                            })
                            .filter(|v| !v.id.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .collect())
    }

    /// The issue types one project offers, as (id, name).
    pub async fn project_issue_types(&self, project_key: &str) -> Result<Vec<(String, String)>> {
        let v = self
            .json(
                self.req(
                    reqwest::Method::GET,
                    &format!("/rest/api/3/issue/createmeta/{}/issuetypes", segment(project_key)?),
                )
                .query(&[("maxResults", "200")]),
            )
            .await?;
        Ok(v.get("issueTypes")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().map(|t| (str_at(t, "id"), str_at(t, "name"))).collect())
            .unwrap_or_default())
    }

    /// Move an issue into whatever this workflow calls "in progress".
    ///
    /// Picked by status category rather than by name: "In Progress", "Doing",
    /// "In Bearbeitung" and whatever else a team has called it all share the
    /// `indeterminate` category, so nothing here has to be told about a
    /// particular Jira.
    ///
    /// Returns the status it moved to, or None when there was nothing to do —
    /// already in progress, or no transition offered.
    pub async fn start_progress(&self, key: &str) -> Result<Option<String>> {
        let issue = self.issue(key).await?;
        if issue.status_category == "indeterminate" {
            return Ok(None);
        }

        let available = self.transitions(key).await?;
        let Some(wanted) = available.iter().find(|t| t.to_category == "indeterminate") else {
            return Ok(None);
        };
        self.transition(key, &wanted.id).await?;
        Ok(Some(wanted.to_status.clone()))
    }

    pub async fn comment(&self, key: &str, text: &str) -> Result<()> {
        // A paragraph per line. One text node holding newlines is not
        // something ADF breaks on, so a list of PR links, or an agent's
        // multi-line note, arrived as one run-on line.
        let body = json!({ "body": text_to_adf(text) });
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/rest/api/3/issue/{}/comment", segment(key)?),
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
            epic_key: epic_key(&f, self.epic_field.as_deref()),
            epic_summary: f
                .pointer("/parent/fields/summary")
                .and_then(|s| s.as_str())
                .map(str::to_string),
        }
    }
}

/// Jira Cloud unified epics onto `parent`, but older company-managed projects
/// still carry the Epic Link in a custom field named per site.
fn epic_key(fields: &Value, epic_field: Option<&str>) -> Option<String> {
    if let Some(key) = fields.pointer("/parent/key").and_then(|k| k.as_str()) {
        return Some(key.to_string());
    }
    fields
        .get(epic_field?)
        .and_then(|v| v.as_str())
        .filter(|s| s.contains('-'))
        .map(str::to_string)
}

/// Plain text as a minimal ADF document: one paragraph per non-empty line.
pub fn text_to_adf(text: &str) -> Value {
    let content: Vec<Value> = text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                json!({ "type": "paragraph", "content": [] })
            } else {
                json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect();

    json!({ "type": "doc", "version": 1, "content": content })
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

/// A field a project insists on before it will accept a new issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateField {
    pub id: String,
    pub name: String,
    pub required: bool,
    /// "array" when Jira expects several values, otherwise the scalar type.
    pub kind: String,
    /// The values this field will accept, when it is a closed set.
    pub allowed: Vec<AllowedValue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllowedValue {
    pub id: String,
    pub name: String,
}

/// A page of search results, and whether Jira had more to give.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub issues: Vec<Issue>,
    /// True when the cap was reached and Jira still had results.
    pub more: bool,
}

/// A key or id as one segment of a REST path.
///
/// Keys reach here from agents through the MCP server, and one reading a
/// poisoned ticket could be steered into passing `../../…`: an authenticated
/// GET anywhere on the site. Keys, ids and project keys are all letters,
/// digits, `-` and `_`.
fn segment(raw: &str) -> Result<&str> {
    let raw = raw.trim();
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(Error::Other(format!("{raw:?} is not a Jira key")));
    }
    Ok(raw)
}

/// The JQL used when the user has not written their own.
pub fn default_jql(project_key: Option<&str>) -> String {
    let scope = project_key
        // Quoted: a key that is a JQL word — IT, DO, IF — failed the whole
        // queue with "'it' is a reserved JQL word".
        .map(|k| format!("project = {} AND ", jql_string(k)))
        .unwrap_or_default();
    format!("{scope}assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC")
}

/// Quote a value as a JQL string literal.
///
/// Search text comes from a text box, and a stray quote would otherwise end
/// the literal and let the rest of what was typed be read as query syntax.
pub(crate) fn jql_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether something reads as an issue key rather than words to search for.
fn looks_like_a_key(text: &str) -> bool {
    let Some((project, number)) = text.split_once('-') else {
        return false;
    };
    !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && project.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

/// Who a browse query is looking at, beyond your own queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    /// Nobody has picked it up.
    Unassigned,
    /// Anyone but you, including nobody.
    NotMine,
    /// Everyone, yourself included.
    Anyone,
}

impl Whose {
    pub fn parse(s: &str) -> Self {
        match s {
            "unassigned" => Whose::Unassigned,
            "anyone" => Whose::Anyone,
            _ => Whose::NotMine,
        }
    }

    fn clause(self) -> Option<&'static str> {
        match self {
            Whose::Unassigned => Some("assignee IS EMPTY"),
            // `assignee != currentUser()` alone drops unassigned issues, which
            // are the ones most worth finding.
            Whose::NotMine => Some("(assignee IS EMPTY OR assignee != currentUser())"),
            Whose::Anyone => None,
        }
    }
}

/// JQL for looking past your own queue.
///
/// An issue key is matched exactly — typing one is asking for that ticket, and
/// a full-text search for it often finds everything that merely mentions it.
pub fn browse_jql(
    project_key: Option<&str>,
    text: Option<&str>,
    whose: Whose,
    include_done: bool,
    types: &[String],
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(key) = project_key.map(str::trim).filter(|k| !k.is_empty()) {
        parts.push(format!("project = {}", jql_string(key)));
    }
    if let Some(clause) = whose.clause() {
        parts.push(clause.to_string());
    }
    if !include_done {
        parts.push("statusCategory != Done".into());
    }
    // Names, not ids: they are what the site reported and what the user picked.
    let types: Vec<String> = types
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(jql_string)
        .collect();
    if !types.is_empty() {
        parts.push(format!("issuetype IN ({})", types.join(", ")));
    }
    if let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(if looks_like_a_key(text) {
            format!("key = {}", jql_string(text))
        } else {
            format!("text ~ {}", jql_string(text))
        });
    }

    if parts.is_empty() {
        return "ORDER BY updated DESC".into();
    }
    format!("{} ORDER BY updated DESC", parts.join(" AND "))
}

#[cfg(test)]
mod browse_tests {
    use super::*;

    #[test]
    fn only_a_key_goes_into_a_path() {
        assert_eq!(segment(" ACME-12 ").unwrap(), "ACME-12");
        assert_eq!(segment("10042").unwrap(), "10042");
        assert!(segment("../../rest/api/3/myself").is_err());
        assert!(segment("ACME-1?expand=x").is_err());
        assert!(segment("").is_err());
    }

    #[test]
    fn a_key_is_looked_up_rather_than_searched_for() {
        assert!(looks_like_a_key("ACME-21042"));
        assert!(looks_like_a_key("AB1-7"));
        assert!(!looks_like_a_key("acme-21042"), "lower case is prose, not a key");
        assert!(!looks_like_a_key("ACME-"));
        assert!(!looks_like_a_key("-21042"));
        assert!(!looks_like_a_key("rounding error"));
        assert!(!looks_like_a_key("ACME-21042-fix"));

        assert!(browse_jql(None, Some("ACME-9"), Whose::Anyone, false, &[]).contains(r#"key = "ACME-9""#));
        assert!(browse_jql(None, Some("rounding"), Whose::Anyone, false, &[])
            .contains("text ~ \"rounding\""));
    }

    #[test]
    fn picked_types_narrow_the_search() {
        let jql = browse_jql(None, None, Whose::Anyone, false, &["Bug".into(), "Epic".into()]);
        assert!(jql.contains(r#"issuetype IN ("Bug", "Epic")"#), "{jql}");

        // Nothing picked means every type, not none of them.
        assert!(!browse_jql(None, None, Whose::Anyone, false, &[]).contains("issuetype"));
        assert!(!browse_jql(None, None, Whose::Anyone, false, &["  ".into()]).contains("issuetype"));
    }

    #[test]
    fn typed_quotes_cannot_escape_the_literal() {
        let jql = browse_jql(None, Some("say \"hi\" \\ bye"), Whose::Anyone, false, &[]);
        assert!(jql.contains("text ~ \"say \\\"hi\\\" \\\\ bye\""), "{jql}");
    }

    #[test]
    fn unassigned_is_kept_in_what_is_not_mine() {
        // The obvious `assignee != currentUser()` silently drops every issue
        // nobody has picked up, which is most of what this view is for.
        let mine_excluded = browse_jql(None, None, Whose::NotMine, false, &[]);
        assert!(mine_excluded.contains("assignee IS EMPTY OR assignee != currentUser()"));

        assert_eq!(
            browse_jql(Some("ACME"), None, Whose::Unassigned, false, &[]),
            r#"project = "ACME" AND assignee IS EMPTY AND statusCategory != Done ORDER BY updated DESC"#,
        );
        assert_eq!(
            browse_jql(None, None, Whose::Anyone, true, &[]),
            "ORDER BY updated DESC",
        );
    }
}
