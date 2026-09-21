//! UI prefs, worktree root, disconnect integrations.

use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::config::{GithubConfig, JiraConfig, SlackConfig, UiPrefs};
use crate::error::{Error, Result};
use crate::secrets;
use crate::shellenv;

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

/// Where Cursor.app lives, if it does. The shell `cursor` CLI is checked
/// separately — either is enough to open a folder.
fn cursor_app() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/Cursor.app")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Cursor.app"));
    }
    candidates.into_iter().find(|p| p.is_dir())
}

/// True when the Cursor IDE is on this machine (the app, or its `cursor` CLI).
/// Distinct from the `cursor-agent` agent CLI listed under agents.
///
/// Off the command thread for the login-shell PATH, as `list_agents` is: this
/// is the first thing the boot refresh asks for, so it was the one most likely
/// to be waiting on it.
#[tauri::command]
pub async fn cursor_ide_installed(app: AppHandle) -> Result<bool> {
    super::blocking(app, |_| Ok(cursor_ide_present())).await
}

fn cursor_ide_present() -> bool {
    shellenv::which("cursor").is_some() || cursor_app().is_some()
}

/// Open a folder in Cursor IDE. Prefers the `cursor` CLI (opens as a window);
/// falls back to launching the .app on macOS.
#[tauri::command]
pub fn open_in_cursor(path: String) -> Result<()> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("folder {path}")));
    }

    if let Some(bin) = shellenv::which("cursor") {
        Command::new(bin)
            .arg(&dir)
            .spawn()
            .map_err(|e| Error::Other(format!("could not start Cursor: {e}")))?;
        return Ok(());
    }

    if let Some(app) = cursor_app() {
        let status = Command::new("open")
            .arg("-a")
            .arg(&app)
            .arg(&dir)
            .status()
            .map_err(|e| Error::Other(format!("could not open Cursor: {e}")))?;
        if status.success() {
            return Ok(());
        }
        return Err(Error::Other(format!(
            "open -a Cursor failed with {}",
            status.code().unwrap_or(-1)
        )));
    }

    Err(Error::Other("Cursor IDE is not installed".into()))
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn missing_folder_is_not_found() {
        let err = open_in_cursor("/no/such/cursor/folder/ever".into()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn detection_agrees_with_the_filesystem() {
        let expected = PathBuf::from("/Applications/Cursor.app").is_dir()
            || std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join("Applications/Cursor.app").is_dir())
                .unwrap_or(false)
            || shellenv::which("cursor").is_some();
        assert_eq!(cursor_ide_present(), expected);
    }
}
