//! A pane's output for a reader that is not the window: a phone (PHONE-6).
//!
//! The window's feed is `watched` and `sent`, and both say what *it* has
//! been given. A second reader moving them would starve the window of
//! output, or send it twice. This one keeps its own position, from the same
//! byte counts, so it neither knows nor changes what the window was sent.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::Utc;

use super::{Pane, PtyManager};
use crate::error::Result;

pub struct Feed {
    pane: Arc<Pane>,
    printed: tokio::sync::watch::Receiver<()>,
}

/// Output after a position, as `Output::since` gives it.
pub struct Chunk {
    pub data: Vec<u8>,
    /// The position just past `data`, to ask from next time.
    pub end: u64,
    /// The position asked from is no longer held: `data` is all the
    /// scrollback, to be drawn on a clean terminal.
    pub reset: bool,
}

impl PtyManager {
    /// Follow a pane's output without becoming its terminal.
    pub fn feed(&self, id: &str) -> Result<Feed> {
        let pane = self.get(id)?;
        let printed = pane.printed.subscribe();
        Ok(Feed { pane, printed })
    }

    /// The pane was on a screen that is not the window's. Done becomes idle
    /// once looked at (STATE-3), wherever it was looked at. True when that
    /// changed what the pane reads as.
    pub fn seen_elsewhere(&self, id: &str) -> Result<bool> {
        let pane = self.get(id)?;
        let watched = pane.watched.load(Ordering::Acquire);
        let mut meta = pane.meta.lock();
        let before = meta.activity(watched, Utc::now());
        meta.seen_at = Utc::now();
        Ok(meta.activity(watched, Utc::now()) != before)
    }
}

impl Feed {
    /// Wait until the pane prints or exits.
    pub async fn printed(&mut self) {
        // The sender lives in the pane, which this holds, so it cannot be
        // dropped under it; and if it ever were, returning at once would spin.
        if self.printed.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    pub fn since(&self, from: Option<u64>) -> Chunk {
        let out = self.pane.output.lock();
        let (data, reset) = out.since(from);
        Chunk { data: data.to_vec(), end: out.total, reset }
    }

    /// The terminal's size as the window last set it: a reader elsewhere
    /// draws at this size and never changes it.
    /// A conversation (§20) has no terminal: its text copy is drawn at the
    /// size a new terminal starts at.
    pub fn size(&self) -> (u16, u16) {
        match &self.pane.io {
            super::Io::Pty { master, .. } => match master.lock().get_size() {
                Ok(s) => (s.rows, s.cols),
                Err(_) => (30, 100),
            },
            super::Io::Acp(_) => (30, 100),
        }
    }

    /// The exit code once the process has ended: `Some(None)` when it ended
    /// without one.
    pub fn exited(&self) -> Option<Option<i32>> {
        let meta = self.pane.meta.lock();
        (!meta.info.running).then_some(meta.info.exit_code)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{PaneKind, SpawnOptions};
    use super::*;
    use std::time::Duration;

    fn spawn(ptys: &PtyManager, app: &tauri::App<tauri::test::MockRuntime>, script: &str) -> String {
        ptys.spawn(
            app.handle(),
            SpawnOptions {
                task_id: "t".into(),
                checkout_id: None,
                cwd: std::env::temp_dir().to_string_lossy().to_string(),
                kind: PaneKind::Shell,
                title: "t".into(),
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                agent_id: None,
                rows: Some(24),
                cols: Some(80),
                initial_input: None,
                prompted: false,
                env: Vec::new(),
                title_activity: None,
                title_topic: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_phone_follows_a_pane_from_its_own_position_and_leaves_the_windows_feed_alone() {
        let app = tauri::test::mock_app();
        let ptys = PtyManager::default();
        let id = spawn(&ptys, &app, "printf one; read x; printf two; read y");
        let mut feed = ptys.feed(&id).unwrap();
        assert_eq!(feed.size(), (24, 80));

        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let wait_for = |feed: &mut Feed, from: Option<u64>, text: &str| {
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let chunk = feed.since(from);
                        if String::from_utf8_lossy(&chunk.data).contains(text) {
                            return chunk;
                        }
                        feed.printed().await;
                    }
                })
                .await
                .expect("the feed never showed it")
            })
        };

        let first = wait_for(&mut feed, None, "one");
        assert!(!first.reset, "nothing drawn yet has nothing to clear");
        ptys.write(&id, "\r").unwrap();
        let next = wait_for(&mut feed, Some(first.end), "two");
        assert!(!String::from_utf8_lossy(&next.data).contains("one"), "only what came after");

        // The window was never attached, so all of it still waits for it.
        let catchup = ptys.attach(&id, None).unwrap();
        assert!(catchup.end >= next.end);

        // Asked from before what is held, it starts again.
        assert!(feed.since(Some(next.end + 1_000_000)).reset);

        ptys.write(&id, "\r").unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while feed.exited().is_none() {
                    feed.printed().await;
                }
            })
            .await
            .expect("the exit never rang")
        });
        assert_eq!(feed.exited(), Some(Some(0)));
        ptys.shutdown(Duration::from_millis(200));
    }
}
