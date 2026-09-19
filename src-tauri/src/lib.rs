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
                Err(e) => eprintln!("villain-layer mcp unavailable: {e}"),
            }
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
            commands::resumable_agents,
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
            commands::task_prompt,
            commands::handoff_prompt,
            commands::request_pr_description,
            commands::take_pr_description,
            commands::github_connect,
            commands::github_task_prs,
            commands::github_open_prs,
            commands::slack_connect,
            commands::slack_notify,
            commands::set_slack_prefs,
            commands::slack_posted_messages,
            commands::slack_diagnose,
            commands::slack_cleanup,
            commands::slack_delete_posted,
            commands::get_settings,
            commands::set_worktree_root,
            commands::set_ui_prefs,
            commands::disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running villain-layer");
}
