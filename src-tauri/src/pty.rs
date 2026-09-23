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
use tauri::{AppHandle, Emitter, Runtime};

use crate::error::{Error, Result};
use crate::shellenv;

/// Roughly one screenful of history per pane, replayed when a terminal is
/// first drawn or has fallen too far behind to catch up.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

/// How far past the limit scrollback may run before it is trimmed.
///
/// Trimming on every read once full meant moving 256KB to append a few bytes,
/// hundreds of times a second for an agent that is redrawing. Letting it
/// overshoot makes the move rare.
const SCROLLBACK_SLACK: usize = 64 * 1024;

/// How long to hold PTY bytes before shipping them to the webview.
///
/// Agent TUIs redraw constantly. Emitting every read as its own event floods
/// the UI thread — and while the window sits in the background those events
/// queue until focus returns, which is why the app feels dead for a second
/// after being idle. One frame of delay is invisible; the catch-up is not.
const OUTPUT_COALESCE: Duration = Duration::from_millis(33);

/// The wait before the first event after a quiet spell.
///
/// That event is usually the echo of a key. Holding it for a whole
/// `OUTPUT_COALESCE` put 33ms between every keypress and its character, which
/// is what made typing into a pane feel like typing over a network. A TUI
/// frame arrives as several back-to-back reads, and this is still long enough
/// for them to leave together rather than drawing the top half first.
const OUTPUT_GATHER: Duration = Duration::from_millis(4);

/// How much of the end of the output is searched for trust and limit phrases.
const NOTICE_TAIL: usize = 4096;

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
/// Best-effort and deliberately specific: matching a bare "rate limit" would
/// fire whenever an agent read code about rate limiting. A bare "usage limit"
/// was the same mistake — the prompt "add a usage limit to the API" is echoed
/// as it is typed — and this notice never clears, so it flagged the pane for
/// good and offered to hand off an agent that was fine.
const LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "reached your usage limit",
    "hit your usage limit",
    "usage limit exceeded",
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

/// Which notice, if any, the end of a pane's output is showing.
///
/// Runs on every read, so it avoids the obvious version — a lossy UTF-8
/// decode and a Unicode lowercase of 4KB, two allocations a read. The phrases
/// are all ASCII, so folding ASCII and blanking everything else keeps every
/// match and lets `str::contains` do the searching. `scratch` is reused by
/// the caller between reads.
fn notice_in(tail: &[u8], scratch: &mut Vec<u8>) -> Option<&'static str> {
    scratch.clear();
    scratch.extend(tail.iter().map(|b| if b.is_ascii() { b.to_ascii_lowercase() } else { b' ' }));
    let text = std::str::from_utf8(scratch).ok()?;
    if LIMIT_MARKERS.iter().any(|m| text.contains(m)) {
        Some("usage_limit")
    } else if TRUST_MARKERS.iter().any(|m| text.contains(m)) {
        Some("trust_prompt")
    } else {
        None
    }
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

/// What a pane has printed, and how much of it the webview has been sent.
///
/// Positions are counted from the pane's first byte, so the webview can say
/// exactly how much it already has. That is what lets a terminal coming back
/// on screen be sent only what it missed, instead of being wiped and
/// repainted from the whole scrollback — which is the flash, and the pause,
/// every time a task or a tab was switched.
struct Output {
    /// The most recent output, from `total - scrollback.len()` to `total`.
    scrollback: Vec<u8>,
    /// Every byte the process has printed.
    total: u64,
    /// Where the webview's feed has got to. Anything after this is waiting
    /// for the next flush.
    sent: u64,
}

impl Output {
    fn start(&self) -> u64 {
        self.total - self.scrollback.len() as u64
    }

    fn push(&mut self, chunk: &[u8]) {
        self.scrollback.extend_from_slice(chunk);
        self.total += chunk.len() as u64;
        if self.scrollback.len() > SCROLLBACK_LIMIT + SCROLLBACK_SLACK {
            let mut cut = self.scrollback.len() - SCROLLBACK_LIMIT;
            // Start the kept history at a line, so a replay does not open on
            // the back half of an escape sequence or of a UTF-8 character.
            let look = &self.scrollback[cut..(cut + 4096).min(self.scrollback.len())];
            if let Some(nl) = look.iter().position(|&b| b == b'\n') {
                cut += nl + 1;
            }
            self.scrollback.drain(..cut);
        }
    }

    /// Output since `from`, or all of it when `from` is no longer held.
    fn since(&self, from: Option<u64>) -> (&[u8], bool) {
        let start = self.start();
        match from {
            Some(f) if f >= start && f <= self.total => {
                (&self.scrollback[(f - start) as usize..], false)
            }
            // A terminal that has never been drawn has nothing to clear.
            // One that is too far behind has to start again.
            other => (&self.scrollback, other.is_some()),
        }
    }

    /// The output the feed has not carried yet, marking it carried.
    fn take_unsent(&mut self) -> Option<(Vec<u8>, u64)> {
        if self.sent >= self.total {
            return None;
        }
        // More than the scrollback holds went by between flushes. Send what
        // is left; the webview sees the gap and asks for a replay.
        let from = self.sent.max(self.start());
        let bytes = self.scrollback[(from - self.start()) as usize..].to_vec();
        self.sent = self.total;
        Some((bytes, self.total))
    }
}

struct Pane {
    meta: Mutex<PaneMeta>,
    /// The child's pid, which is also its process-group id: portable-pty calls
    /// setsid() so the agent leads its own session. Signalling the group
    /// reaches anything the agent spawned as well.
    pid: Option<u32>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Keystrokes on their way to the process, written by a thread of the
    /// pane's own.
    ///
    /// `pty_write` runs on the main thread, and a PTY's input queue is small:
    /// a paste into an agent that is busy and not reading filled it, and the
    /// write then sat on the main thread until the agent read — with the
    /// window frozen for all of it. A queue keeps the keys in order and the
    /// wait somewhere nobody is looking.
    input: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    output: Mutex<Output>,
    flush_scheduled: AtomicBool,
    /// When the last event went out, so a burst is coalesced and a lone key
    /// is not.
    last_flush: Mutex<Instant>,
    /// Whether a terminal is on screen for this pane.
    ///
    /// Output for panes nobody can see used to cross the bridge anyway, at
    /// thirty events a second per busy agent, only to be dropped on the far
    /// side. Now it waits in scrollback until the pane is shown and asks for
    /// what it missed. This also covers the window being in the background:
    /// the webview detaches everything then.
    watched: AtomicBool,
}

/// What a terminal needs to be current: the output after the point it asked
/// from, and whether it has to clear the screen first.
#[derive(Debug, Clone, Serialize)]
pub struct Catchup {
    /// Base64, like the live feed.
    pub data: String,
    /// The position `data` runs up to, to ask from next time.
    pub end: u64,
    /// The point asked from is no longer held, so `data` is the whole
    /// scrollback and has to be drawn on a clean terminal.
    pub reset: bool,
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

#[derive(Default)]
pub struct PtyManager {
    panes: Mutex<HashMap<String, Arc<Pane>>>,
    /// Spawns past the cap check and not yet in `panes`. Spawns run in
    /// parallel now — the blocking pool, the restore thread, MCP — and
    /// several checking the count at 31 before any of them had inserted all
    /// got through. Counted under the `panes` lock, so the check sees them.
    starting: std::sync::atomic::AtomicUsize,
}

/// A slot under `MAX_PANES`, held from the check until the pane is in the map
/// or the spawn has failed.
struct Reserved<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for Reserved<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Serialize, Clone)]
struct OutputEvent<'a> {
    pane_id: &'a str,
    data: String,
    /// The position just past `data`, so the webview can tell a repeat from
    /// a gap.
    end: u64,
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
    fn emit_output<R: Runtime>(app: &AppHandle<R>, id: &str, bytes: &[u8], end: u64) {
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let _ = app.emit("pty:output", OutputEvent { pane_id: id, data, end });
    }

    /// Queue a UI flush if one is not already waiting.
    ///
    /// The reader keeps appending to the output; this just starts the timer
    /// that will ship it. Doing that on a short sleep means a burst of TUI
    /// redraws becomes one event instead of one per read.
    fn schedule_flush<R: Runtime>(app: &AppHandle<R>, pane: &Arc<Pane>, id: &str) {
        if !pane.watched.load(Ordering::Acquire) {
            return;
        }
        if pane
            .flush_scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let quiet = pane.last_flush.lock().elapsed();
        let mut wait = if quiet >= OUTPUT_COALESCE {
            OUTPUT_GATHER
        } else {
            OUTPUT_COALESCE - quiet
        };
        let app = app.clone();
        let pane = pane.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(wait);
                wait = OUTPUT_COALESCE;
                // Hidden since this was scheduled: what is waiting stays in
                // scrollback, and showing the pane again collects it.
                if !pane.watched.load(Ordering::Acquire) {
                    pane.flush_scheduled.store(false, Ordering::Release);
                    // Unless it was shown again in between, and a read that
                    // landed then found this flag still up and left it to us.
                    let more = pane.watched.load(Ordering::Acquire) && {
                        let out = pane.output.lock();
                        out.sent < out.total
                    };
                    if more
                        && pane
                            .flush_scheduled
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        continue;
                    }
                    break;
                }
                let batch = pane.output.lock().take_unsent();
                let Some((bytes, end)) = batch else {
                    pane.flush_scheduled.store(false, Ordering::Release);
                    // A write may have landed between the take and the clear.
                    // Re-arm rather than drop those bytes until the next read.
                    let more = { let out = pane.output.lock(); out.sent < out.total };
                    if more
                        && pane
                            .flush_scheduled
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        continue;
                    }
                    break;
                };
                Self::emit_output(&app, &id, &bytes, end);
                *pane.last_flush.lock() = Instant::now();
            }
        });
    }

    fn flush_pending<R: Runtime>(app: &AppHandle<R>, pane: &Pane, id: &str) {
        if !pane.watched.load(Ordering::Acquire) {
            return;
        }
        let batch = pane.output.lock().take_unsent();
        if let Some((bytes, end)) = batch {
            Self::emit_output(app, id, &bytes, end);
        }
    }

    pub fn spawn<R: Runtime>(&self, app: &AppHandle<R>, opts: SpawnOptions) -> Result<PaneInfo> {
        // Checked before anything is allocated, so refusing costs nothing.
        let slot = {
            let panes = self.panes.lock();
            let live = panes.len() + self.starting.load(Ordering::Acquire);
            if live >= MAX_PANES {
                return Err(Error::Pty(format!(
                    "{live} terminals are already open, which is the limit. Close some \
                     before starting another — and if you did not open this many, \
                     something is starting them on its own."
                )));
            }
            self.starting.fetch_add(1, Ordering::AcqRel);
            Reserved(&self.starting)
        };

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
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| Error::Pty(format!("take writer: {e}")))?;
        let killer = child.clone_killer();

        let (input, keys) = std::sync::mpsc::channel::<Vec<u8>>();
        // Ends when the pane is dropped and the sender with it. A failed write
        // does not end it: dropping the writer sends the child EOF, which is
        // not something a write error on one keystroke should decide.
        std::thread::spawn(move || {
            for bytes in keys {
                let _ = writer.write_all(&bytes).and_then(|_| writer.flush());
            }
        });

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
            input,
            killer: Mutex::new(killer),
            output: Mutex::new(Output { scrollback: Vec::new(), total: 0, sent: 0 }),
            flush_scheduled: AtomicBool::new(false),
            last_flush: Mutex::new(Instant::now()),
            // Nothing is on screen until the webview attaches, and attaching
            // collects whatever was printed before it did.
            watched: AtomicBool::new(false),
        });

        self.panes.lock().insert(id.clone(), pane.clone());
        drop(slot);

        // Reader: pump PTY output to the frontend until EOF.
        {
            let app = app.clone();
            let pane = pane.clone();
            let id = id.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let mut scratch = Vec::with_capacity(NOTICE_TAIL);
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // Check the tail rather than this chunk: the
                            // phrase can straddle a read boundary. Every read,
                            // not a sample of them — a throttled scan skipped
                            // the last read before a quiet spell, and the
                            // trust question is exactly the output that is
                            // followed by one.
                            let found = {
                                let mut out = pane.output.lock();
                                out.push(&buf[..n]);
                                let tail = &out.scrollback
                                    [out.scrollback.len().saturating_sub(NOTICE_TAIL)..];
                                notice_in(tail, &mut scratch)
                            };
                            {
                                let mut meta = pane.meta.lock();
                                meta.info.last_output_at = Utc::now();
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
                            // Scrollback always records. The webview only gets
                            // a feed while the pane is on screen.
                            Self::schedule_flush(&app, &pane, &id);
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
            let keys = pane.input.clone();
            std::thread::spawn(move || {
                // Give the agent's TUI a moment to draw its prompt first.
                std::thread::sleep(std::time::Duration::from_millis(1200));
                let mut bytes = input.into_bytes();
                bytes.push(b'\r');
                let _ = keys.send(bytes);
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
        self.get(id)?
            .input
            .send(data.as_bytes().to_vec())
            .map_err(|_| Error::Pty("the terminal has closed".into()))
    }

    /// Type `text` into a pane and press Enter.
    ///
    /// Agent TUIs (Claude Code in particular) often leave the line sitting in
    /// the prompt when the characters and Enter arrive in one burst — the text
    /// shows up, but nothing is submitted until someone presses Enter again.
    /// A short gap between the two is enough for the TUI to accept the submit.
    pub fn submit(&self, id: &str, text: &str) -> Result<()> {
        let keys = self.get(id)?.input.clone();
        let payload = text.as_bytes().to_vec();
        std::thread::spawn(move || {
            if keys.send(payload).is_ok() {
                std::thread::sleep(std::time::Duration::from_millis(80));
                let _ = keys.send(b"\r".to_vec());
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

    /// A terminal has come on screen: send it what it is missing, and feed it
    /// from here on.
    ///
    /// Marked watched under the same lock that reads the catch-up, so every
    /// byte is in one or the other — at worst in both, which the webview
    /// discards by position.
    pub fn attach(&self, id: &str, since: Option<u64>) -> Result<Catchup> {
        let pane = self.get(id)?;
        let mut out = pane.output.lock();
        pane.watched.store(true, Ordering::Release);
        let (bytes, reset) = out.since(since);
        let catchup = Catchup {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            end: out.total,
            reset,
        };
        out.sent = out.total;
        Ok(catchup)
    }

    /// Nobody is looking at this pane any more; keep its output for later.
    pub fn detach(&self, id: &str) {
        if let Ok(pane) = self.get(id) {
            pane.watched.store(false, Ordering::Release);
        }
    }

    /// Scrollback as readable text, for handing work to another agent.
    pub fn transcript(&self, id: &str, max_lines: usize) -> Result<String> {
        let pane = self.get(id)?;
        let raw = { String::from_utf8_lossy(&pane.output.lock().scrollback).to_string() };
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
            // An interactive shell ignores SIGTERM, so a shell pane sat out
            // the whole grace period and was then killed outright — five
            // seconds on every ✕, and zsh never saved its history. Hangup is
            // what a shell expects when its terminal goes: it exits, and
            // passes the hangup on to the jobs it started.
            let signal = match pane.meta.lock().info.kind {
                PaneKind::Shell => libc::SIGHUP,
                PaneKind::Agent => libc::SIGTERM,
            };
            // Negative pid signals the whole group, catching subprocesses too.
            unsafe { libc::kill(-(pid as i32), signal) };
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

    /// Kill every pane working inside one checkout, used when a repo leaves a
    /// task and its worktree is about to be deleted.
    ///
    /// By where it runs as well as what it was started for: an agent resumed
    /// from the task root can be running inside the worktree with no checkout
    /// recorded, and deleting the folder out from under it is worse than
    /// stopping it.
    pub fn close_checkout(&self, checkout_id: &str, path: &str) {
        let dir = std::path::Path::new(path);
        self.close_matching(|i| {
            i.checkout_id.as_deref() == Some(checkout_id) || std::path::Path::new(&i.cwd).starts_with(dir)
        });
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
    fn a_notice_is_found_through_colour_and_unicode() {
        let mut scratch = Vec::new();
        let tui = "\u{1b}[1m╭─ Do you trust the files in this folder? ─╮\u{1b}[0m".as_bytes();
        assert_eq!(notice_in(tui, &mut scratch), Some("trust_prompt"));
        // A usage limit outranks a trust question still on screen.
        let both = b"Do you trust this folder?\n... You've reached your USAGE LIMIT";
        assert_eq!(notice_in(both, &mut scratch), Some("usage_limit"));
        // A tail cut through the middle of a character is still searchable.
        let cut = &"é usage limit reached".as_bytes()[1..];
        assert_eq!(notice_in(cut, &mut scratch), Some("usage_limit"));
        assert_eq!(notice_in(b"added a rate limiter", &mut scratch), None);
    }

    fn output() -> Output {
        Output { scrollback: Vec::new(), total: 0, sent: 0 }
    }

    #[test]
    fn a_terminal_that_kept_up_is_sent_only_what_it_missed() {
        let mut out = output();
        out.push(b"hello ");
        out.push(b"world");
        assert_eq!(out.since(Some(6)), (&b"world"[..], false));
        assert_eq!(out.since(Some(11)), (&b""[..], false));
        // Never drawn: everything, and nothing to clear.
        assert_eq!(out.since(None), (&b"hello world"[..], false));
    }

    #[test]
    fn trimming_keeps_positions_and_starts_on_a_line() {
        let mut out = output();
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();
        while out.total < (SCROLLBACK_LIMIT + SCROLLBACK_SLACK + 4096) as u64 {
            out.push(&line);
        }
        assert!(out.scrollback.len() <= SCROLLBACK_LIMIT + SCROLLBACK_SLACK);
        assert_eq!(out.start() + out.scrollback.len() as u64, out.total);
        // The kept history opens on a fresh line, not halfway through one.
        assert_eq!(out.scrollback[0], b'x');
        assert_eq!(out.start() % line.len() as u64, 0);

        // A point that has been trimmed away means starting again.
        let (all, reset) = out.since(Some(0));
        assert!(reset);
        assert_eq!(all.len(), out.scrollback.len());
        let (tail, reset) = out.since(Some(out.total - 3));
        assert!(!reset);
        assert_eq!(tail, b"xx\n");
    }

    #[test]
    fn the_feed_carries_each_byte_once() {
        let mut out = output();
        out.push(b"abc");
        assert_eq!(out.take_unsent(), Some((b"abc".to_vec(), 3)));
        assert_eq!(out.take_unsent(), None);
        out.push(b"de");
        assert_eq!(out.take_unsent(), Some((b"de".to_vec(), 5)));
    }

    /// The whole path through real processes: a pane is fed from its first
    /// byte, a terminal that comes back is sent only what it missed, keys
    /// arrive in order through the writer thread, and a shell stops on its
    /// hangup rather than sitting out the grace period.
    #[test]
    fn a_pane_catches_up_by_position_and_a_shell_stops_promptly() {
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let dir = std::env::temp_dir();
        let spawn = |program: &str, args: &[&str]| {
            ptys.spawn(
                app.handle(),
                SpawnOptions {
                    task_id: "t".into(),
                    checkout_id: None,
                    cwd: dir.to_string_lossy().to_string(),
                    kind: PaneKind::Shell,
                    title: "t".into(),
                    program: program.into(),
                    args: args.iter().map(|a| a.to_string()).collect(),
                    agent_id: None,
                    rows: None,
                    cols: None,
                    initial_input: None,
                },
            )
            .unwrap()
        };
        let text = |c: &Catchup| {
            String::from_utf8_lossy(
                &base64::engine::general_purpose::STANDARD.decode(&c.data).unwrap(),
            )
            .to_string()
        };
        let wait_for = |id: &str, since: Option<u64>, want: &str| -> Catchup {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let c = ptys.attach(id, since).unwrap();
                if text(&c).contains(want) || Instant::now() > deadline {
                    return c;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        let pane = spawn("/bin/sh", &["-c", "printf 'ready\\n'; read a; read b; printf \"got $a $b\\n\"; sleep 5"]);
        let first = wait_for(&pane.id, None, "ready");
        assert!(text(&first).contains("ready"));
        assert!(!first.reset, "a terminal that has drawn nothing has nothing to clear");

        // Two writes, in order, through the queue.
        ptys.write(&pane.id, "one\r").unwrap();
        ptys.write(&pane.id, "two\r").unwrap();
        let rest = wait_for(&pane.id, Some(first.end), "got one two");
        assert!(text(&rest).contains("got one two"), "{:?}", text(&rest));
        assert!(!text(&rest).contains("ready"), "sent again what the terminal already had");
        assert!(!rest.reset);
        assert!(rest.end > first.end);
        ptys.close(&pane.id).unwrap();

        // An interactive shell ignores SIGTERM; closing one used to take the
        // whole five-second grace and end in SIGKILL.
        let shell = spawn("/bin/sh", &["-i"]);
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        ptys.close(&shell.id).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "closing a shell took {:?}",
            started.elapsed()
        );
    }

    /// The cap holds when spawns race: forty at once from separate threads,
    /// and no more than `MAX_PANES` get a pane.
    #[test]
    fn the_pane_cap_holds_against_spawns_at_once() {
        let app = tauri::test::mock_app();
        let handle = app.handle().clone();
        let ptys = PtyManager::default();
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let ok = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..40 {
                scope.spawn(|| {
                    let spawned = ptys.spawn(
                        &handle,
                        SpawnOptions {
                            task_id: "t".into(),
                            checkout_id: None,
                            cwd: dir.clone(),
                            kind: PaneKind::Agent,
                            title: "t".into(),
                            program: "/bin/sleep".into(),
                            args: vec!["30".into()],
                            agent_id: None,
                            rows: None,
                            cols: None,
                            initial_input: None,
                        },
                    );
                    if spawned.is_ok() {
                        ok.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        let started = ok.load(Ordering::SeqCst);
        ptys.shutdown(Duration::from_secs(2));
        assert_eq!(started, MAX_PANES);
        assert_eq!(ptys.list(None).len(), MAX_PANES);
    }

    #[test]
    fn recognises_limit_messages_without_firing_on_prose() {
        let hit = |s: &str| LIMIT_MARKERS.iter().any(|m| s.to_lowercase().contains(m));

        assert!(hit("You've reached your usage limit. Resets at 3pm."));
        assert!(hit("Error: quota exceeded for this model"));
        assert!(hit("RESOURCE_EXHAUSTED"));
        assert!(hit("Your credit balance is too low"));

        assert!(hit("Claude usage limit reached. Your limit will reset at 5pm"));
        assert!(hit("You've hit your usage limit for GPT-5"));

        // Reading or writing code about rate limiting must not count.
        assert!(!hit("added a rate limiter to the gateway"));
        assert!(!hit("> add a usage limit to the orders API"));
        assert!(!hit("Do you trust the files in this folder?"));
        assert!(!hit("see docs/rate-limits.md for the policy"));
        assert!(!hit("fn check_quota(user: &User) -> bool"));
    }
}
