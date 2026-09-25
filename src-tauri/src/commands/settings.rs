//! UI prefs, worktree root, disconnect integrations.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

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
        system_notifications: ui.system_notifications,
        notify_waiting_agents: ui.notify_waiting_agents,
    };
    state.config.update(|c| c.ui = ui)
}

/// A banner, and what a click on it should open: `target` is handed back
/// untouched in `system-notify-click`, as `target::Target` wrote it.
///
/// The notification plugin's desktop backend shows the banner and drops the
/// click — `show` never waits for it — so a click could focus the app and
/// still leave you on whichever view you had left. This shows it itself and
/// emits `system-notify-click` only for the activation, not a dismissal.
pub(crate) fn banner(app: &AppHandle, title: String, body: String, target: String) -> Result<()> {
    // A dev build has no bundle id macOS will attribute a notification to.
    // Borrowing Terminal's is what the plugin does, and without it a `tauri
    // dev` banner is delivered to nobody.
    #[cfg(target_os = "macos")]
    {
        let identifier = app.config().identifier.clone();
        let bundle = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            identifier.as_str()
        };
        let _ = notify_rust::set_application(bundle);
    }

    // `wait_for_action` blocks until the banner is clicked or dismissed, and
    // this can be called on the thread that owns the window.
    let app = app.clone();
    std::thread::Builder::new()
        .name("system-notify".into())
        .spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.summary(&title).body(&body);
            let Ok(handle) = notification.show() else {
                return;
            };
            handle.wait_for_action(move |action| {
                if action == "default" {
                    let _ = app.emit("system-notify-click", target);
                }
            });
        })
        .map_err(|e| Error::Other(format!("notification: {e}")))?;
    Ok(())
}

#[tauri::command]
pub fn set_worktree_root(state: State<AppState>, path: Option<String>) -> Result<()> {
    state
        .config
        .update(|c| c.worktree_root = path.filter(|p| !p.trim().is_empty()))
}

/// Off the command thread: deleting the token goes through the keychain,
/// which can stop and ask — and the window froze behind the prompt.
#[tauri::command]
pub async fn disconnect(app: AppHandle, which: String) -> Result<()> {
    super::blocking(app, move |state| disconnect_inner(state, &which)).await
}

fn disconnect_inner(state: &AppState, which: &str) -> Result<()> {
    // What was seen on that site says nothing about the next one.
    state.news.forget();
    match which {
        "jira" => {
            secrets::delete(secrets::JIRA)?;
            // What was learned about that site goes with it; reconnecting to
            // another one should not be told the old one's issue types.
            *state.jira_types.lock() = None;
            *state.epic_field_missing.lock() = None;
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

/// Where Cursor.app lives, if it does: in an Applications folder, or wherever
/// a `cursor` command on the login PATH points into.
///
/// A `cursor` command alone does not mean the IDE. The Cursor agent CLI puts a
/// `cursor` shim in `~/.local/bin`, ahead of the IDE's own, and the shim looks
/// for the IDE on the PATH it was started with. From an app opened in Finder
/// that PATH has neither, so it exited with an error nobody read, and the
/// button did nothing.
fn cursor_app() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("/Applications/Cursor.app")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Cursor.app"));
    }
    candidates.into_iter().find(|p| p.is_dir()).or_else(|| {
        shellenv::path()
            .split(':')
            .filter(|dir| !dir.is_empty())
            .find_map(|dir| app_bundle_of(&Path::new(dir).join("cursor")))
    })
}

/// The `.app` bundle a command really lives in, following links: the IDE's
/// "Install 'cursor' command" links `/usr/local/bin/cursor` into its bundle.
fn app_bundle_of(bin: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(bin).ok()?;
    real.ancestors()
        .skip(1)
        .find(|p| p.extension().is_some_and(|e| e == "app") && p.is_dir())
        .map(Path::to_path_buf)
}

/// True when the Cursor IDE is on this machine.
/// The editor, opened on a task's folder — not an agent CLI.
///
/// Off the command thread for the login-shell PATH, as `list_agents` is: this
/// is the first thing the boot refresh asks for, so it was the one most likely
/// to be waiting on it.
#[tauri::command]
pub async fn cursor_ide_installed(app: AppHandle) -> Result<bool> {
    super::blocking(app, |_| Ok(cursor_app().is_some())).await
}

/// Open a folder in Cursor IDE, through Launch Services.
///
/// Off the command thread: finding the app can wait on the login shell, and
/// `open -a` is waited on until Launch Services answers, so a failure is
/// reported rather than lost.
#[tauri::command]
pub async fn open_in_cursor(app: AppHandle, path: String) -> Result<()> {
    super::blocking(app, move |_| open_in_cursor_inner(path)).await
}

fn open_in_cursor_inner(path: String) -> Result<()> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("folder {path}")));
    }
    let app = cursor_app().ok_or_else(|| Error::Other("Cursor IDE is not installed".into()))?;
    let out = Command::new("open")
        .arg("-a")
        .arg(&app)
        .arg(&dir)
        .output()
        .map_err(|e| Error::Other(format!("could not open Cursor: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(Error::Other(format!(
        "could not open Cursor: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn missing_folder_is_not_found() {
        let err = open_in_cursor_inner("/no/such/cursor/folder/ever".into()).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn a_cursor_command_counts_as_the_ide_only_when_it_lives_in_an_app() {
        let root = std::env::temp_dir().join(format!("vl-cursor-{}", uuid::Uuid::new_v4()));
        let inner = root.join("Cursor.app/Contents/Resources/app/bin");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(root.join("local/bin")).unwrap();
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        std::fs::write(inner.join("code"), "#!/bin/sh\n").unwrap();
        // The agent CLI's shim: a script of its own, in no bundle.
        let shim = root.join("local/bin/cursor");
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();
        // The IDE's "Install 'cursor' command": a link into its bundle.
        let linked = root.join("usr/bin/cursor");
        std::os::unix::fs::symlink(inner.join("code"), &linked).unwrap();

        assert_eq!(app_bundle_of(&shim), None);
        assert_eq!(
            app_bundle_of(&linked),
            Some(std::fs::canonicalize(root.join("Cursor.app")).unwrap())
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
