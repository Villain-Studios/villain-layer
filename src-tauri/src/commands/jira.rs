//! Jira connect, browse, transitions, start work, prompts.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use crate::config::{JiraConfig, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::integrations::jira::{self, Jira};
use crate::secrets;
use crate::shellenv;

use super::AppState;
use super::panes::{agent_file_dir, resolve_scope, start_agent};
use super::tasks::{new_task, NewTask};

// -------------------------------------------------------------------- jira

/// The token as typed, or the one already in the keychain when the field was
/// left blank — which is what the settings form says a blank field means.
pub(crate) fn stored_or(key: &str, typed: &str, what: &'static str) -> Result<String> {
    let typed = typed.trim();
    if !typed.is_empty() {
        return Ok(typed.to_string());
    }
    secrets::get(key)?.ok_or(Error::NotConfigured(what))
}

pub(crate) fn jira_client(state: &AppState) -> Result<(Jira, JiraConfig)> {
    let cfg = state
        .config
        .read()
        .jira
        .ok_or(Error::NotConfigured("Jira"))?;
    let token = secrets::get(secrets::JIRA)?.ok_or(Error::NotConfigured("Jira"))?;
    Ok((Jira::new(&cfg, &token), cfg))
}

#[tauri::command]
pub async fn jira_connect(
    state: State<'_, AppState>,
    base_url: String,
    email: String,
    token: String,
    project_key: Option<String>,
    jql: Option<String>,
) -> Result<String> {
    let mut cfg = JiraConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        email,
        project_key,
        jql,
        epic_field: None,
    };
    // Changing the project key or JQL should not need the token typed again.
    let token = stored_or(secrets::JIRA, &token, "Jira")?;
    // Verify before persisting, so a typo never looks like a working setup.
    let client = Jira::new(&cfg, &token);
    let who = client.myself().await?;
    // Asked once, here, because the id differs on every site.
    cfg.epic_field = client.epic_link_field().await.ok().flatten();
    secrets::set(secrets::JIRA, &token)?;
    state.config.update(|c| c.jira = Some(cfg))?;
    Ok(who.display_name)
}

/// Every epic on a project, not just the ones already carrying work.
///
/// The New task dialog offered whatever epics happened to appear in the user's
/// own issue list, which is the epics that already have tickets on them — the
/// least useful set when the point of the dialog is to file the first one.
/// Asked of Jira directly instead, by the hierarchy level that means "epic"
/// everywhere rather than by a name that means it only here.
#[tauri::command]
pub async fn jira_epics(
    state: State<'_, AppState>,
    project_key: String,
) -> Result<Vec<jira::Issue>> {
    let types = jira_issue_types_inner(&state, false).await?;
    let names: Vec<String> = types
        .iter()
        .filter(|t| t.hierarchy_level >= 1)
        .map(|t| jira::jql_string(&t.name))
        .collect();
    if names.is_empty() || project_key.trim().is_empty() {
        return Ok(Vec::new());
    }

    let (client, _) = jira_client(&state)?;
    let jql = format!(
        "project = {} AND issuetype in ({}) AND statusCategory != Done ORDER BY key DESC",
        jira::jql_string(project_key.trim()),
        names.join(", "),
    );
    Ok(client.search(&jql, 200).await?.issues)
}

/// Every issue type this Jira defines, with its own icon. Nothing about types
/// is hardcoded — a site with custom types renders exactly as it does in Jira.
#[tauri::command]
pub async fn jira_issue_types(
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> Result<Vec<jira::IssueType>> {
    jira_issue_types_inner(&state, refresh == Some(true)).await
}

pub(crate) async fn jira_issue_types_inner(
    state: &AppState,
    refresh: bool,
) -> Result<Vec<jira::IssueType>> {
    if !refresh {
        if let Some(cached) = state.jira_types.lock().clone() {
            return Ok(cached);
        }
    }
    let (client, _) = jira_client(state)?;
    let types = client.issue_types().await?;
    *state.jira_types.lock() = Some(types.clone());
    Ok(types)
}

#[tauri::command]
pub async fn jira_issues(state: State<'_, AppState>) -> Result<jira::Page> {
    learn_epic_field(&state).await;
    let (client, cfg) = jira_client(&state)?;
    let jql = cfg
        .jql
        .clone()
        .unwrap_or_else(|| jira::default_jql(cfg.project_key.as_deref()));
    // Your own queue should be all of it: this is the list the app groups into
    // epics and reasons about, and a silent cut makes that grouping wrong.
    client.search(&jql, 500).await
}

/// Find this site's Epic Link field once, for a connection made before the app
/// knew to ask.
///
/// A site with no such field — the common case now, with the parent field
/// doing the job — is asked once per launch, not on every refresh. The answer
/// comes from the whole field list, which is hundreds of kilobytes, and was
/// being fetched ahead of every search. The miss stays out of the config so a
/// field added later is still found after a restart.
pub(crate) async fn learn_epic_field(state: &AppState) {
    let Ok((client, cfg)) = jira_client(state) else {
        return;
    };
    if cfg.epic_field.is_some()
        || state.epic_field_missing.lock().as_deref() == Some(cfg.base_url.as_str())
    {
        return;
    }
    match client.epic_link_field().await {
        Ok(Some(field)) => {
            let _ = state.config.update(|c| {
                if let Some(j) = c.jira.as_mut() {
                    j.epic_field = Some(field.clone());
                }
            });
        }
        Ok(None) => *state.epic_field_missing.lock() = Some(cfg.base_url.clone()),
        // Not an answer: ask again next time.
        Err(_) => {}
    }
}

/// Look past your own queue: unassigned work, or anyone else's.
///
/// The JQL is built here rather than in the UI so what is typed stays a search
/// term. Results are capped: this is for finding a ticket, not for paging
/// through a backlog.
#[tauri::command]
pub async fn jira_browse(
    state: State<'_, AppState>,
    text: Option<String>,
    whose: String,
    include_done: Option<bool>,
    types: Option<Vec<String>>,
) -> Result<jira::Page> {
    let (client, cfg) = jira_client(&state)?;
    let jql = jira::browse_jql(
        cfg.project_key.as_deref(),
        text.as_deref(),
        jira::Whose::parse(&whose),
        include_done.unwrap_or(false),
        &types.unwrap_or_default(),
    );
    // A search is refined rather than read end to end, so it stays capped —
    // but it now says when there was more.
    client.search(&jql, 100).await
}

#[tauri::command]
pub async fn jira_transitions(
    state: State<'_, AppState>,
    key: String,
) -> Result<Vec<jira::Transition>> {
    let (client, _) = jira_client(&state)?;
    client.transitions(&key).await
}

#[tauri::command]
pub async fn jira_transition(
    state: State<'_, AppState>,
    key: String,
    transition_id: String,
) -> Result<()> {
    let (client, _) = jira_client(&state)?;
    client.transition(&key, &transition_id).await
}

/// The opening prompt. Spells out the layout so the agent knows what it can see
/// from where it was started: the task root (sibling folders) or one repo.
pub(crate) fn ticket_prompt(
    issue: &jira::Issue,
    task: &Task,
    repos: &[(String, String)],
    at_task_root: bool,
) -> String {
    let description = if issue.description.trim().is_empty() {
        "(no description on the ticket)"
    } else {
        issue.description.trim()
    };

    let mut prompt = format!(
        "You are working on Jira issue {} ({}).\n\nTitle: {}\n\nDescription:\n{description}\n\n",
        issue.key, issue.url, issue.summary,
    );

    if !repos.is_empty() {
        if at_task_root {
            prompt.push_str(&format!(
                "You are in the task folder, not in a repository. {} checked out as {} inside it, on branch `{}`:\n",
                if repos.len() == 1 { "One repository is" } else { "Each repository is" },
                if repos.len() == 1 { "a folder" } else { "sibling folders" },
                task.branch,
            ));
            for (folder, origin) in repos {
                prompt.push_str(&format!("  {folder}/  — {origin}\n"));
            }
            prompt.push_str(
                "\nRun git and each repository's own tests from inside its folder. Do not \
                 init or install a package manager project in this task folder — it is only \
                 a container for the checkouts. More repositories can be added to the task \
                 later, with the add_repo tool if you find you need one, and they appear \
                 here as further folders.\n\n",
            );
        } else {
            let (folder, origin) = &repos[0];
            prompt.push_str(&format!(
                "You are inside the `{folder}` repository (checked out from {origin}), \
                 on branch `{}`. Stay in this repository unless asked otherwise.\n\n",
                task.branch,
            ));
        }
    }

    prompt.push_str(
        "Start by exploring the relevant code, then implement the change. Ask before making sweeping refactors.",
    );
    prompt
}

/// The repos in a task, as (folder, origin path) pairs for the prompt.
pub(crate) fn task_repos(state: &AppState, task: &Task) -> Vec<(String, String)> {
    state
        .config
        .checkouts_of(&task.id)
        .iter()
        .map(|c| {
            let folder = PathBuf::from(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let origin = state
                .config
                .project(&c.project_id)
                .map(|p| p.path)
                .unwrap_or_default();
            (folder, origin)
        })
        .collect()
}

/// The opening prompt for an agent started on an existing task.
///
/// Used to prefill the launch dialog, so the text on screen is the text that
/// gets sent. When the task came from a ticket this refetches it, so an agent
/// started later gets the same briefing as the first one rather than just the
/// task's title. `checkout_id` selects "start inside one repo" wording; omit
/// it for the task-root layout.
#[tauri::command]
pub async fn task_prompt(
    state: State<'_, AppState>,
    task_id: String,
    checkout_id: Option<String>,
) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let at_task_root = checkout_id.is_none();
    let repos = match checkout_id.as_deref() {
        Some(id) => {
            let checkout = state.config.checkout(id)?;
            if checkout.task_id != task.id {
                return Err(Error::NotFound(format!("checkout {id}")));
            }
            let folder = PathBuf::from(&checkout.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let origin = state
                .config
                .project(&checkout.project_id)
                .map(|p| p.path)
                .unwrap_or_default();
            vec![(folder, origin)]
        }
        None => task_repos(&state, &task),
    };

    if let Some(key) = task.issue_key.as_deref() {
        if let Ok((client, _)) = jira_client(&state) {
            if let Ok(issue) = client.issue(key).await {
                return Ok(ticket_prompt(&issue, &task, &repos, at_task_root));
            }
        }
    }

    // No ticket, or Jira unreachable: describe what we do know.
    let mut prompt = format!("Task: {}\n\n", task.name);
    if let Some(url) = &task.issue_url {
        prompt.push_str(&format!("Ticket: {url}\n\n"));
    }
    if at_task_root && repos.len() > 1 {
        prompt.push_str(&format!(
            "This task spans {} repositories, checked out as sibling folders in your \
             working directory, all on branch `{}`:\n",
            repos.len(),
            task.branch,
        ));
        for (folder, origin) in &repos {
            prompt.push_str(&format!("  {folder}/  — {origin}\n"));
        }
        prompt.push('\n');
    } else if !at_task_root {
        if let Some((folder, origin)) = repos.first() {
            prompt.push_str(&format!(
                "You are inside the `{folder}` repository (checked out from {origin}), \
                 on branch `{}`.\n\n",
                task.branch,
            ));
        }
    }
    prompt.push_str(
        "Start by exploring the relevant code, then implement the change. Ask before \
         making sweeping refactors.",
    );
    Ok(prompt)
}

/// Where an agent is asked to leave a drafted pull request description.
///
/// In the task folder, never a worktree: a generated file inside a checkout
/// would show up in the very diff the description is about.
pub(crate) fn pr_draft_path(state: &AppState, task: &Task) -> Option<PathBuf> {
    agent_file_dir(state, task).map(|d| d.join("PR_DESCRIPTION.md"))
}

/// Generated files that are noise at review time: machine-written, enormous,
/// and never what a reviewer is asked to look at. They stay in the summary of
/// changed files but are kept out of the diff body, so the model spends its
/// attention — and the request its time — on real code.
pub(crate) const GENERATED: &[&str] = &[
    ":!*package-lock.json",
    ":!*yarn.lock",
    ":!*pnpm-lock.yaml",
    ":!*bun.lockb",
    ":!*bun.lock",
    ":!*Cargo.lock",
    ":!*composer.lock",
    ":!*Gemfile.lock",
    ":!*poetry.lock",
    ":!*go.sum",
];

/// How much diff to send. Past this a description stops getting better and the
/// request only gets slower, so the tail is dropped and the model is told.
pub(crate) const DIFF_BUDGET: usize = 60_000;

/// One repository's place in a task, resolved from config before any git runs.
pub(crate) struct Reviewed {
    repo: String,
    dir: PathBuf,
    base: String,
    base_commit: Option<String>,
}

pub(crate) fn reviewed_repos(state: &AppState, task: &Task) -> Vec<Reviewed> {
    state
        .config
        .checkouts_of(&task.id)
        .into_iter()
        .map(|c| Reviewed {
            repo: state
                .config
                .project(&c.project_id)
                .map(|p| p.name)
                .unwrap_or_else(|_| "repo".into()),
            dir: PathBuf::from(&c.path),
            base: c.base,
            base_commit: c.base_commit,
        })
        .collect()
}

/// What the work looks like from outside: commit subjects, the file summary,
/// and as much of the reviewable diff as fits in the budget.
///
/// Takes resolved repositories rather than the config, so the git calls — a
/// handful of subprocesses, and a slow one on a large tree — can be made off
/// the async runtime instead of holding one of its workers.
pub(crate) fn review_context(repos: &[Reviewed]) -> String {
    let mut out = String::new();
    let mut diff = String::new();

    for checkout in repos {
        let dir = checkout.dir.clone();
        let repo = checkout.repo.clone();
        // The recorded branch point where there is one, so the description
        // covers what this branch did and not what its base has done since.
        let merge_base = git::baseline(&dir, &checkout.base, checkout.base_commit.as_deref());

        out.push_str(&format!("\n## {repo}\n\n"));
        if let Ok(log) = git::run(&dir, &["log", "--no-color", "--format=- %s", &format!("{merge_base}..HEAD")]) {
            if !log.trim().is_empty() {
                out.push_str("Commits:\n");
                out.push_str(log.trim());
                out.push('\n');
            }
        }
        if let Ok(stat) = git::run(&dir, &["diff", "--no-color", "--stat", &merge_base]) {
            if !stat.trim().is_empty() {
                out.push_str("\nFiles changed:\n```\n");
                out.push_str(stat.trim());
                out.push_str("\n```\n");
            }
        }

        let mut args = vec!["diff", "--no-color", &merge_base, "--", "."];
        args.extend_from_slice(GENERATED);
        let mut patch = git::run(&dir, &args).unwrap_or_default();
        if patch.trim().is_empty() {
            // A dependency bump is all generated files. Filtering them out
            // would leave nothing to describe, so in that case they are the
            // change and the whole diff goes through.
            patch = git::run(&dir, &["diff", "--no-color", &merge_base]).unwrap_or_default();
        }
        if !patch.trim().is_empty() {
            diff.push_str(&format!("\n### {repo}\n\n```diff\n{}\n```\n", patch.trim()));
        }
    }

    out.push_str("\n# Diff\n");
    if diff.len() > DIFF_BUDGET {
        // Back off to a character boundary first — slicing into the middle of
        // a multi-byte character panics, and a diff is full of them — then to
        // a line boundary, so the last hunk shown is readable.
        let mut end = DIFF_BUDGET;
        while end > 0 && !diff.is_char_boundary(end) {
            end -= 1;
        }
        let cut = diff[..end].rfind('\n').unwrap_or(end);
        out.push_str(&diff[..cut]);
        out.push_str(
            "\n\n(The diff was longer than fits here and is cut off. Describe what you\
             \ncan see, and say that you only reviewed part of it.)\n",
        );
    } else {
        out.push_str(&diff);
    }
    out
}

#[derive(Clone, Serialize)]
struct DraftChunk<'a> {
    task_id: &'a str,
    text: &'a str,
}

#[derive(Clone, Serialize)]
struct IssueDraftChunk<'a> {
    request_id: &'a str,
    text: &'a str,
}

/// A cheap one-shot `claude -p` run: no tools, no MCP, streamed text deltas.
///
/// Shared by PR drafting and issue/epic description polish — both are
/// summarising jobs that must not disturb a working agent session.
fn oneshot_haiku(
    program: impl AsRef<std::path::Path>,
    cwd: impl AsRef<std::path::Path>,
    prompt: &str,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let env = shellenv::user_env().clone();
    let mut child = Command::new(program.as_ref())
        .args([
            "-p",
            // Cheap and fast: this is a summarising job, not a reasoning one.
            "--model",
            "haiku",
            // Connecting to MCP servers is the single largest part of a cold
            // start, and this run needs none of them.
            "--strict-mcp-config",
            // Streamed, so the description appears as it is written rather
            // than all at once at the end.
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            // Everything it needs is in the prompt. Without this it may go
            // reading the repository and turn seconds into minutes.
            "--disallowed-tools",
            "Bash",
            "Read",
            "Edit",
            "Write",
            "Glob",
            "Grep",
            "WebFetch",
            "WebSearch",
        ])
        .current_dir(cwd.as_ref())
        .env_clear()
        .envs(&env)
        // Nothing here is worth thinking about first, and the thinking block
        // is dead time the reader spends watching a spinner.
        .env("MAX_THINKING_TOKENS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(format!("could not run claude: {e}")))?;

    // Both pipes are pumped on their own threads. The prompt can be larger than
    // a pipe buffer, so writing it inline would block before claude has said
    // anything — and each side would then be waiting on the other.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Other("claude took no input".into()))?;
    let prompt = prompt.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
    });

    let errors = child.stderr.take().map(|e| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(e), &mut buf);
            buf
        })
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Other("claude produced no output".into()))?;

    let mut streamed = String::new();
    let mut result = None;
    let mut reported = None;
    for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let delta = &v["event"]["delta"];
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    continue;
                }
                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                    streamed.push_str(text);
                    on_delta(text);
                }
            }
            Some("result") => {
                let text = v.get("result").and_then(Value::as_str).unwrap_or_default();
                if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                    reported = Some(text.to_string());
                } else {
                    result = Some(text.to_string());
                }
            }
            _ => {}
        }
    }

    let status = child
        .wait()
        .map_err(|e| Error::Other(format!("claude did not finish: {e}")))?;
    let stderr = errors.and_then(|h| h.join().ok()).unwrap_or_default();

    if let Some(why) = reported {
        return Err(Error::Other(format!("claude: {}", why.trim())));
    }
    if !status.success() {
        let why = stderr.trim();
        return Err(Error::Other(if why.is_empty() {
            "claude could not draft the description".into()
        } else {
            format!("claude: {why}")
        }));
    }
    // The final message is authoritative; the deltas are what was shown.
    Ok(result.unwrap_or(streamed).trim().to_string())
}

/// Draft the pull request description without disturbing the working agent.
///
/// The obvious implementation — type the request into the agent that did the
/// work — is slow and fragile: that session carries a large context, it may be
/// mid-turn or sitting on a permission prompt, and the answer has to travel
/// back through a file. This asks a fresh, cheap, one-shot model instead and
/// hands it the diff directly, so nothing has to be discovered and no tools
/// have to run. `request_pr_description` remains for the case where the agent's
/// own account of the work is worth the wait.
#[tauri::command]
pub async fn draft_pr_description(
    app: AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let program = shellenv::which("claude")
        .ok_or_else(|| Error::NotFound("claude is not on your PATH".into()))?;

    let repos = reviewed_repos(&state, &task);
    let branch = task.branch.clone();
    let issue_key = task.issue_key.clone();

    // The same directory an interactive agent for this task would get, from
    // the helper that decides it, rather than a second opinion that will not
    // follow when that one changes its mind.
    let cwd = PathBuf::from(resolve_scope(&state, &task, None)?.0);

    let prompt = format!(
        concat!(
            "Write the body of a pull request description for the work on branch ",
            "`{branch}`{key}.\n\n",
            "It is for a reviewer who has not seen this work and was not part of the ",
            "conversation that produced it. Say what changed and why, call out anything ",
            "risky or worth a closer look, and note what is not covered. Do not walk ",
            "through the diff file by file, and do not pad it — a few short sections is ",
            "right for most changes.\n\n",
            "Reply with the Markdown body and nothing else: no preamble, no closing ",
            "remark, no code fence around the whole thing.\n\n",
            "# The change\n{context}\n",
        ),
        branch = branch,
        key = issue_key
            .as_ref()
            .map(|k| format!(" for {k}"))
            .unwrap_or_default(),
        context = review_context(&repos),
    );

    let out = tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        oneshot_haiku(&program, &cwd, &prompt, |text| {
            let _ = app.emit("pr:draft", DraftChunk { task_id: &task_id, text });
        })
    })
    .await
    .map_err(|e| Error::Other(format!("drafting was interrupted: {e}")))??;

    if out.is_empty() {
        return Err(Error::Other("claude returned an empty description".into()));
    }
    Ok(out)
}

/// Polish (or draft) a Jira issue/epic description from the summary and any
/// draft the user already typed. Same one-shot path as PR drafting — no live
/// agent, no tools — so it works from create dialogs that have no task yet.
#[tauri::command]
pub async fn optimize_issue_description(
    app: AppHandle,
    request_id: String,
    summary: String,
    description: String,
    kind: String,
) -> Result<String> {
    let program = shellenv::which("claude")
        .ok_or_else(|| Error::NotFound("claude is not on your PATH".into()))?;

    let summary = summary.trim().to_string();
    let description = description.trim().to_string();
    if summary.is_empty() && description.is_empty() {
        return Err(Error::Other(
            "give a summary or a draft description to work from".into(),
        ));
    }

    // "epic" vs anything else — names of issue types differ per site; the UI
    // only needs the shape of the write-up to change.
    let what = if kind.eq_ignore_ascii_case("epic") {
        "Jira epic"
    } else {
        "Jira ticket"
    };

    let prompt = if description.is_empty() {
        format!(
            concat!(
                "Write the description body for a {what} with this summary:\n\n",
                "{summary}\n\n",
                "It will be read by engineers and by coding agents that pick the work up. ",
                "Cover scope, intended outcome, and how someone would know it is done. ",
                "Keep it short — a few short paragraphs or bullets, not a spec. ",
                "Reply with the description and nothing else: no preamble, no closing ",
                "remark, no code fence around the whole thing.\n",
            ),
            what = what,
            summary = summary,
        )
    } else {
        format!(
            concat!(
                "Improve this {what} description so it is clearer for engineers and ",
                "coding agents that will pick the work up. Keep the author's intent; ",
                "tighten wording, fill obvious gaps (scope, outcome, done-when), and ",
                "cut fluff. Do not invent requirements that are not implied.\n\n",
                "Summary: {summary}\n\n",
                "Current description:\n{description}\n\n",
                "Reply with the improved description and nothing else: no preamble, ",
                "no closing remark, no code fence around the whole thing.\n",
            ),
            what = what,
            summary = if summary.is_empty() { "(none)" } else { &summary },
            description = description,
        )
    };

    // Home, not `/`: a GUI app's cwd is the filesystem root, and we still want
    // claude to start somewhere that will not trigger macOS permission prompts.
    let cwd = std::env::home_dir().unwrap_or_else(std::env::temp_dir);

    let out = tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        oneshot_haiku(&program, &cwd, &prompt, |text| {
            let _ = app.emit(
                "issue:draft",
                IssueDraftChunk {
                    request_id: &request_id,
                    text,
                },
            );
        })
    })
    .await
    .map_err(|e| Error::Other(format!("optimising was interrupted: {e}")))??;

    if out.is_empty() {
        return Err(Error::Other("claude returned an empty description".into()));
    }
    Ok(out)
}

/// Ask a running agent to write the pull request description.
///
/// The agent has just done the work, so it knows things the diff does not:
/// what it tried, what it deliberately left out, where a reviewer should look
/// hardest. It writes to a file rather than the terminal so the app can pick
/// the text up cleanly.
#[tauri::command]
pub fn request_pr_description(
    state: State<AppState>,
    task_id: String,
    pane_id: String,
) -> Result<String> {
    let task = state.config.task(&task_id)?;
    let path = pr_draft_path(&state, &task).ok_or_else(|| {
        Error::Other(
            "this task has no folder outside its worktrees to write the draft into".into(),
        )
    })?;

    // A stale draft would look like an instant answer.
    let _ = std::fs::remove_file(&path);

    let repos: Vec<String> = state
        .config
        .checkouts_of(&task_id)
        .iter()
        .filter_map(|c| state.config.project(&c.project_id).ok().map(|p| p.name))
        .collect();

    let prompt = format!(
        "Write the pull request description for the work on branch `{}`{}.\n\n         Write it for a reviewer who has not seen any of this and was not in the          conversation. Cover: what changed and why, anything you decided against or          left unfinished, and where review effort is best spent. Mention what you          verified and what you did not. Do not pad it, and do not restate the diff          file by file.\n\n         Base it on the actual diff{}. Save it as Markdown to:\n{}\n\n         Write only that file, change nothing else, and tell me when it is saved.",
        task.branch,
        task.issue_key
            .as_ref()
            .map(|k| format!(" for {k}"))
            .unwrap_or_default(),
        if repos.len() > 1 {
            format!(" across {}", repos.join(", "))
        } else {
            String::new()
        },
        path.display(),
    );

    state.ptys.submit(&pane_id, &prompt)?;
    Ok(path.to_string_lossy().to_string())
}

/// The drafted description, if the agent has saved it yet. Reading it takes it.
#[tauri::command]
pub fn take_pr_description(state: State<AppState>, task_id: String) -> Result<Option<String>> {
    let task = state.config.task(&task_id)?;
    let Some(path) = pr_draft_path(&state, &task) else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let _ = std::fs::remove_file(&path);
            Ok(Some(text))
        }
        Err(_) => Ok(None),
    }
}

/// Everything a different agent needs to pick this work up.
///
/// There is no portable session format between the agent CLIs, so nothing can
/// truly "resume" across them. What travels is the state of the work: the
/// ticket, what has changed, and what the outgoing agent was last doing. The
/// last of those comes from the pane's own scrollback, which matters because
/// running out of budget is exactly when an agent cannot summarise itself.
#[tauri::command]
pub async fn handoff_prompt(state: State<'_, AppState>, pane_id: String) -> Result<String> {
    let pane = state.ptys.info(&pane_id)?;
    let task = state.config.task(&pane.task_id)?;

    let mut out =
        String::from("You are taking over work another agent started and could not finish.\n\n");

    // The original briefing, scoped the same way the outgoing agent was, so a
    // handoff from a pinned repo does not claim the whole task folder.
    out.push_str(
        &task_prompt(state.clone(), task.id.clone(), pane.checkout_id.clone()).await?,
    );
    out.push_str("\n\n---\n\n## What has happened so far\n\n");

    let mut any_change = false;
    for checkout in state.config.checkouts_of(&task.id) {
        let dir = PathBuf::from(&checkout.path);
        let repo = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "unknown".into());

        // From the branch point, like the file list under it: measured against
        // the base branch itself, every commit merged in from elsewhere since
        // would be listed as this branch's own work.
        let point = git::baseline(&dir, &checkout.base, checkout.base_commit.as_deref());
        let commits = git::run(
            &dir,
            &["log", "--oneline", "--no-decorate", &format!("{point}..HEAD")],
        )
        .unwrap_or_default();
        let files = git::changed_files(
            &dir,
            &checkout.base,
            checkout.base_commit.as_deref(),
            git::Scope::Branch,
        ).unwrap_or_default();

        if commits.trim().is_empty() && files.is_empty() {
            continue;
        }
        any_change = true;
        out.push_str(&format!("### {repo}\n"));
        if !commits.trim().is_empty() {
            out.push_str("\nCommits on this branch:\n");
            for line in commits.lines() {
                out.push_str(&format!("- {line}\n"));
            }
        }
        if !files.is_empty() {
            out.push_str("\nWorking tree against the base branch:\n");
            for f in &files {
                out.push_str(&format!("- {} (+{} -{})\n", f.path, f.additions, f.deletions));
            }
        }
        out.push('\n');
    }
    if !any_change {
        out.push_str("Nothing has been committed or changed yet.\n\n");
    }

    // The outgoing agent's own words, as far as they got.
    if let Ok(tail) = state.ptys.transcript(&pane_id, 120) {
        if !tail.trim().is_empty() {
            out.push_str(&format!(
                "## The previous agent's terminal, most recent last\n\n                 This is raw output from {}, not a summary, and may be truncated                  mid-thought.\n\n```\n{tail}\n```\n\n",
                pane.title,
            ));
        }
    }

    out.push_str(concat!(
        "## What to do\n\n",
        "Work out from the diff and the transcript above where the previous agent got ",
        "to, then carry on. Verify its work rather than trusting it — it may have left ",
        "something half-finished. Say what you think was already done before you start ",
        "changing anything.",
    ));
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct NewIssue {
    pub summary: String,
    #[serde(default)]
    pub description: String,
    pub issue_type: String,
    /// Falls back to the parent's project, then to the one in Settings.
    #[serde(default)]
    pub project_key: Option<String>,
    #[serde(default)]
    pub parent_key: Option<String>,
    /// Anything else this project requires, already shaped the way Jira wants
    /// it by the caller, which is the side that has the metadata.
    #[serde(default)]
    pub fields: Option<Value>,
}

/// The project a key belongs to: everything before the first dash.
pub(crate) fn project_of(key: &str) -> Option<String> {
    let (project, number) = key.split_once('-')?;
    (!project.is_empty() && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .then(|| project.to_uppercase())
}

/// Whatever this site calls an ordinary issue.
///
/// Not "Task": that is one site's name for it. Hierarchy level is Jira's own
/// and means the same everywhere — 0 is a standard issue — so the plain type
/// is found rather than assumed, preferring one actually called Task when
/// there is one.
pub(crate) async fn default_issue_type(state: &AppState) -> Result<String> {
    let types = jira_issue_types_inner(state, false).await?;
    let plain: Vec<&jira::IssueType> = types
        .iter()
        .filter(|t| t.hierarchy_level == 0 && !t.subtask)
        .collect();
    plain
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case("task"))
        .or_else(|| plain.first())
        .map(|t| t.name.clone())
        .ok_or_else(|| Error::Other("this Jira defines no ordinary issue type".into()))
}

/// What a project demands before it will accept a new issue of this type.
#[tauri::command]
pub async fn jira_create_fields(
    state: State<'_, AppState>,
    project_key: String,
    issue_type_id: String,
) -> Result<Vec<jira::CreateField>> {
    let (client, _) = jira_client(&state)?;
    client.create_fields(&project_key, &issue_type_id).await
}

/// File a ticket without starting work on it.
///
/// Separate from `jira_create_task` because filing under an epic and opening
/// worktrees are different intentions: adding a ticket to a plan is not saying
/// you will start it now.
#[tauri::command]
pub async fn jira_create_issue(state: State<'_, AppState>, req: NewIssue) -> Result<jira::Issue> {
    let summary = req.summary.trim().to_string();
    if summary.is_empty() {
        return Err(Error::Other("the ticket needs a summary".into()));
    }
    let (client, cfg) = jira_client(&state)?;

    // A sub-task has to be filed in its parent's project, and the parent's key
    // says which that is — so an epic found by searching needs nothing else.
    let project_key = req
        .project_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .or_else(|| req.parent_key.as_deref().and_then(project_of))
        .or_else(|| cfg.project_key.clone())
        .ok_or_else(|| {
            Error::Other("no Jira project to file into — set a project key in Settings".into())
        })?;

    let key = client
        .create_issue(
            &project_key,
            &summary,
            &req.description,
            &req.issue_type,
            req.parent_key.as_deref(),
            &req.fields.clone().unwrap_or(Value::Null),
        )
        .await?;
    client.issue(&key).await
}

#[derive(Debug, Deserialize)]
pub struct NewJiraTask {
    pub summary: String,
    #[serde(default)]
    pub description: String,
    pub issue_type: String,
    /// Falls back to the project configured in Settings.
    #[serde(default)]
    pub project_key: Option<String>,
    /// The epic, or any parent the issue type allows.
    #[serde(default)]
    pub parent_key: Option<String>,
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub branch_suffix: Option<String>,
    /// Branch every worktree is cut from; each repo's default when omitted.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub fields: Option<Value>,
}

/// File the ticket and open the worktrees in one step.
///
/// The ticket goes first because the key it comes back with is what names the
/// branch, the folder and the task. If the worktrees then fail, the ticket is
/// left standing — deleting someone's issue to tidy up after a git error would
/// be worse than the orphan — and the error says so, naming the key, so the
/// work can be picked up with "start work" once the repositories are sorted.
#[tauri::command]
pub async fn jira_create_task(state: State<'_, AppState>, req: NewJiraTask) -> Result<Started> {
    if req.project_ids.is_empty() {
        return Err(Error::Other("pick at least one repository".into()));
    }
    let summary = req.summary.trim().to_string();
    if summary.is_empty() {
        return Err(Error::Other("the ticket needs a summary".into()));
    }

    let (client, cfg) = jira_client(&state)?;
    let project_key = req
        .project_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_uppercase)
        .or_else(|| req.parent_key.as_deref().and_then(project_of))
        .or_else(|| cfg.project_key.clone())
        .ok_or_else(|| {
            Error::Other("no Jira project to file into — set a project key in Settings".into())
        })?;

    let key = client
        .create_issue(
            &project_key,
            &summary,
            &req.description,
            &req.issue_type,
            req.parent_key.as_deref(),
            &req.fields.clone().unwrap_or(Value::Null),
        )
        .await?;
    let url = format!("{}/browse/{key}", cfg.base_url.trim_end_matches('/'));

    let task = new_task(
        &state,
        NewTask {
            name: format!("{key} {summary}"),
            project_ids: req.project_ids,
            branch: None,
            branch_suffix: req.branch_suffix,
            base: req.base,
            issue_key: Some(key.clone()),
            issue_url: Some(url),
            epic_key: req.parent_key.clone(),
        },
    )
    .map_err(|e| {
        Error::Other(format!(
            "{key} was filed in Jira, but its worktrees were not created: {e}. The ticket \
             is still there — start work on it from Tickets once that is sorted."
        ))
    })?;

    let moved = sync_started(&state, &key).await;
    Ok(Started { task, moved })
}

/// Move a ticket into progress alongside the worktrees, if that is wanted.
///
/// Starting work in two places and telling Jira about neither is how a board
/// ends up disagreeing with the app: the ticket reads Open while a branch,
/// a worktree and an agent are all running against it. Best effort — a
/// workflow that will not allow the move, or an account that may not make it,
/// is not a reason to undo a task that was created successfully.
pub(crate) async fn sync_started(state: &AppState, key: &str) -> Option<String> {
    if !state.config.read().ui.sync_jira_status {
        return None;
    }
    let (client, _) = jira_client(state).ok()?;
    match client.start_progress(key).await {
        Ok(moved) => moved,
        Err(e) => {
            eprintln!("could not move {key} into progress: {e}");
            None
        }
    }
}

/// Move a ticket into progress on request, for one that fell out of step.
///
/// Tasks created before this app moved tickets — or while the setting was off,
/// or when the workflow refused — leave a board saying Open next to a branch
/// that is clearly being worked on. This is the one-click way back into step.
#[tauri::command]
pub async fn jira_sync_status(state: State<'_, AppState>, key: String) -> Result<Option<String>> {
    let (client, _) = jira_client(&state)?;
    client.start_progress(&key).await
}

/// A task, and the status its ticket was moved to on the way.
#[derive(Debug, Serialize)]
pub struct Started {
    #[serde(flatten)]
    pub task: Task,
    /// The status Jira was moved to, when it was moved.
    pub moved: Option<String>,
}

/// The one-click path: ticket -> worktree per repo -> agent primed with both
/// the ticket and the layout.
#[tauri::command]
pub async fn jira_start_work(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    project_ids: Vec<String>,
    agent_id: Option<String>,
    branch_suffix: Option<String>,
    base: Option<String>,
) -> Result<Started> {
    // A second task for the same ticket would try to check the same branch out
    // twice and fail deep inside git. Unless a suffix asks for a distinct
    // branch, point at what already exists.
    if branch_suffix.as_deref().unwrap_or_default().trim().is_empty() {
        if let Some(existing) = state
            .config
            .read()
            .tasks
            .iter()
            .find(|t| t.issue_key.as_deref() == Some(key.as_str()))
        {
            return Err(Error::Other(format!(
                "{key} already has a task on branch {}. Open that one, or pass a \
                 branch_suffix to work on the ticket a second time.",
                existing.branch
            )));
        }
    }

    let (client, _) = jira_client(&state)?;
    let issue = client.issue(&key).await?;

    let task = new_task(
        &state,
        NewTask {
            name: format!("{} {}", issue.key, issue.summary),
            project_ids,
            branch: None,
            branch_suffix,
            base,
            issue_key: Some(issue.key.clone()),
            issue_url: Some(issue.url.clone()),
            epic_key: issue.epic_key.clone(),
        },
    )?;

    if let Some(agent_id) = agent_id {
        let repos = task_repos(&state, &task);

        start_agent(
            &app,
            &state,
            task.id.clone(),
            agent_id,
            None,
            Some(ticket_prompt(&issue, &task, &repos, true)),
            false,
            None,
            None,
        )?;
    }

    let moved = sync_started(&state, &issue.key).await;
    Ok(Started { task, moved })
}

