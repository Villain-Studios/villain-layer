//! One browser for the app, a set of tabs in it per task (§18 in
//! features.md).
//!
//! Chrome runs headless with a profile of its own (BRW-1), started the first
//! time anything needs a tab and stopped with the app (BRW-7). Agents drive
//! their task's tab through the tools in `tools.rs`.

mod cdp;
mod chrome;
mod control;
mod events;
pub mod input;
mod keys;
mod page;
mod panel;
mod requests;
mod session;
pub mod sign_in;
pub mod sites;
mod snapshot;
mod tabs;
pub mod tools;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::AppState;
use crate::error::{Error, Result};
use cdp::Cdp;
pub use control::{Dialog, Driver};
pub use page::Page;
pub use panel::AgentAction;
pub use requests::SiteRequest;
pub use tabs::TabInfo;
use tabs::Tabs;
use panel::Watch;
use requests::Asked;

/// Whether there is a Chrome to run. Reads the disk.
pub fn chrome_installed() -> bool {
    chrome::find().is_some()
}

/// Lines of a tab's console kept for `browser_console`.
const CONSOLE_LINES: usize = 200;

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
    dialog: Option<Dialog>,
    /// The tab whose page opened this one (BRW-14), by its target id: it is
    /// active again when this one closes.
    opener: Option<String>,
    /// The password field the app filled a saved password into (BRW-19),
    /// until the page navigates.
    secret: Option<i64>,
    /// How many screencasts the panel started on this tab. Chrome numbers
    /// them the same way, and every frame carries its screencast's number.
    screencasts: i64,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// By task id.
    tabs: HashMap<String, Tabs>,
    watch: Option<Watch>,
    requests: Vec<Asked>,
    next_request: u64,
    /// Tasks whose tab the user has taken over (BRW-12). Kept apart from
    /// the tabs, so a tab made again is still held.
    held: HashSet<String>,
    /// The agent using each task's tab, or that last did (BRW-15).
    drivers: HashMap<String, control::Driver>,
    /// Windows pages opened, being attached as tabs (BRW-14).
    adopting: u32,
}

impl Inner {
    /// The task's active tab, which everything acts on (BRW-16).
    fn active(&self, task: &str) -> Option<&Tab> {
        self.tabs.get(task)?.active()
    }

    fn active_mut(&mut self, task: &str) -> Option<&mut Tab> {
        self.tabs.get_mut(task)?.active_mut()
    }
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
    /// Rung when any tab opens a dialog (`until_dialog`).
    dialog_opened: tokio::sync::Notify,
}

impl Browser {
    /// The task's active tab, its tabs made (and Chrome started) if it has
    /// none yet: those it had when the app last closed (BRW-6), or one
    /// empty one.
    pub async fn page<R: Runtime>(&self, app: &AppHandle<R>, task: &str) -> Result<Page> {
        if let Some(page) = self.existing(task) {
            return Ok(page);
        }
        let _making = self.making.lock().await;
        if let Some(page) = self.existing(task) {
            return Ok(page);
        }
        let (cdp, user_agent) = self.running(app).await?;
        let saved = app.state::<AppState>().config.task(task).map(|t| t.saved_tabs()).unwrap_or_default();
        let urls = if saved.urls.is_empty() { vec![String::new()] } else { saved.urls };
        let mut list = Vec::with_capacity(urls.len());
        for url in &urls {
            let mut tab = make_tab(&cdp, user_agent.as_deref(), Viewport::default()).await?;
            if sites::host_of(url).is_some() {
                cdp.send("Page.navigate", json!({ "url": url }), Some(&tab.session));
                tab.url = url.clone();
            }
            list.push(tab);
        }
        let tabs = Tabs::new(list, saved.active);
        let page = tabs
            .active()
            .map(|t| Page::new(cdp.clone(), t.session.clone(), task.to_string()))
            .ok_or_else(|| Error::Other("the browser made no tab".into()))?;
        self.inner.lock().tabs.insert(task.to_string(), tabs);
        changed(app, task);
        Ok(page)
    }

    fn existing(&self, task: &str) -> Option<Page> {
        let inner = self.inner.lock();
        let running = inner.running.as_ref().filter(|r| !r.cdp.is_closed())?;
        let tab = inner.active(task)?;
        Some(Page::new(running.cdp.clone(), tab.session.clone(), task.to_string()))
    }

    /// The browser, started if it is not running.
    async fn running<R: Runtime>(&self, app: &AppHandle<R>) -> Result<(Arc<Cdp>, Option<String>)> {
        if let Some(r) = self.inner.lock().running.as_ref().filter(|r| !r.cdp.is_closed()) {
            return Ok((r.cdp.clone(), r.user_agent.clone()));
        }
        let started = match self.start(app).await {
            Ok(r) => r,
            // A Chrome that went away as it started, before it answered
            // anything: once more. One did whenever a banner was posted from
            // this process at that moment, and the app posts banners when it
            // likes.
            Err(_) => self.start(app).await?,
        };
        let (cdp, user_agent) = (started.cdp.clone(), started.user_agent.clone());
        let old = {
            let mut inner = self.inner.lock();
            inner.tabs.clear();
            inner.watch = None;
            inner.running.replace(started)
        };
        // One whose end of the pipe closed, but whose reader had not said so
        // yet when this one started.
        if let Some(old) = old {
            std::thread::spawn(move || old.process.lock().stop(Duration::ZERO));
        }
        Ok((cdp, user_agent))
    }

    /// Launch Chrome and see that it answers. One that does not is stopped
    /// here: nothing else knows of it to reap it.
    async fn start<R: Runtime>(&self, app: &AppHandle<R>) -> Result<Running> {
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
        let handshake = async {
            let version = cdp.call("Browser.getVersion", json!({}), None).await?;
            // For targetInfoChanged, which is how a tab's title is heard.
            cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;
            Ok::<_, Error>(
                version.get("userAgent").and_then(Value::as_str).map(|ua| ua.replace("HeadlessChrome", "Chrome")),
            )
        };
        match handshake.await {
            Ok(user_agent) => Ok(Running { cdp, process, user_agent }),
            Err(e) => {
                let gone = process.clone();
                crate::commands::off_runtime(move || gone.lock().stop(Duration::ZERO)).await?;
                Err(e)
            }
        }
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

    /// Close a task's tabs, when the task is deleted or finished (BRW-6).
    pub fn close_task(&self, task: &str) {
        let mut inner = self.inner.lock();
        if inner.watch.as_ref().is_some_and(|w| w.task == task) {
            inner.watch = None;
        }
        let Some(tabs) = inner.tabs.remove(task) else { return };
        let Some(r) = inner.running.as_ref() else { return };
        for t in tabs.list {
            r.cdp.send("Target.closeTarget", json!({ "targetId": t.target }), None);
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

    /// The tab's address, title and whether it is loading, as last heard.
    pub fn state_of(&self, task: &str) -> Option<(String, String, bool)> {
        let inner = self.inner.lock();
        inner.active(task).map(|t| (t.url.clone(), t.title.clone(), t.loading))
    }

    pub fn viewport(&self, task: &str) -> Viewport {
        self.inner.lock().active(task).map(|t| t.viewport).unwrap_or_default()
    }

    /// The last lines the page logged, oldest first; emptied when `clear`.
    pub fn console(&self, task: &str, clear: bool) -> Vec<String> {
        let mut inner = self.inner.lock();
        let Some(tab) = inner.active_mut(task) else { return Vec::new() };
        let lines = tab.console.iter().cloned().collect();
        if clear {
            tab.console.clear();
        }
        lines
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
    attach(cdp, target, user_agent, viewport).await
}

/// A page Chrome has, as one of the app's tabs: the app's own new tab, or
/// a window a page opened (BRW-14).
async fn attach(cdp: &Arc<Cdp>, target: String, user_agent: Option<&str>, viewport: Viewport) -> Result<Tab> {
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
        dialog: None,
        opener: None,
        secret: None,
        screencasts: 0,
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
        Some(app_in_task())
    }

    /// `app_with_task`, for a test that needs no browser.
    pub(super) fn app_in_task() -> (tauri::App<MockRuntime>, std::path::PathBuf, String) {
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
            browser: None,
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
            loops: Default::default(),
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
        (app, root, pane)
    }

    #[test]
    fn a_console_message_is_one_line_with_its_kind() {
        let p = json!({ "type": "warning", "args": [{ "type": "string", "value": "slow" }, { "type": "number", "value": 3 }] });
        assert_eq!(console_line("Runtime.consoleAPICalled", &p), "[warning] slow 3");
        let e = json!({ "exceptionDetails": { "text": "Uncaught", "exception": { "description": "TypeError: x is undefined\n    at f (a.js:1)" } } });
        assert_eq!(console_line("Runtime.exceptionThrown", &e), "[error] TypeError: x is undefined\n      at f (a.js:1)");
    }
}
