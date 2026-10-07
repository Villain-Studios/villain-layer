//! Panes remembered between launches, and put back at the next one
//! (PANE-7).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agents;
use crate::config::SavedPane;
use crate::error::{Error, Result};
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
        waiting: false,
    };
    let live: HashSet<String> = state.ptys.list(None).into_iter().map(|p| p.id).collect();
    let _ = state.config.update(|c| {
        // Replace rather than append: recording the same pane twice is how a
        // restore that also recorded what it restored doubled this list on
        // every launch.
        c.saved_panes.retain(|p| p.id != saved.id);
        c.saved_panes.push(saved);
        // Panes that exited are never removed from here — only closing one on
        // purpose does that — so without a bound this grows for as long as the
        // app is used.
        trim(&mut c.saved_panes, &live);
    });
}

/// Bring the list down to `SAVED_PANE_LIMIT`, losing the least: panes that
/// exited first (neither open nor waiting), then waiting ones, oldest first.
/// Simply dropping the oldest could take a waiting pane, the one thing PANE-7
/// says is never lost, while dead entries newer than it stayed.
fn trim(saved: &mut Vec<SavedPane>, live: &HashSet<String>) {
    let exited = |p: &SavedPane| !p.waiting && !live.contains(&p.id);
    let waiting = |p: &SavedPane| p.waiting;
    let any = |_: &SavedPane| true;
    let order: [&dyn Fn(&SavedPane) -> bool; 3] = [&exited, &waiting, &any];
    for goes in order {
        let mut over = saved.len().saturating_sub(SAVED_PANE_LIMIT);
        saved.retain(|p| {
            let drop = over > 0 && goes(p);
            if drop {
                over -= 1;
            }
            !drop
        });
    }
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

/// What to put back: each remembered pane once, agents before shells, and
/// never more than the machine should be asked to start at once. A pane
/// already waiting is the user's to reopen, not the launch's (PANE-7).
///
/// Identity is the pane's id, not what it looks like. Three shells in one
/// folder are three shells someone wanted; collapsing them because they share a
/// directory throws away work rather than protecting anything. The doubling
/// this guards against wrote the *same* pane twice, which the id catches.
///
/// Agents first because a shell has no conversation to lose: in the order the
/// panes were opened, a restart with 12 agents and 2 shells left out the two
/// newest, one of them a chat with a day's work in it.
pub(crate) fn panes_to_restore(saved: Vec<SavedPane>, limit: usize) -> Vec<SavedPane> {
    let mut seen = HashSet::new();
    let (agents, shells): (Vec<SavedPane>, Vec<SavedPane>) = saved
        .into_iter()
        .filter(|p| !p.waiting && seen.insert(p.id.clone()))
        .partition(|p| p.kind != "shell");
    agents.into_iter().chain(shells).take(limit).collect()
}

/// Everything a launch does not put back waits for the user (PANE-7), and how
/// many began waiting now. Repeats of one id go: they are the same pane,
/// written twice by the doubling bug.
fn wait_for_the_rest(saved: &mut Vec<SavedPane>, putting_back: &HashSet<String>) -> usize {
    let mut seen = HashSet::new();
    saved.retain(|p| seen.insert(p.id.clone()));
    let mut newly = 0;
    for p in saved.iter_mut().filter(|p| !p.waiting && !putting_back.contains(&p.id)) {
        p.waiting = true;
        newly += 1;
    }
    newly
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

    let saved = panes_to_restore(state.config.read().saved_panes, RESTORE_LIMIT);
    // What does not come back now waits rather than being forgotten. It was
    // forgotten here, and the notice said only "Restored 12 of 14 panes":
    // nothing in the app could bring the other two back.
    let putting_back: HashSet<String> = saved.iter().map(|p| p.id.clone()).collect();
    let waiting = state.config.update(|c| wait_for_the_rest(&mut c.saved_panes, &putting_back)).unwrap_or(0);
    if waiting > 0 {
        let text = format!(
            "{waiting} {} not reopened: at most {RESTORE_LIMIT} come back at launch. {} in the Chat view and in {} tasks' Terminals tab, to reopen or forget.",
            if waiting == 1 { "pane was" } else { "panes were" },
            if waiting == 1 { "It waits" } else { "They wait" },
            if waiting == 1 { "its" } else { "their" },
        );
        eprintln!("{text}");
        // Queued for the UI: restore runs off the startup path and may finish
        // before or after the webview is listening.
        super::notify(app, "info", text);
    }
    let _ = app.emit("panes:waiting", ());
    // The list is rebuilt as each pane comes back with a new id: its old
    // entry is taken off only then. Clearing it up front meant a restore that
    // failed — or an app killed part-way through one — lost the record of
    // what had been open.
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
    let mut resumed: HashSet<(String, String)> = Default::default();

    for pane in saved {
        // Likewise: the spawn recorded it, so recording it again here is what
        // made every restart double the list. A pane that did not come back
        // keeps its entry, to be tried again at the next few launches.
        match put_back(app, &state, &pane, &mut resumed) {
            Back::Opened(_) | Back::Gone(_) => forget(&pane.id),
            Back::Failed(e) => {
                eprintln!("could not restore a pane: {e}");
                failed(&pane.id);
            }
        }
    }
}

/// How putting one saved pane back went.
enum Back {
    Opened(PaneInfo),
    /// What it belonged to is gone, so there is nothing to put back: why.
    Gone(&'static str),
    Failed(Error),
}

/// Put one saved pane back as it was, an agent resumed where its CLI can,
/// unless an agent in `resumed` already continues that folder's conversation.
fn put_back(app: &AppHandle, state: &AppState, pane: &SavedPane, resumed: &mut HashSet<(String, String)>) -> Back {
    // Chats belong to no task: they live in their own folder, so they are
    // restored on the agent's own transcript rather than a worktree.
    if pane.task_id == CHAT_TASK_ID {
        let Some(agent_id) = pane.agent_id.clone() else {
            return Back::Gone("it names no agent");
        };
        let room = pane.cwd.clone().map(PathBuf::from);
        // Only resume where there is a conversation to resume: a chat
        // saved before folders were per-chat has nothing of its own.
        let resume = room.as_deref().is_some_and(|d| {
            let dir = d.to_string_lossy().to_string();
            agents::resumable(&dir).iter().any(|r| r.agent_id == agent_id) && resumed.insert((agent_id.clone(), dir))
        });
        // No remember_pane here: spawning records the pane itself.
        return match open_chat(app, state, agent_id, None, room, resume) {
            Ok(p) => Back::Opened(p),
            Err(e) => Back::Failed(e),
        };
    }
    let Ok(task) = state.config.task(&pane.task_id) else {
        return Back::Gone("its task is gone");
    };
    let restored = match pane.kind.as_str() {
        "agent" => {
            let Some(agent_id) = pane.agent_id.clone() else {
                return Back::Gone("it names no agent");
            };
            // Only resume if there is a conversation to resume.
            let found = resolve_scope(state, &task, pane.checkout_id.as_deref()).ok().and_then(|(cwd, _, _)| {
                // Where the agent will actually start. One pinned to
                // a repo runs there, not wherever `resume_dir` would
                // pick — so looking across the whole task found
                // conversations it would then not be started beside,
                // and `--continue` in its own folder had none.
                let (dir, sessions) = if pane.checkout_id.is_some() {
                    (cwd.clone(), agents::resumable(&cwd))
                } else {
                    (resume_dir(state, &task, &agent_id, &cwd), resumable_for(state, &task, &cwd))
                };
                sessions.iter().any(|r| r.agent_id == agent_id).then_some(dir)
            });
            let resume = match found {
                Some(dir) => resumed.insert((agent_id.clone(), dir)),
                None => false,
            };

            start_agent(app, state, pane.task_id.clone(), agent_id, pane.checkout_id.clone(), None, resume, None, None)
        }
        _ => open_shell(app, state, pane.task_id.clone(), pane.checkout_id.clone(), None, None),
    };
    match restored {
        Ok(p) => Back::Opened(p),
        Err(e) => Back::Failed(e),
    }
}

/// A pane a launch did not put back (PANE-7), as the Chat view and a task's
/// Terminals tab list it.
#[derive(Debug, Clone, Serialize)]
pub struct WaitingPane {
    /// Its saved id: what Reopen and Forget name.
    pub id: String,
    pub task_id: String,
    /// "agent" or "shell".
    pub kind: String,
    pub agent_id: Option<String>,
    pub cwd: Option<String>,
    /// What its CLI last called the conversation, where it says.
    pub title: Option<String>,
}

/// The panes waiting to be reopened. Off the command thread: each agent's
/// title is read from its transcript on disk.
#[tauri::command]
pub async fn waiting_panes(app: AppHandle) -> Result<Vec<WaitingPane>> {
    super::blocking(app, |state| Ok(waiting_panes_inner(state))).await
}

pub(crate) fn waiting_panes_inner(state: &AppState) -> Vec<WaitingPane> {
    state
        .config
        .read()
        .saved_panes
        .into_iter()
        .filter(|p| p.waiting)
        .map(|p| WaitingPane {
            title: match (&p.agent_id, &p.cwd) {
                (Some(agent), Some(cwd)) => conversation_title(agent, cwd),
                _ => None,
            },
            id: p.id,
            task_id: p.task_id,
            kind: p.kind,
            agent_id: p.agent_id,
            cwd: p.cwd,
        })
        .collect()
}

/// What a CLI last called the conversation in `cwd`, from its newest
/// transcript. Claude Code writes an `ai-title` entry as the talk goes on:
/// the name the Chat view shows while the pane runs.
fn conversation_title(agent_id: &str, cwd: &str) -> Option<String> {
    let newest = std::fs::read_dir(agents::session_dir(agent_id, cwd)?)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
    title_in(&std::fs::read_to_string(newest.path()).ok()?)
}

/// The last `ai-title` in a Claude Code transcript.
fn title_in(transcript: &str) -> Option<String> {
    transcript.lines().rev().filter(|l| l.contains("\"ai-title\"")).find_map(|l| {
        let v: serde_json::Value = serde_json::from_str(l).ok()?;
        let title = v.get("aiTitle")?.as_str()?.trim();
        (!title.is_empty()).then(|| title.to_string())
    })
}

/// Put one waiting pane back (PANE-7), as a launch would have.
#[tauri::command]
pub async fn reopen_waiting_pane(app: AppHandle, id: String) -> Result<PaneInfo> {
    let handle = app.clone();
    super::blocking(app, move |state| reopen_waiting_pane_inner(&handle, state, &id)).await
}

pub(crate) fn reopen_waiting_pane_inner(app: &AppHandle, state: &AppState, id: &str) -> Result<PaneInfo> {
    // Taken off the list in the same write that finds it: a second click
    // while the first was still starting would otherwise open it twice.
    let pane = state
        .config
        .update(|c| {
            let at = c.saved_panes.iter().position(|p| p.id == id && p.waiting)?;
            Some(c.saved_panes.remove(at))
        })?
        .ok_or_else(|| Error::NotFound("That pane is no longer waiting to be reopened.".into()))?;
    // Not over a conversation an open agent already continues.
    let mut resumed: HashSet<(String, String)> =
        state.ptys.list(None).into_iter().filter_map(|p| Some((p.agent_id?, p.cwd))).collect();
    let out = match put_back(app, state, &pane, &mut resumed) {
        Back::Opened(p) => Ok(p),
        Back::Gone(why) => Err(Error::NotFound(format!("It cannot be reopened: {why}."))),
        Back::Failed(e) => {
            // Back on the list, to try again or to forget.
            let _ = state.config.update(|c| c.saved_panes.push(pane));
            Err(e)
        }
    };
    let _ = app.emit("panes:waiting", ());
    out
}

/// Forget a waiting pane: the user does not want it back. Its CLI's
/// transcript stays where the CLI keeps it.
#[tauri::command]
pub fn forget_waiting_pane(app: AppHandle, state: State<AppState>, id: String) -> Result<()> {
    state.config.update(|c| c.saved_panes.retain(|p| !(p.id == id && p.waiting)))?;
    let _ = app.emit("panes:waiting", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, task: &str, kind: &str) -> SavedPane {
        SavedPane {
            id: id.into(),
            task_id: task.into(),
            checkout_id: None,
            kind: kind.into(),
            agent_id: (kind == "agent").then(|| "claude".into()),
            cwd: Some(format!("/w/{id}")),
            failed: 0,
            waiting: false,
        }
    }

    /// Fourteen open, in the order they were opened: two shells among twelve
    /// agents, the newest a chat. The first twelve came back, and the chat
    /// was forgotten.
    fn the_restart_that_lost_a_chat() -> Vec<SavedPane> {
        let mut saved = vec![pane("shell-a", "t1", "shell"), pane("shell-b", "t2", "shell")];
        saved.extend((0..11).map(|i| pane(&format!("agent-{i}"), "t1", "agent")));
        saved.push(pane("the-chat", CHAT_TASK_ID, "agent"));
        saved
    }

    #[test]
    fn agents_come_back_before_shells_so_a_conversation_is_never_what_waits() {
        let back = panes_to_restore(the_restart_that_lost_a_chat(), RESTORE_LIMIT);
        assert_eq!(back.len(), RESTORE_LIMIT);
        assert!(back.iter().any(|p| p.id == "the-chat"), "the chat comes back");
        assert!(back.iter().all(|p| p.kind == "agent"), "the shells are the ones that wait");
    }

    #[test]
    fn a_pane_over_the_restore_limit_waits_instead_of_being_forgotten() {
        let mut saved = the_restart_that_lost_a_chat();
        saved.extend((0..3).map(|i| pane(&format!("late-{i}"), "t2", "agent")));
        let back = panes_to_restore(saved.clone(), RESTORE_LIMIT);
        let putting_back: HashSet<String> = back.iter().map(|p| p.id.clone()).collect();

        assert_eq!(wait_for_the_rest(&mut saved, &putting_back), 5);
        assert_eq!(saved.len(), 17, "nothing forgotten");
        let waiting: Vec<&str> = saved.iter().filter(|p| p.waiting).map(|p| p.id.as_str()).collect();
        assert_eq!(waiting, ["shell-a", "shell-b", "late-0", "late-1", "late-2"]);
        assert_eq!(wait_for_the_rest(&mut saved, &putting_back), 0, "already waiting is not news");
    }

    #[test]
    fn a_waiting_pane_is_left_for_the_user_not_reopened_by_a_later_launch() {
        let mut waiting = pane("w", "t1", "agent");
        waiting.waiting = true;
        let back = panes_to_restore(vec![waiting, pane("open", "t1", "shell")], RESTORE_LIMIT);
        assert_eq!(back.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["open"]);
    }

    #[test]
    fn the_same_pane_written_twice_waits_once() {
        let mut saved = vec![pane("1", "t", "agent"), pane("1", "t", "agent"), pane("2", "t", "shell")];
        assert_eq!(wait_for_the_rest(&mut saved, &HashSet::new()), 2);
        assert_eq!(saved.len(), 2);
    }

    #[test]
    fn a_full_list_drops_exited_panes_before_waiting_or_open_ones() {
        let mut saved: Vec<SavedPane> = (0..SAVED_PANE_LIMIT + 3).map(|i| pane(&i.to_string(), "t", "agent")).collect();
        saved[0].waiting = true;
        saved[1].waiting = true;
        let live: HashSet<String> = ["2", "3", "4"].map(String::from).into();
        trim(&mut saved, &live);
        assert_eq!(saved.len(), SAVED_PANE_LIMIT);
        let kept: Vec<&str> = saved.iter().take(5).map(|p| p.id.as_str()).collect();
        assert_eq!(kept, ["0", "1", "2", "3", "4"], "the oldest exited panes went, not the waiting or open ones");
    }

    #[test]
    fn a_waiting_chat_is_named_by_its_last_title() {
        let transcript = concat!(
            r#"{"type":"user","message":{"content":"look at the repos"}}"#, "\n",
            r#"{"type":"ai-title","aiTitle":"Repo overview"}"#, "\n",
            r#"{"type":"assistant","message":{"content":"…"}}"#, "\n",
            r#"{"type":"ai-title","aiTitle":"Backend architecture and domain structure review"}"#, "\n",
        );
        assert_eq!(title_in(transcript).as_deref(), Some("Backend architecture and domain structure review"));
        assert_eq!(title_in(r#"{"type":"user"}"#), None);
    }
}
