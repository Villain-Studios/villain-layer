//! What Chrome says without being asked, routed to the tab it is about,
//! wherever it is among its task's tabs: by the tab's session, or its
//! target id.

use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use super::cdp::{Cdp, Event};
use super::{attach, changed, console_line, Browser, Dialog, Inner, Viewport, CONSOLE_LINES};
use crate::commands::AppState;

/// The task, and the target id, of the tab an event is about.
fn tab_of(inner: &Inner, session: Option<&str>, target: Option<&str>) -> Option<(String, String)> {
    inner.tabs.iter().find_map(|(task, tabs)| {
        tabs.list
            .iter()
            .find(|t| match (session, target) {
                (Some(s), _) => t.session == s,
                (None, Some(id)) => t.target == id,
                (None, None) => false,
            })
            .map(|t| (task.clone(), t.target.clone()))
    })
}

/// A window a page opened, to be attached as a tab.
struct Opened {
    cdp: Arc<Cdp>,
    user_agent: Option<String>,
    /// The opener's: the tab is laid out at the panel's size, as it was.
    viewport: Viewport,
    target: String,
    opener: String,
    url: String,
}

impl Browser {
    /// One event from Chrome, on its reading thread: nothing here waits.
    pub(super) fn on_event<R: Runtime>(&self, app: &AppHandle<R>, e: Event) {
        if e.method == "Page.screencastFrame" {
            return self.on_frame(app, e);
        }
        if e.method == "Target.targetCreated" {
            return self.on_window(app, &e.params);
        }
        let mut save = false;
        let mut tell = false;
        let mut ask = None;
        let task = {
            let mut inner = self.inner.lock();
            // Shutting down: Chrome says each tab is destroyed as it closes,
            // and taken at its word, quitting the app forgot every tab it
            // was to bring back (BRW-6).
            if inner.running.is_none() {
                return;
            }
            let target = e
                .params
                .pointer("/targetInfo/targetId")
                .or_else(|| e.params.get("targetId"))
                .and_then(Value::as_str);
            let Some((task, id)) = tab_of(&inner, e.session.as_deref(), target) else { return };
            let Some(tabs) = inner.tabs.get_mut(&task) else { return };

            // Not a crash: a tab whose page crashed is still a tab, and a
            // reload brings it back.
            if matches!(e.method.as_str(), "Target.targetDestroyed" | "Target.detachedFromTarget") {
                tabs.remove(&id);
                // The last tab gone (a page closing itself): the task has
                // none until its browser is next needed, which makes one.
                if tabs.list.is_empty() {
                    inner.tabs.remove(&task);
                }
                save = true;
                tell = true;
            } else if let Some(at) = tabs.position(&id) {
                let active = at == tabs.active;
                let tab = &mut tabs.list[at];
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
                        if tab.url != url {
                            tab.url = url.to_string();
                            save = true;
                            tell = true;
                        }
                    }
                    "Page.frameStartedLoading" | "Page.frameStoppedLoading" => {
                        if e.params.get("frameId").and_then(Value::as_str) == Some(tab.target.as_str()) {
                            tab.loading = e.method == "Page.frameStartedLoading";
                            tell = true;
                            if !tab.loading {
                                ask = Some(tab.target.clone());
                            }
                        }
                    }
                    // A route change in a page that changes its own address.
                    "Page.navigatedWithinDocument" => {
                        if e.params.get("frameId").and_then(Value::as_str) == Some(tab.target.as_str()) {
                            ask = Some(tab.target.clone());
                        }
                    }
                    "Target.targetInfoChanged" => {
                        let title = e.params.pointer("/targetInfo/title").and_then(Value::as_str).unwrap_or_default();
                        if tab.title != title {
                            tab.title = title.to_string();
                            tell = true;
                        }
                    }
                    "Runtime.consoleAPICalled" | "Runtime.exceptionThrown" => {
                        tab.console.push_back(console_line(&e.method, &e.params));
                        while tab.console.len() > CONSOLE_LINES {
                            tab.console.pop_front();
                        }
                    }
                    "Page.javascriptDialogOpening" => {
                        tab.dialog = Some(Dialog::from_event(&e.params));
                        // Only the active tab's dialog is anyone's to answer
                        // now; an action waits on no other.
                        if active {
                            self.dialog_opened.notify_waiters();
                        }
                        tell = true;
                    }
                    "Page.javascriptDialogClosed" => {
                        tab.dialog = None;
                        tell = true;
                    }
                    _ => {}
                }
            }
            task
        };
        if tell {
            changed(app, &task);
        }
        // Outside the lock: a config write waits on the disk.
        if save {
            self.save(app, &task);
        }
        if let Some(target) = ask {
            self.ask_about(app, task, target);
        }
    }

    /// Ask Chrome what a tab is called and where it is, off this thread.
    /// Chrome says when a tab's address changes, but its title as it does is
    /// only the address again: the page's own title, read from the page as
    /// it loads, is never announced.
    fn ask_about<R: Runtime>(&self, app: &AppHandle<R>, task: String, target: String) {
        let Some(cdp) = self.inner.lock().running.as_ref().map(|r| r.cdp.clone()) else { return };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(info) = cdp.call("Target.getTargetInfo", serde_json::json!({ "targetId": target }), None).await else { return };
            let get = |k: &str| info.pointer(&format!("/targetInfo/{k}")).and_then(Value::as_str).unwrap_or_default().to_string();
            let (title, url) = (get("title"), get("url"));
            let browser = &app.state::<AppState>().browser;
            let moved = {
                let mut inner = browser.inner.lock();
                let Some(tab) = inner.tabs.get_mut(&task).and_then(|t| t.list.iter_mut().find(|t| t.target == target)) else { return };
                let moved = !url.is_empty() && !url.starts_with("chrome-error:") && tab.url != url;
                if tab.title == title && !moved {
                    return;
                }
                tab.title = title;
                if moved {
                    tab.url = url;
                }
                moved
            };
            changed(&app, &task);
            if moved {
                browser.save(&app, &task);
            }
        });
    }

    /// A page opened a window: a link to a new tab, `window.open`, a sign-in
    /// popup. It becomes a tab, next to its opener and active (BRW-14).
    /// Attached off this thread, which must not wait on Chrome's answers:
    /// it is the one that reads them.
    fn on_window<R: Runtime>(&self, app: &AppHandle<R>, p: &Value) {
        let info = |k: &str| p.pointer(&format!("/targetInfo/{k}")).and_then(Value::as_str).map(str::to_string);
        let (Some("page"), Some(opener), Some(target)) = (info("type").as_deref(), info("openerId"), info("targetId")) else {
            return;
        };
        let (task, opened) = {
            let inner = self.inner.lock();
            let Some((task, _)) = tab_of(&inner, None, Some(&opener)) else { return };
            // Already one of the app's: a tab it made itself.
            if tab_of(&inner, None, Some(&target)).is_some() {
                return;
            }
            let Some(r) = inner.running.as_ref() else { return };
            let viewport = inner.active(&task).map(|t| t.viewport).unwrap_or_default();
            let opened = Opened {
                cdp: r.cdp.clone(),
                user_agent: r.user_agent.clone(),
                viewport,
                target,
                opener,
                url: info("url").unwrap_or_default(),
            };
            (task, opened)
        };
        self.inner.lock().adopting += 1;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let browser = &app.state::<AppState>().browser;
            let target = opened.target.clone();
            let adopted = browser.adopt(&task, opened).await;
            browser.inner.lock().adopting -= 1;
            if adopted {
                browser.save(&app, &task);
                changed(&app, &task);
                // It may have loaded while it was being attached, unheard.
                browser.ask_about(&app, task, target);
            }
        });
    }

    /// Attach a window a page opened as a tab, beside its opener. False when
    /// it was closed first, as some sign-in windows are once they are done.
    async fn adopt(&self, task: &str, o: Opened) -> bool {
        let Ok(mut tab) = attach(&o.cdp, o.target, o.user_agent.as_deref(), o.viewport).await else { return false };
        if !o.url.is_empty() {
            tab.url = o.url;
        }
        tab.opener = Some(o.opener.clone());
        let mut inner = self.inner.lock();
        let Some(tabs) = inner.tabs.get_mut(task) else { return false };
        tabs.open_next_to(tab, Some(&o.opener));
        true
    }

    /// Whether a window a page opened is still being made a tab: an action
    /// that opened one has not finished until it is (`Page::settle`).
    pub fn adopting(&self) -> bool {
        self.inner.lock().adopting > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::app_with_task;
    use serde_json::json;
    use crate::browser::tools::call;
    use std::time::Duration;

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

    const TABS: &str = r#"<html><head><title>Home</title></head><body>
        <a href="/two" target="_blank">Next page</a>
        <button onclick="window.open('/bye', 'signin', 'width=400,height=500')">Sign in</button>
    </body></html>"#;

    /// Against a real Chrome, the whole of BRW-14 and BRW-16: a link that
    /// opens a new tab opens it beside its own and active; switching, a new
    /// tab, closing one; a sign-in window that closes itself returns to its
    /// opener; the tabs come back after the browser does; and the last tab
    /// closed is left open, empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn tabs_open_beside_their_opener_switch_close_and_come_back() {
        use axum::{response::Html, routing::get, Router};
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let router = Router::new()
            .route("/", get(|| async { Html(TABS) }))
            .route("/two", get(|| async { Html("<html><head><title>Two</title></head><body><p>The second page</p></body></html>") }))
            .route("/bye", get(|| async { Html("<html><head><title>Signing in</title></head><body><script>setTimeout(() => window.close(), 300)</script></body></html>") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move { axum::serve(listener, router).await });
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));
        let names = || state.browser.tabs("t1").iter().map(|t| format!("{}{}", t.name, if t.active { "*" } else { "" })).collect::<Vec<_>>();
        let ref_of = |outline: &str, what: &str| {
            let at = outline.find(what).unwrap() + what.len();
            outline[at..].split("[ref=").nth(1).unwrap().split(']').next().unwrap().to_string()
        };

        let home = text(&tool("browser_navigate", json!({ "url": format!("{base}/") })).await.unwrap());
        let clicked = text(&tool("browser_click", json!({ "ref": ref_of(&home, "link \"Next page\"") })).await.unwrap());
        assert!(clicked.contains("Page: Two"), "the new tab is the active one: {clicked}");
        assert!(clicked.contains("Tabs:\n  1. Home"), "the outline lists the tabs: {clicked}");
        assert_eq!(names(), ["Home", "Two*"]);

        let back = text(&tool("browser_switch_tab", json!({ "tab": 1 })).await.unwrap());
        assert!(back.contains("Page: Home"), "{back}");
        assert_eq!(state.browser.view("t1").last_action.unwrap().text, "Switched to “Home”");

        tool("browser_new_tab", json!({ "url": format!("{base}/two") })).await.unwrap();
        assert_eq!(names(), ["Home", "Two*", "Two"], "beside the active tab");
        tool("browser_close_tab", json!({})).await.unwrap();
        assert_eq!(names(), ["Home*", "Two"]);

        let home = text(&tool("browser_snapshot", json!({})).await.unwrap());
        tool("browser_click", json!({ "ref": ref_of(&home, "button \"Sign in\"") })).await.unwrap();
        let mut seen = Vec::new();
        for _ in 0..60 {
            let now = names();
            if seen.last() != Some(&now) {
                seen.push(now.clone());
            }
            if seen.len() > 1 && now == ["Home*", "Two"] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(seen.iter().any(|n| n.len() == 3), "the sign-in window was a tab: {seen:?}");
        assert_eq!(names(), ["Home*", "Two"], "and closing itself, returned to its opener: {seen:?}");

        let kept = state.config.read().tasks[0].browser.clone().unwrap();
        assert_eq!(kept.urls, [format!("{base}/"), format!("{base}/two")]);
        assert_eq!(kept.active, 0);

        state.browser.shutdown(Duration::from_secs(3));
        let listed = text(&tool("browser_tabs", json!({})).await.unwrap());
        assert!(listed.contains(&format!("1. Home — {base}/ (active)")) || listed.contains(&format!("1. localhost — {base}/ (active)")), "{listed}");
        assert_eq!(state.browser.tabs("t1").len(), 2, "both came back");

        tool("browser_close_tab", json!({ "tab": 2 })).await.unwrap();
        tool("browser_close_tab", json!({})).await.unwrap();
        let left = state.browser.tabs("t1");
        assert_eq!((left.len(), left[0].url.as_str()), (1, "about:blank"), "the last is left, empty");

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
