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

/// How long after a keystroke output still counts as its echo.
///
/// A shell echoes at once; a TUI renders the change on its own schedule, a
/// frame or two later. Past this, output is the program's own again.
const ECHO_WINDOW: Duration = Duration::from_millis(250);

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

/// How far back to look for the window title a CLI last set.
const TITLE_TAIL: usize = 2048;

/// The last whole window title set in `tail` (OSC 0 or 2), if any.
///
/// Read from the tail rather than the chunk just read: a title can straddle
/// two reads, and then neither chunk holds all of it.
fn last_title(tail: &[u8]) -> Option<String> {
    let mut found = None;
    let mut at = 0;
    while let Some(off) = tail[at..].windows(2).position(|w| w == b"\x1b]") {
        let start = at + off + 2;
        let body = &tail[start..];
        let Some(rest) = body.strip_prefix(b"0;").or_else(|| body.strip_prefix(b"2;")) else {
            at = start;
            continue;
        };
        // Ended by BEL or by ST; one with no end yet is still arriving.
        let Some(end) = rest.iter().position(|&b| b == 0x07 || b == 0x1b) else {
            break;
        };
        found = Some(String::from_utf8_lossy(&rest[..end]).to_string());
        at = start + 2 + end;
    }
    found
}

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
    /// What the agent is doing. Worked out when the pane is read, not stored:
    /// "done" turns into "idle" by being looked at.
    pub activity: Activity,
    /// Since when, as far as is known: its own report, or its last output
    /// that was not a repaint. `last_output_at` is neither — an idle Claude
    /// Code prints every few seconds.
    pub activity_since: DateTime<Utc>,
    /// What the conversation is about, in the CLI's own words: the name the
    /// UI gives the pane in place of the agent's (PANE-12). None until it
    /// says, and for a CLI that never does.
    pub topic: Option<String>,
}

/// What an agent is doing, as best the app can tell.
///
/// From the agent itself where it will say — Claude Code's hooks report a
/// prompt taken, a tool run, a permission asked, a turn finished. Otherwise
/// from its output, which is a guess: Claude Code repaints its prompt every
/// few seconds while doing nothing at all, so "printed recently" had an agent
/// two days idle marked as working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    Working,
    /// Stopped on something only you can answer: a permission, the trust
    /// question, a usage limit.
    Asking,
    /// Finished what it was asked, and nobody has looked since.
    Done,
    /// Nothing going on and nothing new.
    Idle,
}

/// Silence past this, from an agent that does not report itself, reads as
/// finished. Matches the label's wording in `store.ts`.
const IDLE_AFTER: chrono::TimeDelta = chrono::TimeDelta::seconds(45);

/// Output this soon after the app sent the pane something is taken as the
/// answer to it — the echo of a key, the repaint after a resize or a focus
/// change — and not as the agent getting on with work.
const ANSWER_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

struct PaneMeta {
    info: PaneInfo,
    /// Asked to stop — Stop, a handoff, quitting — so its exit is not news.
    stopping: bool,
    /// The agent's own last word on what it is doing, and when it said it.
    /// Once there is one, output no longer decides anything.
    reported: Option<(Activity, DateTime<Utc>)>,
    /// The last output that was not an answer to something sent.
    last_work: DateTime<Utc>,
    /// When the app last sent the pane anything: keys, a resize.
    last_input: Instant,
    /// Given something to do since it started — a prompt at launch, or a line
    /// typed and entered. A resumed agent that has only drawn its screen has
    /// not finished anything.
    prompted: bool,
    /// When the pane was last on screen.
    seen_at: DateTime<Utc>,
    /// The window title last read, for a CLI that reports or names its
    /// conversation through it.
    title: String,
    /// A notice the agent's own report showed to be over while its words are
    /// still on screen. Not raised again until they have scrolled away.
    cleared_notice: Option<String>,
    /// When the notice now showing was raised.
    notice_at: DateTime<Utc>,
}

impl PaneMeta {
    fn activity(&self, watched: bool, now: DateTime<Utc>) -> Activity {
        self.state(watched, now).0
    }

    fn state(&self, watched: bool, now: DateTime<Utc>) -> (Activity, DateTime<Utc>) {
        // Times that hold still. A shell's last output, or the last output
        // under a notice, moved with every read: the pane list never matched
        // the one before while a dev server printed, and every poll redrew
        // the app.
        let info = &self.info;
        if info.kind == PaneKind::Shell {
            return (Activity::Idle, info.started_at);
        }
        if !info.running {
            return (Activity::Idle, self.last_work);
        }
        if info.notice.is_some() {
            return (Activity::Asking, self.notice_at);
        }
        let (state, since) = match self.reported {
            Some(reported) => reported,
            None if now - self.last_work < IDLE_AFTER => return (Activity::Working, self.last_work),
            None if !self.prompted => return (Activity::Idle, self.last_work),
            None => (Activity::Done, self.last_work + IDLE_AFTER),
        };
        // Finished while on screen, or looked at since: nothing new.
        if state == Activity::Done && (watched || since <= self.seen_at) {
            return (Activity::Idle, since);
        }
        (state, since)
    }

    fn snapshot(&self, watched: bool) -> PaneInfo {
        let mut info = self.info.clone();
        (info.activity, info.activity_since) = self.state(watched, Utc::now());
        info
    }

    /// Take the agent's word for what it is doing. Finished before it was
    /// ever given anything — a CLI's title saying "ready" as it starts — is
    /// only idle.
    ///
    /// A report also ends a notice it contradicts. Both are read off the
    /// screen, and stay on it: `gh` printing "API rate limit exceeded" held an
    /// agent at "out of budget" for as long as the words were in the tail, and
    /// an answered trust question lingered the same way.
    fn take_report(&mut self, activity: Activity) {
        let activity = if activity == Activity::Done && !self.prompted {
            Activity::Idle
        } else {
            activity
        };
        self.reported = Some((activity, Utc::now()));
        let over = match self.info.notice.as_deref() {
            Some("usage_limit") => activity == Activity::Working,
            Some("trust_prompt") => activity != Activity::Idle,
            _ => false,
        };
        if over {
            self.cleared_notice = self.info.notice.take();
        }
    }

    /// Keys from the person at the terminal, which say something the agent's
    /// own reports leave out.
    fn typed(&mut self, data: &str) {
        self.sent(data);
        let interrupt = matches!(data, "\u{1b}" | "\u{3}");
        match self.reported {
            // Claude Code runs no hook when a turn is interrupted, so a turn
            // stopped with Esc or ^C would read as working for good. Esc at a
            // permission question refuses it and ends the turn the same way,
            // and read as asking for good.
            Some((Activity::Working | Activity::Asking, _)) if interrupt => {
                self.reported = Some((Activity::Idle, Utc::now()));
            }
            // Answered. Nothing reports the moment a permission is given — the
            // next hook is the tool finishing — so a long command, once
            // allowed, read as still asking until it was done. Escape
            // sequences are the terminal talking (focus, a mouse), not a reply.
            Some((Activity::Asking, _)) if !interrupt && !data.starts_with('\u{1b}') => {
                self.reported = Some((Activity::Working, Utc::now()));
            }
            _ => {}
        }
    }

    fn sent(&mut self, data: &str) {
        self.last_input = Instant::now();
        if data.contains('\r') || data.contains('\n') {
            self.prompted = true;
        }
    }
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
    /// When a key was last sent that has not had its echo shipped yet.
    typed: Mutex<Option<Instant>>,
    /// Set when output arrives in answer to a key, to cut the flush thread's
    /// wait short.
    ///
    /// The first event after a quiet spell already went out after
    /// `OUTPUT_GATHER`, but a key pressed while the pane was busy — an agent's
    /// spinner going, a watcher printing — waited out the rest of the frame
    /// behind it: 20ms typical, 35ms often, on every key, in exactly the panes
    /// people type into most. Measured by the `echo_latency` test.
    urgent: Mutex<bool>,
    wake: parking_lot::Condvar,
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
    /// Coming on screen changed what the pane reads as: a finished agent,
    /// now seen, is no longer news.
    #[serde(skip)]
    pub seen: bool,
}

/// The most that is typed into a pane in one go. A terminal's input queue
/// on macOS holds about 1 KB, and a line longer than that, typed while the
/// program is not reading in raw mode, is thrown away: a 1,090-byte hand-off
/// of rebase conflicts reached Claude Code as its last 68 bytes, the same 68
/// every time. Longer text goes in a file the agent is told to read.
pub const MAX_TYPED: usize = 512;

fn too_long_to_type(text: &str) -> Result<()> {
    if text.len() > MAX_TYPED {
        return Err(Error::Pty(format!(
            "{} bytes is too long to type into a terminal, which drops the start of anything much over 1 KB. Leave it in a file and type where it is.",
            text.len()
        )));
    }
    Ok(())
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
    /// Started with something to do, as a prompt argument.
    #[serde(default)]
    pub prompted: bool,
    /// Set in the child's environment, over the login shell's. The pane's own
    /// id is always there too, as `VILLAIN_PANE`.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// For a CLI that keeps its window title on its state: what a title says.
    #[serde(skip)]
    pub title_activity: Option<fn(&str) -> Option<Activity>>,
    /// For a CLI that names its conversation in its window title: the name.
    #[serde(skip)]
    pub title_topic: Option<fn(&str) -> Option<String>>,
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
    fn schedule_flush<R: Runtime>(app: &AppHandle<R>, pane: &Arc<Pane>, id: &str, keyed: bool) {
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
        let mut wait = if keyed || quiet >= OUTPUT_COALESCE {
            OUTPUT_GATHER
        } else {
            OUTPUT_COALESCE - quiet
        };
        let app = app.clone();
        let pane = pane.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            loop {
                // Interruptible: an echo arriving meanwhile cuts it short,
                // then waits `OUTPUT_GATHER` for the rest of its frame.
                {
                    let mut urgent = pane.urgent.lock();
                    if !*urgent {
                        let _ = pane.wake.wait_for(&mut urgent, wait);
                    }
                    if *urgent {
                        *urgent = false;
                        drop(urgent);
                        std::thread::sleep(OUTPUT_GATHER);
                    }
                }
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
        if let Some(input) = &opts.initial_input {
            too_long_to_type(input)?;
        }
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

        // portable-pty starts a child whose folder is missing in $HOME instead,
        // without a word. A worktree deleted by hand then got `claude
        // --continue` in the home folder — resuming the wrong conversation, or
        // walking out into Photos and Downloads.
        if !std::path::Path::new(&opts.cwd).is_dir() {
            return Err(Error::Pty(format!("{} no longer exists", opts.cwd)));
        }

        let system = portable_pty::native_pty_system();
        let size = PtySize {
            rows: opts.rows.unwrap_or(30),
            cols: opts.cols.unwrap_or(100),
            pixel_width: 0,
            pixel_height: 0,
        };

        // One at a time. Panes start from several threads at once (restoring,
        // MCP calls, the blocking pool), and macOS's openpty called from two
        // threads together fails one of them with "Unknown error: -6": a
        // third of forty racing spawns, in the pane-cap test.
        static OPENING: Mutex<()> = Mutex::new(());
        let pair = {
            let _one = OPENING.lock();
            system.openpty(size)
        }
        .map_err(|e| Error::Pty(format!("openpty: {e}")))?;

        let id = uuid::Uuid::new_v4().to_string();
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
        // What the agent's hooks use to say which pane they are reporting on.
        cmd.env("VILLAIN_PANE", &id);
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Pty(format!("spawn {}: {e}", opts.program)))?;
        drop(pair.slave);

        // Started but not yet anyone's: failing past here must not leave it
        // running with nothing able to see or stop it.
        let ends = pair
            .master
            .try_clone_reader()
            .map_err(|e| Error::Pty(format!("clone reader: {e}")))
            .and_then(|r| {
                let w = pair.master.take_writer().map_err(|e| Error::Pty(format!("take writer: {e}")))?;
                Ok((r, w))
            });
        let (reader, mut writer) = match ends {
            Ok(ends) => ends,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
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
            activity: Activity::Idle,
            activity_since: now,
            topic: None,
        };

        let pid = child.process_id();
        let pane = Arc::new(Pane {
            meta: Mutex::new(PaneMeta {
                info: info.clone(),
                stopping: false,
                reported: None,
                last_work: now,
                last_input: Instant::now(),
                prompted: opts.prompted || opts.initial_input.is_some(),
                seen_at: now,
                title: String::new(),
                cleared_notice: None,
                notice_at: now,
            }),
            pid,
            master: Mutex::new(pair.master),
            input,
            killer: Mutex::new(killer),
            output: Mutex::new(Output { scrollback: Vec::new(), total: 0, sent: 0 }),
            flush_scheduled: AtomicBool::new(false),
            last_flush: Mutex::new(Instant::now()),
            typed: Mutex::new(None),
            urgent: Mutex::new(false),
            wake: parking_lot::Condvar::new(),
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
            let title_activity = opts.title_activity;
            let title_topic = opts.title_topic;
            let reads_title = title_activity.is_some() || title_topic.is_some();
            // Only an agent can be out of budget or asking to be trusted. A
            // shell running `gh` read "rate limit exceeded" as the first, and
            // offered to hand the shell off.
            let scan = opts.kind == PaneKind::Agent;
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
                            // Both read under the output lock, and before the
                            // meta one: `attach` takes them in that order, and
                            // the other way round the two could each hold one.
                            let (found, title) = {
                                let mut out = pane.output.lock();
                                out.push(&buf[..n]);
                                let tail = &out.scrollback
                                    [out.scrollback.len().saturating_sub(NOTICE_TAIL)..];
                                let found = if scan { notice_in(tail, &mut scratch) } else { None };
                                let title = reads_title
                                    .then(|| {
                                        last_title(&out.scrollback[out.scrollback.len().saturating_sub(TITLE_TAIL)..])
                                    })
                                    .flatten();
                                (found, title)
                            };
                            {
                                let mut meta = pane.meta.lock();
                                let now = Utc::now();
                                meta.info.last_output_at = now;
                                if meta.last_input.elapsed() > ANSWER_WINDOW {
                                    meta.last_work = now;
                                }
                                if let Some(title) = title.filter(|t| *t != meta.title) {
                                    if let Some(activity) = title_activity.and_then(|read| read(&title)) {
                                        meta.take_report(activity);
                                        let _ = app.emit("pty:activity", &id);
                                    }
                                    // A title without a topic keeps the last
                                    // one: a name that blinks back to "Claude
                                    // Code" on some passing title is worse
                                    // than one that stays a turn too long.
                                    if let Some(topic) = title_topic.and_then(|read| read(&title)) {
                                        if meta.info.topic.as_deref() != Some(topic.as_str()) {
                                            meta.info.topic = Some(topic);
                                            // The pane list is what carries it.
                                            let _ = app.emit("pty:activity", &id);
                                        }
                                    }
                                    meta.title = title;
                                }
                                if found.is_none() {
                                    meta.cleared_notice = None;
                                }
                                let found = found.filter(|f| meta.cleared_notice.as_deref() != Some(*f));
                                // A trust prompt clears once answered, so
                                // let it come and go; a usage limit sticks.
                                if meta.info.notice.as_deref() != found
                                    && meta.info.notice.as_deref() != Some("usage_limit")
                                {
                                    meta.info.notice = found.map(str::to_string);
                                    meta.notice_at = now;
                                    if found.is_some() {
                                        let _ = app.emit(
                                            "pty:notice",
                                            NoticeEvent { pane_id: &id, notice: found },
                                        );
                                    } else {
                                        // Answered: the count and the dot
                                        // say so now, not at the next poll.
                                        let _ = app.emit("pty:activity", &id);
                                    }
                                }
                            }
                            // The first output after a key is its echo, and
                            // goes out without waiting for the frame. Only the
                            // first: a key that starts a flood of output gets
                            // one fast event, and the flood the usual pace.
                            let keyed = {
                                let mut typed = pane.typed.lock();
                                let fresh = typed.is_some_and(|at| at.elapsed() < ECHO_WINDOW);
                                *typed = None;
                                fresh
                            };
                            if keyed {
                                *pane.urgent.lock() = true;
                                pane.wake.notify_one();
                            }
                            // Scrollback always records. The webview only gets
                            // a feed while the pane is on screen.
                            Self::schedule_flush(&app, &pane, &id, keyed);
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

    /// True when the keys changed what the pane reads as — a question
    /// answered, a turn interrupted — which nothing else would announce.
    pub fn write(&self, id: &str, data: &str) -> Result<bool> {
        let pane = self.get(id)?;
        *pane.typed.lock() = Some(Instant::now());
        let changed = {
            let watched = pane.watched.load(Ordering::Acquire);
            let mut meta = pane.meta.lock();
            let before = meta.activity(watched, Utc::now());
            meta.typed(data);
            meta.activity(watched, Utc::now()) != before
        };
        pane.input
            .send(data.as_bytes().to_vec())
            .map_err(|_| Error::Pty("the terminal has closed".into()))?;
        Ok(changed)
    }

    /// Type `text` into a pane and press Enter. At most `MAX_TYPED` bytes:
    /// longer is refused rather than typed and cut (`commands::hand_over`).
    ///
    /// Agent TUIs (Claude Code in particular) often leave the line sitting in
    /// the prompt when the characters and Enter arrive in one burst — the text
    /// shows up, but nothing is submitted until someone presses Enter again.
    /// A short gap between the two is enough for the TUI to accept the submit.
    pub fn submit(&self, id: &str, text: &str) -> Result<()> {
        too_long_to_type(text)?;
        let pane = self.get(id)?;
        pane.meta.lock().sent("\r");
        let keys = pane.input.clone();
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
        pane.meta.lock().last_input = Instant::now();
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
        let was_watched = pane.watched.swap(true, Ordering::AcqRel);
        let seen = {
            let mut meta = pane.meta.lock();
            let before = meta.activity(was_watched, Utc::now());
            meta.seen_at = Utc::now();
            meta.activity(true, Utc::now()) != before
        };
        let (bytes, reset) = out.since(since);
        let catchup = Catchup {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            end: out.total,
            reset,
            seen,
        };
        out.sent = out.total;
        Ok(catchup)
    }

    /// Nobody is looking at this pane any more; keep its output for later.
    pub fn detach(&self, id: &str) {
        if let Ok(pane) = self.get(id) {
            pane.watched.store(false, Ordering::Release);
            pane.meta.lock().seen_at = Utc::now();
        }
    }

    /// Scrollback as readable text, for handing work to another agent.
    pub fn transcript(&self, id: &str, max_lines: usize) -> Result<String> {
        let pane = self.get(id)?;
        let raw = { String::from_utf8_lossy(&pane.output.lock().scrollback).to_string() };
        Ok(readable_tail(&raw, max_lines))
    }

    pub fn info(&self, id: &str) -> Result<PaneInfo> {
        let pane = self.get(id)?;
        let watched = pane.watched.load(Ordering::Acquire);
        let info = pane.meta.lock().snapshot(watched);
        Ok(info)
    }

    /// What the agent says it is doing, from its own hooks. True when that
    /// changed what the pane reads as.
    ///
    /// `decide` is handed what the agent said before and says what this
    /// report makes it, under one lock: hooks post side by side, and asked and
    /// told separately a session's start judged against nothing could land
    /// after its first prompt and put it back to idle.
    pub fn report_with(
        &self,
        id: &str,
        decide: impl FnOnce(Option<Activity>) -> Option<Activity>,
    ) -> Result<bool> {
        let pane = self.get(id)?;
        let watched = pane.watched.load(Ordering::Acquire);
        let mut meta = pane.meta.lock();
        let Some(activity) = decide(meta.reported.map(|(a, _)| a)) else {
            return Ok(false);
        };
        let before = (meta.activity(watched, Utc::now()), meta.info.notice.clone());
        meta.take_report(activity);
        Ok((meta.activity(watched, Utc::now()), meta.info.notice.clone()) != before)
    }

    /// Ask the process group to stop, and only insist if it will not.
    ///
    /// This matters more than it looks: insisting is SIGKILL, which an agent
    /// cannot catch, so it dies without writing its transcript — and that
    /// transcript is the only thing that makes a session resumable later.
    fn request_stop(pane: &Pane) {
        pane.meta.lock().stopping = true;
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

    /// Insist, once asking has not worked.
    ///
    /// Not portable-pty's `ChildKiller::kill`: that is a SIGHUP to the leader
    /// alone, which the agent that ignored SIGTERM ignores too. The pane was
    /// taken off the list regardless, and the agent ran on out of sight —
    /// uncounted by the pane cap, in a worktree about to be deleted under it.
    fn force_kill(pane: &Pane) {
        match pane.pid {
            #[cfg(unix)]
            Some(pid) => unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            },
            _ => {
                let _ = pane.killer.lock().kill();
            }
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
            Self::force_kill(pane);
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
                Self::force_kill(pane);
            }
        }
    }

    pub fn list(&self, task_id: Option<&str>) -> Vec<PaneInfo> {
        let mut out: Vec<PaneInfo> = self
            .panes
            .lock()
            .values()
            .map(|p| p.meta.lock().snapshot(p.watched.load(Ordering::Acquire)))
            .filter(|i| task_id.is_none_or(|t| i.task_id == t))
            .collect();
        out.sort_by_key(|i| i.started_at);
        out
    }

    /// Every pane, and whether it was asked to stop, for the watch that says
    /// when an agent needs you.
    pub fn attention(&self) -> Vec<(PaneInfo, bool)> {
        self.panes
            .lock()
            .values()
            .map(|p| {
                let meta = p.meta.lock();
                (meta.snapshot(p.watched.load(Ordering::Acquire)), meta.stopping)
            })
            .collect()
    }

    /// Kill every pane belonging to a task, used when it is deleted. Returns
    /// the panes closed.
    pub fn close_task(&self, task_id: &str) -> Vec<String> {
        self.close_matching(|i| i.task_id == task_id)
    }

    /// Kill every pane working inside one checkout, used when a repo leaves a
    /// task and its worktree is about to be deleted.
    ///
    /// By where it runs as well as what it was started for: an agent resumed
    /// from the task root can be running inside the worktree with no checkout
    /// recorded, and deleting the folder out from under it is worse than
    /// stopping it.
    pub fn close_checkout(&self, checkout_id: &str, path: &str) -> Vec<String> {
        let dir = std::path::Path::new(path);
        self.close_matching(|i| {
            i.checkout_id.as_deref() == Some(checkout_id) || std::path::Path::new(&i.cwd).starts_with(dir)
        })
    }

    fn close_matching(&self, pred: impl Fn(&PaneInfo) -> bool) -> Vec<String> {
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
            return Vec::new();
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
                Self::force_kill(pane);
            }
        }
        panes.iter().map(|p| p.meta.lock().info.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {

    fn meta(prompted: bool, quiet_secs: i64) -> PaneMeta {
        let now = Utc::now();
        PaneMeta {
            info: PaneInfo {
                id: "p".into(),
                task_id: "t".into(),
                checkout_id: None,
                kind: PaneKind::Agent,
                title: "Claude Code".into(),
                agent_id: Some("claude".into()),
                cwd: "/tmp".into(),
                running: true,
                exit_code: None,
                started_at: now - chrono::TimeDelta::seconds(3600),
                last_output_at: now,
                notice: None,
                activity: Activity::Idle,
                activity_since: now,
                topic: None,
            },
            stopping: false,
            reported: None,
            last_work: now - chrono::TimeDelta::seconds(quiet_secs),
            last_input: Instant::now(),
            prompted,
            seen_at: now - chrono::TimeDelta::seconds(3600),
            title: String::new(),
            cleared_notice: None,
            notice_at: now,
        }
    }

    #[test]
    fn an_agent_that_reports_itself_is_what_it_says_whatever_it_prints() {
        let now = Utc::now();
        // Printing just now, as Claude Code does while it sits at its prompt.
        let mut m = meta(true, 0);
        m.reported = Some((Activity::Done, now - chrono::TimeDelta::seconds(60)));
        assert_eq!(m.activity(false, now), Activity::Done);
        // Looked at since it finished: nothing new.
        m.seen_at = now - chrono::TimeDelta::seconds(10);
        assert_eq!(m.activity(false, now), Activity::Idle);
        // Quiet for a day but saying it is working: working.
        let mut m = meta(true, 86_400);
        m.reported = Some((Activity::Working, now));
        assert_eq!(m.activity(false, now), Activity::Working);
        m.reported = Some((Activity::Asking, now));
        assert_eq!(m.activity(true, now), Activity::Asking);
    }

    #[test]
    fn without_reports_quiet_is_done_until_seen() {
        let now = Utc::now();
        assert_eq!(meta(true, 10).activity(false, now), Activity::Working);
        assert_eq!(meta(true, 60).activity(false, now), Activity::Done);
        // On screen when it went quiet, or looked at since.
        assert_eq!(meta(true, 60).activity(true, now), Activity::Idle);
        let mut m = meta(true, 60);
        m.seen_at = now;
        assert_eq!(m.activity(false, now), Activity::Idle);
        // Resumed and never given anything: it has not finished anything.
        assert_eq!(meta(false, 600).activity(false, now), Activity::Idle);
    }

    #[test]
    fn a_notice_or_an_exit_outranks_the_rest() {
        let now = Utc::now();
        let mut m = meta(true, 0);
        m.info.notice = Some("trust_prompt".into());
        assert_eq!(m.activity(false, now), Activity::Asking);
        let mut m = meta(true, 600);
        m.info.running = false;
        assert_eq!(m.activity(false, now), Activity::Idle);
    }

    #[test]
    fn the_last_whole_title_is_the_one_read() {
        let t = |b: &[u8]| last_title(b);
        assert_eq!(t(b"\x1b]0;\xe2\x97\x87  Ready (api)\x07drawn"), Some("◇  Ready (api)".into()));
        // Two in one read: the later wins. ST ends one as well as BEL does.
        assert_eq!(
            t(b"\x1b]2;one\x07text\x1b]0;two\x1b\\more"),
            Some("two".into())
        );
        // Still arriving: the whole one before it stands.
        assert_eq!(t(b"\x1b]0;done\x07\x1b]0;half"), Some("done".into()));
        // Other OSC sequences are not titles.
        assert_eq!(t(b"\x1b]10;?\x1b\\\x1b]9;hello\x07"), None);
        assert_eq!(t(b"plain"), None);
    }

    #[test]
    fn a_ready_title_before_any_prompt_is_idle_not_done() {
        let mut m = meta(false, 0);
        m.take_report(Activity::Done);
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Idle));
        m.prompted = true;
        m.take_report(Activity::Done);
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Done));
    }

    #[test]
    fn answering_is_working_and_an_interrupt_is_not() {
        let now = Utc::now();
        let mut m = meta(true, 0);
        m.reported = Some((Activity::Asking, now));
        // The terminal reporting focus is not an answer.
        m.typed("\u{1b}[I");
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Asking));
        m.typed("1");
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Working));
        m.typed("\u{1b}");
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Idle));
        // Typing at an idle prompt changes nothing until the agent says so.
        m.typed("fix the tests");
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Idle));
    }

    #[test]
    fn escape_at_a_question_refuses_it() {
        let mut m = meta(true, 0);
        m.reported = Some((Activity::Asking, Utc::now()));
        m.typed("\u{1b}");
        assert_eq!(m.reported.map(|r| r.0), Some(Activity::Idle));
    }

    #[test]
    fn a_report_ends_the_notice_it_contradicts_until_the_words_go() {
        let mut m = meta(true, 0);
        m.info.notice = Some("usage_limit".into());
        // Finishing says nothing about the budget; working again does.
        m.take_report(Activity::Done);
        assert_eq!(m.info.notice.as_deref(), Some("usage_limit"));
        m.take_report(Activity::Working);
        assert_eq!(m.info.notice, None);
        assert_eq!(m.cleared_notice.as_deref(), Some("usage_limit"));

        m.info.notice = Some("trust_prompt".into());
        m.take_report(Activity::Idle);
        assert_eq!(m.info.notice.as_deref(), Some("trust_prompt"), "a session starting is not an answer");
        m.take_report(Activity::Asking);
        assert_eq!(m.info.notice, None);
    }

    #[test]
    fn only_a_line_entered_counts_as_being_given_work() {
        let mut m = meta(false, 0);
        // Focus reports and keystrokes are not a prompt.
        m.sent("\u{1b}[I");
        m.sent("hel");
        assert!(!m.prompted);
        m.sent("\r");
        assert!(m.prompted);
    }

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

    /// How long a keystroke's echo takes to reach the webview, quiet and
    /// beside a process printing every 20ms — an agent's spinner, a watcher.
    /// A measurement, not a check: `cargo test --lib echo_latency -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn echo_latency() {
        use tauri::Listener;
        for (label, script) in [
            ("quiet", "exec cat"),
            ("beside a 20ms printer", "(while :; do printf .; sleep 0.02; done) & exec cat"),
        ] {
            let app = tauri::test::mock_app();
            let ptys = PtyManager::default();
            let (tx, rx) = std::sync::mpsc::channel::<(Instant, String)>();
            app.listen_any("pty:output", move |e| {
                let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(v["data"].as_str().unwrap())
                    .unwrap();
                let _ = tx.send((Instant::now(), String::from_utf8_lossy(&bytes).to_string()));
            });
            let pane = ptys
                .spawn(
                    app.handle(),
                    SpawnOptions {
                        task_id: "t".into(),
                        checkout_id: None,
                        cwd: std::env::temp_dir().to_string_lossy().to_string(),
                        kind: PaneKind::Agent,
                        title: "t".into(),
                        program: "/bin/sh".into(),
                        args: vec!["-c".into(), script.into()],
                        agent_id: None,
                        rows: None,
                        cols: None,
                        initial_input: None,
                        prompted: false,
                        env: Vec::new(),
                        title_activity: None,
                        title_topic: None,
                    },
                )
                .unwrap();
            ptys.attach(&pane.id, None).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let mut took = Vec::new();
            for i in 0..40u8 {
                let key = (b'a' + (i % 26)) as char;
                while rx.try_recv().is_ok() {}
                // Not in step with the printer, so the key lands anywhere in its cycle.
                std::thread::sleep(Duration::from_millis(50 + (i as u64 * 7) % 40));
                let sent = Instant::now();
                ptys.write(&pane.id, &key.to_string()).unwrap();
                loop {
                    let (at, text) = rx.recv_timeout(Duration::from_secs(2)).expect("no echo");
                    if text.contains(key) {
                        took.push(at.duration_since(sent).as_secs_f64() * 1000.0);
                        break;
                    }
                }
            }
            ptys.shutdown(Duration::from_millis(500));
            took.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "{label}: median {:.1}ms  p90 {:.1}ms  max {:.1}ms",
                took[took.len() / 2],
                took[took.len() * 9 / 10],
                took[took.len() - 1]
            );
        }
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
                    prompted: false,
                    env: Vec::new(),
                    title_activity: None,
                    title_topic: None,
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

    /// A 1,090-byte hand-off reached Claude Code as its last 68 bytes: the
    /// terminal dropped the rest. Too long is refused now, never typed.
    #[test]
    fn a_message_too_long_to_type_is_refused_rather_than_cut() {
        assert!(too_long_to_type(&"x".repeat(MAX_TYPED)).is_ok());
        assert!(too_long_to_type(&"x".repeat(MAX_TYPED + 1)).is_err());
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let opts = SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            kind: PaneKind::Agent,
            title: "t".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 5".into()],
            agent_id: None,
            rows: None,
            cols: None,
            initial_input: Some("x".repeat(1090)),
            prompted: false,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        };
        assert!(ptys.spawn(app.handle(), opts).is_err());
        assert!(ptys.list(None).is_empty(), "nothing was started to type it into");
        assert!(ptys.submit("any", &"x".repeat(1090)).is_err());
    }

    /// The name is read from the title as it arrives, not while it is only a
    /// placeholder, and a title that names nothing keeps the last name.
    #[test]
    fn an_agent_is_named_by_the_topic_its_title_gives() {
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let opts = |script: &str| SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
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
            title_topic: Some(crate::agents::claude_title_topic),
        };
        let topic = |id: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let pane = ptys.list(None).into_iter().find(|p| p.id == id).unwrap();
                if !pane.running || Instant::now() > deadline {
                    return pane.topic;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        let unnamed = ptys.spawn(app.handle(), opts(r"printf '\033]0;✳ Claude Code\007'")).unwrap();
        assert_eq!(topic(&unnamed.id), None);

        let named = ptys
            .spawn(
                app.handle(),
                opts(r"printf '\033]0;◐ Fix the login\007'; sleep 0.2; printf '\033]0;◑ Claude Code\007'"),
            )
            .unwrap();
        assert_eq!(topic(&named.id).as_deref(), Some("Fix the login"));
    }

    /// An agent that ignores being asked is still stopped, with its
    /// subprocesses: insisting used to be a hangup, which it ignored as well,
    /// and it ran on after its pane was gone.
    #[test]
    fn a_pane_that_ignores_its_signals_is_still_killed() {
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let dir = std::env::temp_dir().join(format!("villain-stubborn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let opts = |cwd: String| SpawnOptions {
            task_id: "t".into(),
            checkout_id: None,
            cwd,
            kind: PaneKind::Agent,
            title: "t".into(),
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!("trap '' TERM HUP; echo $$ > '{}'; while :; do sleep 1; done", pidfile.display()),
            ],
            agent_id: None,
            rows: None,
            cols: None,
            initial_input: None,
            prompted: false,
            env: Vec::new(),
            title_activity: None,
            title_topic: None,
        };

        let gone = dir.join("gone").to_string_lossy().to_string();
        let err = ptys.spawn(app.handle(), opts(gone)).unwrap_err().to_string();
        assert!(err.contains("no longer exists"), "{err}");

        let pane = ptys.spawn(app.handle(), opts(dir.to_string_lossy().to_string())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid: i32 = loop {
            if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse().ok()) {
                break pid;
            }
            assert!(Instant::now() < deadline, "the script never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        ptys.close(&pane.id).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "pid {pid} is still running");
        std::fs::remove_dir_all(dir).ok();
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
        let refused = parking_lot::Mutex::new(Vec::new());
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
                            prompted: false,
                            env: Vec::new(),
                            title_activity: None,
                            title_topic: None,
                        },
                    );
                    match spawned {
                        Ok(_) => {
                            ok.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) => refused.lock().push(e.to_string()),
                    }
                });
            }
        });
        let started = ok.load(Ordering::SeqCst);
        ptys.shutdown(Duration::from_secs(2));
        assert_eq!(started, MAX_PANES, "refused: {:?}", refused.lock());
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
