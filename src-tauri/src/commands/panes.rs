//! PTY panes: spawn, resume, restore, chat context.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::agents;
use crate::config::Task;
use crate::error::{Error, Result};
use crate::pty::{PaneInfo, PaneKind, SpawnOptions};
use crate::shellenv;

use super::panes_restore::remember_pane;
use super::AppState;
use super::task_context::write_task_context;

// ------------------------------------------------------------------- panes

/// Off the command thread: deciding which CLIs are installed needs the login
/// shell's PATH, and the first caller to want it pays for a full interactive
/// `$SHELL -ilc` — around 300ms on a well-furnished zsh. A warm-up thread in
/// `run()` usually gets there first, but the boot refresh asks for this and
/// the settings at the same moment, and on the main thread whichever lost
/// that race held everything else up.
#[tauri::command]
pub async fn list_agents(app: AppHandle) -> Result<Vec<agents::AgentStatus>> {
    super::blocking(app, |_| Ok(agents::available())).await
}

#[tauri::command]
pub fn list_panes(state: State<AppState>, task_id: Option<String>) -> Vec<PaneInfo> {
    state.ptys.list(task_id.as_deref())
}

/// Where a pane should start, and what to call it.
///
/// An explicit checkout wins. Otherwise everything starts at the task root,
/// where each repository is a sibling folder — including a task that has only
/// one today. Starting inside the single repo read better at the time, but it
/// puts the agent in a directory that cannot grow: a repository added later
/// lands outside its working directory, where it needs permission to look and
/// no reason to think of looking. The root is the same place before and after,
/// which is what lets an agent be told "there is another repo now" and simply
/// carry on.
pub(crate) fn resolve_scope(
    state: &AppState,
    task: &Task,
    checkout_id: Option<&str>,
) -> Result<(String, String, Option<String>)> {
    if let Some(id) = checkout_id {
        let checkout = state.config.checkout(id)?;
        let name = state
            .config
            .project(&checkout.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "repo".into());
        return Ok((checkout.path, name, Some(id.to_string())));
    }

    let checkouts = state.config.checkouts_of(&task.id);
    let name = match checkouts.as_slice() {
        [] => return Err(Error::Other("this task has no repositories".into())),
        [only] => state
            .config
            .project(&only.project_id)
            .map(|p| p.name)
            .unwrap_or_else(|_| "repo".into()),
        many => format!("{} repos", many.len()),
    };
    Ok((task.root.clone(), name, None))
}

/// Conversations that could be picked up for this task, wherever they live.
///
/// Merged across the task root and every worktree, because the directory an
/// agent was started in has changed: a session saved under a worktree is still
/// a session, and refusing to offer it would quietly strand work.
pub(crate) fn resumable_for(state: &AppState, task: &Task, cwd: &str) -> Vec<agents::Resumable> {
    let mut found: Vec<agents::Resumable> = agents::resumable(cwd);
    for checkout in state.config.checkouts_of(&task.id) {
        if checkout.path == cwd {
            continue;
        }
        for r in agents::resumable(&checkout.path) {
            match found.iter_mut().find(|f| f.agent_id == r.agent_id) {
                Some(existing) => {
                    existing.sessions += r.sessions;
                    existing.last_active = existing.last_active.max(r.last_active);
                }
                None => found.push(r),
            }
        }
    }
    found
}

/// Where an agent's saved conversation for this task actually lives.
///
/// Sessions are keyed by working directory, and tasks started before agents
/// moved to the task root have theirs under the worktree. Looking in both
/// means existing work still resumes rather than starting over.
pub(crate) fn resume_dir(state: &AppState, task: &Task, agent_id: &str, cwd: &str) -> String {
    if agents::resumable(cwd).iter().any(|r| r.agent_id == agent_id) {
        return cwd.to_string();
    }
    state
        .config
        .checkouts_of(&task.id)
        .into_iter()
        .find(|c| agents::resumable(&c.path).iter().any(|r| r.agent_id == agent_id))
        .map(|c| c.path)
        .unwrap_or_else(|| cwd.to_string())
}

/// Off the command thread, like the other two spawns: the first spawn after
/// launch waits on the login shell's PATH, and every one writes the config.
#[tauri::command]
pub async fn spawn_shell(
    app: AppHandle,
    task_id: String,
    checkout_id: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let handle = app.clone();
    super::blocking(app, move |state| {
        open_shell(&handle, state, task_id, checkout_id, rows, cols)
    })
    .await
}

pub(crate) fn open_shell(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    checkout_id: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let (cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Shell,
            title: format!("shell · {scope}"),
            program: shellenv::login_shell(),
            args: vec!["-l".into()],
            agent_id: None,
            rows,
            cols,
            initial_input: None,
            prompted: false,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

/// Off the command thread: starting an agent writes its context files and
/// the MCP config, and pre-trusting the folder rewrites `~/.claude.json` —
/// which grows with every project Claude Code has seen. On the main thread
/// that was a stall on every Launch.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn spawn_agent(
    app: AppHandle,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: Option<bool>,
    rows: Option<u16>,
    cols: Option<u16>,
    acp: Option<bool>,
) -> Result<PaneInfo> {
    let handle = app.clone();
    super::blocking(app, move |state| {
        start_agent(
            &handle, state, task_id, agent_id, checkout_id, prompt,
            Resume::newest_if(resume.unwrap_or(false)), rows, cols, acp.unwrap_or(false),
        )
    })
    .await
}

/// Conversations that could be picked up again in a task's working directory.
///
/// Panes do not survive the app closing, but the agent CLIs keep their own
/// transcripts per directory — so the work can continue even though the
/// process cannot.
/// Off the command thread: one directory scan per agent CLI per repository,
/// and the pane menu asks for it while it is already open.
#[tauri::command]
pub async fn resumable_agents(
    app: AppHandle,
    task_id: String,
    checkout_id: Option<String>,
) -> Result<Vec<agents::Resumable>> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        let (cwd, _, _) = resolve_scope(state, &task, checkout_id.as_deref())?;
        Ok(if checkout_id.is_some() {
            agents::resumable(&cwd)
        } else {
            resumable_for(state, &task, &cwd)
        })
    })
    .await
}

/// Answer the trust dialog for a folder this app created, if that is wanted.
///
/// Scoped on purpose: only directories under the app's own worktree root are
/// ever pre-trusted, so opening an agent somewhere else still asks.
pub(crate) fn pretrust_own_dir(state: &AppState, agent_id: &str, cwd: &str) {
    if !state.config.read().ui.trust_agent_dirs {
        return;
    }
    let dir = Path::new(cwd);
    if !dir.starts_with(state.config.worktree_root()) {
        return;
    }
    agents::pretrust(agent_id, dir);
}

/// How an agent starts: on a new conversation, or picking one up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Resume {
    No,
    /// The newest conversation where it starts (`--continue`).
    Newest,
    /// A pane restarted in place (PANE-14): in the folder it ran in, on the
    /// conversation it last said it was in, or else that folder's newest.
    Restart { cwd: String, session: Option<String> },
}

impl Resume {
    /// The newest conversation, when there is one to pick up.
    pub(crate) fn newest_if(found: bool) -> Self {
        if found { Resume::Newest } else { Resume::No }
    }

    /// What the CLI is started with to pick the conversation up. None for a
    /// new one.
    fn args(&self, def: &agents::AgentDef) -> Result<Option<Vec<String>>> {
        let session = match self {
            Resume::No => return Ok(None),
            Resume::Newest => None,
            Resume::Restart { session, .. } => session.as_deref(),
        };
        if let (Some(id), Some(flag)) = (session, def.resume_by_id) {
            return Ok(Some(vec![flag.to_string(), id.to_string()]));
        }
        let flags = def.resume_args.ok_or_else(|| {
            Error::Other(format!("{} cannot resume a previous session", def.name))
        })?;
        Ok(Some(flags.iter().map(|f| f.to_string()).collect()))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_agent(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: Resume,
    rows: Option<u16>,
    cols: Option<u16>,
    acp: bool,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;

    let (mut cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;
    match &resume {
        // An ACP agent lists its own conversations for the folder (ACP-9).
        Resume::Newest if checkout_id.is_none() && !acp => cwd = resume_dir(state, &task, &agent_id, &cwd),
        // Where it ran, which `resume_dir` may once have picked over the root.
        Resume::Restart { cwd: ran, .. } => cwd = ran.clone(),
        _ => {}
    }
    let title = format!("{} · {scope}{}", def.name, if resume != Resume::No { " (resumed)" } else { "" });

    if acp {
        // The task's context file and `.mcp.json` all the same: the agent
        // reads the first, and a CLI run by hand there the second.
        let _ = write_task_context(state, &task);
        if let Some(dir) = agent_file_dir(state, &task) {
            let _ = crate::mcp::write_config(&dir);
        }
        let start = super::acp_panes::Start { task_id, checkout_id, cwd, title, prompt, resume };
        let pane = super::acp_panes::start(app, state, def, start)?;
        remember_pane(state, &pane);
        return Ok(pane);
    }
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    // Resuming means handing the conversation back to the CLI, so an opening
    // prompt would only talk over it.
    let (mut args, initial_input) = match resume.args(def)? {
        Some(args) => (args, None),
        None => agents::launch_args(def, prompt.as_deref()),
    };
    let initial_input = initial_input
        .map(|text| typeable(agent_file_dir(state, &task).as_deref(), FIRST_PROMPT, &text))
        .transpose()?;

    // The task folder keeps a `.mcp.json` of its own too, so running a CLI
    // there by hand gets the same tools the app's agents do.
    // Best effort: neither is a reason to refuse to start the agent.
    let _ = write_task_context(state, &task);
    let own = agent_file_dir(state, &task).filter(|dir| Path::new(&cwd) == dir.as_path());
    if let Some(dir) = agent_file_dir(state, &task) {
        let _ = crate::mcp::write_config(&dir);
    }

    pretrust_own_dir(state, &agent_id, &cwd);
    let env = plug_in(app, def, &mut args, own.as_deref());

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Agent,
            title,
            program,
            args,
            agent_id: Some(agent_id),
            rows,
            cols,
            initial_input,
            prompted: prompt.is_some() && resume == Resume::No,
            env,
            title_activity: agents::title_reader(def.integration),
            title_topic: agents::topic_reader(def.integration),
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

/// Plug an agent into the app for this launch: how it says what it is doing,
/// and the app's own MCP server — with the environment both need.
///
/// What the CLI loads goes in the app's own folder: it is the same for every
/// pane, and the one part that differs — which pane — comes from the
/// environment. The token travels that way too rather than on the command
/// line, where any process on the machine can read it. `own` is the folder
/// the agent starts in when that folder is the app's and not a repository.
/// Best effort: without any of this the pane still works, with its state
/// guessed from output and without the app's tools.
fn plug_in(
    app: &AppHandle,
    def: &agents::AgentDef,
    args: &mut Vec<String>,
    own: Option<&Path>,
) -> Vec<(String, String)> {
    let (Some(url), Some(endpoint), Ok(dir)) =
        (crate::mcp::hook_url(), crate::mcp::endpoint(), app.path().app_config_dir())
    else {
        return Vec::new();
    };
    // One `.mcp.json` of the app's own for every launch to point at: a task
    // from the one-repo layout has no folder for one, and a worktree must not.
    let config = dir.join(".mcp.json");
    let mcp = (std::fs::create_dir_all(&dir).is_ok() && crate::mcp::write_config(&dir).is_ok())
        .then_some(agents::Mcp { url: &endpoint.url, config_file: &config });
    let theirs = shellenv::user_env().get("OPENCODE_CONFIG_CONTENT").map(String::as_str);
    let Ok(launch) = agents::prepare_launch(def.integration, &dir, mcp.as_ref(), own, theirs) else {
        return Vec::new();
    };
    args.extend(launch.args);
    let mut env = launch.env;
    env.push(("VILLAIN_HOOK_URL".into(), url));
    env.push(("VILLAIN_HOOK_TOKEN".into(), endpoint.hook_token.clone()));
    // Only for the CLIs that fill their MCP header in from a variable. Claude
    // Code and Copilot read it from the 0600 file, and a token in the
    // environment is one every command the agent runs inherits.
    if mcp.is_some() && matches!(def.integration, agents::Integration::Opencode | agents::Integration::Gemini) {
        env.push(("VILLAIN_MCP_TOKEN".into(), endpoint.token.clone()));
    }
    env
}

/// Panes that belong to the standing chat rather than to any task.
pub const CHAT_TASK_ID: &str = "chat";

/// The chat agent's working directory: a scratch folder it can keep notes and
/// drafts in, deliberately outside any repository.
pub(crate) fn chat_dir(state: &AppState) -> Result<PathBuf> {
    let dir = state.config.worktree_root().join("_chat");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// One chat's own folder under the chat root.
///
/// The agent CLIs key their saved conversations by working directory, so two
/// chats sharing a folder would both pick up whichever ran last. The folder is
/// what gives a chat its identity across a restart.
pub(crate) fn chat_room(state: &AppState, id: &str) -> Result<PathBuf> {
    let dir = chat_dir(state)?.join(id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Everything the app itself writes into a task folder.
const GENERATED_FILES: &[&str] = &[
    "CLAUDE.md",
    "AGENTS.md",
    super::task_context::TICKET_FILE,
    super::spec::SPEC_FILE,
    ".mcp.json",
    "PR_DESCRIPTION.md",
    super::github::FEEDBACK_FILE,
    ".gemini/settings.json",
    "CONFLICTS.md",
    "REVIEW_COMMENTS.md",
    "PR_DRAFT_REQUEST.md",
    FIRST_PROMPT,
];

/// An opening prompt too long to type, for a CLI that takes it typed.
const FIRST_PROMPT: &str = "FIRST_PROMPT.md";

/// What to type into an agent to give it `text`: the text itself when it
/// fits (`pty::MAX_TYPED`), or else a pointer to `file` in `dir`, where the
/// whole of it is written first. The way PR feedback always went (PR-6).
pub(crate) fn typeable(dir: Option<&Path>, file: &str, text: &str) -> Result<String> {
    if text.len() <= crate::pty::MAX_TYPED {
        return Ok(text.to_string());
    }
    let dir = dir.ok_or_else(|| {
        Error::Other("this is too long to type into a terminal, and this task has no folder of its own to leave it in".into())
    })?;
    let path = dir.join(file);
    std::fs::write(&path, text)?;
    Ok(format!(
        "Villain Layer has left you a message too long to type. Read {} in full, then do what it says.",
        path.display()
    ))
}

/// Type `text` into a running agent in `task`, through `typeable`.
pub(crate) fn hand_over(state: &AppState, task: &Task, pane_id: &str, file: &str, text: &str) -> Result<()> {
    // A conversation takes it whole (ACP-4): only a terminal needs it short.
    if state.ptys.is_acp(pane_id) {
        return state.ptys.submit(pane_id, text);
    }
    let typed = typeable(agent_file_dir(state, task).as_deref(), file, text)?;
    state.ptys.submit(pane_id, &typed)
}

/// Left in a task folder by others, and going with it all the same: Claude
/// Code's record of what was allowed there (left, it was the one file
/// keeping a deleted task's folder on disk) and Finder's `.DS_Store`.
const LEFT_BY_OTHERS: &[&str] = &[".claude/settings.local.json", ".DS_Store"];

/// Whether a file in a task folder, by its path there, is one that goes
/// with the task.
pub(crate) fn is_generated(rel: &str) -> bool {
    GENERATED_FILES.contains(&rel) || LEFT_BY_OTHERS.contains(&rel)
}

/// Take those files out of a task folder: when the task is deleted
/// (DISK-2), and when Clean up finds a folder no task uses. Anything else
/// in there is the user's, and stays.
pub(crate) fn remove_generated(dir: &Path) {
    for name in GENERATED_FILES.iter().chain(LEFT_BY_OTHERS) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    // The folders those sit in, if that emptied them.
    for sub in [".gemini", ".claude"] {
        let _ = std::fs::remove_dir(dir.join(sub));
    }
}

/// Where generated agent files (`.mcp.json`, context) may safely be written.
///
/// Never inside a worktree: a generated file there is an untracked change that
/// shows up in review and can be committed by accident. A task root is only
/// safe when it is not itself a checkout, which it is for tasks migrated from
/// the one-repo-per-task layout.
pub(crate) fn agent_file_dir(state: &AppState, task: &Task) -> Option<PathBuf> {
    let root = PathBuf::from(&task.root);
    let is_a_checkout = state
        .config
        .checkouts_of(&task.id)
        .iter()
        .any(|c| Path::new(&c.path) == root);

    (!is_a_checkout && root.is_dir()).then_some(root)
}

/// What the app knows, written where a coding agent will read it.
///
/// Agents pick up `CLAUDE.md` (and `AGENTS.md`) from their working directory,
/// so the chat scratch folder is a safe place to leave this: it is not a
/// repository, so nothing here can pollute a diff or get committed by mistake.
pub(crate) fn write_chat_context(state: &AppState, dir: &Path) -> Result<()> {
    let cfg = state.config.read();
    let mut md = String::from(concat!(
        "# Villain Layer — session context\n\n",
        "Regenerated by Villain Layer every time a chat agent starts. You are in a ",
        "scratch directory, not a repository; use it freely for notes and drafts.\n\n",
    ));

    md.push_str("## Integrations the app holds credentials for\n\n");
    match &cfg.jira {
        Some(j) => md.push_str(&format!(
            "- **Jira** — {} (project {})\n",
            j.base_url,
            j.project_key.as_deref().unwrap_or("any"),
        )),
        None => md.push_str("- Jira — not connected\n"),
    }
    match &cfg.github {
        Some(g) => md.push_str(&format!("- **GitHub** — {}\n", g.api_url)),
        None => md.push_str("- GitHub — not connected\n"),
    }
    match &cfg.slack {
        Some(sl) => md.push_str(&format!("- **Slack** — {}\n", sl.channel)),
        None => md.push_str("- Slack — not connected\n"),
    }
    md.push_str(concat!(
        "\nYou can act on all of these through the `villain-layer` MCP server, which ",
        "this app hosts and which uses the credentials above — you need none of your ",
        "own. Its tools include `jira_search`, `jira_get_issue`, `jira_create_issue`, ",
        "`jira_comment`, `jira_transition`, `slack_post`, `list_tasks`, `list_repos`, ",
        "`task_diff`, `start_work` and `open_prs`.\n\n",
        "Most writes act immediately: filing a ticket or posting to Slack happens ",
        "for real. Check with the user before anything others will see. A few tools ",
        "(`open_prs`, `jira_transition`, `forget_repo`, `slack_delete`, `slack_cleanup`) ",
        "also need `confirm: true` on the call after they agree — without it the ",
        "tool refuses and changes nothing.\n\n",
    ));

    if !cfg.projects.is_empty() {
        md.push_str("## Repositories\n\n| repo | group | path |\n|---|---|---|\n");
        for p in &cfg.projects {
            md.push_str(&format!(
                "| {} | {} | `{}` |\n",
                p.name,
                p.group.as_deref().unwrap_or("—"),
                p.path,
            ));
        }
        md.push('\n');
    }

    if cfg.tasks.is_empty() {
        md.push_str("## Tasks in flight\n\nNone right now.\n");
    } else {
        md.push_str("## Tasks in flight\n\n");
        for task in &cfg.tasks {
            md.push_str(&format!("### {} — `{}`\n", task.name, task.branch));
            if let Some(url) = &task.issue_url {
                md.push_str(&format!("{url}\n"));
            }
            for c in cfg.checkouts.iter().filter(|c| c.task_id == task.id) {
                let repo = cfg
                    .projects
                    .iter()
                    .find(|p| p.id == c.project_id)
                    .map(|p| p.name.as_str())
                    .unwrap_or("unknown");
                md.push_str(&format!("- {repo}: `{}`\n", c.path));
            }
            md.push('\n');
        }
    }

    std::fs::write(dir.join("CLAUDE.md"), &md)?;
    // Agents that look for AGENTS.md instead should see the same thing.
    std::fs::write(dir.join("AGENTS.md"), &md)?;
    Ok(())
}

/// A standing agent with no worktree, for questions, ticket drafting and
/// whatever MCP servers the user has configured for their own CLI.
#[tauri::command]
pub async fn spawn_chat(
    app: AppHandle,
    agent_id: String,
    prompt: Option<String>,
    acp: Option<bool>,
) -> Result<PaneInfo> {
    let handle = app.clone();
    super::blocking(app, move |state| open_chat(&handle, state, agent_id, prompt, None, Resume::No, acp.unwrap_or(false)))
        .await
}

pub(crate) fn open_chat(
    app: &AppHandle,
    state: &AppState,
    agent_id: String,
    prompt: Option<String>,
    // An existing chat folder to reopen, or None to start a new one.
    room: Option<PathBuf>,
    resume: Resume,
    acp: bool,
) -> Result<PaneInfo> {
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;

    let dir = match room {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            dir
        }
        None => chat_room(state, &uuid::Uuid::new_v4().to_string()[..8])?,
    };
    // Best effort: a context file that cannot be written is not a reason to
    // refuse to start the agent.
    let _ = write_chat_context(state, &dir);
    let _ = crate::mcp::write_config(&dir);
    let title = format!("{}{}", def.name, if resume != Resume::No { " (resumed)" } else { "" });

    if acp {
        let cwd = dir.to_string_lossy().to_string();
        let start = super::acp_panes::Start { task_id: CHAT_TASK_ID.to_string(), checkout_id: None, cwd, title, prompt, resume };
        let pane = super::acp_panes::start(app, state, def, start)?;
        remember_pane(state, &pane);
        return Ok(pane);
    }
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    // Resuming hands the conversation back to the CLI, so an opening prompt
    // would only talk over it.
    let (mut args, initial_input) = match resume.args(def)? {
        Some(args) => (args, None),
        None => agents::launch_args(def, prompt.as_deref()),
    };
    let initial_input = initial_input.map(|text| typeable(Some(&dir), FIRST_PROMPT, &text)).transpose()?;

    pretrust_own_dir(state, &agent_id, &dir.to_string_lossy());
    // The chat's folder is the app's own, so it can hold what a CLI needs.
    let env = plug_in(app, def, &mut args, Some(&dir));

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id: CHAT_TASK_ID.to_string(),
            checkout_id: None,
            cwd: dir.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title,
            program,
            args,
            agent_id: Some(agent_id),
            rows: None,
            cols: None,
            prompted: prompt.is_some() && resume == Resume::No,
            initial_input,
            env,
            title_activity: agents::title_reader(def.integration),
            title_topic: agents::topic_reader(def.integration),
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

/// The count of agents that need you, the dots and the dock follow a key
/// that answers a question at once, not at the next poll.
#[tauri::command]
pub fn pty_write(app: AppHandle, state: State<AppState>, pane_id: String, data: String) -> Result<()> {
    if state.ptys.write(&pane_id, &data)? {
        let _ = app.emit("pty:activity", &pane_id);
    }
    Ok(())
}

#[tauri::command]
pub fn pty_resize(state: State<AppState>, pane_id: String, rows: u16, cols: u16) -> Result<()> {
    state.ptys.resize(&pane_id, rows, cols)
}

/// A terminal has come on screen: what it missed since `since`, and a live
/// feed from then on. `since` is null for a terminal that has drawn nothing.
#[tauri::command]
pub fn pty_attach(
    state: State<AppState>,
    pane_id: String,
    since: Option<u64>,
    app: AppHandle,
) -> Result<crate::pty::Catchup> {
    let catchup = state.ptys.attach(&pane_id, since)?;
    if catchup.seen {
        let _ = app.emit("pty:activity", &pane_id);
    }
    Ok(catchup)
}

/// A terminal has gone off screen, or the window into the background. The
/// agent keeps running; its output waits in scrollback until it is shown.
#[tauri::command]
pub fn pty_detach(state: State<AppState>, pane_id: String) {
    state.ptys.detach(&pane_id);
}

/// Off the command thread: stopping a pane signals the agent and then waits
/// up to five seconds for it to save and exit. `PtyManager::close` already
/// takes care not to hold the pane map for that long, but the wait itself was
/// still on the thread the window runs on.
#[tauri::command]
pub async fn close_pane(app: AppHandle, pane_id: String) -> Result<()> {
    super::blocking(app, move |state| {
        // Closing a pane on purpose means not wanting it back.
        let _ = state.config.update(|c| c.saved_panes.retain(|p| p.id != pane_id));
        state.ptys.close(&pane_id)
    })
    .await
}

/// Stop the process but keep the pane and its scrollback on screen.
///
/// Off the command thread for the same five-second grace period as
/// `close_pane`.
#[tauri::command]
pub async fn kill_pane(app: AppHandle, pane_id: String) -> Result<()> {
    super::blocking(app, move |state| state.ptys.kill(&pane_id)).await
}

/// Stop an agent and start it again where it was, on the same conversation
/// (PANE-14): how a CLI that updated itself gets to run its new version, and
/// how one that exited is picked back up. Off the command thread: stopping
/// waits up to five seconds for the agent to save.
#[tauri::command]
pub async fn restart_pane(app: AppHandle, pane_id: String) -> Result<PaneInfo> {
    let handle = app.clone();
    super::blocking(app, move |state| restart(&handle, state, &pane_id)).await
}

fn restart(app: &AppHandle, state: &AppState, id: &str) -> Result<PaneInfo> {
    let old = state.ptys.info(id)?;
    // Over ACP the conversation is picked up by its id, whatever the CLI
    // (ACP-9).
    let def = old
        .agent_id
        .as_deref()
        .and_then(agents::find)
        .filter(|d| if old.acp { d.acp.is_some() } else { d.resume_args.is_some() })
        .ok_or_else(|| Error::Other(format!("{} cannot pick its conversation back up, so a restart would lose it.", old.title)))?;
    // Checked before it is stopped: stopped, then not started, is the one
    // way this loses an agent.
    let program = match (old.acp, def.acp) {
        (true, Some(cmd)) => cmd.program,
        _ => def.program,
    };
    shellenv::which(program).ok_or_else(|| Error::NotFound(format!("{program} is not on your PATH")))?;
    let session = state.ptys.session(id)?;
    // Without its id, `--continue` takes the folder's newest conversation,
    // and another agent there may be the one writing it.
    let shared = state.ptys.list(None).into_iter().any(|p| p.id != id && p.running && p.agent_id == old.agent_id && p.cwd == old.cwd);
    if session.is_none() && shared && !old.acp {
        return Err(Error::Other(format!(
            "Another {} is open in this folder, and this one never said which conversation it is in, so a restart could pick up the other's.",
            def.name,
        )));
    }

    // Stopped first, so its transcript is saved before the CLI reads it back.
    state.ptys.close(id)?;
    let resume = Resume::Restart { cwd: old.cwd.clone(), session };
    let agent_id = def.id.to_string();
    let started = if old.task_id == CHAT_TASK_ID {
        open_chat(app, state, agent_id, None, Some(PathBuf::from(&old.cwd)), resume, old.acp)
    } else {
        start_agent(app, state, old.task_id.clone(), agent_id, old.checkout_id.clone(), None, resume, None, None, old.acp)
    };
    let _ = state.config.update(|c| match &started {
        Ok(_) => c.saved_panes.retain(|p| p.id != id),
        // Not lost: it waits to be reopened, as one a launch did not put
        // back does (PANE-7).
        Err(_) => c.saved_panes.iter_mut().filter(|p| p.id == id).for_each(|p| p.waiting = true),
    });
    if started.is_err() {
        let _ = app.emit("panes:waiting", ());
    }
    let pane = started?;
    state.ptys.keep_place(&pane.id, old.started_at)?;
    state.ptys.info(&pane.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_hand_off_is_left_in_a_file_and_only_a_pointer_is_typed() {
        let dir = std::env::temp_dir().join(format!("vl-handover-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(typeable(Some(&dir), "CONFLICTS.md", "short").unwrap(), "short");

        let long = "Resolve each conflict. ".repeat(60);
        let typed = typeable(Some(&dir), "CONFLICTS.md", &long).unwrap();
        assert!(typed.len() <= crate::pty::MAX_TYPED);
        assert!(typed.contains(&dir.join("CONFLICTS.md").display().to_string()));
        assert_eq!(std::fs::read_to_string(dir.join("CONFLICTS.md")).unwrap(), long, "all of it, from the start");
        assert!(typeable(None, "CONFLICTS.md", &long).is_err(), "nowhere to leave it: refused, not cut");
        assert!(is_generated("CONFLICTS.md"), "deleting the task takes it too (DISK-2)");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_restarted_claude_resumes_the_conversation_it_named_not_the_folders_newest() {
        let claude = agents::find("claude").unwrap();
        let id = "024e8fc6-e2f5-44cd-a238-12dc258b29c8";
        let restart = |session: Option<&str>| Resume::Restart { cwd: "/t".into(), session: session.map(String::from) };
        assert_eq!(restart(Some(id)).args(claude).unwrap(), Some(vec!["--resume".into(), id.into()]));
        // One that never said, from before the app asked: the folder's newest.
        assert_eq!(restart(None).args(claude).unwrap(), Some(vec!["--continue".into()]));
        assert_eq!(Resume::Newest.args(claude).unwrap(), Some(vec!["--continue".into()]));
        assert_eq!(Resume::No.args(claude).unwrap(), None);
        // A CLI that cannot resume is refused, not started on a new conversation.
        assert!(restart(Some(id)).args(agents::find("gemini").unwrap()).is_err());
    }
}
