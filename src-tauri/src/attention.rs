//! Agents waiting on you: the count on the dock icon, and a banner when one
//! starts waiting while the window is in the background.
//!
//! Here rather than in the webview, which already works out "idle — may need
//! you" for its own labels: the webview's polls stop while the window is away,
//! and macOS may suspend a hidden webview's timers altogether — so the one
//! moment this exists for is the one the webview is least able to notice.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tauri::{AppHandle, Manager};

use crate::commands::{banner, AppState, CHAT_TASK_ID};
use crate::pty::{PaneInfo, PaneKind};

/// Silence past this reads as waiting. Matches `IDLE_AFTER_MS` in `store.ts`,
/// so the banner and the label on the pane agree.
const IDLE_AFTER: Duration = Duration::from_secs(45);
const TICK: Duration = Duration::from_secs(5);
/// Past this many at once, one banner rather than a stack of them.
const BATCH: usize = 3;
/// A pane that keeps flickering between working and quiet — a watcher that
/// prints now and then — would otherwise announce itself every minute.
const REPEAT_AFTER: Duration = Duration::from_secs(120);

/// Why this pane is waiting on you, if it is.
///
/// Only agents in tasks: a shell sits quietly by nature, and a chat agent is
/// idle most of the time because that is what a standing chat is for.
pub(crate) fn waiting(info: &PaneInfo, now: DateTime<Utc>) -> Option<&'static str> {
    if info.kind != PaneKind::Agent || !info.running || info.task_id == CHAT_TASK_ID {
        return None;
    }
    match info.notice.as_deref() {
        Some("usage_limit") => return Some("hit a usage limit — hand it off"),
        Some("trust_prompt") => return Some("is asking whether to trust its folder"),
        _ => {}
    }
    let quiet = now
        .signed_duration_since(info.last_output_at)
        .to_std()
        .unwrap_or_default();
    (quiet >= IDLE_AFTER).then_some("has gone quiet — it may need you")
}

/// What one pass found that is worth a banner.
#[derive(Debug, PartialEq)]
struct News {
    pane_id: String,
    task_id: String,
    title: String,
    what: String,
}

#[derive(Default)]
struct Watch {
    /// Panes waiting at the last pass. None until the first pass, which only
    /// records: whatever is already waiting at launch is not news.
    waiting: Option<HashSet<String>>,
    running: HashSet<String>,
    announced: HashMap<String, Instant>,
}

impl Watch {
    /// One pass over the panes: the count for the badge, and what changed.
    fn pass(&mut self, panes: &[(PaneInfo, bool)], now: DateTime<Utc>, at: Instant) -> (usize, Vec<News>) {
        let mut quiet = HashSet::new();
        let mut running = HashSet::new();
        let mut news = Vec::new();

        for (info, stopping) in panes {
            if info.running {
                running.insert(info.id.clone());
            }
            if let Some(what) = waiting(info, now) {
                quiet.insert(info.id.clone());
                if self.waiting.as_ref().is_some_and(|w| !w.contains(&info.id)) {
                    news.push(News {
                        pane_id: info.id.clone(),
                        task_id: info.task_id.clone(),
                        title: info.title.clone(),
                        what: what.to_string(),
                    });
                }
            }
            // An agent that exited by itself has stopped working as surely as
            // one sitting at a prompt. One stopped on purpose — Stop, a
            // handoff, quitting — is not news.
            let exited = !info.running
                && !stopping
                && info.kind == PaneKind::Agent
                && info.task_id != CHAT_TASK_ID
                && self.running.contains(&info.id);
            if exited {
                news.push(News {
                    pane_id: info.id.clone(),
                    task_id: info.task_id.clone(),
                    title: info.title.clone(),
                    what: match info.exit_code {
                        Some(0) => "finished".to_string(),
                        code => format!(
                            "exited with {}",
                            code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                        ),
                    },
                });
            }
        }

        let present: HashSet<&String> = panes.iter().map(|(i, _)| &i.id).collect();
        self.announced
            .retain(|id, when| present.contains(id) && at.duration_since(*when) < REPEAT_AFTER);
        news.retain(|n| {
            self.announced
                .get(&n.pane_id)
                .is_none_or(|when| at.duration_since(*when) >= REPEAT_AFTER)
        });
        for n in &news {
            self.announced.insert(n.pane_id.clone(), at);
        }

        let count = quiet.len();
        self.waiting = Some(quiet);
        self.running = running;
        (count, news)
    }
}

/// Watch the panes for as long as the app runs.
pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("attention".into())
        .spawn(move || {
            let mut watch = Watch::default();
            let mut badge: Option<usize> = None;
            loop {
                std::thread::sleep(TICK);
                let state = app.state::<AppState>();
                let on = state.config.read().ui.notify_waiting_agents;
                let (count, news) = watch.pass(&state.ptys.attention(), Utc::now(), Instant::now());

                let Some(window) = app.get_webview_window("main") else {
                    continue;
                };
                let shown = if on { count } else { 0 };
                if badge != Some(shown) {
                    // Unsupported on Windows, and nothing to be done about it.
                    let _ = window.set_badge_count((shown > 0).then_some(shown as i64));
                    badge = Some(shown);
                }

                if !on || news.is_empty() || window.is_focused().unwrap_or(false) {
                    continue;
                }
                for (title, body, target) in banners(&state, &news) {
                    let _ = banner(&app, title, body, target);
                }
            }
        })
        .expect("spawn the attention watch");
}

fn banners(state: &AppState, news: &[News]) -> Vec<(String, String, String)> {
    let name = |task_id: &str| {
        state
            .config
            .task(task_id)
            .map(|t| t.name)
            .unwrap_or_else(|_| "a task".into())
    };
    if news.len() > BATCH {
        let tasks: HashSet<&str> = news.iter().map(|n| n.task_id.as_str()).collect();
        // One task: open it. Several: the Work overview lists every agent.
        let target = match tasks.iter().next() {
            Some(only) if tasks.len() == 1 => format!("task:{only}"),
            _ => "work".to_string(),
        };
        return vec![(
            "Agents waiting".into(),
            format!("{} agents have stopped and may need you", news.len()),
            target,
        )];
    }
    news.iter()
        .map(|n| {
            (
                name(&n.task_id),
                format!("{} {}", n.title, n.what),
                format!("task:{}", n.task_id),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, quiet_secs: i64, now: DateTime<Utc>) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            task_id: "t1".into(),
            checkout_id: None,
            kind: PaneKind::Agent,
            title: "Claude Code".into(),
            agent_id: Some("claude".into()),
            cwd: "/tmp".into(),
            running: true,
            exit_code: None,
            started_at: now - chrono::Duration::seconds(600),
            last_output_at: now - chrono::Duration::seconds(quiet_secs),
            notice: None,
        }
    }

    #[test]
    fn only_agents_in_tasks_can_be_waiting() {
        let now = Utc::now();
        assert!(waiting(&pane("a", 60, now), now).is_some());
        assert!(waiting(&pane("a", 10, now), now).is_none());

        let mut shell = pane("s", 600, now);
        shell.kind = PaneKind::Shell;
        assert!(waiting(&shell, now).is_none());

        let mut chat = pane("c", 600, now);
        chat.task_id = CHAT_TASK_ID.into();
        assert!(waiting(&chat, now).is_none());

        let mut exited = pane("x", 600, now);
        exited.running = false;
        assert!(waiting(&exited, now).is_none());
    }

    #[test]
    fn a_notice_is_waiting_however_recent_the_output() {
        let now = Utc::now();
        let mut p = pane("a", 0, now);
        p.notice = Some("trust_prompt".into());
        assert_eq!(waiting(&p, now), Some("is asking whether to trust its folder"));
    }

    #[test]
    fn the_first_pass_records_without_announcing() {
        let now = Utc::now();
        let at = Instant::now();
        let mut w = Watch::default();
        let (count, news) = w.pass(&[(pane("a", 60, now), false)], now, at);
        assert_eq!(count, 1);
        assert!(news.is_empty());
    }

    #[test]
    fn going_quiet_is_announced_once() {
        let now = Utc::now();
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", 5, now), false)], now, at);

        let later = now + chrono::Duration::seconds(50);
        let (count, news) = w.pass(&[(pane("a", 55, later), false)], later, at + TICK);
        assert_eq!(count, 1);
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].pane_id, "a");

        // Still quiet on the next pass: not news again.
        let (_, news) = w.pass(&[(pane("a", 60, later), false)], later, at + TICK * 2);
        assert!(news.is_empty());
    }

    #[test]
    fn flickering_back_to_quiet_soon_after_is_not_announced_again() {
        let now = Utc::now();
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", 5, now), false)], now, at);
        let (_, news) = w.pass(&[(pane("a", 50, now), false)], now, at + TICK);
        assert_eq!(news.len(), 1);
        // Printed something, then quiet again a minute later.
        w.pass(&[(pane("a", 1, now), false)], now, at + TICK * 2);
        let (_, news) = w.pass(&[(pane("a", 50, now), false)], now, at + Duration::from_secs(60));
        assert!(news.is_empty());
        // Much later, it is worth saying again.
        w.pass(&[(pane("a", 1, now), false)], now, at + Duration::from_secs(300));
        let (_, news) = w.pass(&[(pane("a", 50, now), false)], now, at + Duration::from_secs(305));
        assert_eq!(news.len(), 1);
    }

    #[test]
    fn an_exit_is_news_unless_it_was_asked_for() {
        let now = Utc::now();
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", 1, now), false), (pane("b", 1, now), false)], now, at);

        let mut a = pane("a", 1, now);
        a.running = false;
        a.exit_code = Some(0);
        let mut b = pane("b", 1, now);
        b.running = false;
        b.exit_code = Some(143);
        let (_, news) = w.pass(&[(a, false), (b, true)], now, at + TICK);
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].pane_id, "a");
        assert_eq!(news[0].what, "finished");
    }
}
