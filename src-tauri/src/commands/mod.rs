//! Everything the frontend can call. Thin orchestration over git, PTYs and the
//! three integrations.
//!
//! The unit of work is a **task**: one ticket, N repositories. Most commands
//! take a task id and fan out over its checkouts.

use crate::config::ConfigStore;
use crate::pty::PtyManager;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Instant;

pub struct AppState {
    pub config: ConfigStore,
    pub ptys: PtyManager,
    /// Issue types are per-site and change about never, but each icon is a
    /// separate authenticated fetch, so they are pulled once per run.
    pub jira_types: parking_lot::Mutex<Option<Vec<crate::integrations::jira::IssueType>>>,
    /// The Jira site already searched for an Epic Link field and found none.
    pub epic_field_missing: parking_lot::Mutex<Option<String>>,
    /// Notices raised before the UI is listening. Drained once on boot.
    pub pending_notices: parking_lot::Mutex<Vec<AppNotice>>,
    /// Last git-status per checkout. Cold tasks reuse this so a poll does not
    /// shell out once per every worktree the user has ever opened.
    pub status_cache: parking_lot::Mutex<HashMap<String, CachedStatus>>,
}

/// Snapshot of one checkout's status, reused until it goes hot or ages out.
pub struct CachedStatus {
    pub status: Option<crate::git::WorktreeStatus>,
    pub changed: u32,
    pub exists: bool,
    pub at: Instant,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppNotice {
    pub kind: String,
    pub text: String,
}

/// Run blocking work off the command thread.
///
/// A plain `#[tauri::command] fn` is dispatched on the main thread, so
/// anything that shells out to git, walks the disk or waits on a child holds
/// the event loop for as long as it takes. While it is held, no other invoke
/// is delivered and no event reaches the webview: terminals stop printing,
/// polls stall, and the window stops answering the mouse. `delete_task` found
/// this first — a task with three agents froze the app for the fifteen seconds
/// it spent stopping them — and every command that does the same kind of work
/// now goes through here.
///
/// `spawn_blocking`, not the async runtime: this work is subprocesses and
/// file I/O, and putting it on tokio's worker threads would starve the MCP
/// server and the Jira/GitHub clients that share them.
pub(crate) async fn blocking<T, F>(app: tauri::AppHandle, f: F) -> crate::error::Result<T>
where
    F: FnOnce(&AppState) -> crate::error::Result<T> + Send + 'static,
    T: Send + 'static,
{
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || f(&app.state::<AppState>()))
        .await
        .map_err(|e| crate::error::Error::Other(format!("background work failed: {e}")))?
}

/// Queue a notice for the UI. Emitting at startup is useless — the webview has
/// not subscribed yet — so everything goes through the pending list and the
/// frontend drains it when it is ready.
pub fn push_notice(state: &AppState, kind: &str, text: impl Into<String>) {
    state.pending_notices.lock().push(AppNotice {
        kind: kind.into(),
        text: text.into(),
    });
}

mod projects;
mod tasks;
mod panes;
mod diff;
mod jira;
mod github;
mod slack;
mod settings;

pub use projects::*;
pub use tasks::*;
pub use panes::*;
pub use diff::*;
pub use jira::*;
pub use github::*;
pub use slack::*;
pub use settings::*;

#[cfg(test)]
mod tests {
    use super::panes::{panes_to_restore, RESTORE_LIMIT};
    use super::slack::slack_allows;
    use super::tasks::{derive_branch, suggest_from};
    use super::jira::project_of;
    use crate::config::{AppConfig, Project, SavedPane, SlackConfig};

    fn project(id: &str, group: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: id.into(),
            path: format!("/repos/{id}"),
            default_branch: "main".into(),
            group: group.map(str::to_string),
        }
    }

    fn cfg() -> AppConfig {
        AppConfig {
            projects: vec![
                project("web", Some("frontend")),
                project("admin", Some("frontend")),
                project("api-orders", Some("backend")),
                project("api-billing", Some("backend")),
            ],
            ..Default::default()
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn epic_memory_beats_project_memory() {
        let mut c = cfg();
        c.last_repos.insert("epic:ACME-100".into(), strs(&["api-orders", "api-billing"]));
        c.last_repos.insert("project:ACME".into(), strs(&["web"]));

        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-100"));
        assert_eq!(s.project_ids, strs(&["api-orders", "api-billing"]));
        assert_eq!(s.reason.as_deref(), Some("last task under ACME-100"));

        // A ticket in the project but under no known epic falls through.
        let s = suggest_from(&c, Some("ACME-7"), Some("ACME-999"));
        assert_eq!(s.project_ids, strs(&["web"]));
        assert_eq!(s.reason.as_deref(), Some("last ACME ticket"));
    }

    #[test]
    fn falls_back_to_v1_bare_project_keys() {
        let mut c = cfg();
        c.last_repos.insert("ACME".into(), strs(&["web"]));
        let s = suggest_from(&c, Some("ACME-3"), None);
        assert_eq!(s.project_ids, strs(&["web"]));
    }

    #[test]
    fn never_suggests_a_repo_that_was_removed() {
        let mut c = cfg();
        c.last_repos.insert("epic:ACME-100".into(), strs(&["deleted-repo"]));
        c.last_repos.insert("project:ACME".into(), strs(&["deleted-repo", "web"]));

        // The epic remembers a repository that is gone, so that tier resolves
        // to nothing and the cascade continues rather than preselecting an id
        // that matches no repository the user still has.
        let s = suggest_from(&c, Some("ACME-1"), Some("ACME-100"));
        assert_eq!(s.project_ids, strs(&["web"]));
    }

    #[test]
    fn ticket_branches_are_the_key_plus_an_optional_suffix() {
        // Jira task names already start with the key, so folding the name into
        // the branch used to repeat it: acme-1234-acme-1234-fix-the-thing.
        let name = "ACME-1234 Fix the thing";

        assert_eq!(derive_branch(None, Some("ACME-1234"), None, name), "ACME-1234");
        assert_eq!(
            derive_branch(None, Some("ACME-1234"), Some("some feature"), name),
            "ACME-1234-some-feature",
        );
        // The UI sends the suffix exactly as typed; slugifying happens here.
        assert_eq!(
            derive_branch(None, Some("ACME-21215"), Some("Locale Switch!!"), name),
            "ACME-21215-locale-switch",
        );
        // A blank or punctuation-only suffix is the same as none.
        assert_eq!(derive_branch(None, Some("ACME-1234"), Some("   "), name), "ACME-1234");
        assert_eq!(derive_branch(None, Some("ACME-1234"), Some("--"), name), "ACME-1234");
    }

    #[test]
    fn explicit_branch_wins_and_ticketless_tasks_use_the_name() {
        assert_eq!(
            derive_branch(Some("hotfix/prod"), Some("ACME-1"), Some("ignored"), "whatever"),
            "hotfix/prod",
        );
        assert_eq!(derive_branch(Some("  "), Some("ACME-1"), None, "n"), "ACME-1");
        assert_eq!(
            derive_branch(None, None, None, "fix checkout rounding"),
            "villain/fix-checkout-rounding",
        );
        // Nothing to slug still gives a branch git will make.
        let b = derive_branch(None, None, None, "Корзина");
        assert!(b.starts_with("villain/task-") && b.len() > "villain/task-".len(), "{b}");
    }

    #[test]
    fn slack_switches_gate_each_kind_of_message() {
        let all_on = SlackConfig::default();
        assert!(slack_allows(&all_on, "agent_done"));
        assert!(slack_allows(&all_on, "prs"));
        assert!(slack_allows(&all_on, "agent_tool"));

        // The master switch silences everything, including explicit actions.
        let muted = SlackConfig { enabled: false, ..SlackConfig::default() };
        for kind in ["agent_done", "prs", "agent_tool", "manual"] {
            assert!(!slack_allows(&muted, kind), "{kind} should be muted");
        }

        // Each switch is independent of the others.
        let no_agents = SlackConfig { allow_agent_posts: false, ..SlackConfig::default() };
        assert!(!slack_allows(&no_agents, "agent_tool"));
        assert!(slack_allows(&no_agents, "agent_done"));
        assert!(slack_allows(&no_agents, "prs"));

        let no_prs = SlackConfig { notify_on_prs: false, ..SlackConfig::default() };
        assert!(!slack_allows(&no_prs, "prs"));
        assert!(slack_allows(&no_prs, "agent_done"));

        // A connection test is an explicit user action, never an event.
        assert!(slack_allows(&no_prs, "manual"));
    }

    #[test]
    fn suggests_nothing_when_there_is_no_signal() {
        let s = suggest_from(&cfg(), Some("ACME-1"), None);
        assert!(s.project_ids.is_empty());
        assert!(s.reason.is_none());
    }
    fn saved(id: &str, task: &str, kind: &str, cwd: &str) -> SavedPane {
        SavedPane {
            id: id.into(),
            task_id: task.into(),
            checkout_id: None,
            kind: kind.into(),
            agent_id: if kind == "agent" { Some("claude".into()) } else { None },
            cwd: Some(cwd.into()),
            failed: 0,
        }
    }

    #[test]
    fn a_key_says_which_project_it_belongs_to() {
        // An epic found by searching carries its project in its key, which is
        // what lets a ticket be filed under it with nothing else configured.
        assert_eq!(project_of("ACME-19335").as_deref(), Some("ACME"));
        assert_eq!(project_of("sre-1189").as_deref(), Some("SRE"));
        assert_eq!(project_of("ACME-19335-suffix"), None);
        assert_eq!(project_of("ACME-"), None);
        assert_eq!(project_of("-1"), None);
        assert_eq!(project_of("nodash"), None);
    }

    #[test]
    fn the_same_pane_listed_twice_is_only_restored_once() {
        // The doubling wrote each pane under its own id, twice.
        let doubled = vec![
            saved("1", "t", "agent", "/w"),
            saved("1", "t", "agent", "/w"),
            saved("2", "t", "shell", "/w"),
            saved("2", "t", "shell", "/w"),
        ];
        assert_eq!(panes_to_restore(doubled, RESTORE_LIMIT).len(), 2);
    }

    #[test]
    fn several_shells_in_one_folder_all_come_back() {
        // Three shells in the same worktree are three shells someone opened on
        // purpose. Treating them as duplicates of each other silently threw two
        // of them away at every launch.
        let shells = vec![
            saved("1", "t", "shell", "/w"),
            saved("2", "t", "shell", "/w"),
            saved("3", "t", "shell", "/w"),
        ];
        assert_eq!(panes_to_restore(shells, RESTORE_LIMIT).len(), 3);
    }

    #[test]
    fn restoring_is_capped_however_the_config_got_that_way() {
        // The ceiling is the thing that keeps a bad config from being able to
        // take the machine down, so it holds even when every entry is distinct.
        let many: Vec<SavedPane> = (0..500)
            .map(|i| saved(&i.to_string(), &format!("task{i}"), "shell", &format!("/w{i}")))
            .collect();
        assert_eq!(panes_to_restore(many, RESTORE_LIMIT).len(), RESTORE_LIMIT);
    }

    #[test]
    fn panes_in_different_places_are_all_kept() {
        let distinct = vec![
            saved("1", "t", "agent", "/a"),
            saved("2", "t", "agent", "/b"),
            saved("3", "chat", "agent", "/c"),
        ];
        assert_eq!(panes_to_restore(distinct, RESTORE_LIMIT).len(), 3);
    }

    /// The fan-out matches its answers back to checkouts by position, so an
    /// off-by-one would quietly show one repo's dirty count against another's
    /// name. Enough paths to cross `STATUS_FANOUT`, with real repos, empty
    /// directories and missing paths interleaved so every branch of
    /// `fresh_status` appears in the same batch.
    #[test]
    fn parallel_status_keeps_every_answer_with_its_own_worktree() {
        use super::tasks::fresh_statuses;

        let sandbox = std::env::temp_dir().join(format!("vl-fanout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sandbox).unwrap();

        let mut dirs = Vec::new();
        let mut expect_dirty = Vec::new();
        for i in 0..20 {
            match i % 4 {
                // A repo with `i` modified files, so its own count identifies it.
                0 | 1 => {
                    let repo = sandbox.join(format!("repo{i}"));
                    std::fs::create_dir_all(&repo).unwrap();
                    crate::git::run(&repo, &["init", "-q", "-b", "main"]).unwrap();
                    crate::git::run(&repo, &["config", "user.email", "t@villain.local"]).unwrap();
                    crate::git::run(&repo, &["config", "user.name", "Test"]).unwrap();
                    std::fs::write(repo.join("tracked.txt"), "x\n").unwrap();
                    crate::git::run(&repo, &["add", "-A"]).unwrap();
                    crate::git::run(&repo, &["commit", "-qm", "init"]).unwrap();
                    for n in 0..i {
                        std::fs::write(repo.join(format!("new{n}.txt")), "y\n").unwrap();
                    }
                    dirs.push(repo);
                    expect_dirty.push(Some(i as u32));
                }
                // A directory that is not a repo: it exists, git says nothing.
                2 => {
                    let plain = sandbox.join(format!("plain{i}"));
                    std::fs::create_dir_all(&plain).unwrap();
                    dirs.push(plain);
                    expect_dirty.push(None);
                }
                // A worktree somebody deleted by hand.
                _ => {
                    dirs.push(sandbox.join(format!("gone{i}")));
                    expect_dirty.push(None);
                }
            }
        }

        let out = fresh_statuses(&dirs);
        assert_eq!(out.len(), dirs.len());
        for (i, ((status, changed, exists), want)) in out.iter().zip(&expect_dirty).enumerate() {
            assert_eq!(*exists, dirs[i].is_dir(), "existence of {}", dirs[i].display());
            match want {
                Some(n) => {
                    assert!(status.is_some(), "no status for {}", dirs[i].display());
                    assert_eq!(changed, n, "dirty count landed on the wrong repo at {i}");
                }
                None => assert!(status.is_none(), "status for {}", dirs[i].display()),
            }
        }

        std::fs::remove_dir_all(&sandbox).ok();
    }

    #[test]
    fn a_dropped_pane_is_only_ever_a_repeat_or_over_the_cap() {
        // Nothing else may be dropped: the message the user sees says
        // "duplicates dropped", and it has to be true.
        let many: Vec<SavedPane> = (0..RESTORE_LIMIT)
            .map(|i| saved(&i.to_string(), "t", "shell", "/same"))
            .collect();
        assert_eq!(panes_to_restore(many, RESTORE_LIMIT).len(), RESTORE_LIMIT);
    }
}
