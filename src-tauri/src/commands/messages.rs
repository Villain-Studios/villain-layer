//! The message center (MSG-1…5): the log is `crate::messages`; these only
//! read and change it in memory, so they stay plain commands.

use tauri::{AppHandle, State};

use super::AppState;
use crate::messages::{changed, Message};

#[tauri::command]
pub fn list_messages(state: State<AppState>) -> Vec<Message> {
    state.messages.list()
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
