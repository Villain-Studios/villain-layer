//! Putting an agent on a loop, and the check command a loop runs (§21). The
//! loop itself is `loops.rs`; this is its way into the app.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, State};

use crate::config::SavedLoop;
use crate::error::{Error, Result};
use crate::loops::{Host, LoopView, Target, CHECKS_FILE, DEFAULT_ROUNDS};
use crate::pty::{PaneInfo, PtyManager};

use super::AppState;

/// The app, as a loop needs it: its panes, its agents, its window.
struct App(AppHandle);

impl Host for App {
    fn ptys(&self) -> &PtyManager {
        &self.0.state::<AppState>().inner().ptys
    }

    fn deliver(&self, pane: &str, text: &str) -> Result<()> {
        let state = self.0.state::<AppState>();
        let task = state.config.task(&state.ptys.info(pane)?.task_id)?;
        super::panes::hand_over(&state, &task, pane, CHECKS_FILE, text)
    }

    fn keep(&self, pane: &str, kept: Option<SavedLoop>) {
        let state = self.0.state::<AppState>();
        let _ = state.config.update(|c| {
            for p in c.saved_panes.iter_mut().filter(|p| p.id == pane) {
                p.on_loop = kept.clone();
            }
        });
    }

    fn changed(&self, pane: &str) {
        let _ = self.0.emit("loop:changed", pane);
        // What the pane reads as goes with it (LOOP-8).
        let _ = self.0.emit("pty:activity", pane);
    }
}

/// Put a running agent on a loop (LOOP-2): the check commands of the
/// repositories it works in, run each time it ends a turn, for at most
/// `rounds` rounds of failures sent back.
#[tauri::command]
pub async fn start_loop(app: AppHandle, pane_id: String, rounds: Option<u32>) -> Result<LoopView> {
    let host = Arc::new(App(app.clone()));
    super::blocking(app, move |state| {
        let targets = loop_targets(state, &state.ptys.info(&pane_id)?)?;
        state.loops.start(host, &pane_id, targets, rounds.unwrap_or(DEFAULT_ROUNDS), None)
    })
    .await
}

/// Put a loop kept from the last launch back on its pane, now back with a
/// new id (LOOP-11). Not when none of its repositories has a check command
/// any more: then it is only dropped.
pub(crate) fn resume_loop(app: &AppHandle, state: &AppState, pane: &PaneInfo, kept: &SavedLoop) {
    let host = Arc::new(App(app.clone()));
    let started = loop_targets(state, pane)
        .and_then(|targets| state.loops.start(host, &pane.id, targets, kept.rounds, Some(kept.clone())));
    if let Err(e) = started {
        eprintln!("could not put a loop back on its agent: {e}");
    }
}

/// Stop an agent's loop, and a check it is running. The agent goes on as
/// it is (LOOP-7).
#[tauri::command]
pub fn stop_loop(state: State<AppState>, pane_id: String) {
    state.loops.stop(&pane_id);
}

/// An agent's loop, running or ended; None if it was never on one.
#[tauri::command]
pub fn loop_view(state: State<AppState>, pane_id: String) -> Option<LoopView> {
    state.loops.view(&pane_id)
}

/// Set the command a loop runs in a repository (LOOP-1), or clear it with
/// nothing, and rewrite the context files of the tasks it is in, which
/// name it (LOOP-10).
#[tauri::command]
pub async fn set_project_check(app: AppHandle, project_id: String, command: Option<String>) -> Result<()> {
    super::blocking(app, move |state| {
        let command = command.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let found = state.config.update(|c| match c.projects.iter_mut().find(|p| p.id == project_id) {
            Some(p) => {
                p.check = command;
                true
            }
            None => false,
        })?;
        if !found {
            return Err(Error::NotFound(format!("repository {project_id}")));
        }
        let cfg = state.config.read();
        let tasks: HashSet<&str> =
            cfg.checkouts.iter().filter(|c| c.project_id == project_id).map(|c| c.task_id.as_str()).collect();
        for task in cfg.tasks.iter().filter(|t| tasks.contains(t.id.as_str())) {
            // Best effort: the next agent started there writes it again.
            let _ = super::task_context::write_task_context(state, task);
        }
        Ok(())
    })
    .await
}

/// The repositories `pane` works in that have a check command: its own,
/// for a pane started in one, else every one of its task (LOOP-2). Each
/// is named by its folder, as the agent sees it.
pub(crate) fn loop_targets(state: &AppState, pane: &PaneInfo) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    for c in state.config.checkouts_of(&pane.task_id) {
        if pane.checkout_id.as_ref().is_some_and(|id| *id != c.id) {
            continue;
        }
        let project = state.config.project(&c.project_id)?;
        let Some(command) = project.check.filter(|c| !c.trim().is_empty()) else { continue };
        let repo = Path::new(&c.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(project.name);
        targets.push(Target { repo, dir: PathBuf::from(&c.path), command });
    }
    Ok(targets)
}
