//! One browser for the app, one tab in it per task (§18 in features.md).
//!
//! Chrome runs headless with a profile of its own (BRW-1), started the first
//! time anything needs a tab and stopped with the app (BRW-7). Agents drive
//! their task's tab through the tools in `tools.rs`.

mod cdp;
mod chrome;
mod keys;
mod page;
pub mod sites;
mod snapshot;
pub mod tools;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use crate::commands::AppState;
use crate::error::{Error, Result};
use cdp::{Cdp, Event};
pub use page::Page;

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
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// By task id.
    tabs: HashMap<String, Tab>,
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
            move || closes.state::<AppState>().browser.lost(),
        )?;
        let version = cdp.call("Browser.getVersion", json!({}), None).await;
        let user_agent = version
            .ok()
            .and_then(|v| v.get("userAgent").and_then(Value::as_str).map(|ua| ua.replace("HeadlessChrome", "Chrome")));
        // For targetInfoChanged, which is how a tab's title is heard.
        cdp.call("Target.setDiscoverTargets", json!({ "discover": true }), None).await?;

        let mut inner = self.inner.lock();
        inner.tabs.clear();
        inner.running = Some(Running { cdp: cdp.clone(), process, user_agent: user_agent.clone() });
        Ok((cdp, user_agent))
    }

    /// Chrome went away (crashed, killed, or closed by `shutdown`). Its tabs
    /// went with it; the next call starts it again (BRW-7).
    fn lost(&self) {
        let process = {
            let mut inner = self.inner.lock();
            let dead = inner.running.as_ref().is_some_and(|r| r.cdp.is_closed());
            if !dead {
                return;
            }
            inner.tabs.clear();
            inner.running.take().map(|r| r.process)
        };
        // Reaped on the reading thread that called this, which waits on
        // nothing else any more.
        if let Some(p) = process {
            p.lock().stop(Duration::ZERO);
        }
    }

    /// Close a task's tab, when the task is deleted or finished (BRW-6).
    pub fn close_task(&self, task: &str) {
        let mut inner = self.inner.lock();
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
        let mut save: Option<(String, String)> = None;
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

    #[test]
    fn a_console_message_is_one_line_with_its_kind() {
        let p = json!({ "type": "warning", "args": [{ "type": "string", "value": "slow" }, { "type": "number", "value": 3 }] });
        assert_eq!(console_line("Runtime.consoleAPICalled", &p), "[warning] slow 3");
        let e = json!({ "exceptionDetails": { "text": "Uncaught", "exception": { "description": "TypeError: x is undefined\n    at f (a.js:1)" } } });
        assert_eq!(console_line("Runtime.exceptionThrown", &e), "[error] TypeError: x is undefined\n      at f (a.js:1)");
    }
}
