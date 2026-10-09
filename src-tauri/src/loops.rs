//! Loops (§21): an agent put on one has the checks of its repositories run
//! each time it ends a turn, and what failed sent back to it, until they
//! pass or its rounds run out. Then the pane is handed back to you, with
//! what the loop has to say about it (LOOP-8).
//!
//! One driver per loop: an async task that waits on the pane's turns
//! (`PtyManager::turns`, rung by the agent's own reports) and puts what
//! waits — git, the checks, typing into a terminal — on the blocking pool.

mod check;
mod message;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::{Notify, Semaphore};

use crate::error::{Error, Result};
use crate::config::SavedLoop;
use crate::pty::PtyManager;
use message::{count, failures, tail};

pub const DEFAULT_ROUNDS: u32 = 5;
/// A ceiling, not a preference: each round is a turn of an agent and a run
/// of every check.
pub const MAX_ROUNDS: u32 = 20;
/// A check still running after this is stopped, and so is its loop (LOOP-5).
const CHECK_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// Loops whose checks may run at the same time, in the whole app (LOOP-5).
/// A check is often a whole build; a dozen at once took a laptop down.
const AT_ONCE: usize = 2;
/// How long a loop put back after a restart waits for its agent to say it
/// is ready before it checks anyway (LOOP-11).
const READY_WAIT: Duration = Duration::from_secs(60);
/// Check runs a loop keeps to show.
const RUNS_KEPT: usize = 25;
/// Where failures too long to type into a terminal are left (PANE-11).
pub const CHECKS_FILE: &str = "CHECKS.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The agent is in a turn, to be checked at its end.
    Waiting,
    /// Its turn has ended, and other loops' checks are running (LOOP-5).
    Queued,
    Checking,
    /// Waiting on you: the turn changed nothing, or ended on a usage limit
    /// (LOOP-4). It goes on at the next turn that changes something.
    Held,
    Passed,
    GaveUp,
    Stopped,
}

impl Phase {
    pub fn ended(self) -> bool {
        matches!(self, Phase::Passed | Phase::GaveUp | Phase::Stopped)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    /// The repository's folder in the task.
    pub repo: String,
    pub command: String,
    pub passed: bool,
    pub code: Option<i32>,
    pub secs: f64,
    /// The end of its output, as the agent is sent it.
    pub output: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckRun {
    pub at: DateTime<Utc>,
    /// The rounds used before it: 0 for the first check.
    pub round: u32,
    pub results: Vec<CheckResult>,
}

/// A loop as the window shows it (LOOP-9).
#[derive(Debug, Clone, Serialize)]
pub struct LoopView {
    pub pane_id: String,
    pub task_id: String,
    pub phase: Phase,
    /// Rounds used: failures sent back to the agent.
    pub round: u32,
    pub rounds: u32,
    pub started_at: DateTime<Utc>,
    /// The repositories it checks, by folder.
    pub repos: Vec<String>,
    /// The one being checked now.
    pub checking: Option<String>,
    /// Why it waits, or how it ended, in a sentence.
    pub note: Option<String>,
    /// Newest first.
    pub runs: Vec<CheckRun>,
}

/// One repository a loop checks, and how.
#[derive(Debug, Clone)]
pub struct Target {
    pub repo: String,
    pub dir: PathBuf,
    pub command: String,
}

/// What a loop needs of the app, so a test can stand in for it.
pub trait Host: Send + Sync + 'static {
    fn ptys(&self) -> &PtyManager;
    /// Give the agent `text`. Blocking: it may write a file and type.
    fn deliver(&self, pane: &str, text: &str) -> Result<()>;
    /// The loop, or what its pane reads as, changed.
    fn changed(&self, pane: &str);
    /// Keep the loop with the saved pane, or None to drop it (LOOP-11).
    /// Blocking: it writes the config.
    fn keep(&self, pane: &str, kept: Option<SavedLoop>);
}

struct Shared {
    view: Mutex<LoopView>,
    /// Raised to stop. The driver and a running check both look.
    stop: AtomicBool,
    wake: Notify,
    /// The process group of the check running now, 0 for none.
    group: AtomicI32,
    /// Stopped because the app is quitting: kept with its pane, to come
    /// back at the next launch (LOOP-11), not dropped as ended.
    quitting: AtomicBool,
}

impl Shared {
    fn set(&self, phase: Phase, note: Option<String>) {
        let mut view = self.view.lock();
        view.phase = phase;
        view.note = note;
        view.checking = None;
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Every loop this run of the app has started, by pane.
pub struct Loops {
    map: Mutex<HashMap<String, Arc<Shared>>>,
    permits: Arc<Semaphore>,
}

impl Default for Loops {
    fn default() -> Self {
        Self { map: Default::default(), permits: Arc::new(Semaphore::new(AT_ONCE)) }
    }
}

impl Loops {
    pub fn view(&self, pane: &str) -> Option<LoopView> {
        self.map.lock().get(pane).map(|s| s.view.lock().clone())
    }

    /// Put `pane` on a loop over `targets` (LOOP-2). One per pane: a loop
    /// that has ended is replaced.
    /// `from` is a loop kept from the last launch, put back (LOOP-11).
    pub fn start<H: Host>(
        &self,
        host: Arc<H>,
        pane: &str,
        targets: Vec<Target>,
        rounds: u32,
        from: Option<SavedLoop>,
    ) -> Result<LoopView> {
        if targets.is_empty() {
            return Err(Error::Other(
                "None of the repositories this agent works in has a check command. Set one first.".into(),
            ));
        }
        let info = host.ptys().info(pane)?;
        if info.kind != crate::pty::PaneKind::Agent || !info.running {
            return Err(Error::Other("Only an agent that is running can be put on a loop.".into()));
        }
        let shared = {
            let mut map = self.map.lock();
            // Loops of panes since closed have nothing more to show.
            map.retain(|id, _| host.ptys().info(id).is_ok());
            if map.get(pane).is_some_and(|s| !s.view.lock().phase.ended()) {
                return Err(Error::Other("This agent is on a loop already.".into()));
            }
            let shared = Arc::new(Shared {
                view: Mutex::new(LoopView {
                    pane_id: pane.to_string(),
                    task_id: info.task_id,
                    phase: Phase::Waiting,
                    round: from.as_ref().map_or(0, |f| f.round),
                    rounds: rounds.clamp(1, MAX_ROUNDS),
                    started_at: Utc::now(),
                    repos: targets.iter().map(|t| t.repo.clone()).collect(),
                    checking: None,
                    note: None,
                    runs: Vec::new(),
                }),
                stop: AtomicBool::new(false),
                wake: Notify::new(),
                group: AtomicI32::new(0),
                quitting: AtomicBool::new(false),
            });
            map.insert(pane.to_string(), shared.clone());
            shared
        };
        let view = shared.view.lock().clone();
        tauri::async_runtime::spawn(drive(host, shared, targets, self.permits.clone(), from));
        Ok(view)
    }

    /// Quitting: every loop stopped, and its check asked to end now and
    /// killed by `kill_checks` once the agents' grace is over (PANE-5). A
    /// check runs in a process group of its own, which nothing else ends:
    /// a build half done would run on after the app had gone.
    pub fn stop_all(&self) {
        for shared in self.map.lock().values() {
            shared.quitting.store(true, Ordering::Release);
            shared.stop.store(true, Ordering::Release);
            shared.wake.notify_one();
            let group = shared.group.load(Ordering::Acquire);
            if group > 0 {
                check::signal(group, libc::SIGTERM);
            }
        }
    }

    pub fn kill_checks(&self) {
        for shared in self.map.lock().values() {
            let group = shared.group.load(Ordering::Acquire);
            if group > 0 {
                check::signal(group, libc::SIGKILL);
            }
        }
    }

    /// Stop `pane`'s loop, and a check it is running. The agent is left as
    /// it is (LOOP-7).
    pub fn stop(&self, pane: &str) {
        if let Some(shared) = self.map.lock().get(pane) {
            shared.stop.store(true, Ordering::Release);
            shared.wake.notify_one();
        }
    }
}

/// How a loop ended: its phase, the note it shows, and what the pane says
/// in the banner and the message center (LOOP-8). Nothing to say for a
/// loop you stopped yourself.
struct End {
    phase: Phase,
    note: String,
    said: Option<String>,
}

impl End {
    fn stopped(note: &str, said: Option<String>) -> Self {
        Self { phase: Phase::Stopped, note: note.to_string(), said }
    }
}

async fn drive<H: Host>(host: Arc<H>, me: Arc<Shared>, targets: Vec<Target>, permits: Arc<Semaphore>, from: Option<SavedLoop>) {
    let pane = me.view.lock().pane_id.clone();
    let end = go_round(&host, &me, &pane, &targets, &permits, from).await;
    // Quitting is not ending: the app is going, and the loop with it, to
    // come back at the next launch (LOOP-11). Taken for an end, every loop
    // was dropped by the very quit that was to keep it.
    if me.quitting.load(Ordering::Acquire) {
        return;
    }
    me.set(end.phase, Some(end.note));
    let _ = host.ptys().set_looping(&pane, false, end.said);
    host.changed(&pane);
    keep(&host, &pane, None).await;
}

/// Keep where the loop is with its saved pane (LOOP-11), off the runtime.
async fn keep<H: Host>(host: &Arc<H>, pane: &str, kept: Option<SavedLoop>) {
    let (h, p) = (host.clone(), pane.to_string());
    let _ = tauri::async_runtime::spawn_blocking(move || h.keep(&p, kept)).await;
}

fn kept(me: &Shared, failed_at: &Option<String>, held: bool) -> SavedLoop {
    let view = me.view.lock();
    SavedLoop { rounds: view.rounds, round: view.round, failed_at: failed_at.clone(), held }
}

/// Waiting on a turn's end or a stop, and what the loop is about to do.
fn show<H: Host>(host: &Arc<H>, me: &Shared, pane: &str, phase: Phase, note: Option<String>, looping: bool, said: Option<String>) {
    me.set(phase, note);
    let _ = host.ptys().set_looping(pane, looping, said);
    host.changed(pane);
}

async fn go_round<H: Host>(
    host: &Arc<H>,
    me: &Arc<Shared>,
    pane: &str,
    targets: &[Target],
    permits: &Arc<Semaphore>,
    from: Option<SavedLoop>,
) -> End {
    let gone = || End::stopped("The agent was closed.", None);
    let exited = || End::stopped("The agent exited.", None);
    let by_you = || End::stopped("Stopped.", None);

    let Ok(mut turns) = host.ptys().turns(pane) else { return gone() };
    let mut seen = *turns.borrow_and_update();
    // What the worktrees were when the checks last failed (LOOP-4).
    let mut failed_at: Option<String> = None;
    let mut check_now = match host.ptys().turn(pane) {
        Ok(turn) => !turn.mid,
        Err(_) => return gone(),
    };
    match &from {
        // Put back after a restart (LOOP-11). The agent has only just been
        // started again, on its conversation: nothing is sent it until it
        // says it is ready, which is a hook's or the protocol's first word: a
        // terminal still drawing itself can drop what is typed, which is why
        // an opening prompt waits too.
        // Looked for every quarter second, for a minute at most, since an
        // agent that never says still deserves its checks.
        Some(kept_from) => {
            let until = tokio::time::Instant::now() + READY_WAIT;
            while tokio::time::Instant::now() < until {
                match host.ptys().turn(pane) {
                    Ok(turn) if !turn.running => return exited(),
                    Ok(turn) if turn.said => break,
                    Ok(_) => {}
                    Err(_) => return gone(),
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    _ = me.wake.notified() => if me.stopped() { return by_you() },
                }
            }
            seen = *turns.borrow_and_update();
            if kept_from.held {
                // Still waiting on you, for a turn that changes something.
                failed_at = kept_from.failed_at.clone();
                check_now = false;
                let note = "Put back after a restart, waiting on you as before: the loop goes on after a turn that changes something.";
                show(host, me, pane, Phase::Held, Some(note.into()), false, None);
            } else {
                // The turn it waited for ended with the app.
                check_now = true;
            }
            keep(host, pane, Some(kept(me, &failed_at, kept_from.held))).await;
        }
        None => {
            if !check_now {
                show(host, me, pane, Phase::Waiting, None, true, None);
            }
            keep(host, pane, Some(kept(me, &None, false))).await;
        }
    }

    loop {
        // The end of a turn (LOOP-3), or a stop.
        while !check_now {
            tokio::select! {
                rung = turns.changed() => if rung.is_err() { return gone() },
                _ = me.wake.notified() => {}
            }
            if me.stopped() {
                return by_you();
            }
            match host.ptys().turn(pane) {
                Ok(turn) if !turn.running => return exited(),
                Ok(_) => {}
                Err(_) => return gone(),
            }
            let now = *turns.borrow_and_update();
            if now != seen {
                seen = now;
                check_now = true;
            }
        }
        check_now = false;

        let turn = match host.ptys().turn(pane) {
            Ok(turn) if !turn.running => return exited(),
            Ok(turn) => turn,
            Err(_) => return gone(),
        };
        if turn.notice.as_deref() == Some("usage_limit") {
            let note = "The agent hit its usage limit. The loop goes on after its next turn.";
            show(host, me, pane, Phase::Held, Some(note.into()), false, None);
            continue;
        }
        let print = fingerprint(targets).await;
        if failed_at.as_ref() == Some(&print) {
            let note = "The agent ended its turn without changing anything: it may be asking you something. \
                        The loop goes on after a turn that changes something.";
            let said = "ended a turn without changing anything — its loop waits for you";
            show(host, me, pane, Phase::Held, Some(note.into()), false, Some(said.into()));
            keep(host, pane, Some(kept(me, &failed_at, true))).await;
            continue;
        }

        show(host, me, pane, Phase::Queued, None, true, None);
        let permit = loop {
            tokio::select! {
                permit = permits.clone().acquire_owned() => break permit,
                _ = me.wake.notified() => if me.stopped() { return by_you() },
            }
        };
        let Ok(_permit) = permit else { return End::stopped("The app is shutting down.", None) };
        show(host, me, pane, Phase::Checking, None, true, None);
        let round = me.view.lock().round;
        let mut results = Vec::new();
        for target in targets {
            me.view.lock().checking = Some(target.repo.clone());
            host.changed(pane);
            let (t, stop) = (target.clone(), me.clone());
            let ran = tauri::async_runtime::spawn_blocking(move || {
                check::run(&t.dir, &t.command, CHECK_TIMEOUT, &stop.stop, &stop.group)
            })
            .await;
            let outcome = match ran {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(e)) => return End::stopped(&format!("{}: {e}", target.repo), Some(format!("its loop stopped: {e}"))),
                Err(e) => return End::stopped(&format!("The check could not run: {e}"), None),
            };
            if me.stopped() || outcome.ended == check::Ended::Stopped {
                return by_you();
            }
            let result = CheckResult {
                repo: target.repo.clone(),
                command: target.command.clone(),
                passed: outcome.ended == check::Ended::Exited && outcome.code == Some(0),
                code: outcome.code,
                secs: outcome.secs,
                output: tail(&outcome.output),
            };
            results.push(result);
            // A check that cannot be run, or never ends, is no agent's to fix
            // (LOOP-5).
            let why = match (outcome.ended, outcome.code) {
                (check::Ended::TimedOut, _) => Some(format!(
                    "`{}` in {} ran past {} minutes and was stopped",
                    target.command,
                    target.repo,
                    CHECK_TIMEOUT.as_secs() / 60
                )),
                (_, Some(127)) => Some(format!(
                    "`{}` in {} exited 127: a command it runs was not found",
                    target.command, target.repo
                )),
                _ => None,
            };
            if let Some(why) = why {
                record(me, round, results);
                return End::stopped(&format!("Stopped: {why}."), Some(format!("its loop stopped: {why}")));
            }
        }
        drop(_permit);

        // Someone started a turn while the checks ran: what they found is
        // about work that has moved on, and typed now would land in the
        // middle of it (LOOP-6).
        let turn = match host.ptys().turn(pane) {
            Ok(turn) if !turn.running => return exited(),
            Ok(turn) => turn,
            Err(_) => return gone(),
        };
        if *turns.borrow() != seen || turn.mid {
            check_now = !turn.mid;
            seen = *turns.borrow_and_update();
            show(host, me, pane, Phase::Waiting, None, true, None);
            continue;
        }

        let passed = results.iter().all(|r| r.passed);
        let feedback = (!passed).then(|| failures(&results, targets, round + 1, me.view.lock().rounds));
        record(me, round, results);
        if passed {
            let note = if round == 0 { "The checks pass.".to_string() } else { format!("The checks pass, after {}.", count(round)) };
            return End { phase: Phase::Passed, note, said: Some("passed its checks — your turn".into()) };
        }
        let rounds = me.view.lock().rounds;
        if round >= rounds {
            return End {
                phase: Phase::GaveUp,
                note: format!("The checks still fail after {}.", count(rounds)),
                said: Some(format!("still fails its checks after {} — your turn", count(rounds))),
            };
        }

        me.view.lock().round = round + 1;
        // What the checks left, not what they found: one that writes files
        // of its own (a coverage report, a new snapshot) changed the
        // worktree, and taken for the agent's work, the same failures went
        // back to it after a turn that changed nothing.
        failed_at = Some(fingerprint(targets).await);
        keep(host, pane, Some(kept(me, &failed_at, false))).await;
        // Before it is sent: the end of the turn it starts must be news.
        seen = *turns.borrow_and_update();
        show(host, me, pane, Phase::Waiting, None, true, None);
        let (h, p, text) = (host.clone(), pane.to_string(), feedback.unwrap_or_default());
        match tauri::async_runtime::spawn_blocking(move || h.deliver(&p, &text)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return End::stopped(&format!("The failures could not be sent: {e}"), None),
            Err(e) => return End::stopped(&format!("The failures could not be sent: {e}"), None),
        }
    }
}

/// Keep a run to show, newest first.
fn record(me: &Shared, round: u32, results: Vec<CheckResult>) {
    let mut view = me.view.lock();
    view.runs.insert(0, CheckRun { at: Utc::now(), round, results });
    view.runs.truncate(RUNS_KEPT);
}

/// What the worktrees hold now, all of them (LOOP-4). A worktree git cannot
/// read is never the same as before, so it is checked rather than held.
async fn fingerprint(targets: &[Target]) -> String {
    let dirs: Vec<PathBuf> = targets.iter().map(|t| t.dir.clone()).collect();
    tauri::async_runtime::spawn_blocking(move || {
        dirs.iter()
            .map(|d| crate::git::fingerprint(d).unwrap_or_else(|e| format!("unreadable {}: {e}", uuid::Uuid::new_v4())))
            .collect::<Vec<_>>()
            .join(" ")
    })
    .await
    .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::pty::{Activity, PaneKind, SpawnOptions};

    /// An agent over ACP in a few lines of shell: it opens session `s-1`,
    /// and answers each prompt by running `on_prompt` (with `$n`, the
    /// prompt's number) in its folder and ending its turn.
    fn fake_agent(on_prompt: &str) -> String {
        format!(
            r#"
read -r _; printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}'
read -r _; printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"sessionId":"s-1"}}}}'
n=0
while read -r line; do
  case "$line" in *session/prompt*) ;; *) continue ;; esac
  id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
  n=$((n+1))
  {on_prompt}
  printf '%s\n' "{{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{{\"stopReason\":\"end_turn\"}}}}"
done
"#
        )
    }

    struct TestHost {
        ptys: PtyManager,
        sent: Mutex<Vec<String>>,
        kept: Mutex<Vec<Option<SavedLoop>>>,
    }

    impl Host for TestHost {
        fn ptys(&self) -> &PtyManager {
            &self.ptys
        }
        fn deliver(&self, pane: &str, text: &str) -> Result<()> {
            self.sent.lock().push(text.to_string());
            self.ptys.submit(pane, text)
        }
        fn changed(&self, _pane: &str) {}
        fn keep(&self, _pane: &str, kept: Option<SavedLoop>) {
            self.kept.lock().push(kept);
        }
    }

    /// A repository, an agent working in it that has finished its first
    /// turn, and the loop machinery around them.
    fn world(on_prompt: &str) -> (tauri::App<tauri::test::MockRuntime>, Arc<TestHost>, String, PathBuf) {
        let dir = std::env::temp_dir().join(format!("vl-loop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| crate::git::run_for_tests(&dir, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), "web\n").unwrap();
        git(&["add", "README"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "one"]);

        let app = tauri::test::mock_app();
        let host = Arc::new(TestHost { ptys: PtyManager::default(), sent: Mutex::new(Vec::new()), kept: Mutex::new(Vec::new()) });
        let opts = SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd: dir.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: "Fake".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), fake_agent(on_prompt)],
            agent_id: Some("fake".into()),
            rows: None,
            cols: None,
            initial_input: None,
            prompted: true,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        };
        let launch = crate::acp::Launch { mcp: None, pick: crate::acp::Pick::New, prompt: Some("start".into()), on_session: None };
        let pane = host.ptys.spawn_acp(app.handle(), opts, launch).unwrap().id;
        let deadline = Instant::now() + Duration::from_secs(10);
        while host.ptys.turn(&pane).unwrap().mid || *host.ptys.turns(&pane).unwrap().borrow() == 0 {
            assert!(Instant::now() < deadline, "the agent never ended its first turn");
            std::thread::sleep(Duration::from_millis(20));
        }
        (app, host, pane, dir)
    }

    fn checks(dir: &std::path::Path, command: &str) -> Vec<Target> {
        vec![Target { repo: "web".into(), dir: dir.to_path_buf(), command: command.into() }]
    }

    fn until_ended(loops: &Loops, pane: &str) -> LoopView {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let view = loops.view(pane).unwrap();
            if view.phase.ended() {
                return view;
            }
            assert!(Instant::now() < deadline, "the loop never ended: {view:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_loop_sends_the_failures_back_and_ends_when_the_agent_has_fixed_them() {
        // The agent fixes it when it is told what failed, its second prompt.
        let (_app, host, pane, dir) = world(r#"if [ $n -ge 2 ]; then touch fixed; fi"#);
        let loops = Loops::default();
        loops.start(host.clone(), &pane, checks(&dir, "test -f fixed || { echo 'fixed is missing'; exit 1; }"), 5, None).unwrap();
        let view = until_ended(&loops, &pane);

        assert_eq!(view.phase, Phase::Passed, "{view:?}");
        assert_eq!(view.round, 1);
        assert_eq!(view.runs.len(), 2);
        assert!(view.runs[0].results[0].passed);
        assert!(!view.runs[1].results[0].passed);
        let sent = host.sent.lock().clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("(round 1 of 5)") && sent[0].contains("fixed is missing"), "{}", sent[0]);
        // Handed back, a finished turn is yours again, and says how it went.
        let info = host.ptys.info(&pane).unwrap();
        assert_eq!(info.activity, Activity::Done);
        assert_eq!(info.loop_said.as_deref(), Some("passed its checks — your turn"));
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_gives_up_when_its_rounds_are_used() {
        // Busy, never right.
        let (_app, host, pane, dir) = world(r#"echo "try $n" >> attempts"#);
        let loops = Loops::default();
        loops.start(host.clone(), &pane, checks(&dir, "exit 1"), 2, None).unwrap();
        let view = until_ended(&loops, &pane);
        assert_eq!(view.phase, Phase::GaveUp, "{view:?}");
        assert_eq!(view.runs.len(), 3);
        assert_eq!(host.sent.lock().len(), 2);
        assert!(host.ptys.info(&pane).unwrap().loop_said.unwrap().contains("after 2 rounds"));
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_turn_that_changes_nothing_waits_for_you_and_runs_nothing_again() {
        let (_app, host, pane, dir) = world(":");
        let loops = Loops::default();
        loops.start(host.clone(), &pane, checks(&dir, "exit 1"), 5, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while loops.view(&pane).unwrap().phase != Phase::Held {
            assert!(Instant::now() < deadline, "never held: {:?}", loops.view(&pane));
            std::thread::sleep(Duration::from_millis(20));
        }
        let view = loops.view(&pane).unwrap();
        assert_eq!(view.runs.len(), 1, "the same failures are not run again");
        assert_eq!(host.sent.lock().len(), 1);
        assert_eq!(host.ptys.info(&pane).unwrap().activity, Activity::Done, "it is your turn");

        loops.stop(&pane);
        let view = until_ended(&loops, &pane);
        assert_eq!(view.phase, Phase::Stopped);
        assert!(host.ptys.info(&pane).unwrap().loop_said.is_none(), "a loop you stopped has nothing to tell you");
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole loop against a real agent over ACP, which this machine must
    /// have and be signed in to: its first turn leaves the check failing,
    /// the loop sends it the failure, and it fixes it. The agent is put in
    /// its mode that works without asking, as a user would before leaving
    /// it on a loop. Not run by default, and run with a clean environment,
    /// or the agent inherits the Claude Code session it was started from:
    /// `env -i HOME="$HOME" PATH="$PATH" USER="$USER" VILLAIN_ACP_AGENT=claude-agent-acp cargo test --lib real_agent -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_real_agent_on_a_loop_fixes_what_its_check_reports() {
        let Ok(command) = std::env::var("VILLAIN_ACP_AGENT") else {
            panic!("set VILLAIN_ACP_AGENT to the agent's ACP command, e.g. \"claude-agent-acp\"");
        };
        let mut words = command.split_whitespace();
        let program = crate::shellenv::which(words.next().unwrap()).expect("the agent is not on the PATH");
        let dir = std::env::temp_dir().join(format!("vl-loop-real-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| crate::git::run_for_tests(&dir, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), "A folder for one answer.\n").unwrap();
        git(&["add", "README"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "one"]);

        let app = tauri::test::mock_app();
        let host = Arc::new(TestHost { ptys: PtyManager::default(), sent: Mutex::new(Vec::new()), kept: Mutex::new(Vec::new()) });
        let opts = SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd: dir.to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: "Real".into(),
            program,
            args: words.map(str::to_string).collect(),
            agent_id: None,
            rows: None,
            cols: None,
            initial_input: None,
            prompted: true,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        };
        let launch = crate::acp::Launch { mcp: None, pick: crate::acp::Pick::New, prompt: None, on_session: None };
        let pane = host.ptys.spawn_acp(app.handle(), opts, launch).unwrap().id;
        let wait = |what: &str, secs: u64, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while !done() {
                let text = host.ptys.transcript(&pane, 80).unwrap_or_default();
                assert!(Instant::now() < deadline, "{what} timed out:\n{text}");
                assert_ne!(host.ptys.info(&pane).map(|i| i.activity).ok(), Some(Activity::Asking), "{what}: the agent asked for permission:\n{text}");
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        wait("opening the conversation", 60, &|| host.ptys.acp_view(&pane, None).is_ok_and(|(v, _)| v.ready));
        let (view, _) = host.ptys.acp_view(&pane, None).unwrap();
        println!("settings: {:?}", view.settings.iter().map(|s| (&s.id, &s.current, s.options.iter().map(|o| &o.value).collect::<Vec<_>>())).collect::<Vec<_>>());
        let mode = view.settings.iter().find(|s| s.category.as_deref() == Some("mode")).expect("the agent offers no mode");
        // Claude Code's `auto` judges each action itself, and runs a harmless
        // `ls` without asking, which `acceptEdits` does not.
        let edits = ["auto", "acceptedits"]
            .iter()
            .find_map(|want| mode.options.iter().find(|o| o.value.to_lowercase() == *want))
            .expect("no mode that works without asking");
        host.ptys.acp_set(&pane, &mode.id, &edits.value).unwrap();
        wait("changing the mode", 30, &|| {
            host.ptys.acp_view(&pane, None).is_ok_and(|(v, _)| v.settings.iter().any(|s| s.id == mode.id && s.current == edits.value))
        });

        host.ptys.submit(&pane, "Reply with the one word: ready.").unwrap();
        wait("the first turn", 180, &|| *host.ptys.turns(&pane).unwrap().borrow() >= 1 && !host.ptys.turn(&pane).unwrap().mid);

        let loops = Loops::default();
        let check = "test \"$(cat answer.txt 2>/dev/null)\" = 42 || { echo 'answer.txt must hold exactly the number 42, and nothing else.'; exit 1; }";
        loops.start(host.clone(), &pane, checks(&dir, check), 3, None).unwrap();
        wait("the loop", 300, &|| loops.view(&pane).is_some_and(|v| v.phase.ended() || v.phase == Phase::Held));
        let view = loops.view(&pane).unwrap();
        println!("---- transcript\n{}\n---- loop\n{view:#?}", host.ptys.transcript(&pane, 200).unwrap());
        assert_eq!(view.phase, Phase::Passed, "{view:?}");
        assert_eq!(std::fs::read_to_string(dir.join("answer.txt")).unwrap().trim(), "42");
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_a_check_writes_itself_is_not_taken_for_the_agents_work() {
        let (_app, host, pane, dir) = world(":");
        let loops = Loops::default();
        // A report of its own, written on every run and never ignored.
        loops.start(host.clone(), &pane, checks(&dir, "date +%s%N >> report.txt; exit 1"), 5, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while loops.view(&pane).unwrap().phase != Phase::Held {
            assert!(Instant::now() < deadline, "never held: {:?}", loops.view(&pane));
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(loops.view(&pane).unwrap().runs.len(), 1);
        assert_eq!(host.sent.lock().len(), 1);
        loops.stop(&pane);
        until_ended(&loops, &pane);
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_is_kept_with_its_pane_as_it_goes_and_dropped_when_it_ends() {
        let (_app, host, pane, dir) = world(r#"if [ $n -ge 2 ]; then touch fixed; fi"#);
        let loops = Loops::default();
        loops.start(host.clone(), &pane, checks(&dir, "test -f fixed"), 5, None).unwrap();
        until_ended(&loops, &pane);
        // The driver drops it last, a moment after the view says passed.
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.kept.lock().last().is_some_and(|k| k.is_some()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let kept = host.kept.lock().clone();
        assert_eq!(kept.first(), Some(&Some(SavedLoop { rounds: 5, round: 0, failed_at: None, held: false })));
        assert!(
            kept.iter().flatten().any(|k| k.round == 1 && k.failed_at.is_some() && !k.held),
            "a round is kept as it is sent: {kept:?}"
        );
        assert_eq!(kept.last(), Some(&None), "passed, it is not put back");
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quitting_keeps_a_loop_for_the_next_launch() {
        let (_app, host, pane, dir) = world(":");
        let loops = Loops::default();
        loops.start(host.clone(), &pane, checks(&dir, "sleep 30"), 5, None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while loops.view(&pane).unwrap().phase != Phase::Checking {
            assert!(Instant::now() < deadline, "never checked");
            std::thread::sleep(Duration::from_millis(20));
        }
        loops.stop_all();
        loops.kill_checks();
        std::thread::sleep(Duration::from_millis(1500));
        let kept = host.kept.lock().clone();
        assert!(!kept.is_empty() && kept.iter().all(|k| k.is_some()), "quitting dropped it: {kept:?}");
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_put_back_after_a_restart_checks_at_once_and_counts_on_from_its_rounds() {
        let (_app, host, pane, dir) = world(r#"if [ $n -ge 2 ]; then touch fixed; fi"#);
        let loops = Loops::default();
        let from = SavedLoop { rounds: 5, round: 2, failed_at: Some("before the restart".into()), held: false };
        loops.start(host.clone(), &pane, checks(&dir, "test -f fixed"), 5, Some(from)).unwrap();
        let view = until_ended(&loops, &pane);
        assert_eq!(view.phase, Phase::Passed, "{view:?}");
        assert_eq!(view.round, 3);
        let sent = host.sent.lock().clone();
        assert!(sent[0].contains("(round 3 of 5)"), "{}", sent[0]);
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_that_waited_on_you_waits_again_after_a_restart() {
        let (_app, host, pane, dir) = world(":");
        let loops = Loops::default();
        let now = crate::git::fingerprint(&dir).unwrap();
        let from = SavedLoop { rounds: 5, round: 1, failed_at: Some(now), held: true };
        loops.start(host.clone(), &pane, checks(&dir, "exit 1"), 5, Some(from)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while loops.view(&pane).unwrap().phase != Phase::Held {
            assert!(Instant::now() < deadline, "never held: {:?}", loops.view(&pane));
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(500));
        let view = loops.view(&pane).unwrap();
        assert_eq!(view.phase, Phase::Held);
        assert!(view.runs.is_empty(), "nothing is run before a turn that changes something");
        assert!(host.sent.lock().is_empty());
        loops.stop(&pane);
        until_ended(&loops, &pane);
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_needs_something_to_check_and_only_one_runs_per_agent() {
        let (_app, host, pane, dir) = world(":");
        let loops = Loops::default();
        assert!(loops.start(host.clone(), &pane, Vec::new(), 5, None).is_err());
        loops.start(host.clone(), &pane, checks(&dir, "sleep 5"), 5, None).unwrap();
        assert!(loops.start(host.clone(), &pane, checks(&dir, "true"), 5, None).is_err());
        loops.stop(&pane);
        assert_eq!(until_ended(&loops, &pane).phase, Phase::Stopped);
        // Ended, it can start again.
        loops.start(host.clone(), &pane, checks(&dir, "true"), 5, None).unwrap();
        assert_eq!(until_ended(&loops, &pane).phase, Phase::Passed);
        host.ptys.close(&pane).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
