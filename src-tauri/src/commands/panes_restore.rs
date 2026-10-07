//! Panes remembered between launches, and put back at the next one
//! (PANE-7).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::agents;
use crate::config::SavedPane;
use crate::pty::{PaneInfo, PaneKind};

use super::panes::{open_chat, open_shell, resolve_scope, resumable_for, resume_dir, start_agent, CHAT_TASK_ID};
use super::AppState;

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
        failed: 0,
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

/// How many launches a saved pane may fail to come back on before it is
/// forgotten.
pub(crate) const RESTORE_TRIES: u8 = 3;

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
        super::notify(app, "info", text);
    }
    // The list is rebuilt as each pane comes back with a new id: its old
    // entry is taken off only then. Clearing it up front meant a restore that
    // failed — or an app killed part-way through one — lost the record of
    // what had been open. Only what is over the limit goes now, as the notice
    // above says.
    let attempted: std::collections::HashSet<String> = saved.iter().map(|p| p.id.clone()).collect();
    let _ = state.config.update(|c| c.saved_panes.retain(|p| attempted.contains(&p.id)));
    let forget = |id: &str| {
        let _ = state.config.update(|c| c.saved_panes.retain(|p| p.id != id));
    };
    // Kept for another try, a few times: a CLI missing from PATH at one
    // launch may be back at the next, but a pane whose repo has left the task
    // never will be.
    let failed = |id: &str| {
        let _ = state.config.update(|c| {
            for p in c.saved_panes.iter_mut().filter(|p| p.id == id) {
                p.failed = p.failed.saturating_add(1);
            }
            c.saved_panes.retain(|p| p.failed < RESTORE_TRIES);
        });
    };

    // Which agent has already resumed in which folder this restore. Two
    // agents in one folder both ran `--continue`, which picks the newest
    // conversation there: both came back as the same one, writing into one
    // transcript, and the other conversation was not resumed at all.
    let mut resumed: std::collections::HashSet<(String, String)> = Default::default();

    for pane in saved {
        // Chats belong to no task: they live in their own folder, so they are
        // restored on the agent's own transcript rather than a worktree.
        if pane.task_id == CHAT_TASK_ID {
            let Some(agent_id) = pane.agent_id.clone() else {
                forget(&pane.id);
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
            match open_chat(app, &state, agent_id, None, room, resume) {
                Ok(_) => forget(&pane.id),
                Err(e) => {
                    eprintln!("could not restore a chat: {e}");
                    failed(&pane.id);
                }
            }
            continue;
        }
        if state.config.task(&pane.task_id).is_err() {
            forget(&pane.id);
            continue;
        }
        let restored = match pane.kind.as_str() {
            "agent" => {
                let Some(agent_id) = pane.agent_id.clone() else {
                    forget(&pane.id);
                    continue;
                };
                // Only resume if there is a conversation to resume.
                let Ok(task) = state.config.task(&pane.task_id) else {
                    forget(&pane.id);
                    continue;
                };
                let found = resolve_scope(&state, &task, pane.checkout_id.as_deref()).ok().and_then(
                    |(cwd, _, _)| {
                        // Where the agent will actually start. One pinned to
                        // a repo runs there, not wherever `resume_dir` would
                        // pick — so looking across the whole task found
                        // conversations it would then not be started beside,
                        // and `--continue` in its own folder had none.
                        let (dir, sessions) = if pane.checkout_id.is_some() {
                            (cwd.clone(), agents::resumable(&cwd))
                        } else {
                            (resume_dir(&state, &task, &agent_id, &cwd), resumable_for(&state, &task, &cwd))
                        };
                        sessions.iter().any(|r| r.agent_id == agent_id).then_some(dir)
                    },
                );
                let resume = match found {
                    Some(dir) => resumed.insert((agent_id.clone(), dir)),
                    None => false,
                };

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
        // made every restart double the list. A pane that did not come back
        // keeps its entry, to be tried again at the next few launches.
        match restored {
            Ok(_) => forget(&pane.id),
            Err(e) => {
                eprintln!("could not restore a pane: {e}");
                failed(&pane.id);
            }
        }
    }
}
