//! The Browser panel's side of a tab (BRW-9, BRW-10): what it shows, the
//! frames it is sent, the user's own input, and what an agent last did.

use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Runtime};

use super::cdp::Event;
use super::{changed, input, Browser, Viewport};
use crate::error::{Error, Result};

/// The least time between two frames sent to the panel: 30 a second
/// (BRW-9). A video plays in the page at Chrome's rate, and each frame is
/// a JPEG crossing into the webview. At 15 a second, scrolling and typing
/// in the panel felt like lag; Chrome itself kept up with 60.
const FRAME_GAP: Duration = Duration::from_millis(33);

/// What an agent last did in a tab, for the panel to show (BRW-10).
#[derive(Clone, Debug, Serialize)]
pub struct AgentAction {
    pub text: String,
    /// Where it clicked or hovered, in the page's CSS pixels.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Milliseconds since the epoch.
    pub at: i64,
}

/// A task's tab as the panel shows it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TabView {
    /// The tab's session: a new one means the tab was made again, and the
    /// panel watches it afresh.
    pub tab: Option<String>,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub last_action: Option<AgentAction>,
    /// A dialog the page opened, waiting on an answer (BRW-13).
    pub dialog: Option<super::Dialog>,
    /// The user has taken the tab over (BRW-12).
    pub held: bool,
    /// What is shown is a window the page opened, over it (BRW-14).
    pub window: bool,
    /// The agent using the tab, or that last did (BRW-15).
    pub driver: Option<super::Driver>,
}

/// The tab on screen in the panel, which alone is sent frames (BRW-9).
pub(super) struct Watch {
    pub(super) task: String,
    pub(super) session: String,
    frames: Channel<InvokeResponseBody>,
    /// When the next frame may go.
    next: Instant,
}

impl Browser {
    /// The task's tab for the panel; a default one when there is no tab.
    pub fn view(&self, task: &str) -> TabView {
        let inner = self.inner.lock();
        let held = inner.held.contains(task);
        let driver = inner.drivers.get(task).cloned();
        inner
            .tabs
            .get(task)
            .map(|t| TabView {
                tab: Some(t.session.clone()),
                url: t.url.clone(),
                title: t.title.clone(),
                loading: t.loading,
                last_action: t.last_action.clone(),
                dialog: t.dialog.clone(),
                held,
                window: t.opener.is_some(),
                driver: driver.clone(),
            })
            .unwrap_or(TabView { held, driver, ..TabView::default() })
    }

    /// Send the task's tab to the panel: sized to it, and its frames as they
    /// are drawn, until `unwatch` or another tab is watched (BRW-9).
    pub async fn watch<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        task: &str,
        viewport: Viewport,
        frames: Channel<InvokeResponseBody>,
    ) -> Result<()> {
        let page = self.page(app, task).await?;
        let resize = {
            let mut inner = self.inner.lock();
            let tab = inner.tabs.get_mut(task).ok_or_else(|| Error::Other("the tab closed".into()))?;
            let resize = tab.viewport != viewport;
            tab.viewport = viewport;
            let session = tab.session.clone();
            if let Some(old) = inner.watch.take().filter(|w| w.session != session) {
                if let Some(r) = inner.running.as_ref() {
                    r.cdp.send("Page.stopScreencast", json!({}), Some(&old.session));
                }
            }
            inner.watch = Some(Watch { task: task.to_string(), session, frames, next: Instant::now() });
            resize
        };
        if resize {
            page.call(
                "Emulation.setDeviceMetricsOverride",
                json!({ "width": viewport.width, "height": viewport.height, "deviceScaleFactor": viewport.scale, "mobile": false }),
            )
            .await?;
        }
        let device = |css: u32| (css as f64 * viewport.scale).round() as u32;
        page.call(
            "Page.startScreencast",
            json!({ "format": "jpeg", "quality": 85, "maxWidth": device(viewport.width), "maxHeight": device(viewport.height), "everyNthFrame": 1 }),
        )
        .await?;
        Ok(())
    }

    /// The panel closed or moved to another task.
    pub fn unwatch(&self, task: &str) {
        let mut inner = self.inner.lock();
        let Some(w) = inner.watch.take_if(|w| w.task == task) else { return };
        if let Some(r) = inner.running.as_ref() {
            r.cdp.send("Page.stopScreencast", json!({}), Some(&w.session));
        }
    }

    /// The user's own input, from the panel. Not awaited: a mouse move is
    /// one of many, and the next must not wait on this one's answer.
    pub fn input(&self, task: &str, event: &input::BrowserInput) -> Result<()> {
        let inner = self.inner.lock();
        let (Some(r), Some(tab)) = (inner.running.as_ref(), inner.tabs.get(task)) else {
            return Err(Error::Other("this task's tab is not open".into()));
        };
        if let Some((method, params)) = input::to_cdp(event) {
            r.cdp.send(method, params, Some(&tab.session));
        }
        Ok(())
    }

    /// Say what an agent did, for the panel (BRW-10).
    pub fn record<R: Runtime>(&self, app: &AppHandle<R>, task: &str, text: &str, at: Option<(f64, f64)>) {
        if let Some(tab) = self.inner.lock().tabs.get_mut(task) {
            tab.last_action = Some(AgentAction {
                text: text.to_string(),
                x: at.map(|a| a.0),
                y: at.map(|a| a.1),
                at: chrono::Utc::now().timestamp_millis(),
            });
        }
        changed(app, task);
    }

}

impl Browser {
    /// A frame of the watched tab: to the panel, and acknowledged so Chrome
    /// sends the next, no sooner than `FRAME_GAP` after the last (BRW-9).
    pub(super) fn on_frame(&self, e: Event) {
        let ack_id = e.params.get("sessionId").cloned().unwrap_or(Value::Null);
        let Some(data) = e.params.get("data").and_then(Value::as_str) else { return };
        let (frames, cdp, session, wait) = {
            let mut inner = self.inner.lock();
            let Some(cdp) = inner.running.as_ref().map(|r| r.cdp.clone()) else { return };
            // A frame of a tab no longer watched goes unanswered, and so is
            // the last Chrome sends of it.
            let Some(w) = inner.watch.as_mut().filter(|w| e.session.as_deref() == Some(w.session.as_str())) else {
                return;
            };
            let now = Instant::now();
            let wait = w.next.saturating_duration_since(now);
            w.next = now + wait + FRAME_GAP;
            (w.frames.clone(), cdp, w.session.clone(), wait)
        };
        if let Ok(jpeg) = base64::engine::general_purpose::STANDARD.decode(data) {
            let _ = frames.send(InvokeResponseBody::Raw(jpeg));
        }
        let ack = move || cdp.send("Page.screencastFrameAck", json!({ "sessionId": ack_id }), Some(&session));
        if wait.is_zero() {
            ack();
        } else {
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(wait).await;
                ack();
            });
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve, HOME};
    use crate::commands::AppState;
    use tauri::Manager;

    /// A JPEG's width and height, from its frame header.
    fn jpeg_size(jpeg: &[u8]) -> Option<(u16, u16)> {
        let at = jpeg.windows(2).position(|w| w[0] == 0xFF && (w[1] == 0xC0 || w[1] == 0xC2))?;
        let h = u16::from_be_bytes([*jpeg.get(at + 5)?, *jpeg.get(at + 6)?]);
        let w = u16::from_be_bytes([*jpeg.get(at + 7)?, *jpeg.get(at + 8)?]);
        Some((w, h))
    }

    /// The panel's way, against a real Chrome: watching the tab sends it
    /// JPEG frames at the panel's size, and the user's click and keys reach
    /// the page as real input.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_panel_is_sent_frames_and_its_input_reaches_the_page() {
        let Some((app, root, _)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(HOME).await;
        let (tx, frames) = std::sync::mpsc::channel::<Vec<u8>>();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                let _ = tx.send(bytes);
            }
            Ok(())
        });
        let viewport = Viewport { width: 640, height: 480, scale: 2.0 };
        state.browser.watch(app.handle(), "t1", viewport, channel).await.unwrap();
        let page = state.browser.page(app.handle(), "t1").await.unwrap();
        page.navigate(&format!("{base}/")).await.unwrap();
        page.settle(&state.browser).await;

        let jpeg = frames.recv_timeout(Duration::from_secs(10)).expect("a frame");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "a JPEG");
        assert_eq!(jpeg_size(&jpeg), Some((1280, 960)), "as sharp as the panel: a device pixel each");
        let size = page.eval("[innerWidth, innerHeight, devicePixelRatio]").await.unwrap();
        assert_eq!(size, json!([640, 480, 2]), "laid out at the panel's size");

        let field = page.eval("(() => { const r = document.querySelector('input').getBoundingClientRect(); return [r.x + 5, r.y + 5]; })()").await.unwrap();
        let (x, y) = (field[0].as_f64().unwrap(), field[1].as_f64().unwrap());
        for kind in ["mousePressed", "mouseReleased"] {
            let click = input::BrowserInput::Mouse {
                r#type: kind.into(), x, y, button: "left".into(), buttons: 1, click_count: 1, modifiers: 0,
            };
            state.browser.input("t1", &click).unwrap();
        }
        for (kind, text) in [("keyDown", Some("q")), ("keyUp", None)] {
            let k = input::BrowserInput::Key {
                r#type: kind.into(), key: "q".into(), code: "KeyQ".into(), key_code: 81,
                text: text.map(str::to_string), modifiers: 0, location: 0,
            };
            state.browser.input("t1", &k).unwrap();
        }
        state.browser.input("t1", &input::BrowserInput::Text { text: "rst".into() }).unwrap();
        let mut typed = Value::Null;
        for _ in 0..50 {
            typed = page.eval("document.querySelector('input').value").await.unwrap();
            if typed == "qrst" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(typed, "qrst");

        state.browser.unwatch("t1");
        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

}
