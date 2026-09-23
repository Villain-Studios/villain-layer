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

use axum::extract::{Path, State as AxumState};
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

/// What the server is called in every config that names it. Claude Code's
/// approval of it is recorded under this name, so it must not change.
pub const SERVER_NAME: &str = "villain-layer";

/// Where an agent's hooks report what it is doing: `<this>/<pane id>`.
pub fn hook_url() -> Option<String> {
    endpoint().map(|e| format!("{}/hook", e.url.trim_end_matches("/mcp")))
}

/// The `.mcp.json` an agent reads from its working directory.
pub fn mcp_json() -> Option<Value> {
    let e = endpoint()?;
    Some(json!({
        "mcpServers": {
            SERVER_NAME: {
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
        .route("/hook/{pane}", post(hook))
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
    if !bearer_ok(&headers, &ctx.token) {
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

/// One of an agent's hooks, saying what it is doing.
///
/// Answers 204 whatever it made of the event: the hook has nothing to do with
/// the answer, and anything else is printed into the agent's own transcript.
async fn hook(
    AxumState(ctx): AxumState<Ctx>,
    Path(pane): Path<String>,
    headers: HeaderMap,
    body: Json<Value>,
) -> impl IntoResponse {
    if !bearer_ok(&headers, &ctx.token) {
        return StatusCode::UNAUTHORIZED;
    }
    if take_hook(&ctx.app.state::<AppState>().ptys, &pane, &body.0) {
        use tauri::Emitter;
        let _ = ctx.app.emit("pty:activity", &pane);
    }
    StatusCode::NO_CONTENT
}

/// Read one hook post into its pane. True when the pane now reads
/// differently.
///
/// Each CLI posts in its own shape; which one this is follows from what the
/// pane is running, not from anything in the post.
pub(crate) fn take_hook(ptys: &crate::pty::PtyManager, pane: &str, payload: &Value) -> bool {
    let Some(def) = ptys
        .info(pane)
        .ok()
        .and_then(|p| p.agent_id)
        .and_then(|id| crate::agents::find(&id))
    else {
        return false;
    };
    ptys.report_with(pane, |now| crate::agents::hook_activity(def.integration, payload, now))
        .unwrap_or(false)
}

fn bearer_ok(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token)
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

fn bool_prop(desc: &str) -> Value {
    json!({ "type": "boolean", "description": desc })
}

/// High-impact writes that change shared state. Soft tools (create issue, post
/// to Slack, start work) still run unattended — that is the point — but these
/// need an explicit second call with `confirm: true` after the user agrees.
const DANGER: &[&str] = &[
    "jira_transition",
    "slack_delete",
    "slack_cleanup",
    "forget_repo",
    "open_prs",
];

fn confirm_prop() -> Value {
    bool_prop(
        "Set true only after the user agrees. Calls without it are refused — \
         this tool changes shared state that is hard to undo.",
    )
}

fn require_confirm(args: &Value, tool: &str) -> Result<()> {
    if args.get("confirm").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(crate::error::Error::Other(format!(
        "{tool} needs confirm: true after the user agrees; nothing was changed"
    )))
}

/// Listing transitions and dry-run cleanup are reads in all but name.
fn tool_needs_confirm(name: &str, args: &Value) -> bool {
    match name {
        "jira_transition" => arg(args, "transition").is_some(),
        "slack_cleanup" => !args.get("dry_run").and_then(Value::as_bool).unwrap_or(false),
        other => DANGER.contains(&other),
    }
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
            "jira_issue_types",
            "The issue types this Jira defines, with their hierarchy level: 1 and \
             above is epic-level, 0 a standard issue, -1 a sub-task. Names differ \
             between sites, so ask rather than assuming one is called Task.",
            json!({}),
            vec![],
        ),
        tool(
            "jira_create_fields",
            "What a project demands before it will accept a new issue of a given \
             type: the required fields and the values each will take. Ask this \
             before jira_create_issue — many projects make components or a custom \
             field mandatory, and the create is refused without them.",
            json!({
                "project_key": str_prop("Project key, e.g. the ACME in ACME-1234"),
                "issue_type_id": str_prop("Issue type id; jira_issue_types lists them")
            }),
            vec!["project_key", "issue_type_id"],
        ),
        tool(
            "jira_create_issue",
            "File a new Jira issue. Returns the new key.",
            json!({
                "summary": str_prop("One-line title"),
                "description": str_prop("Body text; blank lines separate paragraphs"),
                "issue_type": str_prop("Type name as this site defines it, e.g. Bug or Task. Defaults to Task."),
                "project_key": str_prop("Defaults to the project configured in the app"),
                "parent_key": str_prop("Epic or parent key to file this under"),
                "fields": {
                    "type": "object",
                    "description": "The other fields jira_create_fields says this project \
                                    requires, by field id: {\"id\": \"…\"} for one allowed \
                                    value, a list of them for an array field, e.g. \
                                    {\"components\": [{\"id\": \"10001\"}]}"
                }
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
             'Start progress'. Call with no transition to list what is available. \
             Applying a transition needs confirm: true after the user agrees.",
            json!({
                "key": str_prop("Issue key"),
                "transition": str_prop("Transition name; omit to list the options"),
                "confirm": confirm_prop()
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
            "slack_cleanup",
            "Delete messages this app posted to its Slack channel. Only a bot can \
             delete a bot's messages, so this is the only way to clear them. \
             Deleting needs confirm: true; dry_run does not.",
            json!({
                "dry_run": { "type": "boolean", "description": "Count without deleting" },
                "confirm": confirm_prop()
            }),
            vec![],
        ),
        tool(
            "slack_delete",
            "Delete specific messages this app posted, by Slack permalink. Works \
             with only chat:write, unlike slack_cleanup which has to search. \
             Needs confirm: true after the user agrees.",
            json!({
                "links": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Slack permalinks (Copy link on the message)"
                },
                "confirm": confirm_prop()
            }),
            vec!["links"],
        ),
        tool(
            "slack_diagnose",
            "What the app's Slack token can see and do: bot id, granted scopes, \
             configured channel.",
            json!({}),
            vec![],
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
                "branch_suffix": str_prop("Optional suffix appended to the ticket key"),
                "base": str_prop("Branch to cut from in every repo. Each repo's default when omitted.")
            }),
            vec!["issue_key", "repos"],
        ),
        tool(
            "handoff_prompt",
            "Everything another agent needs to take over a pane's work: the \
             ticket, the diff so far, and the outgoing agent's terminal tail. \
             There is no portable session format between CLIs, so this is what \
             travels.",
            json!({ "pane_id": str_prop("Pane id from list_panes") }),
            vec!["pane_id"],
        ),
        tool(
            "list_panes",
            "Panes running in the app, with which task they belong to and \
             whether the agent has reported hitting a usage limit.",
            json!({ "task_id": str_prop("Only this task's panes; omit for all") }),
            vec![],
        ),
        tool(
            "pane_output",
            "What a terminal in Villain Layer has printed, with the escape codes \
             stripped: a dev server's log, a test run, or another agent's session. \
             Read it rather than starting a second copy of something already \
             running. list_panes gives the ids. Off unless the user has turned it \
             on, since a shell's scrollback is a record of everything typed in it.",
            json!({
                "pane_id": str_prop("Pane id from list_panes"),
                "lines": { "type": "integer", "description": "How many lines from the end (default 200, max 2000)" }
            }),
            vec!["pane_id"],
        ),
        tool(
            "task_prs",
            "Pull request state for every repository in a task: the open PR if \
             there is one, its check runs, and how many files have changed.",
            json!({ "task_id": str_prop("Task id from list_tasks") }),
            vec!["task_id"],
        ),
        tool(
            "add_repo",
            "Add a repository to a task you are already working in, when the work \
             turns out to need code that is not in front of you. A worktree on the \
             task's branch is created beside the ones already there, and the path \
             comes back. Prefer this to reading the original clone: that one is on \
             its own branch and is not yours to change.",
            json!({
                "task_id": str_prop("Task id from list_tasks; match it by the directory you are working in"),
                "repo": str_prop("Repository name from list_repos")
            }),
            vec!["task_id", "repo"],
        ),
        tool(
            "forget_repo",
            "Remove a repository from Villain Layer's list. The clone and any \
             worktrees stay on disk untouched; only the app forgets it. Needs \
             confirm: true after the user agrees.",
            json!({
                "repo": str_prop("Repository name, or its exact path when two share a name"),
                "confirm": confirm_prop()
            }),
            vec!["repo"],
        ),
        tool(
            "create_task",
            "Create a task with no Jira ticket behind it: a worktree in each named \
             repository, all on one branch.",
            json!({
                "name": str_prop("What the work is; the branch is derived from it"),
                "repos": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Repository names from list_repos"
                },
                "branch": str_prop("Explicit branch name, overriding the derived one"),
                "base": str_prop("Branch to cut from in every repo. Each repo's default when omitted.")
            }),
            vec!["name", "repos"],
        ),
        tool(
            "open_prs",
            "Push every repository in a task that has changes and open a pull \
             request for each, then post the links back to the Jira ticket. Needs \
             confirm: true after the user agrees.",
            json!({
                "task_id": str_prop("Task id from list_tasks"),
                "title": str_prop("Pull request title"),
                "body": str_prop("Pull request description"),
                "draft": { "type": "boolean", "description": "Open as draft, default true" },
                "confirm": confirm_prop()
            }),
            vec!["task_id", "title"],
        ),
    ]
}

/// Agents think in repository names, not ids.
fn resolve_repos(
    state: &tauri::State<'_, AppState>,
    args: &Value,
) -> Result<Vec<String>> {
    let wanted: Vec<String> = args
        .get("repos")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    if wanted.is_empty() {
        return Err(crate::error::Error::Other("repos must not be empty".into()));
    }

    let projects = state.config.read().projects;
    wanted
        .iter()
        .map(|name| {
            projects
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| p.id.clone())
                .ok_or_else(|| {
                    crate::error::Error::NotFound(format!(
                        "no repository named {name}; known: {}",
                        projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                    ))
                })
        })
        .collect()
}

async fn call(app: &AppHandle, name: &str, args: Value) -> Result<Value> {
    let state = app.state::<AppState>();

    // Listing transitions and dry-run cleanup are reads in all but name — they
    // do not need the confirm gate. Applying them does.
    if tool_needs_confirm(name, &args) {
        require_confirm(&args, name)?;
    }

    match name {
        // The `*_inner` forms, handed to the blocking pool here. This handler
        // is off the main thread already, but it is on the async runtime, and
        // a git status of every worktree there holds up this server and the
        // Jira and GitHub clients that share its workers.
        "list_tasks" => Ok(serde_json::to_value(
            commands::blocking(app.clone(), |s| Ok(commands::list_tasks_inner(s, None))).await?,
        )?),

        "list_repos" => Ok(serde_json::to_value(commands::list_projects(state))?),

        "task_diff" => {
            let id = required(&args, "task_id")?.to_string();
            // An agent asking what a task changed means the branch, not what
            // happens to be uncommitted at this second.
            Ok(serde_json::to_value(
                commands::blocking(app.clone(), move |s| {
                    commands::diff_files_inner(s, id, Some(crate::git::Scope::Branch), None, None)
                })
                .await?,
            )?)
        }

        "jira_search" => {
            let jql = required(&args, "jql")?.to_string();
            // Clamped before the cast: a huge value wrapped to 0 and came back
            // empty, and one just under the wrap paged through the whole site.
            let max = args.get("max").and_then(|m| m.as_u64()).unwrap_or(25).clamp(1, 500) as u32;
            let (client, _) = commands::jira_client(&state)?;
            Ok(serde_json::to_value(client.search(&jql, max).await?.issues)?)
        }

        "jira_get_issue" => {
            let key = required(&args, "key")?.to_string();
            let (client, _) = commands::jira_client(&state)?;
            Ok(serde_json::to_value(client.issue(&key).await?)?)
        }

        "jira_issue_types" => Ok(serde_json::to_value(
            commands::jira_issue_types_inner(&state, false).await?,
        )?),

        "jira_create_fields" => {
            Ok(serde_json::to_value(
                commands::create_fields_for(
                    &state,
                    required(&args, "project_key")?,
                    required(&args, "issue_type_id")?,
                )
                .await?,
            )?)
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
                    // Not "Task": that is one site's name for it.
                    &match arg(&args, "issue_type") {
                        Some(t) => t.to_string(),
                        None => commands::default_issue_type(&state).await?,
                    },
                    arg(&args, "parent_key"),
                    // Without it jira_create_fields was advice nobody could
                    // act on: a project that demands a component refused
                    // every issue an agent filed.
                    // Some clients send an object argument as its JSON text.
                    &match args.get("fields") {
                        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
                        Some(v) => v.clone(),
                        None => Value::Null,
                    },
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
            let sent = commands::slack_notify(
                state,
                text,
                context,
                Some("agent_tool".to_string()),
            )
            .await?;
            if !sent {
                return Err(crate::error::Error::Other(
                    "Slack posting from agents is switched off in the app's settings; \
                     nothing was sent."
                        .into(),
                ));
            }
            Ok(json!({ "ok": true }))
        }

        "add_repo" => {
            let task_id = required(&args, "task_id")?.to_string();
            let wanted = required(&args, "repo")?;
            let project = state
                .config
                .read()
                .projects
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(wanted))
                .ok_or_else(|| {
                    crate::error::Error::NotFound(format!(
                        "no repository named {wanted}; see list_repos"
                    ))
                })?;

            let project_id = project.id.clone();
            let added = commands::blocking(app.clone(), move |s| {
                commands::add_repo(s, &task_id, &project_id)
            })
            .await?;
            Ok(json!({
                "repo": project.name,
                "path": added.checkout.path,
                "base": added.checkout.base,
            }))
        }

        "pane_output" => {
            let pane_id = required(&args, "pane_id")?.to_string();
            let lines = args
                .get("lines")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            Ok(Value::String(commands::pane_output(&state, &pane_id, lines)?))
        }

        "forget_repo" => {
            let wanted = required(&args, "repo")?;
            let projects = state.config.read().projects;
            // Names are not unique across clones, so a path wins when given.
            let found = projects
                .iter()
                .find(|p| p.path == wanted)
                .or_else(|| {
                    let mut byname = projects.iter().filter(|p| p.name.eq_ignore_ascii_case(wanted));
                    let first = byname.next();
                    if byname.next().is_some() { None } else { first }
                })
                .ok_or_else(|| {
                    crate::error::Error::NotFound(format!(
                        "no single repository matches {wanted}; give the exact path"
                    ))
                })?
                .clone();

            commands::remove_project(state, found.id.clone())?;
            Ok(json!({ "forgot": found.name, "path": found.path }))
        }

        "create_task" => {
            let ids = resolve_repos(&state, &args)?;
            let req = commands::NewTask {
                name: required(&args, "name")?.to_string(),
                project_ids: ids,
                branch: arg(&args, "branch").map(str::to_string),
                branch_suffix: None,
                base: arg(&args, "base").map(str::to_string),
                issue_key: None,
                issue_url: None,
                epic_key: None,
            };
            let task = commands::blocking(app.clone(), move |s| commands::new_task(s, req)).await?;
            Ok(serde_json::to_value(task)?)
        }

        "slack_diagnose" => Ok(commands::slack_diagnose(state).await?),

        "slack_delete" => {
            let links: Vec<String> = args
                .get("links")
                .and_then(|l| l.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            // An empty list means "everything recorded" to the command, so a
            // missing or malformed `links` — with confirm given for two
            // particular messages — deleted up to a hundred of them.
            if links.is_empty() {
                return Err(crate::error::Error::Other(
                    "slack_delete needs `links`: the permalinks of the messages to delete. \
                     Nothing was deleted."
                        .into(),
                ));
            }
            Ok(serde_json::to_value(
                commands::slack_delete_posted(state, Some(links)).await?,
            )?)
        }

        "slack_cleanup" => {
            let dry = args.get("dry_run").and_then(|d| d.as_bool()).unwrap_or(false);
            Ok(commands::slack_cleanup(state, dry).await?)
        }

        "start_work" => {
            let issue_key = required(&args, "issue_key")?.to_string();

            let ids = resolve_repos(&state, &args)?;

            let task = commands::jira_start_work(
                app.clone(),
                state,
                issue_key,
                ids,
                arg(&args, "agent").map(str::to_string),
                arg(&args, "branch_suffix").map(str::to_string),
                arg(&args, "base").map(str::to_string),
            )
            .await?;
            Ok(serde_json::to_value(task)?)
        }

        "list_panes" => Ok(serde_json::to_value(
            commands::list_panes(state, arg(&args, "task_id").map(str::to_string)),
        )?),

        "handoff_prompt" => {
            let pane_id = required(&args, "pane_id")?.to_string();
            // It carries the pane's last 120 lines, so it answers to the same
            // setting as `pane_output` — without this it was a way round it.
            if !state.config.read().ui.agents_read_panes {
                return Err(crate::error::Error::Other(
                    "a handoff briefing includes terminal output, and reading terminal output \
                     is switched off in the app's settings."
                        .into(),
                ));
            }
            Ok(Value::String(commands::handoff_prompt(state, pane_id).await?))
        }

        "task_prs" => {
            let id = required(&args, "task_id")?.to_string();
            Ok(serde_json::to_value(
                commands::github_task_prs(state, id).await?,
            )?)
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
///
/// Mode 0600: the file holds the bearer token for this run. Loopback is not
/// authorisation on a shared machine; neither is a world-readable config.
pub fn write_config(dir: &std::path::Path) -> Result<()> {
    let Some(config) = mcp_json() else {
        return Ok(());
    };
    let path = dir.join(".mcp.json");
    // Created 0600 rather than chmodded after: written with the default mode
    // first, the token was readable by anyone on the machine until the chmod.
    crate::agents::replace_file(&path, &serde_json::to_vec_pretty(&config)?, Some(0o600))?;
    // `mode` applies only when the file is created; one left from an older
    // run keeps whatever it had until this.
    lock_config_perms(&path)?;
    Ok(())
}

fn lock_config_perms(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn a_missing_or_wrong_bearer_is_refused() {
        let mut headers = HeaderMap::new();
        assert!(!bearer_ok(&headers, "secret"));

        headers.insert("authorization", HeaderValue::from_static("Bearer other"));
        assert!(!bearer_ok(&headers, "secret"));

        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        assert!(bearer_ok(&headers, "secret"));
    }

    #[test]
    fn tools_list_names_the_danger_tier() {
        let listed = tools();
        let names: Vec<&str> = listed
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for danger in DANGER {
            assert!(names.contains(danger), "{danger} missing from tools/list");
        }
    }

    #[test]
    fn danger_tools_advertise_confirm() {
        for tool in tools() {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            if !DANGER.contains(&name) {
                continue;
            }
            let props = &tool["inputSchema"]["properties"];
            assert!(
                props.get("confirm").is_some(),
                "{name} should advertise confirm in its schema"
            );
        }
    }

    #[test]
    fn high_impact_writes_need_confirm_reads_do_not() {
        assert!(tool_needs_confirm("open_prs", &json!({})));
        assert!(tool_needs_confirm("forget_repo", &json!({})));
        assert!(tool_needs_confirm("slack_delete", &json!({})));
        assert!(tool_needs_confirm(
            "jira_transition",
            &json!({ "transition": "Done" })
        ));
        assert!(!tool_needs_confirm("jira_transition", &json!({ "key": "ACME-1" })));
        assert!(!tool_needs_confirm("slack_cleanup", &json!({ "dry_run": true })));
        assert!(tool_needs_confirm("slack_cleanup", &json!({})));
        assert!(!tool_needs_confirm("list_tasks", &json!({})));
        assert!(!tool_needs_confirm("slack_post", &json!({})));
    }

    #[test]
    fn confirm_false_or_absent_changes_nothing() {
        assert!(require_confirm(&json!({}), "open_prs").is_err());
        assert!(require_confirm(&json!({ "confirm": false }), "open_prs").is_err());
        assert!(require_confirm(&json!({ "confirm": true }), "open_prs").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn mcp_json_on_disk_is_owner_readable_only() {
        let path = std::env::temp_dir().join(format!(
            "villain-mcp-perm-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"{}").unwrap();
        lock_config_perms(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }

    fn agent(ptys: &crate::pty::PtyManager, agent_id: &str, script: &str, title: bool) -> String {
        use crate::pty::{PaneKind, SpawnOptions};
        let app = tauri::test::mock_app();
        ptys.spawn(
            app.handle(),
            SpawnOptions {
                task_id: "t".into(),
                checkout_id: None,
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
                kind: PaneKind::Agent,
                title: agent_id.into(),
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                agent_id: Some(agent_id.into()),
                rows: None,
                cols: None,
                initial_input: None,
                prompted: true,
                env: Vec::new(),
                title_activity: title.then_some(crate::agents::gemini_title_activity as fn(&str) -> _),
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_hook_post_moves_its_pane_by_what_that_pane_runs() {
        use crate::pty::Activity;
        let ptys = crate::pty::PtyManager::default();
        let claude = agent(&ptys, "claude", "sleep 5", false);
        let opencode = agent(&ptys, "opencode", "sleep 5", false);
        let gemini = agent(&ptys, "gemini", "sleep 5", false);
        let activity = |id: &str| ptys.info(id).unwrap().activity;

        // Just started, it already reads as working from its own output: said
        // again by a hook, nothing changes and nothing needs redrawing.
        assert!(!take_hook(&ptys, &claude, &json!({"hook_event_name": "UserPromptSubmit"})));
        assert_eq!(activity(&claude), Activity::Working);
        assert!(take_hook(&ptys, &claude, &json!({"hook_event_name": "Notification", "notification_type": "permission_prompt"})));
        assert_eq!(activity(&claude), Activity::Asking);
        assert!(take_hook(&ptys, &claude, &json!({"hook_event_name": "Stop"})));
        assert_eq!(activity(&claude), Activity::Done);

        // The same post means nothing from a CLI that posts in another shape.
        assert!(!take_hook(&ptys, &opencode, &json!({"hook_event_name": "Stop"})));
        assert!(take_hook(&ptys, &opencode, &json!({"state": "asking"})));
        assert_eq!(activity(&opencode), Activity::Asking);
        // Nor from one that reports through its title, or a pane not there.
        assert!(!take_hook(&ptys, &gemini, &json!({"hook_event_name": "Stop"})));
        assert!(!take_hook(&ptys, "no-such-pane", &json!({"hook_event_name": "Stop"})));
        for id in [claude, opencode, gemini] {
            let _ = ptys.close(&id);
        }
    }

    #[test]
    fn a_title_the_agent_sets_is_read_as_it_is_printed() {
        use crate::pty::Activity;
        let ptys = crate::pty::PtyManager::default();
        let id = agent(
            &ptys,
            "gemini",
            r"printf '\033]0;\342\234\213  Action Required (x)\007'; sleep 5",
            true,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ptys.info(&id).unwrap().activity != Activity::Asking {
            assert!(std::time::Instant::now() < deadline, "the title was never read");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = ptys.close(&id);
    }
}
