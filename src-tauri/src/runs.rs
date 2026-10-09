//! The run log (§22): one row for each agent's process, from its start to
//! its exit, kept on this Mac and nowhere else (RUN-1…6).
//!
//! In memory first, as the message center's log is. A pane's waiter thread
//! records the run and never touches the disk. A writer thread of its own
//! puts the whole log in `runs.json`, beside `config.json`, a moment after
//! it changes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::AppState;
use crate::config::AppConfig;
use crate::error::Result;
use crate::loops::{LoopView, Phase};
use crate::pty::PaneInfo;

/// The newest this many are kept; older ones go (RUN-5).
pub const CAP: usize = 1000;
/// Agents ending in a burst — a task closed, the app quit — are one write.
const SETTLE: Duration = Duration::from_millis(300);
const VERSION: u32 = 1;

/// How an agent's process ended (RUN-2).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum End {
    /// Asked to: Stop, a restart, its task closed, the app quitting.
    Stopped,
    /// On its own, with any code but 0, or none.
    Failed,
    /// On its own, with exit code 0. What a newer build's kind reads as.
    #[default]
    #[serde(other)]
    Exited,
}

/// How the last loop the agent was on ended (RUN-2).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopEnd {
    Passed,
    GaveUp,
    /// Stopped by you, or still going when the agent ended.
    #[default]
    #[serde(other)]
    Stopped,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopRun {
    /// Failures sent back to the agent.
    #[serde(default)]
    pub used: u32,
    /// The most it was allowed.
    #[serde(default)]
    pub rounds: u32,
    #[serde(default)]
    pub end: LoopEnd,
}

/// Tokens the agent said its finished turns used (RUN-4): an agent over ACP
/// that puts them on its answer to each prompt (Claude's does), or Claude
/// Code's transcript for one in a terminal (`agents::transcript_tokens`).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tokens {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cached_read: u64,
    #[serde(default)]
    pub cached_write: u64,
}

impl Tokens {
    pub fn add(&mut self, other: Tokens) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cached_read = self.cached_read.saturating_add(other.cached_read);
        self.cached_write = self.cached_write.saturating_add(other.cached_write);
    }
}

/// A repository a run worked in, named as it was then.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRepo {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Run {
    /// The pane's id: one run per process.
    pub id: String,
    /// The agent CLI's id ("claude", …).
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub task_id: String,
    /// The task's name as it was. Empty when the task had already gone.
    #[serde(default)]
    pub task: String,
    /// Its own repository, or every one of its task's when it ran at the
    /// task's root, where it could change any of them.
    #[serde(default)]
    pub repos: Vec<RunRepo>,
    #[serde(default)]
    pub acp: bool,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    #[serde(default)]
    pub end: End,
    #[serde(default)]
    pub code: Option<i32>,
    /// The last loop it was on, if any.
    #[serde(default, rename = "loop")]
    pub on_loop: Option<LoopRun>,
    #[serde(default)]
    pub tokens: Option<Tokens>,
    /// Turns it finished: how often it was given something and came back.
    #[serde(default)]
    pub turns: u32,
    /// Times it stopped on something only you could answer: a permission,
    /// the trust question, a usage limit (RUN-7).
    #[serde(default)]
    pub asks: u32,
    /// How long those waited on you, in seconds.
    #[serde(default)]
    pub waited_secs: u64,
    /// It hit its plan's usage limit.
    #[serde(default)]
    pub limited: bool,
    /// Its tool calls, where the agent says: Claude Code's and Copilot's
    /// hooks, or an agent over ACP.
    #[serde(default)]
    pub tools: Option<ToolCalls>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCalls {
    #[serde(default)]
    pub calls: u32,
    #[serde(default)]
    pub failed: u32,
}

impl ToolCalls {
    pub fn finished(&mut self, failed: bool) {
        self.calls = self.calls.saturating_add(1);
        if failed {
            self.failed = self.failed.saturating_add(1);
        }
    }
}

/// What a run needed of you and did, counted as it goes (RUN-7). Asking is
/// worked out from the agent's report and a notice, each changed in several
/// places; each of them settles the tally, which counts at the edges.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    asks: u32,
    /// Since when it has been asking, while it is.
    asking_since: Option<DateTime<Utc>>,
    waited_ms: i64,
    limited: bool,
    tools: Option<ToolCalls>,
}

impl Tally {
    /// Where it stands now: asking or not, on a usage limit or not.
    pub fn settle(&mut self, asking: bool, limited: bool, now: DateTime<Utc>) {
        match (asking, self.asking_since) {
            (true, None) => {
                self.asks = self.asks.saturating_add(1);
                self.asking_since = Some(now);
            }
            (false, Some(since)) => {
                self.waited_ms += (now - since).num_milliseconds().max(0);
                self.asking_since = None;
            }
            _ => {}
        }
        self.limited |= limited;
    }

    /// A tool call a hook says finished.
    pub fn tool(&mut self, failed: bool) {
        self.tools.get_or_insert_default().finished(failed);
    }

    /// The same, from an agent that counts its own (ACP).
    pub fn tools_from(&mut self, tools: ToolCalls) {
        self.tools = Some(tools);
    }
}

/// What a pane knew as its agent's process ended.
pub struct Ended {
    pub info: PaneInfo,
    /// When the process started. `info.started_at` is the pane's place in
    /// the list, which a restarted pane takes from the one it replaced.
    pub born: DateTime<Utc>,
    /// Asked to stop: Stop, a restart, its task closed, quitting.
    pub stopping: bool,
    pub code: Option<i32>,
    pub tokens: Option<Tokens>,
    pub turns: u32,
    pub tally: Tally,
}

/// How a process that ended reads (RUN-2). Being asked to stop comes
/// first: portable-pty reports every death by signal as exit code 1, and
/// read by its code alone, Stop, closing a task and quitting were failures.
pub fn end_of(stopping: bool, code: Option<i32>) -> End {
    match (stopping, code) {
        (true, _) => End::Stopped,
        (false, Some(0)) => End::Exited,
        _ => End::Failed,
    }
}

/// The row for a run that just ended.
fn describe(config: &AppConfig, ended: Ended, on_loop: Option<LoopView>, now: DateTime<Utc>) -> Run {
    let Ended { info, born, stopping, code, tokens, turns, mut tally } = ended;
    // A question still open is one that waited until the end.
    tally.settle(false, false, now);
    let repo = |project_id: &str| RunRepo {
        id: project_id.to_string(),
        name: config
            .projects
            .iter()
            .find(|p| p.id == project_id)
            .map_or_else(|| project_id.to_string(), |p| p.name.clone()),
    };
    let repos = config
        .checkouts
        .iter()
        .filter(|c| match &info.checkout_id {
            Some(own) => &c.id == own,
            None => c.task_id == info.task_id,
        })
        .map(|c| repo(&c.project_id))
        .collect();
    let task = match config.tasks.iter().find(|t| t.id == info.task_id) {
        Some(t) => t.name.clone(),
        None if info.task_id == crate::commands::CHAT_TASK_ID => "Chat".into(),
        None => String::new(),
    };
    Run {
        id: info.id,
        agent: info.agent_id.unwrap_or_default(),
        task_id: info.task_id,
        task,
        repos,
        acp: info.acp,
        started_at: born,
        ended_at: now,
        end: end_of(stopping, code),
        code,
        on_loop: on_loop.map(|v| LoopRun {
            used: v.round,
            rounds: v.rounds,
            end: match v.phase {
                Phase::Passed => LoopEnd::Passed,
                Phase::GaveUp => LoopEnd::GaveUp,
                _ => LoopEnd::Stopped,
            },
        }),
        tokens,
        turns,
        asks: tally.asks,
        waited_secs: (tally.waited_ms / 1000) as u64,
        limited: tally.limited,
        tools: tally.tools,
    }
}

/// Put an agent's ended run in the log, and say so (RUN-1).
pub fn record<R: Runtime>(app: &AppHandle<R>, ended: Ended) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let on_loop = state.loops.view(&ended.info.id);
    let run = describe(&state.config.read(), ended, on_loop, Utc::now());
    state.runs.record(run);
    changed(app);
}

/// Say the log changed, so an open Runs view follows.
pub fn changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit("runs:changed", ());
}

#[derive(Default)]
struct Log {
    /// Newest first.
    items: VecDeque<Run>,
}

impl Log {
    fn record(&mut self, run: Run) {
        self.items.push_front(run);
        self.items.truncate(CAP);
    }

    fn to_json(&self) -> Result<Vec<u8>> {
        let items: Vec<&Run> = self.items.iter().collect();
        Ok(serde_json::to_vec(&serde_json::json!({
            "version": VERSION,
            "runs": items,
        }))?)
    }

    /// A log read back. `None` when the file is not a log at all; a single
    /// row this build cannot read is dropped on its own.
    fn parse(raw: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(raw).ok()?;
        let entries = value.get("runs")?.as_array()?;
        let mut items: VecDeque<Run> = entries
            .iter()
            .filter_map(|e| serde_json::from_value(e.clone()).ok())
            .collect();
        items.truncate(CAP);
        Some(Self { items })
    }
}

pub struct Runs {
    path: PathBuf,
    log: parking_lot::Mutex<Log>,
    /// Tells the writer the log changed.
    dirty: mpsc::Sender<()>,
    /// Held while a write is out, so two never interleave on the temp file.
    disk: parking_lot::Mutex<()>,
}

impl Runs {
    /// The log in `dir`, and the receiving end the writer thread waits on.
    /// A missing file is an empty log. One that cannot be read is kept
    /// beside it, since the next write replaces it.
    pub fn load(dir: &Path) -> (Self, mpsc::Receiver<()>) {
        let path = dir.join("runs.json");
        let log = match std::fs::read_to_string(&path) {
            Ok(raw) => Log::parse(&raw).unwrap_or_else(|| {
                let kept = path.with_extension("json.unreadable");
                let _ = std::fs::copy(&path, &kept);
                eprintln!("runs.json could not be read; starting empty, the old one is at {}", kept.display());
                Log::default()
            }),
            Err(_) => Log::default(),
        };
        let (dirty, rx) = mpsc::channel();
        let store = Self { path, log: parking_lot::Mutex::new(log), dirty, disk: parking_lot::Mutex::new(()) };
        (store, rx)
    }

    /// In memory, waking the writer. The caller announces it.
    pub fn record(&self, run: Run) {
        self.log.lock().record(run);
        let _ = self.dirty.send(());
    }

    pub fn list(&self) -> Vec<Run> {
        self.log.lock().items.iter().cloned().collect()
    }

    pub fn clear(&self) -> bool {
        let mut log = self.log.lock();
        if log.items.is_empty() {
            return false;
        }
        log.items.clear();
        drop(log);
        let _ = self.dirty.send(());
        true
    }

    /// Write the whole log now. The writer thread's, and quitting's.
    pub fn flush(&self) -> Result<()> {
        let _disk = self.disk.lock();
        let bytes = self.log.lock().to_json()?;
        crate::config::write_whole(&self.path, &bytes)
    }

    #[cfg(test)]
    pub fn for_tests(path: PathBuf) -> Self {
        let (dirty, _) = mpsc::channel();
        Self { path, log: Default::default(), dirty, disk: parking_lot::Mutex::new(()) }
    }
}

/// Write the log whenever it changes, for as long as the app runs.
pub fn spawn_writer(app: AppHandle, dirty: mpsc::Receiver<()>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("runs".into())
        .spawn(move || {
            while dirty.recv().is_ok() {
                std::thread::sleep(SETTLE);
                while dirty.try_recv().is_ok() {}
                if let Err(e) = app.state::<AppState>().runs.flush() {
                    eprintln!("runs.json could not be written: {e}");
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{PaneKind, SpawnOptions};
    use std::time::Instant;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-runs-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn run(id: &str) -> Run {
        Run {
            id: id.into(),
            agent: "claude".into(),
            task_id: "t1".into(),
            task: "Fix the login".into(),
            repos: Vec::new(),
            acp: false,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            end: End::Exited,
            code: Some(0),
            on_loop: None,
            tokens: None,
            turns: 0,
            asks: 0,
            waited_secs: 0,
            limited: false,
            tools: None,
        }
    }

    /// An agent pane of task t1's, at the task's root.
    fn info() -> PaneInfo {
        PaneInfo {
            id: "p1".into(),
            task_id: "t1".into(),
            checkout_id: None,
            kind: PaneKind::Agent,
            title: "Claude Code".into(),
            agent_id: Some("claude".into()),
            cwd: "/Users/you/code".into(),
            running: false,
            exit_code: Some(1),
            started_at: Utc::now(),
            last_output_at: Utc::now(),
            notice: None,
            activity: crate::pty::Activity::Idle,
            activity_since: Utc::now(),
            topic: None,
            acp: false,
            loop_said: None,
        }
    }

    /// Two repositories, and one task with a worktree of each.
    fn config(root: &Path) -> AppConfig {
        let root = root.to_string_lossy();
        let mut value = serde_json::json!({
            "projects": [],
            "checkouts": [],
            "tasks": [{ "id": "t1", "name": "Fix the login", "root": root, "branch": "fix-login", "created_at": Utc::now() }],
        });
        for (id, name) in [("api", "ACME API"), ("web", "ACME Web")] {
            value["projects"].as_array_mut().unwrap().push(serde_json::json!({
                "id": id, "name": name, "path": format!("/Users/you/code/{id}"), "default_branch": "main",
            }));
            value["checkouts"].as_array_mut().unwrap().push(serde_json::json!({
                "id": format!("t1-{id}"), "task_id": "t1", "project_id": id, "path": format!("{root}/{id}"), "base": "main",
            }));
        }
        serde_json::from_value(value).unwrap()
    }

    fn state(root: &Path) -> AppState {
        AppState {
            config: crate::config::ConfigStore::for_tests(root.join("config.json"), config(root)),
            ptys: Default::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
            messages: crate::messages::Messages::for_tests(root.join("messages.json")),
            notes: crate::notes::Notes::load(root),
            browser: Default::default(),
            loops: Default::default(),
            runs: Runs::for_tests(root.join("runs.json")),
        }
    }

    fn agent(root: &Path, checkout: Option<&str>, script: &str) -> SpawnOptions {
        SpawnOptions {
            task_id: "t1".into(),
            checkout_id: checkout.map(String::from),
            cwd: root.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: "Claude Code".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            agent_id: Some("claude".into()),
            rows: None,
            cols: None,
            initial_input: None,
            prompted: false,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        }
    }

    /// Stop read as a failure: portable-pty says exit code 1 of a process
    /// ended by a signal, and the code was all a run was judged by.
    #[test]
    fn an_agent_asked_to_stop_reads_as_stopped_and_one_that_ends_badly_as_failed() {
        let root = temp_dir();
        let app = tauri::test::mock_app();
        app.manage(state(&root));
        let st = app.state::<AppState>();
        let start = |checkout: Option<&str>, script: &str| st.ptys.spawn(app.handle(), agent(&root, checkout, script)).unwrap().id;
        let stopped = start(None, "sleep 30");
        let failed = start(Some("t1-web"), "exit 3");
        let exited = start(None, "exit 0");
        st.ptys.kill(&stopped).unwrap();

        let ended = |id: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(r) = st.runs.list().into_iter().find(|r| r.id == id) {
                    return r;
                }
                assert!(Instant::now() < deadline, "no run was recorded for {id}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let names = |r: &Run| r.repos.iter().map(|p| p.name.clone()).collect::<Vec<_>>();

        let r = ended(&stopped);
        assert_eq!((r.end, r.code), (End::Stopped, Some(1)), "a signal, as portable-pty reports it");
        assert_eq!(r.task, "Fix the login");
        assert_eq!(names(&r), ["ACME API", "ACME Web"], "at the task's root it could change either");

        let r = ended(&failed);
        assert_eq!((r.end, r.code), (End::Failed, Some(3)));
        assert_eq!(names(&r), ["ACME Web"]);

        let r = ended(&exited);
        assert_eq!((r.end, r.code), (End::Exited, Some(0)));
        assert!(r.ended_at >= r.started_at);

        // Given a prompt, asked a question, answered with a key, a tool
        // call failed, its turn done, stopped.
        let asked = start(None, "sleep 30");
        st.ptys.write(&asked, "fix the login\r").unwrap();
        st.ptys.report_with(&asked, None, |_| Some(crate::pty::Activity::Asking)).unwrap();
        st.ptys.write(&asked, "1").unwrap();
        st.ptys.report_with(&asked, Some(true), |_| None).unwrap();
        st.ptys.report_with(&asked, Some(false), |_| Some(crate::pty::Activity::Done)).unwrap();
        st.ptys.kill(&asked).unwrap();
        let r = ended(&asked);
        assert_eq!((r.asks, r.turns, r.tools), (1, 1, Some(ToolCalls { calls: 2, failed: 1 })));
        assert_eq!(ended(&exited).tools, None, "a terminal agent whose hooks said nothing");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_run_keeps_how_its_last_loop_ended_and_one_still_going_reads_as_stopped() {
        let root = temp_dir();
        let cfg = config(&root);
        let ended = || Ended {
            info: info(),
            born: Utc::now(),
            stopping: true,
            code: Some(1),
            tokens: None,
            turns: 0,
            tally: Tally::default(),
        };
        let view = |phase: Phase| LoopView {
            pane_id: "p1".into(),
            task_id: "t1".into(),
            phase,
            round: 2,
            rounds: 5,
            started_at: Utc::now(),
            repos: vec!["api".into()],
            checking: None,
            note: None,
            runs: Vec::new(),
        };
        let looped = |phase| describe(&cfg, ended(), Some(view(phase)), Utc::now()).on_loop;
        assert_eq!(looped(Phase::Passed), Some(LoopRun { used: 2, rounds: 5, end: LoopEnd::Passed }));
        assert_eq!(looped(Phase::GaveUp).map(|l| l.end), Some(LoopEnd::GaveUp));
        assert_eq!(looped(Phase::Waiting).map(|l| l.end), Some(LoopEnd::Stopped));
        assert_eq!(describe(&cfg, ended(), None, Utc::now()).on_loop, None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// Asking is worked out afresh from several places, each of which
    /// settles the tally: the same question settled twice is one question.
    #[test]
    fn a_question_is_counted_once_and_its_wait_until_answered_or_the_end() {
        let at = |s: i64| DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(s);
        let mut tally = Tally::default();
        tally.settle(true, false, at(0));
        tally.settle(true, false, at(5));
        tally.settle(false, false, at(30));
        tally.settle(true, true, at(100));
        tally.tool(false);
        tally.tool(true);
        let ended = Ended {
            info: info(),
            born: at(0),
            stopping: true,
            code: Some(1),
            tokens: None,
            turns: 3,
            tally,
        };
        let run = describe(&AppConfig::default(), ended, None, at(160));
        assert_eq!((run.asks, run.waited_secs, run.limited, run.turns), (2, 90, true, 3), "30s answered, 60s open at the end");
        assert_eq!(run.tools, Some(ToolCalls { calls: 2, failed: 1 }));
    }

    #[test]
    fn the_oldest_run_goes_once_there_are_a_thousand() {
        let mut log = Log::default();
        for i in 0..CAP + 5 {
            log.record(run(&format!("r{i}")));
        }
        assert_eq!(log.items.len(), CAP);
        assert_eq!(log.items.front().map(|r| r.id.as_str()), Some("r1004"));
        assert_eq!(log.items.back().map(|r| r.id.as_str()), Some("r5"));
    }

    /// One field added by a later build must not cost every run kept.
    #[test]
    fn a_log_reads_back_and_a_row_it_cannot_read_goes_alone() {
        let dir = temp_dir();
        let runs = Runs::for_tests(dir.join("runs.json"));
        let mut kept = run("r1");
        kept.tokens = Some(Tokens { input: 10, output: 20, cached_read: 300, cached_write: 4 });
        kept.on_loop = Some(LoopRun { used: 1, rounds: 5, end: LoopEnd::Passed });
        runs.record(kept.clone());
        runs.flush().unwrap();
        assert_eq!(Runs::load(&dir).0.list(), vec![kept.clone()]);

        let raw = serde_json::json!({ "version": 2, "runs": [
            { "id": "new", "started_at": Utc::now(), "ended_at": Utc::now(), "end": "timed_out", "mood": "fine" },
            { "id": "broken" },
            serde_json::to_value(&kept).unwrap(),
        ]});
        std::fs::write(dir.join("runs.json"), raw.to_string()).unwrap();
        let read = Runs::load(&dir).0.list();
        assert_eq!(read.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["new", "r1"]);
        assert_eq!(read[0].end, End::Exited, "a kind this build does not know");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_log_is_kept_aside_rather_than_written_over() {
        let dir = temp_dir();
        std::fs::write(dir.join("runs.json"), "{ not json").unwrap();
        let (runs, _) = Runs::load(&dir);
        assert!(runs.list().is_empty());
        assert_eq!(std::fs::read_to_string(dir.join("runs.json.unreadable")).unwrap(), "{ not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_empties_the_file_and_an_empty_log_has_nothing_to_clear() {
        let dir = temp_dir();
        let runs = Runs::for_tests(dir.join("runs.json"));
        assert!(!runs.clear());
        runs.record(run("r1"));
        assert!(runs.clear());
        runs.flush().unwrap();
        assert!(Runs::load(&dir).0.list().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
