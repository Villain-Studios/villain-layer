//! A Model Context Protocol server, hosted by the app itself.
//!
//! Agents launched from Villain Layer get an `.mcp.json` pointing here, so they
//! reach Jira, GitHub and Slack through the credentials the app already holds —
//! no second login, and no copy of the tokens in the agent's environment.
//! Because the server talks to the *running* app, the tools also see live state
//! (tasks, worktrees, panes) and can drive it.
//!
//! Transport is Streamable HTTP with JSON responses, bound to loopback and
//! gated on a bearer token minted fresh each run.

use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::commands::{self, AppState};
use crate::error::Result;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Where the server ended up listening, and the token that admits callers.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();

pub fn endpoint() -> Option<&'static Endpoint> {
    ENDPOINT.get()
}

/// The `.mcp.json` an agent reads from its working directory.
pub fn mcp_json() -> Option<Value> {
    let e = endpoint()?;
    Some(json!({
        "mcpServers": {
            "villain-layer": {
                "type": "http",
                "url": e.url,
                "headers": { "Authorization": format!("Bearer {}", e.token) }
            }
        }
    }))
}

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    token: String,
}

/// Bind to an ephemeral loopback port and serve until the app exits.
pub async fn serve(app: AppHandle) -> Result<Endpoint> {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|e| crate::error::Error::Other(format!("mcp bind: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| crate::error::Error::Other(format!("mcp addr: {e}")))?;

    let endpoint = Endpoint {
        url: format!("http://127.0.0.1:{}/mcp", addr.port()),
        token: token.clone(),
    };
    let _ = ENDPOINT.set(endpoint.clone());

    let ctx = Ctx { app, token };
    let router = Router::new()
        .route("/mcp", post(handle))
        .with_state(ctx);

    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            eprintln!("villain-layer mcp server stopped: {e}");
        }
    });

    Ok(endpoint)
}

async fn handle(
    AxumState(ctx): AxumState<Ctx>,
    headers: HeaderMap,
    body: Json<Value>,
) -> impl IntoResponse {
    // Loopback is not authorisation on a shared machine; the token is.
    let ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == ctx.token);
    if !ok {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad token" }))).into_response();
    }

    let req = body.0;
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or_default();
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    // A notification carries no id and expects no body.
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }

    let result = dispatch(&ctx.app, method, params).await;
    let response = match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": e.to_string() }
        }),
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn dispatch(app: &AppHandle, method: &str, params: Value) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "villain-layer", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));

            // A tool failure is reported to the model, not as a protocol error,
            // so it can read the message and try something else.
            match call(app, &name, args).await {
                Ok(value) => Ok(json!({
                    "content": [{ "type": "text", "text": to_text(&value) }],
                    "isError": false
                })),
                Err(e) => Ok(json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true
                })),
            }
        }
        other => Err(crate::error::Error::Other(format!("unknown method {other}"))),
    }
}

fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    arg(args, key).ok_or_else(|| crate::error::Error::Other(format!("{key} is required")))
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

fn str_prop(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

fn tools() -> Vec<Value> {
    vec![
        tool(
            "list_tasks",
            "Tasks currently in flight in Villain Layer: branch, Jira key, and the \
             repositories each one has a worktree in, with uncommitted-change counts.",
            json!({}),
            vec![],
        ),
        tool(
            "list_repos",
            "Repositories registered in Villain Layer, with their group and path.",
            json!({}),
            vec![],
        ),
        tool(
            "task_diff",
            "Every file changed against the base branch across all of a task's \
             repositories.",
            json!({ "task_id": str_prop("Task id from list_tasks") }),
            vec!["task_id"],
        ),
        tool(
            "jira_search",
            "Search Jira with JQL, using the credentials stored in the app.",
            json!({
                "jql": str_prop("A JQL query, e.g. project = ACME AND statusCategory != Done"),
                "max": { "type": "integer", "description": "Maximum results, default 25" }
            }),
            vec!["jql"],
        ),
        tool(
            "jira_get_issue",
            "Read one Jira issue in full, including its description and epic.",
            json!({ "key": str_prop("Issue key, e.g. ACME-1234") }),
            vec!["key"],
        ),
        tool(
            "jira_create_issue",
            "File a new Jira issue. Returns the new key.",
            json!({
                "summary": str_prop("One-line title"),
                "description": str_prop("Body text; blank lines separate paragraphs"),
                "issue_type": str_prop("Type name as this site defines it, e.g. Bug or Task. Defaults to Task."),
                "project_key": str_prop("Defaults to the project configured in the app"),
                "parent_key": str_prop("Epic or parent key to file this under")
            }),
            vec!["summary"],
        ),
        tool(
            "jira_comment",
            "Add a comment to a Jira issue.",
            json!({ "key": str_prop("Issue key"), "text": str_prop("Comment body") }),
            vec!["key", "text"],
        ),
        tool(
            "jira_transition",
            "Move a Jira issue to another status by the transition's name, e.g. \
             'Start progress'. Call with no transition to list what is available.",
            json!({
                "key": str_prop("Issue key"),
                "transition": str_prop("Transition name; omit to list the options")
            }),
            vec!["key"],
        ),
        tool(
            "slack_post",
            "Post a message to the Slack channel configured in the app.",
            json!({
                "text": str_prop("Message body; Slack mrkdwn is supported"),
                "context": str_prop("Optional smaller context line underneath")
            }),
            vec!["text"],
        ),
        tool(
            "start_work",
            "Create a task from a Jira issue: a worktree in each named repository, \
             all on one branch, optionally with an agent started in it.",
            json!({
                "issue_key": str_prop("Jira issue key"),
                "repos": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Repository names from list_repos"
                },
                "agent": str_prop("Agent id to launch, e.g. claude. Omit for worktrees only."),
                "branch_suffix": str_prop("Optional suffix appended to the ticket key")
            }),
            vec!["issue_key", "repos"],
        ),
        tool(
            "open_prs",
            "Push every repository in a task that has changes and open a pull \
             request for each, then post the links back to the Jira ticket.",
            json!({
                "task_id": str_prop("Task id from list_tasks"),
                "title": str_prop("Pull request title"),
                "body": str_prop("Pull request description"),
                "draft": { "type": "boolean", "description": "Open as draft, default true" }
            }),
            vec!["task_id", "title"],
        ),
    ]
}

async fn call(app: &AppHandle, name: &str, args: Value) -> Result<Value> {
    let state = app.state::<AppState>();

    match name {
        "list_tasks" => Ok(serde_json::to_value(commands::list_tasks(state))?),

        "list_repos" => Ok(serde_json::to_value(commands::list_projects(state))?),

        "task_diff" => {
            let id = required(&args, "task_id")?.to_string();
            Ok(serde_json::to_value(commands::diff_files(state, id)?)?)
        }

        "jira_search" => {
            let jql = required(&args, "jql")?.to_string();
            let max = args.get("max").and_then(|m| m.as_u64()).unwrap_or(25) as u32;
            let (client, _) = commands::jira_client(&state)?;
            Ok(serde_json::to_value(client.search(&jql, max).await?)?)
        }

        "jira_get_issue" => {
            let key = required(&args, "key")?.to_string();
            let (client, _) = commands::jira_client(&state)?;
            Ok(serde_json::to_value(client.issue(&key).await?)?)
        }

        "jira_create_issue" => {
            let (client, cfg) = commands::jira_client(&state)?;
            let project = arg(&args, "project_key")
                .map(str::to_string)
                .or_else(|| cfg.project_key.clone())
                .ok_or_else(|| {
                    crate::error::Error::Other(
                        "no project_key given and none configured in the app".into(),
                    )
                })?;

            let key = client
                .create_issue(
                    &project,
                    required(&args, "summary")?,
                    arg(&args, "description").unwrap_or_default(),
                    arg(&args, "issue_type").unwrap_or("Task"),
                    arg(&args, "parent_key"),
                )
                .await?;
            Ok(json!({ "key": key, "url": format!("{}/browse/{key}", cfg.base_url) }))
        }

        "jira_comment" => {
            let (client, _) = commands::jira_client(&state)?;
            client
                .comment(required(&args, "key")?, required(&args, "text")?)
                .await?;
            Ok(json!({ "ok": true }))
        }

        "jira_transition" => {
            let key = required(&args, "key")?;
            let (client, _) = commands::jira_client(&state)?;
            let available = client.transitions(key).await?;

            let Some(wanted) = arg(&args, "transition") else {
                return Ok(serde_json::to_value(available)?);
            };
            let found = available
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(wanted))
                .ok_or_else(|| {
                    crate::error::Error::Other(format!(
                        "no transition named {wanted}; available: {}",
                        available
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            client.transition(key, &found.id).await?;
            Ok(json!({ "ok": true, "status": found.to_status }))
        }

        "slack_post" => {
            let text = required(&args, "text")?.to_string();
            let context = arg(&args, "context").map(str::to_string);
            commands::slack_notify(state, text, context).await?;
            Ok(json!({ "ok": true }))
        }

        "start_work" => {
            let issue_key = required(&args, "issue_key")?.to_string();
            let wanted: Vec<String> = args
                .get("repos")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if wanted.is_empty() {
                return Err(crate::error::Error::Other("repos must not be empty".into()));
            }

            // Agents think in repository names, not ids.
            let projects = state.config.read().projects;
            let mut ids = Vec::new();
            for name in &wanted {
                let found = projects
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        crate::error::Error::NotFound(format!(
                            "no repository named {name}; known: {}",
                            projects
                                .iter()
                                .map(|p| p.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                ids.push(found.id.clone());
            }

            let task = commands::jira_start_work(
                app.clone(),
                state,
                issue_key,
                ids,
                arg(&args, "agent").map(str::to_string),
                arg(&args, "branch_suffix").map(str::to_string),
            )
            .await?;
            Ok(serde_json::to_value(task)?)
        }

        "open_prs" => {
            let task_id = required(&args, "task_id")?.to_string();
            let title = required(&args, "title")?.to_string();
            let body = arg(&args, "body").unwrap_or_default().to_string();
            let draft = args.get("draft").and_then(|d| d.as_bool()).unwrap_or(true);
            Ok(serde_json::to_value(
                commands::github_open_prs(state, task_id, title, body, draft).await?,
            )?)
        }

        other => Err(crate::error::Error::NotFound(format!("tool {other}"))),
    }
}

/// Drop an `.mcp.json` next to an agent so it picks the server up on start.
pub fn write_config(dir: &std::path::Path) -> Result<()> {
    let Some(config) = mcp_json() else {
        return Ok(());
    };
    std::fs::write(dir.join(".mcp.json"), serde_json::to_vec_pretty(&config)?)?;
    Ok(())
}
