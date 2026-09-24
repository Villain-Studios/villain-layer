//! The message center (MSG-1…5): the log is `crate::messages`; these only
//! read and change it in memory, so they stay plain commands.

use tauri::{AppHandle, State};

use super::AppState;
use crate::error::{Error, Result};
use crate::messages::{changed, Kind, Level, Message, New};

#[tauri::command]
pub fn list_messages(state: State<AppState>) -> Vec<Message> {
    state.messages.list()
}

/// News only the UI sees (a pull request's verdict, a ticket the sweep
/// moved) and every error it shows, kept (MSG-1). What the backend noticed
/// itself it records itself, so a toast of it does not come through here.
#[tauri::command]
pub fn add_message(
    app: AppHandle,
    state: State<AppState>,
    kind: Kind,
    level: Level,
    title: String,
    target: Option<String>,
) -> Result<u64> {
    if title.trim().is_empty() {
        return Err(Error::Other("a message needs something to say".into()));
    }
    let id = state.messages.record(New {
        kind,
        level,
        title,
        body: String::new(),
        target: target.filter(|t| !t.is_empty()),
    });
    changed(&app);
    Ok(id)
}

/// These read, or every one when `ids` is left out.
#[tauri::command]
pub fn mark_messages_read(app: AppHandle, state: State<AppState>, ids: Option<Vec<u64>>) {
    if state.messages.mark_read(ids.as_deref()) {
        changed(&app);
    }
}

#[tauri::command]
pub fn clear_messages(app: AppHandle, state: State<AppState>) {
    if state.messages.clear() {
        changed(&app);
    }
}
