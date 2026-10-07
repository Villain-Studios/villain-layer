//! What Chrome says without being asked, routed to the page it is about.
//!
//! A task's tab is a stack: the page shown, and under it the page that
//! opened it, if it is a window a page opened (BRW-14). An event finds its
//! page anywhere in the stack, by the page's session or its target id.

use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use super::cdp::{Cdp, Event};
use super::{attach, changed, console_line, sites, Browser, Dialog, Inner, Tab, Viewport, CONSOLE_LINES};
use crate::commands::AppState;

/// The page in `tab`'s stack, itself or one it covers, that `is` picks.
fn find<'a>(tab: &'a mut Tab, is: &dyn Fn(&Tab) -> bool) -> Option<&'a mut Tab> {
    if is(tab) {
        return Some(tab);
    }
    tab.opener.as_deref_mut().and_then(|under| find(under, is))
}

fn holds(tab: &Tab, is: &dyn Fn(&Tab) -> bool) -> bool {
    is(tab) || tab.opener.as_deref().is_some_and(|under| holds(under, is))
}

/// The task whose stack holds the page `is` picks.
fn task_with(inner: &Inner, is: &dyn Fn(&Tab) -> bool) -> Option<String> {
    inner.tabs.iter().find(|(_, t)| holds(t, is)).map(|(task, _)| task.clone())
}

/// Take the page `target` out of `tab`'s stack, below the top.
fn remove_below(tab: &mut Tab, target: &str) {
    let Some(under) = tab.opener.as_mut() else { return };
    if under.target == target {
        let next = under.opener.take();
        tab.opener = next;
    } else {
        remove_below(under, target);
    }
}

/// Take the page `target` out of the task's stack. The top one gone, the
/// page it covered is shown again; the last one gone, the task has no tab.
fn unstack(inner: &mut Inner, task: &str, target: &str) {
    let Some(mut top) = inner.tabs.remove(task) else { return };
    if top.target == target {
        if let Some(under) = top.opener.take() {
            inner.tabs.insert(task.to_string(), *under);
        }
        return;
    }
    remove_below(&mut top, target);
    inner.tabs.insert(task.to_string(), top);
}

/// A window a page opened, to be attached.
struct Opened {
    cdp: Arc<Cdp>,
    user_agent: Option<String>,
    /// The opener's: the window is shown where it was.
    viewport: Viewport,
    target: String,
    url: String,
}

impl Browser {
    /// One event from Chrome, on its reading thread: nothing here waits.
    pub(super) fn on_event<R: Runtime>(&self, app: &AppHandle<R>, e: Event) {
        if e.method == "Page.screencastFrame" {
            return self.on_frame(e);
        }
        if e.method == "Target.targetCreated" {
            return self.on_window(app, &e.params);
        }
        let mut save: Option<(String, String)> = None;
        let mut tell: Option<String> = None;
        {
            let mut inner = self.inner.lock();
            let session = e.session.clone();
            let target = e
                .params
                .pointer("/targetInfo/targetId")
                .or_else(|| e.params.get("targetId"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let is = move |t: &Tab| match (&session, &target) {
                (Some(s), _) => &t.session == s,
                (None, Some(id)) => &t.target == id,
                (None, None) => false,
            };
            let Some(task) = task_with(&inner, &is) else { return };
            let shown = |inner: &Inner| inner.tabs.get(&task).map(|t| (t.session.clone(), t.url.clone(), t.title.clone(), t.loading));
            let before = shown(&inner);

            if matches!(e.method.as_str(), "Target.targetDestroyed" | "Target.targetCrashed" | "Target.detachedFromTarget") {
                let gone = inner.tabs.get_mut(&task).and_then(|t| find(t, &is)).map(|t| t.target.clone());
                if let Some(gone) = gone {
                    unstack(&mut inner, &task, &gone);
                }
            } else if let Some(page) = inner.tabs.get_mut(&task).and_then(|t| find(t, &is)) {
                // Only the task's own tab is kept for next time, not a
                // window a page opened over it.
                let own = page.opener.is_none();
                match e.method.as_str() {
                    "Page.frameNavigated" if e.params.pointer("/frame/parentId").is_none() => {
                        // A page that could not load is shown as Chrome's own
                        // error page, whose address is not a site; the one that
                        // failed is what the tab is on, for the panel and for
                        // agents alike.
                        let url = e
                            .params
                            .pointer("/frame/unreachableUrl")
                            .or_else(|| e.params.pointer("/frame/url"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if page.url != url {
                            page.url = url.to_string();
                            if own && sites::host_of(url).is_some() {
                                save = Some((task.clone(), url.to_string()));
                            }
                        }
                    }
                    "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                        if e.params.get("frameId").and_then(Value::as_str) == Some(page.target.as_str()) {
                            page.loading = e.method == "Page.frameStartedLoading";
                        }
                    }
                    "Target.targetInfoChanged" => {
                        if let Some(title) = e.params.pointer("/targetInfo/title").and_then(Value::as_str) {
                            page.title = title.to_string();
                        }
                    }
                    "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" => {
                        page.console.push_back(console_line(&e.method, &e.params));
                        while page.console.len() > CONSOLE_LINES {
                            page.console.pop_front();
                        }
                    }
                    "Page.javascriptDialogOpening" => {
                        page.dialog = Some(Dialog::from_event(&e.params));
                        self.dialog_opened.notify_waiters();
                        tell = Some(task.clone());
                    }
                    "Page.javascriptDialogClosed" => {
                        page.dialog = None;
                        tell = Some(task.clone());
                    }
                    _ => {}
                }
            }
            if before != shown(&inner) {
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

    /// A page opened a window: a sign-in popup, or a link to a new tab. It
    /// is shown in the opener's place, over it (BRW-14). Attached off this
    /// thread, which must not wait on Chrome's answers: it is the one that
    /// reads them.
    fn on_window<R: Runtime>(&self, app: &AppHandle<R>, p: &Value) {
        let info = |k: &str| p.pointer(&format!("/targetInfo/{k}")).and_then(Value::as_str).map(str::to_string);
        let (Some("page"), Some(opener), Some(target)) = (info("type").as_deref(), info("openerId"), info("targetId")) else {
            return;
        };
        let (task, cdp, user_agent, viewport) = {
            let inner = self.inner.lock();
            let Some(task) = task_with(&inner, &|t: &Tab| t.target == opener) else { return };
            let Some(r) = inner.running.as_ref() else { return };
            let viewport = inner.tabs.get(&task).map(|t| t.viewport).unwrap_or_default();
            (task, r.cdp.clone(), r.user_agent.clone(), viewport)
        };
        let opened = Opened { cdp, user_agent, viewport, target, url: info("url").unwrap_or_default() };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            app.state::<AppState>().browser.adopt(&app, &task, opened).await;
        });
    }

    async fn adopt<R: Runtime>(&self, app: &AppHandle<R>, task: &str, o: Opened) {
        // Closed already, as some sign-in windows are once they are done.
        let Ok(mut window) = attach(&o.cdp, o.target, o.user_agent.as_deref(), o.viewport).await else { return };
        if !o.url.is_empty() {
            window.url = o.url;
        }
        {
            let mut inner = self.inner.lock();
            if let Some(under) = inner.tabs.remove(task) {
                window.opener = Some(Box::new(under));
            }
            inner.tabs.insert(task.to_string(), window);
        }
        changed(app, task);
    }

    /// Close the window a page opened, showing the page under it again.
    /// False when the task's tab is its own, not such a window.
    pub fn close_window(&self, task: &str) -> bool {
        let inner = self.inner.lock();
        let (Some(r), Some(top)) = (inner.running.as_ref(), inner.tabs.get(task)) else { return false };
        if top.opener.is_none() {
            return false;
        }
        r.cdp.send("Target.closeTarget", json!({ "targetId": top.target }), None);
        true
    }

    /// Whether the task's tab shows a window a page opened.
    pub fn in_window(&self, task: &str) -> bool {
        self.inner.lock().tabs.get(task).is_some_and(|t| t.opener.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve};
    use crate::browser::tools::call;
    use std::time::Duration;

    const OPENS: &str = r#"<html><head><title>Opener</title></head><body>
        <button onclick="window.open('/two', 'signin', 'width=400,height=500')">Sign in</button>
    </body></html>"#;

    fn text(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect()
    }

    /// Against a real Chrome: a page that cannot be reached keeps its own
    /// address, not Chrome's error page's (`chrome-error://chromewebdata/`),
    /// which is not a site: the panel offered to allow "chromewebdata", and
    /// agents were refused a page on this machine.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_page_that_cannot_be_reached_keeps_its_address() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let dead = format!("http://localhost:{port}/");

        let page = state.browser.page(app.handle(), "t1").await.unwrap();
        let refused = page.navigate(&dead).await.unwrap_err();
        assert!(refused.to_string().contains("ERR_CONNECTION_REFUSED"), "{refused}");
        page.settle(&state.browser).await;
        assert_eq!(state.browser.view("t1").url, dead);

        let read = call(app.handle(), "browser_snapshot", json!({}), Some(&pane)).await.unwrap();
        assert!(text(&read).contains(&format!("URL: {dead}")), "{}", text(&read));

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Against a real Chrome: a window the page opens is shown in the task's
    /// tab, over the page that opened it; the agent reads it and is told
    /// what it is; closing it shows the opener again, and the opener is
    /// what is kept for next time.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_window_the_page_opens_is_shown_over_it_until_it_closes() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(OPENS).await;
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));

        let opened = text(&tool("browser_navigate", json!({ "url": format!("{base}/") })).await.unwrap());
        let at = opened.find("button \"Sign in\" [ref=").unwrap() + "button \"Sign in\" [ref=".len();
        let button = opened[at..].split(']').next().unwrap().to_string();

        let clicked = text(&tool("browser_click", json!({ "ref": button })).await.unwrap());
        assert!(clicked.contains("The second page"), "the window is what is shown: {clicked}");
        assert!(clicked.contains("This is a window the page opened"), "{clicked}");
        assert!(state.browser.view("t1").window);

        let closed = text(&tool("browser_close_window", json!({})).await.unwrap());
        assert!(closed.contains("Page: Opener"), "{closed}");
        assert!(!state.browser.view("t1").window);
        assert_eq!(state.config.read().tasks[0].browser_url.as_deref(), Some(format!("{base}/").as_str()));

        let none = tool("browser_close_window", json!({})).await.unwrap_err();
        assert!(none.to_string().contains("nothing to close"), "{none}");

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
