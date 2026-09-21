//! PTY-backed panes. Each pane is a real terminal: an agent CLI or a shell,
//! rooted in a workspace's worktree. Output is streamed to the frontend as
//! base64 so multi-byte sequences never get split by a chunk boundary.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::error::{Error, Result};
use crate::shellenv;

/// Roughly one screenful of history per pane, replayed when React remounts it.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// How long to hold PTY bytes before shipping them to the webview.
///
/// Agent TUIs redraw constantly. Emitting every read as its own event floods
/// the UI thread — and while the window sits in the background those events
/// queue until focus returns, which is why the app feels dead for a second
/// after being idle. One frame of delay is invisible; the catch-up is not.
const OUTPUT_COALESCE: Duration = Duration::from_millis(33);

/// Phrases a CLI prints when it is waiting on the user rather than working.
///
/// Every task is a brand-new directory, so the trust question is not a rare
/// edge case — it is the first thing an agent asks on every new task, and an
/// unanswered one looks exactly like an agent that has silently done nothing.
const TRUST_MARKERS: &[&str] = &[
    "do you trust the files in this folder",
    "trust the files in this directory",
    "do you trust this folder",
];

/// Phrases the agent CLIs print when they will not do any more work.
///
/// Best-effort and deliberately specific: a false positive only mislabels a
/// pane, but matching a bare "rate limit" would fire whenever an agent read
/// code about rate limiting.
const LIMIT_MARKERS: &[&str] = &[
    "usage limit",
    "rate limit reached",
    "rate limit exceeded",
    "quota exceeded",
    "resource_exhausted",
    "resource exhausted",
    "insufficient_quota",
    "out of credits",
    "credit balance is too low",
    "upgrade to continue",
];

/// Drop ANSI escapes so terminal output can be read as text.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters then a final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or ST
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Terminal history as readable text: a TUI redraws constantly, so identical
/// consecutive lines and blank runs are collapsed before taking the tail.
pub fn readable_tail(raw: &str, max_lines: usize) -> String {
    let plain = strip_ansi(raw);
    let mut lines: Vec<&str> = Vec::new();

    for line in plain.lines() {
        let line = line.trim_end();
        let blank = line.trim().is_empty();
        if blank && lines.last().is_some_and(|l: &&str| l.trim().is_empty()) {
            continue;
        }
        // A TUI repaints the same rows continually; one copy is enough.
        if lines.last() == Some(&line) {
            continue;
        }
        lines.push(line);
    }

    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n").trim().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneKind {
    Agent,
    Shell,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaneInfo {
    pub id: String,
    pub task_id: String,
    /// The repo this pane is rooted in; None means the task root, where every
    /// repo is visible as a sibling directory.
    pub checkout_id: Option<String>,
    pub kind: PaneKind,
    pub title: String,
    /// Agent id ("claude", "gemini", ...) when kind == Agent.
    pub agent_id: Option<String>,
    pub cwd: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub last_output_at: DateTime<Utc>,
    /// Something is waiting on the user: "usage_limit" or "trust_prompt".
    /// Read from the agent's own output, so best-effort.
    pub notice: Option<String>,
}

struct PaneMeta {
    info: PaneInfo,
}

struct Pane {
    meta: Mutex<PaneMeta>,
    /// The child's pid, which is also its process-group id: portable-pty calls
    /// setsid() so the agent leads its own session. Signalling the group
    /// reaches anything the agent spawned as well.
    pid: Option<u32>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    scrollback: Mutex<Vec<u8>>,
    /// Bytes waiting to cross to the webview, coalesced so a TUI redraw is
    /// one event instead of dozens.
    pending: Mutex<Vec<u8>>,
    flush_scheduled: AtomicBool,
    /// Shared with the manager: when false, output stays in scrollback only.
    ui_awake: Arc<AtomicBool>,
    /// Last time we scanned scrollback for trust/limit phrases. Throttled while
    /// the UI sleeps so a background agent is not a constant UTF-8 walk.
    notice_scan_at: Mutex<Instant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnOptions {
    pub task_id: String,
    #[serde(default)]
    pub checkout_id: Option<String>,
    pub cwd: String,
    pub kind: PaneKind,
    pub title: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub rows: Option<u16>,
    #[serde(default)]
    pub cols: Option<u16>,
    /// Typed into the pane once it is up, for agents that take no prompt flag.
    #[serde(default)]
    pub initial_input: Option<String>,
}

/// The most panes that may exist at once.
///
/// A backstop, not a feature. Every pane is a real process — usually an agent
/// CLI, which is not a cheap one — and every way of opening one, from a click
/// to a restore to a tool an agent calls on the app's own MCP server, comes
/// through here. A bug upstream of this that asks for hundreds gets an error
/// instead of the machine.
pub(crate) const MAX_PANES: usize = 32;

pub struct PtyManager {
    panes: Mutex<HashMap<String, Arc<Pane>>>,
    /// False while the window is in the background. Agents keep running and
    /// scrollback keeps filling; the webview is not fed until focus returns.
    ui_awake: Arc<AtomicBool>,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            panes: Mutex::new(HashMap::new()),
            ui_awake: Arc::new(AtomicBool::new(true)),
        }
    }
}

#[derive(Serialize, Clone)]
struct OutputEvent<'a> {
    pane_id: &'a str,
    data: String,
}

#[derive(Serialize, Clone)]
struct NoticeEvent<'a> {
    pane_id: &'a str,
    notice: Option<&'a str>,
}

#[derive(Serialize, Clone)]
struct ExitEvent<'a> {
    pane_id: &'a str,
    code: Option<i32>,
}

impl PtyManager {
    /// Tell the PTY layer whether anyone is looking.
    ///
    /// Agents never stop. What stops is shipping their redraws across to a
    /// webview that is not on screen — those events queue up and make coming
    /// back feel like a hitch. Scrollback still records everything; the UI
    /// replays it on focus.
    pub fn set_ui_awake(&self, awake: bool) {
        self.ui_awake.store(awake, Ordering::Release);
        if !awake {
            for pane in self.panes.lock().values() {
                pane.pending.lock().clear();
            }
        }
    }

    /// Queue a UI flush if one is not already waiting.
    ///
    /// The reader keeps appending under `pending`; this just starts the timer
    /// that will drain it. Doing the drain on a short sleep means a burst of
    /// TUI redraws becomes one event instead of one per read.
    fn schedule_flush(app: &AppHandle, pane: &Arc<Pane>, id: &str) {
        if !pane.ui_awake.load(Ordering::Acquire) {
            return;
        }
        if pane
            .flush_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let app = app.clone();
        let pane = pane.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(OUTPUT_COALESCE);
                if !pane.ui_awake.load(Ordering::Acquire) {
                    pane.pending.lock().clear();
                    pane.flush_scheduled.store(false, Ordering::Release);
                    break;
                }
                let batch = {
                    let mut pending = pane.pending.lock();
                    std::mem::take(&mut *pending)
                };
                if batch.is_empty() {
                    pane.flush_scheduled.store(false, Ordering::Release);
                    // A write may have landed between the take and the clear.
                    // Re-arm rather than drop those bytes until the next read.
                    if !pane.pending.lock().is_empty()
                        && pane
                            .flush_scheduled
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        continue;
                    }
                    break;
                }
                let data = base64::engine::general_purpose::STANDARD.encode(&batch);
                let _ = app.emit(
                    "pty:output",
                    OutputEvent {
                        pane_id: &id,
                        data,
                    },
                );
            }
        });
    }

    fn flush_pending(app: &AppHandle, pane: &Pane, id: &str) {
        let batch = {
            let mut pending = pane.pending.lock();
            std::mem::take(&mut *pending)
        };
        if batch.is_empty() {
            return;
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&batch);
        let _ = app.emit(
            "pty:output",
            OutputEvent {
                pane_id: id,
                data,
            },
        );
    }

    pub fn spawn(&self, app: &AppHandle, opts: SpawnOptions) -> Result<PaneInfo> {
        // Checked before anything is allocated, so refusing costs nothing.
        let live = self.panes.lock().len();
        if live >= MAX_PANES {
            return Err(Error::Pty(format!(
                "{live} terminals are already open, which is the limit. Close some \
                 before starting another — and if you did not open this many, \
                 something is starting them on its own."
            )));
        }

        let system = portable_pty::native_pty_system();
        let size = PtySize {
            rows: opts.rows.unwrap_or(30),
            cols: opts.cols.unwrap_or(100),
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = system
            .openpty(size)
            .map_err(|e| Error::Pty(format!("openpty: {e}")))?;

        let mut cmd = CommandBuilder::new(&opts.program);
        for arg in &opts.args {
            cmd.arg(arg);
        }
        cmd.cwd(&opts.cwd);
        // Start from the login-shell env, not the GUI process env. CommandBuilder
        // seeds itself from our process — which often carries FORCE_COLOR=0 /
        // TERM=dumb from a non-TTY parent — and `env()` only overlays, it never
        // removes. Clearing first is what lets shellenv's scrub stick.
        cmd.env_clear();
        for (k, v) in shellenv::user_env() {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Pty(format!("spawn {}: {e}", opts.program)))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Pty(format!("clone reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Pty(format!("take writer: {e}")))?;
        let killer = child.clone_killer();

        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let info = PaneInfo {
            id: id.clone(),
            task_id: opts.task_id.clone(),
            checkout_id: opts.checkout_id.clone(),
            kind: opts.kind,
            title: opts.title.clone(),
            agent_id: opts.agent_id.clone(),
            cwd: opts.cwd.clone(),
            running: true,
            exit_code: None,
            started_at: now,
            last_output_at: now,
            notice: None,
        };

        let pid = child.process_id();
        let pane = Arc::new(Pane {
            meta: Mutex::new(PaneMeta { info: info.clone() }),
            pid,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            scrollback: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            flush_scheduled: AtomicBool::new(false),
            ui_awake: self.ui_awake.clone(),
            notice_scan_at: Mutex::new(Instant::now()),
        });

        self.panes.lock().insert(id.clone(), pane.clone());

        // Reader: pump PTY output to the frontend until EOF.
        {
            let app = app.clone();
            let pane = pane.clone();
            let id = id.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            {
                                let mut sb = pane.scrollback.lock();
                                sb.extend_from_slice(chunk);
                                if sb.len() > SCROLLBACK_LIMIT {
                                    let drop_to = sb.len() - SCROLLBACK_LIMIT;
                                    sb.drain(..drop_to);
                                }
                            }
                            {
                                let mut meta = pane.meta.lock();
                                meta.info.last_output_at = Utc::now();
                                // Check the tail rather than this chunk: the
                                // phrase can straddle a read boundary. While
                                // the window is asleep this is throttled — a
                                // TUI redrawing 60 times a second should not
                                // mean 60 UTF-8 walks of the scrollback.
                                let awake = pane.ui_awake.load(Ordering::Acquire);
                                let scan = awake || {
                                    let mut at = pane.notice_scan_at.lock();
                                    if at.elapsed() >= Duration::from_millis(750) {
                                        *at = Instant::now();
                                        true
                                    } else {
                                        false
                                    }
                                };
                                if scan {
                                    let sb = pane.scrollback.lock();
                                    let tail = String::from_utf8_lossy(
                                        &sb[sb.len().saturating_sub(4096)..],
                                    )
                                    .to_lowercase();

                                    let found = if LIMIT_MARKERS.iter().any(|m| tail.contains(m)) {
                                        Some("usage_limit")
                                    } else if TRUST_MARKERS.iter().any(|m| tail.contains(m)) {
                                        Some("trust_prompt")
                                    } else {
                                        None
                                    };

                                    // A trust prompt clears once answered, so
                                    // let it come and go; a usage limit sticks.
                                    if meta.info.notice.as_deref() != found
                                        && meta.info.notice.as_deref() != Some("usage_limit")
                                    {
                                        meta.info.notice = found.map(str::to_string);
                                        if found.is_some() {
                                            let _ = app.emit(
                                                "pty:notice",
                                                NoticeEvent { pane_id: &id, notice: found },
                                            );
                                        }
                                    }
                                }
                            }
                            // Scrollback always records. The webview only gets
                            // a feed while someone is looking — otherwise the
                            // IPC queue builds up until focus returns.
                            if pane.ui_awake.load(Ordering::Acquire) {
                                pane.pending.lock().extend_from_slice(chunk);
                                Self::schedule_flush(&app, &pane, &id);
                            }
                        }
                    }
                }
                // Drain anything still held so the last paint is not lost.
                Self::flush_pending(&app, &pane, &id);
            });
        }

        // Waiter: record the exit status and tell the UI.
        {
            let app = app.clone();
            let pane = pane.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                let code = child.wait().ok().map(|s| s.exit_code() as i32);
                {
                    let mut meta = pane.meta.lock();
                    meta.info.running = false;
                    meta.info.exit_code = code;
                }
                let _ = app.emit("pty:exit", ExitEvent { pane_id: &id, code });
            });
        }

        if let Some(input) = opts.initial_input {
            let pane = pane.clone();
            std::thread::spawn(move || {
                // Give the agent's TUI a moment to draw its prompt first.
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let mut w = pane.writer.lock();
                let _ = w.write_all(input.as_bytes());
                let _ = w.write_all(b"\r");
                let _ = w.flush();
            });
        }

        Ok(info)
    }

    fn get(&self, id: &str) -> Result<Arc<Pane>> {
        self.panes
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("pane {id}")))
    }

    pub fn write(&self, id: &str, data: &str) -> Result<()> {
        let pane = self.get(id)?;
        let mut w = pane.writer.lock();
        w.write_all(data.as_bytes())
            .map_err(|e| Error::Pty(format!("write: {e}")))?;
        w.flush().map_err(|e| Error::Pty(format!("flush: {e}")))?;
        Ok(())
    }

    /// Type `text` into a pane and press Enter.
    ///
    /// Agent TUIs (Claude Code in particular) often leave the line sitting in
    /// the prompt when the characters and Enter arrive in one burst — the text
    /// shows up, but nothing is submitted until someone presses Enter again.
    /// A short gap between the two is enough for the TUI to accept the submit.
    pub fn submit(&self, id: &str, text: &str) -> Result<()> {
        let pane = self.get(id)?;
        let payload = text.to_string();
        std::thread::spawn(move || {
            {
                let mut w = pane.writer.lock();
                let _ = w.write_all(payload.as_bytes());
                let _ = w.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
            {
                let mut w = pane.writer.lock();
                let _ = w.write_all(b"\r");
                let _ = w.flush();
            }
        });
        Ok(())
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<()> {
        let pane = self.get(id)?;
        let master = pane.master.lock();
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| Error::Pty(format!("resize: {e}")))?;
        Ok(())
    }

    pub fn scrollback(&self, id: &str) -> Result<String> {
        let pane = self.get(id)?;
        let sb = pane.scrollback.lock();
        Ok(base64::engine::general_purpose::STANDARD.encode(sb.as_slice()))
    }

    /// Scrollback as readable text, for handing work to another agent.
    pub fn transcript(&self, id: &str, max_lines: usize) -> Result<String> {
        let pane = self.get(id)?;
        let raw = { String::from_utf8_lossy(&pane.scrollback.lock()).to_string() };
        Ok(readable_tail(&raw, max_lines))
    }

    pub fn info(&self, id: &str) -> Result<PaneInfo> {
        Ok(self.get(id)?.meta.lock().info.clone())
    }

    /// Ask the process group to stop, and only insist if it will not.
    ///
    /// This matters more than it looks: `ChildKiller::kill` is SIGKILL, which
    /// an agent cannot catch, so it dies without writing its transcript — and
    /// that transcript is the only thing that makes a session resumable later.
    fn request_stop(pane: &Pane) {
        if let Some(pid) = pane.pid {
            // Negative pid signals the whole group, catching subprocesses too.
            unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
        } else {
            let _ = pane.killer.lock().kill();
        }
    }

    fn wait_for_exit(pane: &Pane, deadline: std::time::Instant) -> bool {
        while std::time::Instant::now() < deadline {
            if !pane.meta.lock().info.running {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        !pane.meta.lock().info.running
    }

    fn stop(pane: &Pane, grace: std::time::Duration) {
        if !pane.meta.lock().info.running {
            return;
        }
        Self::request_stop(pane);
        if !Self::wait_for_exit(pane, std::time::Instant::now() + grace) {
            let _ = pane.killer.lock().kill();
        }
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        let pane = self.get(id)?;
        Self::stop(&pane, std::time::Duration::from_secs(5));
        Ok(())
    }

    pub fn close(&self, id: &str) -> Result<()> {
        // Take the pane out under the lock, then stop it outside it. Stopping
        // waits out the grace period, and an `if let` would hold the map for
        // all of it — freezing every other pane operation, including the poll
        // that keeps the window alive, on the thread the UI runs on.
        let pane = self.panes.lock().remove(id);
        if let Some(pane) = pane {
            Self::stop(&pane, std::time::Duration::from_secs(5));
        }
        Ok(())
    }

    /// Stop every pane on the way out, giving agents a chance to save.
    ///
    /// Signals them all first and waits once, so quitting takes the grace
    /// period rather than the grace period multiplied by the pane count.
    pub fn shutdown(&self, grace: std::time::Duration) {
        let panes: Vec<Arc<Pane>> = self.panes.lock().values().cloned().collect();
        if panes.is_empty() {
            return;
        }

        for pane in &panes {
            if pane.meta.lock().info.running {
                Self::request_stop(pane);
            }
        }

        let deadline = std::time::Instant::now() + grace;
        for pane in &panes {
            Self::wait_for_exit(pane, deadline);
        }
        for pane in &panes {
            if pane.meta.lock().info.running {
                let _ = pane.killer.lock().kill();
            }
        }
    }

    pub fn list(&self, task_id: Option<&str>) -> Vec<PaneInfo> {
        let mut out: Vec<PaneInfo> = self
            .panes
            .lock()
            .values()
            .map(|p| p.meta.lock().info.clone())
            .filter(|i| task_id.is_none_or(|t| i.task_id == t))
            .collect();
        out.sort_by_key(|i| i.started_at);
        out
    }

    /// Kill every pane belonging to a task, used when it is deleted.
    pub fn close_task(&self, task_id: &str) {
        self.close_matching(|i| i.task_id == task_id);
    }

    /// Kill every pane rooted in one checkout, used when a repo leaves a task.
    pub fn close_checkout(&self, checkout_id: &str) {
        self.close_matching(|i| i.checkout_id.as_deref() == Some(checkout_id));
    }

    fn close_matching(&self, pred: impl Fn(&PaneInfo) -> bool) {
        // Signal every match first and wait once — stopping them one by one
        // with a five-second grace each is how deleting a task with three
        // agents froze the UI for fifteen seconds.
        let panes: Vec<Arc<Pane>> = {
            let mut map = self.panes.lock();
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, p)| pred(&p.meta.lock().info))
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        };
        if panes.is_empty() {
            return;
        }
        for pane in &panes {
            if pane.meta.lock().info.running {
                Self::request_stop(pane);
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        for pane in &panes {
            Self::wait_for_exit(pane, deadline);
        }
        for pane in &panes {
            if pane.meta.lock().info.running {
                let _ = pane.killer.lock().kill();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_escapes_a_tui_emits() {
        let raw = "\u{1b}[32mgreen\u{1b}[0m plain\u{1b}[1;31mred\u{1b}[m";
        assert_eq!(strip_ansi(raw), "green plainred");

        // OSC title sequences end with BEL or ST.
        assert_eq!(strip_ansi("\u{1b}]0;a title\u{7}after"), "after");
        assert_eq!(strip_ansi("\u{1b}]0;t\u{1b}\\after"), "after");

        // Carriage returns are progress-bar redraw, not content.
        assert_eq!(strip_ansi("a\rb"), "ab");
    }

    #[test]
    fn recognises_the_trust_question_every_new_worktree_triggers() {
        let hit = |s: &str| TRUST_MARKERS.iter().any(|m| s.to_lowercase().contains(m));
        assert!(hit("Do you trust the files in this folder?"));
        assert!(hit("  Do you trust this folder?  "));
        // Not the usage-limit wording, which is handled separately.
        assert!(!hit("You've reached your usage limit"));
    }

    #[test]
    fn collapses_the_repaints_and_keeps_the_tail() {
        // A TUI rewrites the same row over and over.
        let raw = "thinking\nthinking\nthinking\n\n\n\ndone\nfinal";
        assert_eq!(readable_tail(raw, 10), "thinking\n\ndone\nfinal");

        let many: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let tail = readable_tail(&many, 3);
        assert_eq!(tail, "line 47\nline 48\nline 49");
    }

    #[test]
    fn recognises_limit_messages_without_firing_on_prose() {
        let hit = |s: &str| LIMIT_MARKERS.iter().any(|m| s.to_lowercase().contains(m));

        assert!(hit("You've reached your usage limit. Resets at 3pm."));
        assert!(hit("Error: quota exceeded for this model"));
        assert!(hit("RESOURCE_EXHAUSTED"));
        assert!(hit("Your credit balance is too low"));

        // Reading or writing code about rate limiting must not count.
        assert!(!hit("added a rate limiter to the gateway"));
        assert!(!hit("Do you trust the files in this folder?"));
        assert!(!hit("see docs/rate-limits.md for the policy"));
        assert!(!hit("fn check_quota(user: &User) -> bool"));
    }
}
