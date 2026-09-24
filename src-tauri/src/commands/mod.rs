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
    /// Reviews and tickets already seen, so a banner is only for what is new.
    pub news: crate::news::Seen,
    /// The message center's log (MSG-1).
    pub messages: crate::messages::Messages,
}

/// Snapshot of one checkout's status, reused until it goes hot or ages out.
pub struct CachedStatus {
    pub status: Option<crate::git::WorktreeStatus>,
    pub changed: u32,
    pub exists: bool,
    pub broken: Option<String>,
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

/// `blocking` for work that needs no `AppState`: the same pool, for the same
/// reason.
pub(crate) async fn off_runtime<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> crate::error::Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| crate::error::Error::Other(format!("background work failed: {e}")))
}

/// Queue a notice for the UI. Emitting at startup is useless — the webview has
/// not subscribed yet — so everything goes through the pending list and the
/// frontend drains it when it is ready.
pub fn push_notice(state: &AppState, kind: &str, text: impl Into<String>) {
    let text = text.into();
    // Kept too (MSG-1): a notice is a toast once, while you may be away.
    state.messages.record(crate::messages::New {
        kind: crate::messages::Kind::Notice,
        level: if kind == "error" { crate::messages::Level::Error } else { crate::messages::Level::Info },
        title: text.clone(),
        body: String::new(),
        target: None,
    });
    state.pending_notices.lock().push(AppNotice { kind: kind.into(), text });
}

/// A notice raised after the UI may already have drained the queue, at any
/// time: queued as ever, and announced, so the UI takes it now. Notices from
/// startup work that ran past the UI's second look were never shown.
pub fn notify(app: &tauri::AppHandle, kind: &str, text: impl Into<String>) {
    use tauri::{Emitter, Manager};
    push_notice(&app.state::<AppState>(), kind, text);
    let _ = app.emit("app:notices", ());
    crate::messages::changed(app);
}

mod projects;
mod repos;
mod cleanup;
mod tasks;
mod panes;
mod diff;
mod jira;
mod github;
mod slack;
mod settings;
mod ticket_flow;
mod messages;

pub use projects::*;
pub use repos::*;
pub use cleanup::*;
pub use tasks::*;
pub use panes::*;
pub use diff::*;
pub use jira::*;
pub use github::*;
pub use slack::*;
pub use settings::*;
pub use ticket_flow::*;
pub use messages::*;

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
            store: None,
            update_by: None,
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
                    crate::git::run_for_tests(&repo, &["init", "-q", "-b", "main"]).unwrap();
                    crate::git::run_for_tests(&repo, &["config", "user.email", "t@villain.local"]).unwrap();
                    crate::git::run_for_tests(&repo, &["config", "user.name", "Test"]).unwrap();
                    std::fs::write(repo.join("tracked.txt"), "x\n").unwrap();
                    crate::git::run_for_tests(&repo, &["add", "-A"]).unwrap();
                    crate::git::run_for_tests(&repo, &["commit", "-qm", "init"]).unwrap();
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
        for (i, (fresh, want)) in out.iter().zip(&expect_dirty).enumerate() {
            assert_eq!(fresh.exists, dirs[i].is_dir(), "existence of {}", dirs[i].display());
            match want {
                Some(n) => {
                    assert!(fresh.status.is_some(), "no status for {}", dirs[i].display());
                    assert_eq!(&fresh.changed, n, "dirty count landed on the wrong repo at {i}");
                    assert!(fresh.broken.is_none(), "a healthy repo reads as broken at {i}");
                }
                None => assert!(fresh.status.is_none(), "status for {}", dirs[i].display()),
            }
            // A folder git cannot read says so; a folder that is not there
            // is "missing", not broken.
            assert_eq!(fresh.broken.is_some(), fresh.exists && want.is_none(), "broken at {i}");
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

    /// The clone a task was cut from is cloned again, as happened to seven
    /// task folders at once: after adoption the folders belong to the app's
    /// copy and do not notice.
    #[test]
    fn task_folders_move_onto_the_apps_copy_and_outlive_a_reclone() {
        use crate::config::{Checkout, ConfigStore, Task};
        let root = std::env::temp_dir().join(format!("vl-adopt-{}", uuid::Uuid::new_v4()));
        let git = |dir: &std::path::Path, args: &[&str]| crate::git::run_for_tests(dir, args).unwrap();
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "-b", "main", "--bare"]);
        let clone = root.join("code/api");
        std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
        git(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        git(&clone, &["config", "user.email", "t@villain.local"]);
        git(&clone, &["config", "user.name", "Test"]);
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-qm", "init"]);
        git(&clone, &["push", "-q", "-u", "origin", "main"]);

        let tasks_root = root.join("worktrees");
        let wt = tasks_root.join("T-1/api");
        crate::git::add_worktree(&clone, &wt, "T-1", "main").unwrap();
        std::fs::write(wt.join("a.txt"), "work\n").unwrap();
        git(&wt, &["commit", "-qam", "unpushed"]);
        let unpushed = git(&wt, &["rev-parse", "HEAD"]).trim().to_string();
        std::fs::write(wt.join("a.txt"), "more work\n").unwrap();

        let mut cfg = AppConfig {
            worktree_root: Some(tasks_root.to_string_lossy().to_string()),
            ..Default::default()
        };
        let mut p = project("api", None);
        p.path = clone.to_string_lossy().to_string();
        cfg.projects.push(p);
        cfg.tasks.push(Task {
            id: "t1".into(),
            name: "T-1".into(),
            root: tasks_root.join("T-1").to_string_lossy().to_string(),
            branch: "T-1".into(),
            issue_key: None,
            issue_url: None,
            created_at: chrono::Utc::now(),
            ticket_stage: None,
        });
        cfg.checkouts.push(Checkout {
            id: "c1".into(),
            task_id: "t1".into(),
            project_id: "api".into(),
            path: wt.to_string_lossy().to_string(),
            base: "main".into(),
            base_commit: None,
            push_lease: None,
            point_before_update: None,
            last_head: None,
        });
        let state = super::AppState {
            config: ConfigStore::for_tests(root.join("config.json"), cfg),
            ptys: crate::pty::PtyManager::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
            messages: crate::messages::Messages::for_tests(std::env::temp_dir().join("vl-test-messages.json")),
        };

        assert_eq!(super::adopt_worktrees(&state), (1, Vec::new()));
        let store = state.config.project("api").unwrap().store.expect("a copy was made");
        assert!(crate::git::belongs_to(&wt, std::path::Path::new(&store)));
        assert_eq!(super::adopt_worktrees(&state), (0, Vec::new()), "the second launch has nothing to do");

        // What happened on 2026-09-23: the clone is deleted and cloned again.
        std::fs::remove_dir_all(&clone).unwrap();
        git(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);

        assert!(crate::git::unlinked(&wt).is_none());
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]).trim(), unpushed, "the unpushed commit is safe");
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "more work\n");
        let views = super::list_tasks_inner(&state, Some("t1".into()));
        let seen = &views[0].checkouts[0];
        assert!(seen.broken.is_none() && seen.changed == 1, "reads as one uncommitted edit");
        assert_eq!(
            state.config.read().checkouts[0].last_head.as_deref(),
            Some(unpushed.as_str()),
            "the poll remembers where the worktree is"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
