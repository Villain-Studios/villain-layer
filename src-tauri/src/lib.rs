mod agents;
mod commands;
mod config;
mod error;
mod git;
mod integrations;
mod mcp;
mod pty;
mod secrets;
mod shellenv;

use commands::AppState;
use config::ConfigStore;
use pty::PtyManager;
use tauri::Manager;

/// Where a crash is written down, so a packaged app leaves evidence too.
fn panic_log() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Logs/villain-layer"))
        .unwrap_or_else(std::env::temp_dir)
        .join("panic.log")
}

/// Say why, before the process dies.
///
/// A panic that crosses an `extern "C"` frame — anything reached from the
/// webview or from AppKit — aborts instead of unwinding, and all the default
/// handler prints is "panic in a function that cannot unwind", with no file,
/// no line and no trace. The hook still runs first, so this is the only
/// chance to record where it actually happened.
///
/// Nothing in here may panic: a panic inside the hook aborts immediately and
/// takes the report with it.
fn report_panics() {
    std::panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "an unknown location".into());
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "no message".into());

        let text = format!(
            "\n=== villain-layer panicked at {at} ===\n{what}\n{}\n",
            std::backtrace::Backtrace::force_capture(),
        );
        eprintln!("{text}");

        let path = panic_log();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // A crash loop should not fill the disk with its own account of itself.
        if std::fs::metadata(&path)
            .map(|m| m.len() > 1_000_000)
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{} {text}", chrono::Utc::now().to_rfc3339());
        }
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    report_panics();

    // A GUI app launched from Finder starts at `/`, and every child process
    // inherits it. A coding CLI spawned there treats the whole filesystem as
    // its project and walks out into Photos, Downloads and Music, which macOS
    // answers with a permission prompt apiece. Setting it once here is the
    // only place that cannot be forgotten by the next spawn site added.
    // Nothing in the app resolves a relative path, so this changes nothing
    // else.
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::env::set_current_dir(home);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle();
            let state = AppState {
                config: ConfigStore::load(handle)?,
                ptys: PtyManager::default(),
                jira_types: Default::default(),
                pending_notices: Default::default(),
                status_cache: Default::default(),
            };
            app.manage(state);

            // Agents reach the app's Jira, GitHub and Slack connections through
            // this rather than holding their own credentials. Bound before any
            // agent can start, so the endpoint is always ready to hand out.
            match tauri::async_runtime::block_on(mcp::serve(handle.clone())) {
                Ok(e) => {
                    println!("villain-layer mcp listening on {}", e.url);
                    // Publish into the chat folder up front, not only when the
                    // app launches an agent: running `claude` in that folder by
                    // hand should get the same tools and the same context.
                    let state = handle.state::<AppState>();
                    if let Ok(dir) = commands::chat_dir(&state) {
                        let _ = commands::write_chat_context(&state, &dir);
                        let _ = mcp::write_config(&dir);
                    }
                }
                Err(e) => {
                    eprintln!("villain-layer mcp unavailable: {e}");
                    // Agents will start without the app's tools; say so once
                    // rather than leaving a missing .mcp.json to discover later.
                    // Queued, not emitted: the webview has not subscribed yet.
                    commands::push_notice(
                        &handle.state::<AppState>(),
                        "error",
                        format!(
                            "MCP server unavailable ({e}). Agents will not get the app's Jira, GitHub or Slack tools this run."
                        ),
                    );
                }
            }
            // Resolve the login shell's PATH once, off the startup path.
            std::thread::spawn(|| {
                shellenv::user_env();
            });

            // Put back the panes that were open last time. Off the startup
            // path too: each agent spawn waits on the login shell's PATH.
            let restore = handle.clone();
            std::thread::spawn(move || {
                commands::restore_panes(&restore);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_projects,
            commands::add_projects,
            commands::scan_repos,
            commands::remove_project,
            commands::project_branches,
            commands::list_tasks,
            commands::create_task,
            commands::delete_task,
            commands::add_checkout,
            commands::remove_checkout,
            commands::suggest_repos,
            commands::set_project_group,
            commands::list_agents,
            commands::list_panes,
            commands::spawn_shell,
            commands::spawn_agent,
            commands::resumable_agents,
            commands::spawn_chat,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_attach,
            commands::pty_detach,
            commands::close_pane,
            commands::kill_pane,
            commands::diff_files,
            commands::diff_file,
            commands::task_commits,
            commands::send_review,
            commands::commit_task,
            commands::push_task,
            commands::jira_connect,
            commands::jira_issues,
            commands::jira_issue_types,
            commands::jira_epics,
            commands::jira_browse,
            commands::jira_transitions,
            commands::jira_sync_status,
            commands::jira_transition,
            commands::jira_create_fields,
            commands::jira_create_issue,
            commands::jira_create_task,
            commands::jira_start_work,
            commands::task_prompt,
            commands::handoff_prompt,
            commands::draft_pr_description,
            commands::optimize_issue_description,
            commands::request_pr_description,
            commands::take_pr_description,
            commands::github_connect,
            commands::github_task_prs,
            commands::github_all_prs,
            commands::github_review_queue,
            commands::set_checkout_base,
            commands::checkout_branches,
            commands::task_branch_facts,
            commands::github_retarget_pr,
            commands::github_open_prs,
            commands::slack_connect,
            commands::slack_notify,
            commands::set_slack_prefs,
            commands::slack_diagnose,
            commands::slack_cleanup,
            commands::slack_delete_posted,
            commands::get_settings,
            commands::system_notify,
            commands::take_notices,
            commands::set_worktree_root,
            commands::set_ui_prefs,
            commands::disconnect,
            commands::cursor_ide_installed,
            commands::open_in_cursor,
        ])
        .build(tauri::generate_context!())
        .expect("error while building villain-layer")
        .run(|app, event| {
            // Quitting used to take the agents down with SIGHUP, so they never
            // wrote their transcripts and nothing could be resumed next time.
            // Ask them to stop and give them a moment to save.
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                app.state::<AppState>()
                    .ptys
                    .shutdown(std::time::Duration::from_secs(5));
            }
        });
}
