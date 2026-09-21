//! PTY panes: spawn, resume, restore, chat context.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, State};

use crate::agents;
use crate::config::{SavedPane, Task};
use crate::error::{Error, Result};
use crate::pty::{PaneInfo, PaneKind, SpawnOptions};
use crate::shellenv;

use super::AppState;

// ------------------------------------------------------------------- panes

#[tauri::command]
pub fn list_agents() -> Vec<agents::AgentStatus> {
    agents::available()
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

#[tauri::command]
pub fn spawn_shell(
    app: AppHandle,
    state: State<AppState>,
    task_id: String,
    checkout_id: Option<String>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    open_shell(&app, &state, task_id, checkout_id, rows, cols)
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
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn spawn_agent(
    app: AppHandle,
    state: State<AppState>,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: Option<bool>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    start_agent(
        &app, &state, task_id, agent_id, checkout_id, prompt,
        resume.unwrap_or(false), rows, cols,
    )
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_agent(
    app: &AppHandle,
    state: &AppState,
    task_id: String,
    agent_id: String,
    checkout_id: Option<String>,
    prompt: Option<String>,
    resume: bool,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<PaneInfo> {
    let task = state.config.task(&task_id)?;
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

    let (mut cwd, scope, checkout_id) = resolve_scope(state, &task, checkout_id.as_deref())?;
    if resume && checkout_id.is_none() {
        cwd = resume_dir(state, &task, &agent_id, &cwd);
    }

    // Resuming means handing the conversation back to the CLI, so an opening
    // prompt would only talk over it.
    let (mut args, initial_input) = if resume {
        let flags = def.resume_args.ok_or_else(|| {
            Error::Other(format!("{} cannot resume a previous session", def.name))
        })?;
        (flags.iter().map(|f| f.to_string()).collect::<Vec<_>>(), None)
    } else {
        agents::launch_args(def, prompt.as_deref())
    };

    // Give the agent the app's own MCP tools. When its cwd is the task root it
    // picks .mcp.json up by itself; when the cwd is a worktree the file has to
    // live elsewhere and be pointed at explicitly.
    // Best effort: neither is a reason to refuse to start the agent.
    let _ = write_task_context(state, &task);
    if let Some(dir) = agent_file_dir(state, &task) {
        let _ = crate::mcp::write_config(&dir);
        if Path::new(&cwd) != dir {
            if let Some(flag) = def.mcp_config_flag {
                args.push(flag.to_string());
                args.push(dir.join(".mcp.json").to_string_lossy().to_string());
            }
        }
    }

    pretrust_own_dir(state, &agent_id, &cwd);

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id,
            checkout_id,
            cwd,
            kind: PaneKind::Agent,
            title: format!("{} · {scope}{}", def.name, if resume { " (resumed)" } else { "" }),
            program,
            args,
            agent_id: Some(agent_id),
            rows,
            cols,
            initial_input,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
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
pub(crate) const GENERATED_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md", ".mcp.json", "PR_DESCRIPTION.md"];

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

/// The layout of a task, written into its folder for the agent standing in it.
///
/// The opening prompt says all of this too, but a prompt is said once: it
/// scrolls away, and a resumed conversation never hears it at all. A file in
/// the working directory is read every time, which is what an agent needs when
/// the task gains a repository weeks after it started.
///
/// Never written into a worktree — a generated file there is an untracked
/// change that turns up in review.
pub(crate) fn write_task_context(state: &AppState, task: &Task) -> Result<()> {
    let Some(dir) = agent_file_dir(state, task) else {
        return Ok(());
    };
    let checkouts = state.config.checkouts_of(&task.id);

    let mut md = format!(
        "# {}\n\nWritten by Villain Layer. You are in the task folder, not inside a \
         repository: each repository below is checked out as a folder here, all on \
         branch `{}`.\n\n",
        task.name, task.branch,
    );
    if let Some(url) = &task.issue_url {
        md.push_str(&format!("Ticket: {url}\n\n"));
    }

    md.push_str("## Repositories in this task\n\n");
    if checkouts.is_empty() {
        md.push_str("None yet.\n\n");
    } else {
        md.push_str("| folder | clone it came from |\n|---|---|\n");
        for c in &checkouts {
            let origin = state
                .config
                .project(&c.project_id)
                .map(|p| p.path)
                .unwrap_or_default();
            let folder = PathBuf::from(&c.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            md.push_str(&format!("| `{folder}/` | `{origin}` |\n"));
        }
        md.push('\n');
    }

    md.push_str(concat!(
        "Run git, and each repository's own tests, from inside its folder. A ",
        "repository's own CLAUDE.md or AGENTS.md lives in that folder and applies ",
        "there. Do not `npm`/`pnpm`/`yarn` init, install, or drop a lockfile in ",
        "this task folder — it is not a package, only a container for the checkouts.\n\n",
        "## If the work needs a repository that is not here\n\n",
        "Call `add_repo` on the `villain-layer` MCP server with this task's id and the ",
        "repository's name, and it is checked out here on the same branch. Do that ",
        "rather than reading or editing the original clone: that one is on its own ",
        "branch and is not yours to change. `list_repos` shows what is available and ",
        "`list_tasks` gives the task id.\n",
    ));

    std::fs::write(dir.join("CLAUDE.md"), &md)?;
    // Agents that look for AGENTS.md instead should see the same thing.
    std::fs::write(dir.join("AGENTS.md"), &md)?;
    Ok(())
}

/// A standing agent with no worktree, for questions, ticket drafting and
/// whatever MCP servers the user has configured for their own CLI.
#[tauri::command]
pub fn spawn_chat(
    app: AppHandle,
    state: State<AppState>,
    agent_id: String,
    prompt: Option<String>,
) -> Result<PaneInfo> {
    open_chat(&app, &state, agent_id, prompt, None, false)
}

pub(crate) fn open_chat(
    app: &AppHandle,
    state: &AppState,
    agent_id: String,
    prompt: Option<String>,
    // An existing chat folder to reopen, or None to start a new one.
    room: Option<PathBuf>,
    resume: bool,
) -> Result<PaneInfo> {
    let def = agents::find(&agent_id)
        .ok_or_else(|| Error::NotFound(format!("agent {agent_id}")))?;
    let program = shellenv::which(def.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", def.program)))?;

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

    // Resuming hands the conversation back to the CLI, so an opening prompt
    // would only talk over it.
    let (args, initial_input) = if resume {
        let flags = def.resume_args.ok_or_else(|| {
            Error::Other(format!("{} cannot resume a previous session", def.name))
        })?;
        (flags.iter().map(|f| f.to_string()).collect::<Vec<_>>(), None)
    } else {
        agents::launch_args(def, prompt.as_deref())
    };

    pretrust_own_dir(state, &agent_id, &dir.to_string_lossy());

    let pane = state.ptys.spawn(
        app,
        SpawnOptions {
            task_id: CHAT_TASK_ID.to_string(),
            checkout_id: None,
            cwd: dir.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: format!("{}{}", def.name, if resume { " (resumed)" } else { "" }),
            program,
            args,
            agent_id: Some(agent_id),
            rows: None,
            cols: None,
            initial_input,
        },
    )?;
    remember_pane(state, &pane);
    Ok(pane)
}

#[tauri::command]
pub fn pty_write(state: State<AppState>, pane_id: String, data: String) -> Result<()> {
    state.ptys.write(&pane_id, &data)
}

#[tauri::command]
pub fn pty_resize(state: State<AppState>, pane_id: String, rows: u16, cols: u16) -> Result<()> {
    state.ptys.resize(&pane_id, rows, cols)
}

#[tauri::command]
pub fn pty_scrollback(state: State<AppState>, pane_id: String) -> Result<String> {
    state.ptys.scrollback(&pane_id)
}

/// Remember a pane so it can be put back next launch.
///
/// Called where the pane is actually created rather than by each caller: an
/// agent started from a ticket went through `start_agent` directly and so was
/// never recorded, and vanished for good at the next restart.
pub(crate) fn remember_pane(state: &AppState, pane: &PaneInfo) {
    let saved = SavedPane {
        id: pane.id.clone(),
        task_id: pane.task_id.clone(),
        checkout_id: pane.checkout_id.clone(),
        kind: match pane.kind {
            PaneKind::Agent => "agent".into(),
            PaneKind::Shell => "shell".into(),
        },
        agent_id: pane.agent_id.clone(),
        cwd: Some(pane.cwd.clone()),
    };
    let _ = state.config.update(|c| {
        // Replace rather than append: recording the same pane twice is how a
        // restore that also recorded what it restored doubled this list on
        // every launch.
        c.saved_panes.retain(|p| p.id != saved.id);
        c.saved_panes.push(saved);
        // Panes that exited are never removed from here — only closing one on
        // purpose does that — so without a bound this grows for as long as the
        // app is used. Oldest first, since the newest are what you had open.
        let over = c.saved_panes.len().saturating_sub(SAVED_PANE_LIMIT);
        if over > 0 {
            c.saved_panes.drain(..over);
        }
    });
}

/// How many panes are remembered between launches.
///
/// Larger than what a restore will actually open, so the record survives a
/// session with a lot of churn, and still bounded: this file is written every
/// time a pane starts.
pub(crate) const SAVED_PANE_LIMIT: usize = 40;

/// The most panes a restore will ever open.
///
/// A ceiling, not a preference. Restoring is the one path that turns a number
/// in a config file into that many processes, so it must not be able to take
/// the machine down however the file got that way.
pub(crate) const RESTORE_LIMIT: usize = 12;

// Raising the restore limit past what the manager will open would make a
// restore fail part-way through, which is the confusing version of the bug
// rather than the dangerous one. Checked at compile time, so it cannot drift.
const _: () = assert!(RESTORE_LIMIT < crate::pty::MAX_PANES);
const _: () = assert!(RESTORE_LIMIT <= SAVED_PANE_LIMIT);

/// What to put back: each remembered pane once, and never more than the machine
/// should be asked to start at once.
///
/// Identity is the pane's id, not what it looks like. Three shells in one
/// folder are three shells someone wanted; collapsing them because they share a
/// directory throws away work rather than protecting anything. The doubling
/// this guards against wrote the *same* pane twice, which the id catches.
pub(crate) fn panes_to_restore(saved: Vec<SavedPane>, limit: usize) -> Vec<SavedPane> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for pane in saved {
        if !seen.insert(pane.id.clone()) {
            continue;
        }
        out.push(pane);
        if out.len() == limit {
            break;
        }
    }
    out
}

#[tauri::command]
pub fn close_pane(state: State<AppState>, pane_id: String) -> Result<()> {
    // Closing a pane on purpose means not wanting it back.
    let _ = state.config.update(|c| c.saved_panes.retain(|p| p.id != pane_id));
    state.ptys.close(&pane_id)
}

/// Whether the window is in front and worth feeding terminal output.
///
/// Agents keep running either way. This only gates shipping their redraws to
/// the webview — leaving that on while the app sits in the background is how
/// a long idle ends in a hitch when you come back.
#[tauri::command]
pub fn set_ui_awake(state: State<AppState>, awake: bool) {
    state.ptys.set_ui_awake(awake);
}

/// Put back what was open when the app last closed.
///
/// Agents are resumed rather than restarted where their CLI can do it, so the
/// conversation continues instead of beginning again. Anything whose worktree
/// or repository has since gone is dropped rather than failing the restore.
pub fn restore_panes(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.config.read().ui.restore_panes {
        return;
    }

    let saved = state.config.read().saved_panes;
    let wanted = saved.len();
    let saved = panes_to_restore(saved, RESTORE_LIMIT);
    if saved.is_empty() {
        return;
    }
    if wanted > saved.len() {
        let text = format!(
            "Restored {} of {wanted} panes (duplicates dropped, {RESTORE_LIMIT} at most)",
            saved.len(),
        );
        eprintln!("{text}");
        // Queued for the UI: restore runs off the startup path and may finish
        // before or after the webview is listening.
        super::push_notice(&state, "info", text);
    }
    // The list is rebuilt as each pane comes back with a new id, and only then
    // written. Clearing it up front meant a restore that failed — or an app
    // killed part-way through one — lost the record of what had been open.
    let _ = state.config.update(|c| c.saved_panes.clear());

    for pane in saved {
        // Chats belong to no task: they live in their own folder, so they are
        // restored on the agent's own transcript rather than a worktree.
        if pane.task_id == CHAT_TASK_ID {
            let Some(agent_id) = pane.agent_id.clone() else {
                continue;
            };
            let room = pane.cwd.clone().map(PathBuf::from);
            // Only resume where there is a conversation to resume: a chat
            // saved before folders were per-chat has nothing of its own.
            let resume = room
                .as_deref()
                .map(|d| {
                    agents::resumable(&d.to_string_lossy())
                        .iter()
                        .any(|r| r.agent_id == agent_id)
                })
                .unwrap_or(false);
            // No remember_pane here: spawning records the pane itself.
            if let Err(e) = open_chat(app, &state, agent_id, None, room, resume) {
                eprintln!("could not restore a chat: {e}");
            }
            continue;
        }
        if state.config.task(&pane.task_id).is_err() {
            continue;
        }
        let restored = match pane.kind.as_str() {
            "agent" => {
                let Some(agent_id) = pane.agent_id.clone() else {
                    continue;
                };
                // Only resume if there is a conversation to resume.
                let Ok(task) = state.config.task(&pane.task_id) else {
                    continue;
                };
                let resume = resolve_scope(&state, &task, pane.checkout_id.as_deref())
                    .map(|(cwd, _, _)| {
                        resumable_for(&state, &task, &cwd)
                            .iter()
                            .any(|r| r.agent_id == agent_id)
                    })
                    .unwrap_or(false);

                start_agent(
                    app,
                    &state,
                    pane.task_id.clone(),
                    agent_id,
                    pane.checkout_id.clone(),
                    None,
                    resume,
                    None,
                    None,
                )
            }
            _ => open_shell(app, &state, pane.task_id.clone(), pane.checkout_id.clone(), None, None),
        };

        // Likewise: the spawn recorded it, so recording it again here is what
        // made every restart double the list.
        if let Err(e) = restored {
            eprintln!("could not restore a pane: {e}");
        }
    }
}

/// Stop the process but keep the pane and its scrollback on screen.
#[tauri::command]
pub fn kill_pane(state: State<AppState>, pane_id: String) -> Result<()> {
    state.ptys.kill(&pane_id)
}

