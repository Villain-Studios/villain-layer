//! UI prefs, worktree root, disconnect integrations.

use serde::Serialize;
use tauri::State;

use crate::config::{GithubConfig, JiraConfig, SlackConfig, UiPrefs};
use crate::error::{Error, Result};
use crate::secrets;

use super::{AppNotice, AppState};

// ---------------------------------------------------------------- settings

#[derive(Debug, Serialize)]
pub struct Settings {
    pub ui: UiPrefs,
    pub jira: Option<JiraConfig>,
    pub github: Option<GithubConfig>,
    pub slack: Option<SlackConfig>,
    pub worktree_root: String,
    pub worktree_root_is_default: bool,
    pub jira_connected: bool,
    pub github_connected: bool,
    pub slack_connected: bool,
}

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    let c = state.config.read();
    Settings {
        ui: c.ui.clone(),
        // These configs are only persisted after the token verified, so their
        // presence is the connection state. No keychain read, no password prompt.
        jira_connected: c.jira.is_some(),
        github_connected: c.github.is_some(),
        slack_connected: c.slack.is_some(),
        worktree_root: state.config.worktree_root().to_string_lossy().to_string(),
        worktree_root_is_default: c.worktree_root.is_none(),
        jira: c.jira,
        github: c.github,
        slack: c.slack,
    }
}

/// Notices queued before the UI was listening — MCP bind failure, restore
/// truncation, and anything else raised on the startup path.
#[tauri::command]
pub fn take_notices(state: State<AppState>) -> Vec<AppNotice> {
    std::mem::take(&mut *state.pending_notices.lock())
}

#[tauri::command]
pub fn set_ui_prefs(state: State<AppState>, ui: UiPrefs) -> Result<()> {
    // Clamp rather than reject: the UI sends slider values.
    let ui = UiPrefs {
        scale: ui.scale.clamp(0.8, 1.6),
        terminal_font_size: ui.terminal_font_size.clamp(9, 24),
        restore_panes: ui.restore_panes,
        agents_read_panes: ui.agents_read_panes,
        trust_agent_dirs: ui.trust_agent_dirs,
        sync_jira_status: ui.sync_jira_status,
    };
    state.config.update(|c| c.ui = ui)
}

#[tauri::command]
pub fn set_worktree_root(state: State<AppState>, path: Option<String>) -> Result<()> {
    state
        .config
        .update(|c| c.worktree_root = path.filter(|p| !p.trim().is_empty()))
}

#[tauri::command]
pub fn disconnect(state: State<AppState>, which: String) -> Result<()> {
    match which.as_str() {
        "jira" => {
            secrets::delete(secrets::JIRA)?;
            state.config.update(|c| c.jira = None)
        }
        "github" => {
            secrets::delete(secrets::GITHUB)?;
            state.config.update(|c| c.github = None)
        }
        "slack" => {
            secrets::delete(secrets::SLACK)?;
            state.config.update(|c| c.slack = None)
        }
        other => Err(Error::NotFound(format!("integration {other}"))),
    }
}
