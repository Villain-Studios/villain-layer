//! The Browser panel's side of a tab (BRW-9, BRW-10): what it shows, the
//! frames it is sent, the user's own input, and what an agent last did.

use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager, Runtime};

use super::cdp::{Cdp, Event};
use super::{changed, input, Browser, Viewport};
use crate::error::{Error, Result};

/// The least time between two frames sent to the panel: 30 a second
/// (BRW-9). A video plays in the page at Chrome's rate, and each frame is
/// a JPEG crossing into the webview. At 15 a second, scrolling and typing
/// in the panel felt like lag; Chrome itself kept up with 60.
const FRAME_GAP: Duration = Duration::from_millis(16);

/// How long the page is still before a sharp picture of it is sent.
const STILL_AFTER: Duration = Duration::from_millis(150);

/// How often the panel's size is set again while frames come at another.
const RESIZE_AGAIN: Duration = Duration::from_millis(250);

/// How long the sharp frame may take before a picture is taken instead.
const SHARP_WAIT: Duration = Duration::from_millis(400);

/// Changes this close together are the page moving (a scroll, an
/// animation): `MOVING.0` of them within `MOVING.1`. One change on its own
/// is drawn sharp. A caret blinks twice a second, and with every change
/// counted as moving, text flipped between sharp and blurred with each
/// blink, and with each click.
const MOVING: (usize, Duration) = (3, Duration::from_millis(250));

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

/// A task's browser as the panel shows it: its active tab, and the rest.
#[derive(Clone, Debug, Default, Serialize)]
pub struct TabView {
    /// The active tab's session: a new one means another tab, or the tab
    /// made again, and the panel watches it afresh.
    pub tab: Option<String>,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub last_action: Option<AgentAction>,
    /// A dialog the page opened, waiting on an answer (BRW-13).
    pub dialog: Option<super::Dialog>,
    /// The user has taken the tab over (BRW-12).
    pub held: bool,
    /// Every tab, in order (BRW-16).
    pub tabs: Vec<super::TabInfo>,
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
    /// The panel's size, which frames are drawn at.
    viewport: Viewport,
    /// Frames are drawn sharp: the page is still.
    sharp: bool,
    /// The next frame is the one Chrome sends for a change of `sharp`, not
    /// for a change in the page.
    switching: bool,
    /// When the page will have been still long enough to be drawn sharp, if
    /// that is waited for.
    still_due: Option<Instant>,
    /// When the last changes came while sharp: only a run of them is the
    /// page moving (`MOVING`).
    recent: std::collections::VecDeque<Instant>,
    /// When the panel's size was last set again for a frame of another one.
    resized: Option<Instant>,
}

impl Browser {
    /// The task's tab for the panel; a default one when there is no tab.
    pub fn view(&self, task: &str) -> TabView {
        let tabs = self.tabs(task);
        let inner = self.inner.lock();
        let held = inner.held.contains(task);
        let driver = inner.drivers.get(task).cloned();
        let last_action = inner.tabs.get(task).and_then(|t| t.last_action.clone());
        inner
            .active(task)
            .map(|t| TabView {
                tab: Some(t.session.clone()),
                url: t.url.clone(),
                title: t.title.clone(),
                loading: t.loading,
                last_action: last_action.clone(),
                dialog: t.dialog.clone(),
                held,
                tabs: tabs.clone(),
                driver: driver.clone(),
            })
            .unwrap_or(TabView { held, driver, last_action, ..TabView::default() })
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
        {
            let mut inner = self.inner.lock();
            let tab = inner.active_mut(task).ok_or_else(|| Error::Other("the tab closed".into()))?;
            tab.viewport = viewport;
            let session = tab.session.clone();
            if let Some(old) = inner.watch.take().filter(|w| w.session != session) {
                if let Some(r) = inner.running.as_ref() {
                    r.cdp.send("Page.stopScreencast", json!({}), Some(&old.session));
                }
            }
            inner.watch = Some(Watch {
                task: task.to_string(),
                session,
                frames,
                next: Instant::now(),
                viewport,
                sharp: false,
                switching: false,
                still_due: None,
                recent: Default::default(),
                resized: None,
            });
        }
        // Every time, not only when the panel's size changed: a tab shown
        // again after another one in front of it closed kept its layout but
        // was drawn at Chrome's window size, and the panel stretched it, text
        // squeezed narrow and tall.
        page.call(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": viewport.width, "height": viewport.height, "deviceScaleFactor": viewport.scale, "mobile": false }),
        )
        .await?;
        page.call("Page.startScreencast", screencast(viewport, false)).await?;
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
        let mut inner = self.inner.lock();
        let (Some(cdp), Some(session)) =
            (inner.running.as_ref().map(|r| r.cdp.clone()), inner.active(task).map(|t| t.session.clone()))
        else {
            return Err(Error::Other("this task's tab is not open".into()));
        };
        // A scroll is about to move the page: fast frames from now, not a
        // sharp one first. Not a click or a key, which change a page once
        // and leave text to be read: made fast, each blurred the page for a
        // moment.
        let acts = matches!(event, input::BrowserInput::Wheel { .. });
        if let Some(w) = inner.watch.as_mut().filter(|w| acts && w.sharp && w.session == session) {
            w.sharp = false;
            w.switching = true;
            restart(&cdp, &session, w.viewport, false);
        }
        drop(inner);
        if let Some((method, params)) = input::to_cdp(event) {
            cdp.send(method, params, Some(&session));
        }
        Ok(())
    }

    /// Say what an agent did, for the panel (BRW-10).
    pub fn record<R: Runtime>(&self, app: &AppHandle<R>, task: &str, text: &str, at: Option<(f64, f64)>) {
        if let Some(tabs) = self.inner.lock().tabs.get_mut(task) {
            tabs.last_action = Some(AgentAction {
                text: text.to_string(),
                x: at.map(|a| a.0),
                y: at.map(|a| a.1),
                at: chrono::Utc::now().timestamp_millis(),
            });
        }
        changed(app, task);
    }

}

/// The screencast's settings: one pixel per CSS pixel while the page moves,
/// the screen's own sharpness once it is still.
///
/// Sharp all the time, a scroll took 40ms to reach the app where it now
/// takes about 15, at three times the bytes a frame: that was the lag in
/// scrolling and typing. A still page is what is read, and is drawn sharp.
fn screencast(v: Viewport, sharp: bool) -> Value {
    let scale = if sharp { v.scale } else { 1.0 };
    let device = |css: u32| (css as f64 * scale).round() as u32;
    json!({
        "format": "jpeg",
        "quality": if sharp { 90 } else { 80 },
        "maxWidth": device(v.width),
        "maxHeight": device(v.height),
        "everyNthFrame": 1,
    })
}

impl Browser {
    /// A frame of the watched tab: acknowledged first, so Chrome draws the
    /// next while this one crosses to the panel, then sent. The page moving
    /// again after being still is drawn at one pixel per CSS pixel until it
    /// stops (`sharpen`).
    pub(super) fn on_frame<R: Runtime>(&self, app: &AppHandle<R>, e: Event) {
        let ack_id = e.params.get("sessionId").cloned().unwrap_or(Value::Null);
        let Some(data) = e.params.get("data").and_then(Value::as_str) else { return };
        // The size of what Chrome drew, in CSS pixels.
        let drawn = |k: &str| e.params.pointer(&format!("/metadata/{k}")).and_then(Value::as_f64);
        let (drawn_w, drawn_h) = (drawn("deviceWidth"), drawn("deviceHeight"));
        let (frames, cdp, session, wait, soften, wait_still) = {
            let mut inner = self.inner.lock();
            let Some(cdp) = inner.running.as_ref().map(|r| r.cdp.clone()) else { return };
            // A frame of a tab no longer watched goes unanswered, and so is
            // the last Chrome sends of it.
            let Some(w) = inner.watch.as_mut().filter(|w| e.session.as_deref() == Some(w.session.as_str())) else {
                return;
            };
            let now = Instant::now();
            // Drawn at another size than the panel's: Chrome sometimes drops
            // the size a tab was given when it comes to the front again (after
            // the tab before it closed), and draws it at its window's size,
            // which the panel stretched out of shape. Not shown; the size is
            // set again, and the next frame is right.
            let off = |drawn: Option<f64>, want: u32| drawn.is_some_and(|d| (d - f64::from(want)).abs() > 1.5);
            if off(drawn_w, w.viewport.width) || off(drawn_h, w.viewport.height) {
                let v = w.viewport;
                let again = w.resized.is_none_or(|t| now.duration_since(t) > RESIZE_AGAIN);
                if again {
                    w.resized = Some(now);
                }
                let session = w.session.clone();
                drop(inner);
                cdp.send("Page.screencastFrameAck", json!({ "sessionId": ack_id }), Some(&session));
                if again {
                    cdp.send(
                        "Emulation.setDeviceMetricsOverride",
                        json!({ "width": v.width, "height": v.height, "deviceScaleFactor": v.scale, "mobile": false }),
                        Some(&session),
                    );
                }
                return;
            }
            let wait = w.next.saturating_duration_since(now);
            w.next = now + wait + FRAME_GAP;
            let moved = !std::mem::take(&mut w.switching);
            if moved && w.sharp {
                w.recent.push_back(now);
                while w.recent.front().is_some_and(|t| now.duration_since(*t) > MOVING.1) {
                    w.recent.pop_front();
                }
            }
            let soften = moved && w.sharp && w.recent.len() >= MOVING.0;
            if soften {
                w.recent.clear();
            }
            if soften {
                w.sharp = false;
                w.switching = true;
            }
            // Every frame drawn fast, whatever drew it, puts off the sharp
            // one: a change, or the user's input switching to fast frames
            // ahead of one. The sharp frame itself waits for nothing.
            let wait_still = !w.sharp && w.still_due.is_none();
            if !w.sharp {
                w.still_due = Some(now + STILL_AFTER);
            }
            (w.frames.clone(), cdp, w.session.clone(), wait, soften.then_some(w.viewport), wait_still)
        };
        let ack = {
            let (cdp, session) = (cdp.clone(), session.clone());
            move || cdp.send("Page.screencastFrameAck", json!({ "sessionId": ack_id }), Some(&session))
        };
        if wait.is_zero() {
            ack();
        } else {
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(wait).await;
                ack();
            });
        }
        if let Ok(jpeg) = base64::engine::general_purpose::STANDARD.decode(data) {
            let _ = frames.send(InvokeResponseBody::Raw(jpeg));
        }
        if let Some(v) = soften {
            restart(&cdp, &session, v, false);
        }
        if wait_still {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                app.state::<crate::commands::AppState>().browser.sharpen(&cdp, &session).await;
            });
        }
    }

    /// Once the page has been still for `STILL_AFTER`, draw it sharp. Not a
    /// screenshot: taking one makes Chrome draw a frame, which came at one
    /// pixel per CSS pixel straight after it and replaced it, every 150ms.
    async fn sharpen(&self, cdp: &Cdp, session: &str) {
        loop {
            let due = {
                let inner = self.inner.lock();
                match inner.watch.as_ref().filter(|w| w.session == session) {
                    Some(w) => w.still_due,
                    None => return,
                }
            };
            let Some(due) = due else { return };
            let now = Instant::now();
            if now < due {
                tokio::time::sleep(due - now).await;
                continue;
            }
            {
                let mut inner = self.inner.lock();
                let Some(w) = inner.watch.as_mut().filter(|w| w.session == session) else { return };
                w.still_due = None;
                if w.sharp {
                    return;
                }
                w.sharp = true;
                w.switching = true;
                restart(cdp, session, w.viewport, true);
            }
            return self.make_sure_sharp(cdp, session).await;
        }
    }

    /// A restarted screencast sends a frame of a still page, but not always:
    /// on a busy machine none came, and the page stayed blurred until it
    /// next changed. Then a picture of it is taken instead. The frame that
    /// taking it draws is the one `switching` waits for, so it does not
    /// start the page moving again.
    async fn make_sure_sharp(&self, cdp: &Cdp, session: &str) {
        tokio::time::sleep(SHARP_WAIT).await;
        let missing = |inner: &super::Inner| {
            inner.watch.as_ref().is_some_and(|w| w.session == session && w.sharp && w.switching)
        };
        if !missing(&self.inner.lock()) {
            return;
        }
        let Some(v) = self.inner.lock().watch.as_ref().map(|w| w.viewport) else { return };
        // The panel's part of the page, where it is scrolled to: without a
        // clip, Chrome takes its own window's size, and the panel stretched
        // that out of shape.
        let Ok(m) = cdp.call("Page.getLayoutMetrics", json!({}), Some(session)).await else { return };
        let at = |k: &str| m.pointer(&format!("/cssVisualViewport/{k}")).and_then(Value::as_f64).unwrap_or(0.0);
        let clip = json!({ "x": at("pageX"), "y": at("pageY"), "width": v.width, "height": v.height, "scale": 1 });
        let Ok(shot) = cdp
            .call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 90, "clip": clip }), Some(session))
            .await
        else {
            return;
        };
        let inner = self.inner.lock();
        let Some(w) = inner.watch.as_ref().filter(|w| w.session == session && w.sharp) else { return };
        if let Some(jpeg) = shot
            .get("data")
            .and_then(Value::as_str)
            .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
        {
            let _ = w.frames.send(InvokeResponseBody::Raw(jpeg));
        }
    }
}

/// The screencast again with other settings. Chrome sends a frame as it
/// starts, which is the one `switching` expects.
fn restart(cdp: &Cdp, session: &str, v: Viewport, sharp: bool) {
    cdp.send("Page.stopScreencast", json!({}), Some(session));
    cdp.send("Page.startScreencast", screencast(v, sharp), Some(session));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve, HOME};
    use crate::commands::AppState;
    use tauri::Manager;

    const HEAVY: &str = r#"<html><head><title>Heavy</title><style>
        body{font:15px -apple-system,sans-serif;margin:0} .card{margin:12px;padding:16px;border-radius:8px;box-shadow:0 2px 8px #0003;background:linear-gradient(135deg,#fafafa,#e8eefc)}
        .row{display:flex;gap:8px} .pill{padding:4px 10px;border-radius:12px;background:#4f46e5;color:#fff}
    </style></head><body><script>
        let h=''; for(let i=0;i<400;i++){h+=`<div class=card><h3>Booking ${i}</h3><div class=row><span class=pill>Flight</span><span class=pill>Hotel</span></div><p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.</p><table><tr><td>Traveler</td><td>Ada Lovelace</td><td>SEK 12 400</td></tr></table></div>`}
        document.body.innerHTML=h;
    </script></body></html>"#;

    /// Against a real Chrome: a caret blinking in a field changes the page
    /// twice a second, and a click changes it once, and the page stays sharp
    /// through both. Each blink and each click counted as the page moving,
    /// and the panel flipped between sharp and blurred text.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_blinking_caret_or_a_click_does_not_blur_the_page() {
        let Some((app, root, _)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(r#"<html><head><title>Field</title></head><body><p>Some text to read</p><input autofocus value="typed"></body></html>"#).await;
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
        page.eval("document.querySelector('input').focus()").await.unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        while frames.try_recv().is_ok() {}

        let mut sizes = Vec::new();
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if let Ok(jpeg) = frames.recv_timeout(Duration::from_millis(100)) {
                sizes.push(jpeg_size(&jpeg));
            }
        }
        assert!(sizes.len() >= 3, "the caret blinks: {sizes:?}");
        assert!(sizes.iter().all(|s| *s == Some((1280, 960))), "sharp through every blink: {sizes:?}");

        // Nor does a click: it changes the page once, and text stays sharp.
        for kind in ["mousePressed", "mouseReleased"] {
            let click = input::BrowserInput::Mouse {
                r#type: kind.into(), x: 20.0, y: 20.0, button: "left".into(), buttons: 1, click_count: 1, modifiers: 0,
            };
            state.browser.input("t1", &click).unwrap();
        }
        let mut after = Vec::new();
        let until = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < until {
            if let Ok(jpeg) = frames.recv_timeout(Duration::from_millis(100)) {
                after.push(jpeg_size(&jpeg));
            }
        }
        assert!(!after.is_empty() && after.iter().all(|s| *s == Some((1280, 960))), "sharp after a click: {after:?}");

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Against a real Chrome: a tab shown again after the one in front of it
    /// closed is drawn at the panel's size. It kept the panel's layout but
    /// was drawn at Chrome's window size, and the panel stretched it: text
    /// squeezed narrow and tall.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tab_shown_again_after_another_closes_is_drawn_at_the_panel_s_size() {
        let Some((app, root, _)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(HOME).await;
        let (tx, frames) = std::sync::mpsc::channel::<Vec<u8>>();
        let channel = || {
            let tx = tx.clone();
            Channel::new(move |body| {
                if let InvokeResponseBody::Raw(b) = body {
                    let _ = tx.send(b);
                }
                Ok(())
            })
        };
        let panel = Viewport { width: 900, height: 910, scale: 2.0 };
        state.browser.watch(app.handle(), "t1", panel, channel()).await.unwrap();
        let page = state.browser.page(app.handle(), "t1").await.unwrap();
        page.navigate(&format!("{base}/")).await.unwrap();
        page.settle(&state.browser).await;

        // An agent opens a tab and closes it; the panel watches whatever is
        // active each time, as it does when the active tab changes.
        let other = state.browser.new_tab(app.handle(), "t1", Some(&format!("{base}/two"))).await.unwrap();
        other.settle(&state.browser).await;
        state.browser.watch(app.handle(), "t1", panel, channel()).await.unwrap();
        let id = state.browser.tabs("t1").iter().find(|t| t.active).unwrap().id.clone();
        state.browser.close_tab(app.handle(), "t1", &id).await.unwrap();
        state.browser.watch(app.handle(), "t1", panel, channel()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;

        let mut newest = None;
        while let Ok(f) = frames.try_recv() {
            newest = jpeg_size(&f);
        }
        assert_eq!(newest, Some((1800, 1820)), "drawn at the panel's size, sharp");

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A measurement rather than a test, like `echo_latency`: how long a
    /// scroll takes to reach the app as a frame, the frame rate and size
    /// while scrolling, and the frames once it stops, on a long page at a
    /// panel's size. Run it with
    /// `cargo test --lib scroll_latency -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn scroll_latency() {
        let Some((app, root, _)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(HEAVY).await;
        let (tx, frames) = std::sync::mpsc::channel::<(Instant, usize)>();
        let channel = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                let _ = tx.send((Instant::now(), bytes.len()));
            }
            Ok(())
        });
        let viewport = Viewport { width: 930, height: 960, scale: 2.0 };
        state.browser.watch(app.handle(), "t1", viewport, channel).await.unwrap();
        let page = state.browser.page(app.handle(), "t1").await.unwrap();
        page.navigate(&format!("{base}/")).await.unwrap();
        page.settle(&state.browser).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        while frames.try_recv().is_ok() {}

        // One wheel tick at a time: how long until a frame comes.
        let mut lat = Vec::new();
        for _ in 0..20 {
            while frames.try_recv().is_ok() {}
            let t = Instant::now();
            let wheel = input::BrowserInput::Wheel { x: 400.0, y: 400.0, dx: 0.0, dy: 120.0, modifiers: 0 };
            state.browser.input("t1", &wheel).unwrap();
            if let Ok((at, _)) = frames.recv_timeout(Duration::from_secs(2)) {
                lat.push(at.duration_since(t).as_millis());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        lat.sort();
        // A continuous scroll, as a trackpad: a wheel every 16ms for 2s.
        while frames.try_recv().is_ok() {}
        let t0 = Instant::now();
        let mut n = 0; let mut bytes = 0;
        while t0.elapsed() < Duration::from_secs(2) {
            let wheel = input::BrowserInput::Wheel { x: 400.0, y: 400.0, dx: 0.0, dy: 30.0, modifiers: 0 };
            state.browser.input("t1", &wheel).unwrap();
            tokio::time::sleep(Duration::from_millis(16)).await;
            while let Ok((_, b)) = frames.try_recv() { n += 1; bytes += b; }
        }
        let stop = Instant::now();
        let mut idle = Vec::new();
        while stop.elapsed() < Duration::from_millis(1500) {
            if let Ok((at, b)) = frames.recv_timeout(Duration::from_millis(100)) {
                idle.push((at.duration_since(stop).as_millis(), b / 1024));
            }
        }
        eprintln!("scroll latency ms median {} p90 {} | scroll fps {} avg {} KB | after stopping (ms, KB): {:?}",
            lat.get(lat.len() / 2).copied().unwrap_or(0), lat.get(lat.len() * 9 / 10).copied().unwrap_or(0), n / 2, bytes / n.max(1) / 1024, idle);
        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

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

        // Frames while the page loads are fast ones, at a pixel per CSS
        // pixel; the settled page comes sharp, however loaded the machine.
        let mut seen = Vec::new();
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            let Ok(jpeg) = frames.recv_timeout(Duration::from_millis(200)) else { continue };
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "a JPEG");
            seen.push(jpeg_size(&jpeg));
            if seen.last() == Some(&Some((1280, 960))) {
                break;
            }
        }
        assert_eq!(seen.last(), Some(&Some((1280, 960))), "as sharp as the panel, a device pixel each: {seen:?}");
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
