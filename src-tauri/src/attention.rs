//! Agents waiting on you: the count on the dock icon, and a banner when one
//! starts waiting while the window is in the background.
//!
//! Here rather than in the webview: its polls stop while the window is away,
//! and macOS may suspend a hidden webview's timers altogether — so the one
//! moment this exists for is the one the webview is least able to notice.
//!
//! "Waiting" is asking for something, or finished and not yet looked at. An
//! agent finished and seen is only idle: counting those kept a number on the
//! dock that nothing in the window explained.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::commands::{banner, AppState, CHAT_TASK_ID};
use crate::pty::{Activity, PaneInfo, PaneKind};

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
pub(crate) fn waiting(info: &PaneInfo) -> Option<&'static str> {
    if info.kind != PaneKind::Agent || !info.running || info.task_id == CHAT_TASK_ID {
        return None;
    }
    match info.notice.as_deref() {
        Some("usage_limit") => return Some("hit a usage limit — hand it off"),
        Some("trust_prompt") => return Some("is asking whether to trust its folder"),
        _ => {}
    }
    match info.activity {
        Activity::Asking => Some("is asking for your permission"),
        Activity::Done => Some("has finished — your turn"),
        Activity::Working | Activity::Idle => None,
    }
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
    fn pass(&mut self, panes: &[(PaneInfo, bool)], at: Instant) -> (usize, Vec<News>) {
        let mut quiet = HashSet::new();
        let mut running = HashSet::new();
        let mut news = Vec::new();

        for (info, stopping) in panes {
            if info.running {
                running.insert(info.id.clone());
            }
            if let Some(what) = waiting(info) {
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
                let (count, news) = watch.pass(&state.ptys.attention(), Instant::now());

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

    fn pane(id: &str, activity: Activity) -> PaneInfo {
        let now = chrono::Utc::now();
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
            started_at: now,
            last_output_at: now,
            notice: None,
            activity,
            activity_since: now,
        }
    }

    #[test]
    fn only_asking_or_unseen_work_in_a_task_is_waiting() {
        assert!(waiting(&pane("a", Activity::Asking)).is_some());
        assert!(waiting(&pane("a", Activity::Done)).is_some());
        // Finished and looked at: idle, not news.
        assert!(waiting(&pane("a", Activity::Idle)).is_none());
        assert!(waiting(&pane("a", Activity::Working)).is_none());

        let mut shell = pane("s", Activity::Done);
        shell.kind = PaneKind::Shell;
        assert!(waiting(&shell).is_none());
        let mut chat = pane("c", Activity::Done);
        chat.task_id = CHAT_TASK_ID.into();
        assert!(waiting(&chat).is_none());
        let mut exited = pane("x", Activity::Done);
        exited.running = false;
        assert!(waiting(&exited).is_none());
    }

    #[test]
    fn a_notice_is_waiting_whatever_else_it_is_doing() {
        let mut p = pane("a", Activity::Working);
        p.notice = Some("trust_prompt".into());
        assert_eq!(waiting(&p), Some("is asking whether to trust its folder"));
    }

    #[test]
    fn the_first_pass_records_without_announcing() {
        let mut w = Watch::default();
        let (count, news) = w.pass(&[(pane("a", Activity::Done), false)], Instant::now());
        assert_eq!(count, 1);
        assert!(news.is_empty());
    }

    #[test]
    fn finishing_is_announced_once() {
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", Activity::Working), false)], at);
        let (count, news) = w.pass(&[(pane("a", Activity::Done), false)], at + TICK);
        assert_eq!(count, 1);
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].what, "has finished — your turn");
        let (_, news) = w.pass(&[(pane("a", Activity::Done), false)], at + TICK * 2);
        assert!(news.is_empty());
        // Looked at: no longer counted.
        let (count, _) = w.pass(&[(pane("a", Activity::Idle), false)], at + TICK * 3);
        assert_eq!(count, 0);
    }

    #[test]
    fn flickering_back_soon_after_is_not_announced_again() {
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", Activity::Working), false)], at);
        let (_, news) = w.pass(&[(pane("a", Activity::Asking), false)], at + TICK);
        assert_eq!(news.len(), 1);
        w.pass(&[(pane("a", Activity::Working), false)], at + TICK * 2);
        let (_, news) = w.pass(&[(pane("a", Activity::Done), false)], at + Duration::from_secs(60));
        assert!(news.is_empty());
        w.pass(&[(pane("a", Activity::Working), false)], at + Duration::from_secs(300));
        let (_, news) = w.pass(&[(pane("a", Activity::Done), false)], at + Duration::from_secs(305));
        assert_eq!(news.len(), 1);
    }

    #[test]
    fn an_exit_is_news_unless_it_was_asked_for() {
        let at = Instant::now();
        let mut w = Watch::default();
        w.pass(&[(pane("a", Activity::Working), false), (pane("b", Activity::Working), false)], at);

        let mut a = pane("a", Activity::Idle);
        a.running = false;
        a.exit_code = Some(0);
        let mut b = pane("b", Activity::Idle);
        b.running = false;
        b.exit_code = Some(143);
        let (_, news) = w.pass(&[(a, false), (b, true)], at + TICK);
        assert_eq!(news.len(), 1);
        assert_eq!(news[0].pane_id, "a");
        assert_eq!(news[0].what, "finished");
    }
}
