//! One browser for the app, one tab in it per task (§18 in features.md).
//!
//! Chrome runs headless with a profile of its own (BRW-1), started the first
//! time anything needs a tab and stopped with the app (BRW-7). Agents drive
//! their task's tab through the tools in `tools.rs`.

mod cdp;
mod chrome;
mod control;
pub mod input;
mod keys;
mod page;
mod panel;
mod requests;
pub mod sites;
mod snapshot;
pub mod tools;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::AppState;
use crate::error::{Error, Result};
use cdp::{Cdp, Event};
pub use control::Dialog;
pub use page::Page;
pub use panel::AgentAction;
pub use requests::SiteRequest;
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
    last_action: Option<AgentAction>,
    dialog: Option<Dialog>,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// By task id.
    tabs: HashMap<String, Tab>,
    watch: Option<Watch>,
    requests: Vec<Asked>,
    next_request: u64,
    /// Tasks whose tab the user has taken over (BRW-12). Kept apart from
    /// the tabs, so a tab made again is still held.
    held: HashSet<String>,
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
                "Page.javascriptDialogOpening" => {
                    if let Some(tab) = inner.tabs.get_mut(&task) {
                        tab.dialog = Some(Dialog::from_event(&e.params));
                    }
                    self.dialog_opened.notify_waiters();
                    tell = Some(task.clone());
                }
                "Page.javascriptDialogClosed" => {
                    if let Some(tab) = inner.tabs.get_mut(&task) {
                        tab.dialog = None;
                    }
                    tell = Some(task.clone());
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
        dialog: None,
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
