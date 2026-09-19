mod agents;
mod commands;
mod config;
mod error;
mod git;
mod integrations;
mod pty;
mod secrets;
mod shellenv;

use commands::AppState;
use config::ConfigStore;
use pty::PtyManager;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle();
            let state = AppState {
                config: ConfigStore::load(handle)?,
                ptys: PtyManager::default(),
                jira_types: Default::default(),
            };
            app.manage(state);
            // Resolve the login shell's PATH once, off the startup path.
            std::thread::spawn(|| {
                shellenv::user_env();
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_projects,
            commands::add_project,
            commands::add_projects,
            commands::scan_repos,
            commands::remove_project,
            commands::list_tasks,
            commands::create_task,
            commands::delete_task,
            commands::add_checkout,
            commands::remove_checkout,
            commands::suggest_repos,
            commands::set_project_group,
            commands::list_repo_sets,
            commands::save_repo_set,
            commands::delete_repo_set,
            commands::list_repo_rules,
            commands::save_repo_rule,
            commands::delete_repo_rule,
            commands::list_agents,
            commands::list_panes,
            commands::spawn_shell,
            commands::spawn_agent,
            commands::spawn_chat,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_scrollback,
            commands::close_pane,
            commands::kill_pane,
            commands::scan_worktrees,
            commands::adopt_worktree,
            commands::diff_files,
            commands::diff_file,
            commands::send_review,
            commands::commit_task,
            commands::push_task,
            commands::jira_connect,
            commands::jira_issues,
            commands::jira_issue_types,
            commands::jira_issue,
            commands::jira_transitions,
            commands::jira_transition,
            commands::jira_comment,
            commands::jira_start_work,
            commands::github_connect,
            commands::github_task_prs,
            commands::github_open_prs,
            commands::slack_connect,
            commands::slack_notify,
            commands::get_settings,
            commands::set_worktree_root,
            commands::set_ui_prefs,
            commands::disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running villain-layer");
}
