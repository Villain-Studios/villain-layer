//! One browser for the app, one tab in it per task (§18 in features.md).
//!
//! Chrome runs headless with a profile of its own (BRW-1), started the first
//! time anything needs a tab and stopped with the app (BRW-7). Agents drive
//! their task's tab through the tools in `tools.rs`.

mod cdp;
mod chrome;
pub mod input;
mod keys;
mod page;
pub mod sites;
mod snapshot;
pub mod tools;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::AppState;
use crate::error::{Error, Result};
use cdp::{Cdp, Event};
pub use page::Page;

/// Whether there is a Chrome to run. Reads the disk.
pub fn chrome_installed() -> bool {
    chrome::find().is_some()
}

/// Lines of a tab's console kept for `browser_console`.
const CONSOLE_LINES: usize = 200;

/// The least time between two frames sent to the panel: 15 a second
/// (BRW-9). A video plays in the page at Chrome's rate, and each frame is
/// a JPEG crossing into the webview.
const FRAME_GAP: Duration = Duration::from_millis(66);

/// A tab's size, in CSS pixels, and how many device pixels each is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self { width: 1280, height: 800, scale: 1.0 }
    }
}

struct Running {
    cdp: Arc<Cdp>,
    process: Arc<Mutex<chrome::Process>>,
    /// Chrome's own, without the "Headless" that some sites refuse.
    user_agent: Option<String>,
}

struct Tab {
    target: String,
    session: String,
    url: String,
    title: String,
    loading: bool,
    viewport: Viewport,
    console: VecDeque<String>,
    last_action: Option<AgentAction>,
}

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
}

/// The tab on screen in the panel, which alone is sent frames (BRW-9).
struct Watch {
    task: String,
    session: String,
    frames: Channel<InvokeResponseBody>,
    /// When the next frame may go.
    next: Instant,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// By task id.
    tabs: HashMap<String, Tab>,
    watch: Option<Watch>,
}

/// Tell the panel a task's tab changed; an empty task is every tab.
fn changed<R: Runtime>(app: &AppHandle<R>, task: &str) {
    let _ = app.emit("browser:changed", task);
}

#[derive(Default)]
pub struct Browser {
    inner: Mutex<Inner>,
    /// Held while Chrome starts or a tab is made, so two first calls at once
    /// make one browser and one tab.
    making: tokio::sync::Mutex<()>,
}

impl Browser {
    /// The task's tab, made (and Chrome started) if there is none yet.
    pub async fn page<R: Runtime>(&self, app: &AppHandle<R>, task: &str) -> Result<Page> {
        if let Some(page) = self.existing(task) {
            return Ok(page);
        }
        let _making = self.making.lock().await;
        if let Some(page) = self.existing(task) {
            return Ok(page);
        }
        let (cdp, user_agent) = self.running(app).await?;
        let made = make_tab(&cdp, user_agent.as_deref(), Viewport::default()).await?;
        let page = Page::new(cdp.clone(), made.session.clone(), task.to_string());
        self.inner.lock().tabs.insert(task.to_string(), made);
        changed(app, task);

        // Where it was when the app last closed (BRW-6).
        let saved = app.state::<AppState>().config.task(task).ok().and_then(|t| t.browser_url);
        if let Some(url) = saved.filter(|u| sites::host_of(u).is_some()) {
            page.send("Page.navigate", json!({ "url": url }));
        }
        Ok(page)
    }

    fn existing(&self, task: &str) -> Option<Page> {
        let inner = self.inner.lock();
        let running = inner.running.as_ref().filter(|r| !r.cdp.is_closed())?;
        let tab = inner.tabs.get(task)?;
        Some(Page::new(running.cdp.clone(), tab.session.clone(), task.to_string()))
    }

    /// The browser, started if it is not running.
    async fn running<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(Arc<Cdp>, Option<String>)> {
        if let Some(r) = self.inner.lock().running.as_ref().filter(|r| !r.cdp.is_closed()) {
            return Ok((r.cdp.clone(), r.user_agent.clone()));
        }
        let binary = chrome::find().ok_or_else(|| Error::Other(chrome::MISSING.into()))?;
        let profile = app.state::<AppState>().config.folder().join("browser");
        let (process, to_chrome, from_chrome) =
            crate::commands::off_runtime(move || chrome::launch(&binary, &profile)).await??;
        let process = Arc::new(Mutex::new(process));

        let (events, closes) = (app.clone(), app.clone());
        let cdp = Cdp::start(
            from_chrome,
            to_chrome,
            move |e| events.state::<AppState>().browser.on_event(&events, e),
            move || closes.state::<AppState>().browser.lost(&closes),
        )?;
        let version = cdp.call("Browser.getVersion", json!({}), None).await;
        let user_agent = version
            .ok()
            .and_then(|v| v.get("userAgent").and_then(Value::as_str).map(|ua| ua.replace("HeadlessChrome", "Chrome")));
        // For targetInfoChanged, which is how a tab's title is heard.
        cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;

        let old = {
            let mut inner = self.inner.lock();
            inner.tabs.clear();
            inner.watch = None;
            inner.running.replace(Running { cdp: cdp.clone(), process, user_agent: user_agent.clone() })
        };
        // One whose end of the pipe closed, but whose reader had not said so
        // yet when this one started.
        if let Some(old) = old {
            std::thread::spawn(move || old.process.lock().stop(Duration::ZERO));
        }
        Ok((cdp, user_agent))
    }

    /// Chrome went away (crashed, killed, or closed by `shutdown`). Its tabs
    /// went with it; the next call starts it again (BRW-7).
    fn lost<R: Runtime>(&self, app: &AppHandle<R>) {
        let process = {
            let mut inner = self.inner.lock();
            let dead = inner.running.as_ref().is_some_and(|r| r.cdp.is_closed());
            if !dead {
                return;
            }
            inner.tabs.clear();
            inner.watch = None;
            inner.running.take().map(|r| r.process)
        };
        changed(app, "");
        // Reaped on the reading thread that called this, which waits on
        // nothing else any more.
        if let Some(p) = process {
            p.lock().stop(Duration::ZERO);
        }
    }

    /// Close a task's tab, when the task is deleted or finished (BRW-6).
    pub fn close_task(&self, task: &str) {
        let mut inner = self.inner.lock();
        if inner.watch.as_ref().is_some_and(|w| w.task == task) {
            inner.watch = None;
        }
        let Some(tab) = inner.tabs.remove(task) else { return };
        if let Some(r) = inner.running.as_ref() {
            r.cdp.send("Target.closeTarget", json!({ "targetId": tab.target }), None);
        }
    }

    /// Ask Chrome to close, then make sure it has (BRW-7). Blocking: called
    /// as the app exits.
    pub fn shutdown(&self, grace: Duration) {
        let running = self.inner.lock().running.take();
        let Some(r) = running else { return };
        r.cdp.send("Browser.close", json!({}), None);
        r.process.lock().stop(grace);
    }

    /// The task's tab for the panel; a default one when there is no tab.
    pub fn view(&self, task: &str) -> TabView {
        let inner = self.inner.lock();
        inner
            .tabs
            .get(task)
            .map(|t| TabView {
                tab: Some(t.session.clone()),
                url: t.url.clone(),
                title: t.title.clone(),
                loading: t.loading,
                last_action: t.last_action.clone(),
            })
            .unwrap_or_default()
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
            json!({ "format": "jpeg", "quality": 75, "maxWidth": device(viewport.width), "maxHeight": device(viewport.height), "everyNthFrame": 1 }),
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

    /// The tab's address, title and whether it is loading, as last heard.
    pub fn state_of(&self, task: &str) -> Option<(String, String, bool)> {
        let inner = self.inner.lock();
        inner.tabs.get(task).map(|t| (t.url.clone(), t.title.clone(), t.loading))
    }

    pub fn viewport(&self, task: &str) -> Viewport {
        self.inner.lock().tabs.get(task).map(|t| t.viewport).unwrap_or_default()
    }

    /// The last lines the page logged, oldest first; emptied when `clear`.
    pub fn console(&self, task: &str, clear: bool) -> Vec<String> {
        let mut inner = self.inner.lock();
        let Some(tab) = inner.tabs.get_mut(task) else { return Vec::new() };
        let lines = tab.console.iter().cloned().collect();
        if clear {
            tab.console.clear();
        }
        lines
    }

    fn task_of_session(inner: &Inner, session: &str) -> Option<String> {
        inner.tabs.iter().find(|(_, t)| t.session == session).map(|(k, _)| k.clone())
    }

    fn task_of_target(inner: &Inner, target: &str) -> Option<String> {
        inner.tabs.iter().find(|(_, t)| t.target == target).map(|(k, _)| k.clone())
    }

    /// One event from Chrome, on its reading thread: nothing here waits.
    fn on_event<R: Runtime>(&self, app: &AppHandle<R>, e: Event) {
        if e.method == "Page.screencastFrame" {
            return self.on_frame(e);
        }
        let mut save: Option<(String, String)> = None;
        let mut tell: Option<String> = None;
        {
            let mut inner = self.inner.lock();
            let task = match e.session.as_deref() {
                Some(s) => Self::task_of_session(&inner, s),
                None => e
                    .params
                    .pointer("/targetInfo/targetId")
                    .or_else(|| e.params.get("targetId"))
                    .and_then(Value::as_str)
                    .and_then(|t| Self::task_of_target(&inner, t)),
            };
            let Some(task) = task else { return };
            let before = inner.tabs.get(&task).map(|t| (t.url.clone(), t.title.clone(), t.loading));

            match e.method.as_str() {
                "Page.frameNavigated" if e.params.pointer("/frame/parentId").is_none() => {
                    let url = e.params.pointer("/frame/url").and_then(Value::as_str).unwrap_or_default();
                    if let Some(tab) = inner.tabs.get_mut(&task) {
                        if tab.url != url {
                            tab.url = url.to_string();
                            if sites::host_of(url).is_some() {
                                save = Some((task.clone(), url.to_string()));
                            }
                        }
                    }
                }
                "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                    let frame = e.params.get("frameId").and_then(Value::as_str);
                    if let Some(tab) = inner.tabs.get_mut(&task).filter(|t| Some(t.target.as_str()) == frame) {
                        tab.loading = e.method == "Page.frameStartedLoading";
                    }
                }
                "Target.targetInfoChanged" => {
                    if let Some(tab) = inner.tabs.get_mut(&task) {
                        if let Some(title) = e.params.pointer("/targetInfo/title").and_then(Value::as_str) {
                            tab.title = title.to_string();
                        }
                    }
                }
                "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" => {
                    if let Some(tab) = inner.tabs.get_mut(&task) {
                        tab.console.push_back(console_line(&e.method, &e.params));
                        while tab.console.len() > CONSOLE_LINES {
                            tab.console.pop_front();
                        }
                    }
                }
                "Target.targetDestroyed" | "Target.targetCrashed" | "Target.detachedFromTarget" => {
                    inner.tabs.remove(&task);
                }
                _ => {}
            }
            let after = inner.tabs.get(&task).map(|t| (t.url.clone(), t.title.clone(), t.loading));
            if before != after {
                tell = Some(task);
            }
        }
        if let Some(task) = tell {
            changed(app, &task);
        }
        // Outside the lock: a config write waits on the disk.
        if let Some((task, url)) = save {
            let _ = app.state::<AppState>().config.update(|c| {
                if let Some(t) = c.tasks.iter_mut().find(|t| t.id == task) {
                    t.browser_url = Some(url);
                }
            });
        }
    }
}

impl Browser {
    /// A frame of the watched tab: to the panel, and acknowledged so Chrome
    /// sends the next, no sooner than `FRAME_GAP` after the last (BRW-9).
    fn on_frame(&self, e: Event) {
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

/// A new tab, attached, with what every tab needs switched on.
async fn make_tab(cdp: &Arc<Cdp>, user_agent: Option<&str>, viewport: Viewport) -> Result<Tab> {
    let made = cdp.call("Target.createTarget", json!({ "url": "about:blank" }), None).await?;
    let target = made
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("the browser made no tab".into()))?
        .to_string();
    let attached = cdp
        .call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None)
        .await?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("the browser did not attach to its tab".into()))?
        .to_string();

    let s = Some(session.as_str());
    cdp.call("Page.enable", json!({}), s).await?;
    cdp.call("Runtime.enable", json!({}), s).await?;
    // A headless page is never the focused window, so without this it
    // never sees focus: no caret, no :focus styles, and some forms wait for
    // a focus event before they accept input.
    cdp.call("Emulation.setFocusEmulationEnabled", json!({ "enabled": true }), s).await?;
    if let Some(ua) = user_agent {
        cdp.call("Emulation.setUserAgentOverride", json!({ "userAgent": ua }), s).await?;
    }
    set_viewport(cdp, &session, viewport).await?;
    Ok(Tab {
        target,
        session,
        url: "about:blank".into(),
        title: String::new(),
        loading: false,
        viewport,
        console: VecDeque::new(),
        last_action: None,
    })
}

async fn set_viewport(cdp: &Cdp, session: &str, v: Viewport) -> Result<()> {
    cdp.call(
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": v.width, "height": v.height, "deviceScaleFactor": v.scale, "mobile": false }),
        Some(session),
    )
    .await
    .map(|_| ())
}

/// One console message or uncaught error, as one line.
fn console_line(method: &str, p: &Value) -> String {
    if method == "Runtime.exceptionThrown" {
        let d = p.get("exceptionDetails").cloned().unwrap_or(Value::Null);
        let text = d
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| d.get("text").and_then(Value::as_str))
            .unwrap_or("uncaught error");
        return format!("[error] {}", text.lines().take(6).collect::<Vec<_>>().join("\n  "));
    }
    let kind = p.get("type").and_then(Value::as_str).unwrap_or("log");
    let args: Vec<String> = p
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| match v.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => v.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let line = args.join(" ");
    let clipped: String = line.chars().take(2000).collect();
    format!("[{kind}] {clipped}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ConfigStore, Task};
    use crate::pty::{PaneKind, SpawnOptions};
    use tauri::test::MockRuntime;

    pub(super) const HOME: &str = r#"<html><head><title>Home</title></head><body>
        <h1>Hello</h1>
        <a href="/two">Next page</a>
        <input aria-label="User name">
        <select aria-label="Size"><option value="s">Small</option><option value="l">Large</option></select>
        <button onclick="console.log('saved', document.querySelector('input').value); document.title = 'Saved ' + event.isTrusted">Save</button>
    </body></html>"#;

    /// This machine's page `home` at `/`, and a second page at `/two`.
    pub(super) async fn serve(home: &'static str) -> String {
        use axum::{response::Html, routing::get, Router};
        let router = Router::new()
            .route("/", get(move || async move { Html(home) }))
            .route("/two", get(|| async { Html("<html><head><title>Two</title></head><body><p>The second page</p></body></html>") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move { axum::serve(listener, router).await });
        base
    }

    /// An app with one task, `t1`, and an agent's pane in it: the app, its
    /// folder, and the pane. None where no Chrome is installed, which skips
    /// the test that asked.
    pub(super) fn app_with_task() -> Option<(tauri::App<MockRuntime>, std::path::PathBuf, String)> {
        if chrome::find().is_none() {
            eprintln!("skipped: no Chrome installed");
            return None;
        }
        let root = std::env::temp_dir().join(format!("vl-browser-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task = Task {
            id: "t1".into(),
            name: "Try the form".into(),
            root: root.to_string_lossy().to_string(),
            branch: "t1".into(),
            issue_key: None,
            issue_url: None,
            created_at: chrono::Utc::now(),
            ticket_stage: None,
            chat: None,
            review: None,
            browser_url: None,
        };
        let cfg = AppConfig { tasks: vec![task], ..Default::default() };
        let app = tauri::test::mock_app();
        app.manage(AppState {
            config: ConfigStore::for_tests(root.join("config.json"), cfg),
            ptys: crate::pty::PtyManager::default(),
            jira_types: Default::default(),
            epic_field_missing: Default::default(),
            pending_notices: Default::default(),
            status_cache: Default::default(),
            news: Default::default(),
            messages: crate::messages::Messages::for_tests(root.join("messages.json")),
            notes: crate::notes::Notes::load(&root),
            browser: Default::default(),
        });
        let pane = app
            .state::<AppState>()
            .ptys
            .spawn(
                app.handle(),
                SpawnOptions {
                    task_id: "t1".into(),
                    checkout_id: None,
                    cwd: root.to_string_lossy().to_string(),
                    kind: PaneKind::Agent,
                    title: "Agent".into(),
                    program: "/bin/sleep".into(),
                    args: vec!["60".into()],
                    agent_id: Some("claude".into()),
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
            .id;
        Some((app, root, pane))
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

    #[test]
    fn a_console_message_is_one_line_with_its_kind() {
        let p = json!({ "type": "warning", "args": [{ "type": "string", "value": "slow" }, { "type": "number", "value": 3 }] });
        assert_eq!(console_line("Runtime.consoleAPICalled", &p), "[warning] slow 3");
        let e = json!({ "exceptionDetails": { "text": "Uncaught", "exception": { "description": "TypeError: x is undefined\n    at f (a.js:1)" } } });
        assert_eq!(console_line("Runtime.exceptionThrown", &e), "[error] TypeError: x is undefined\n      at f (a.js:1)");
    }
}
