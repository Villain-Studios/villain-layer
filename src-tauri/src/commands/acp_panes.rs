//! Agents over ACP (§20): starting one in a pane, and the window's side of
//! its conversation.

use tauri::{AppHandle, Emitter, Manager, State};

use crate::acp;
use crate::agents;
use crate::error::{Error, Result};
use crate::pty::{PaneInfo, PaneKind, SpawnOptions};
use crate::shellenv;

use super::panes::Resume;
use super::AppState;

/// What starting an ACP agent needs beyond its catalogue entry.
pub(crate) struct Start {
    pub task_id: String,
    pub checkout_id: Option<String>,
    pub cwd: String,
    pub title: String,
    pub prompt: Option<String>,
    pub resume: Resume,
}

/// Start `def` over ACP in a pane of its own (ACP-1). Nothing is written for
/// the CLI to load: no hooks, no plugin, no MCP config. The app's tools go to
/// it over its stdin (ACP-3), and it reports what it is doing through the
/// protocol (ACP-2).
pub(crate) fn start(app: &AppHandle, state: &AppState, def: &agents::AgentDef, s: Start) -> Result<PaneInfo> {
    let cmd = def.acp.ok_or_else(|| Error::Other(format!("{} cannot run as a conversation (ACP)", def.name)))?;
    let program = shellenv::which(cmd.program)
        .ok_or_else(|| Error::NotFound(format!("{} is not on your PATH", cmd.program)))?;
    let pick = match &s.resume {
        Resume::No => acp::Pick::New,
        Resume::Newest => acp::Pick::Newest,
        Resume::Restart { session: Some(id), .. } => acp::Pick::Id(id.clone()),
        Resume::Restart { session: None, .. } => acp::Pick::New,
    };
    let mcp = crate::mcp::endpoint().map(|e| acp::Mcp {
        name: crate::mcp::SERVER_NAME.to_string(),
        url: e.url.clone(),
        token: e.token.clone(),
    });
    let handle = app.clone();
    // Remembered with the pane, so a restore or a restart picks up this
    // conversation and no other (ACP-9). `remember_pane` reads it too, for a
    // conversation that opened before the pane was first remembered.
    let on_session: acp::OnSession = Box::new(move |pane, session| {
        let _ = handle.state::<AppState>().config.update(|c| {
            for p in c.saved_panes.iter_mut().filter(|p| p.id == pane) {
                p.session = Some(session.to_string());
            }
        });
    });
    // Picking a conversation up hands it back to the agent, so an opening
    // prompt would only talk over it.
    let prompt = s.prompt.filter(|p| !p.trim().is_empty() && s.resume == Resume::No);
    let opts = SpawnOptions {
        task_id: s.task_id,
        checkout_id: s.checkout_id,
        cwd: s.cwd,
        kind: PaneKind::Agent,
        title: s.title,
        program,
        args: cmd.args.iter().map(|a| a.to_string()).collect(),
        agent_id: Some(def.id.to_string()),
        rows: None,
        cols: None,
        initial_input: None,
        prompted: prompt.is_some(),
        env: Vec::new(),
        title_activity: None,
        title_topic: None,
    };
    state.ptys.spawn_acp(app, opts, acp::Launch { mcp, pick, prompt, on_session: Some(on_session) })
}

/// What changed in a conversation after `since`, and the pane is on screen
/// from now (ACP-7). Undone by `pty_detach`, as for a terminal.
#[tauri::command]
pub fn acp_view(app: AppHandle, state: State<AppState>, pane_id: String, since: Option<u64>) -> Result<acp::View> {
    let (view, seen) = state.ptys.acp_view(&pane_id, since)?;
    if seen {
        let _ = app.emit("pty:activity", &pane_id);
    }
    Ok(view)
}

/// Send a prompt, whole (ACP-4).
#[tauri::command]
pub fn acp_prompt(state: State<AppState>, pane_id: String, text: String) -> Result<()> {
    state.ptys.acp_prompt(&pane_id, &text)
}

/// Stop the running turn (ACP-6).
#[tauri::command]
pub fn acp_cancel(state: State<AppState>, pane_id: String) -> Result<bool> {
    state.ptys.acp_cancel(&pane_id)
}

/// Answer a permission question with one of its options (ACP-5).
#[tauri::command]
pub fn acp_answer(state: State<AppState>, pane_id: String, entry: usize, option: String) -> Result<()> {
    state.ptys.acp_answer(&pane_id, entry, &option)
}

/// Change a setting the agent offers: its mode, its model (ACP-12).
#[tauri::command]
pub fn acp_set(state: State<AppState>, pane_id: String, setting: String, value: String) -> Result<()> {
    state.ptys.acp_set(&pane_id, &setting, &value)
}
