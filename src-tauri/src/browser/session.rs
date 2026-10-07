//! Copying a sign-in from one of a task's tabs to another (BRW-21): an app
//! signed in on one port of this machine, needed signed in on another. A
//! sign-in server often lets only one address through (CORS), so the
//! task's own copy of the app, on its own port, could not sign in itself.
//!
//! An agent could do this with `browser_evaluate`, but it would read every
//! token on the way, and its own CLI rightly stops it. Here the values go
//! from page to page in the app, and the agent is told only their names.

use serde_json::{json, Value};
use tauri::{AppHandle, Runtime};

use super::{page::Page, sites, tabs, Browser};
use crate::commands::AppState;
use crate::error::{Error, Result};

/// Both storages of a page, as `[[key, value], …]`.
const READ: &str = "({ local: Object.entries(localStorage), session: Object.entries(sessionStorage) })";

/// Writes what `READ` read; an item already there under the same key is
/// replaced, others are kept.
const WRITE: &str = "(d) => {
    for (const [k, v] of d.local) localStorage.setItem(k, v);
    for (const [k, v] of d.session) sessionStorage.setItem(k, v);
}";

/// How many names of each kind the answer lists.
const NAMES_SHOWN: usize = 20;

/// The origin of a page on this machine (`http://localhost:4200`); None for
/// a page anywhere else, or one that is not a web page.
fn local_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // `host_of` reads past a userinfo part; an origin has none.
    if authority.contains('@') || !sites::host_of(url).is_some_and(|h| sites::is_local(&h)) {
        return None;
    }
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), authority.to_ascii_lowercase()))
}

/// `names`, listed, up to `NAMES_SHOWN`.
fn listed(what: &str, names: &[String]) -> String {
    let n = names.len();
    if n == 0 {
        return format!("no {what}s");
    }
    let mut shown: Vec<&str> = names.iter().take(NAMES_SHOWN).map(String::as_str).collect();
    if n > NAMES_SHOWN {
        shown.push("…");
    }
    format!("{n} {what}{} ({})", if n == 1 { "" } else { "s" }, shown.join(", "))
}

fn keys(items: Option<&Value>) -> Vec<String> {
    items
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|i| i.get(0).and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default()
}

impl Browser {
    /// The task's tab `id`, to read from. Not while a password the app
    /// filled in is on it (BRW-19): that page's storage is the page's own
    /// to write, and could hold what was typed.
    fn tab_to_read(&self, task: &str, id: &str) -> Result<Page> {
        let inner = self.inner.lock();
        let cdp = inner.running.as_ref().map(|r| r.cdp.clone()).ok_or_else(|| Error::Other("the browser closed".into()))?;
        let tab = inner
            .tabs
            .get(task)
            .and_then(|t| t.list.iter().find(|t| t.target == id))
            .ok_or_else(|| Error::Other("that tab is closed".into()))?;
        if tab.secret.is_some() {
            return Err(Error::Other("a saved password is filled into that tab's page: copy once it has signed in".into()));
        }
        Ok(Page::new(cdp, tab.session.clone(), task.to_string()))
    }
}

/// Copy the sign-in of the tab the agent named into `page`, the active
/// tab; what was copied, by name.
pub(super) async fn copy_session<R: Runtime>(app: &AppHandle<R>, state: &AppState, page: &Page, args: &Value) -> Result<String> {
    let browser = &state.browser;
    let task = &page.task;
    let n = args.get("from_tab").and_then(Value::as_u64).ok_or_else(|| {
        Error::Other("from_tab is required: the number of the tab that is signed in (browser_tabs lists them)".into())
    })?;
    let from = tabs::tab_numbered(&browser.tabs(task), &json!({ "tab": n }))?;
    if from.active {
        return Err(Error::Other(
            "that is the active tab, which is copied into: switch to the tab that needs the sign-in first".into(),
        ));
    }
    if browser.secret_on_page(task).is_some() {
        return Err(Error::Other("a saved password is filled into this page: copy once it has signed in".into()));
    }
    let source = browser.tab_to_read(task, &from.id)?;
    let (from_url, _) = source.location().await?;
    let (to_url, _) = page.location().await?;
    let only_here = |url: &str, which: &str| {
        local_origin(url).ok_or_else(|| {
            Error::Other(format!(
                "{which} is not a page on this machine ({url}): a sign-in is copied only between this machine's pages"
            ))
        })
    };
    let from_origin = only_here(&from_url, &format!("tab {n}"))?;
    let to_origin = only_here(&to_url, "the active tab")?;
    if from_origin == to_origin {
        return Err(Error::Other(format!("both tabs are on {to_origin}, which shares its sign-in already")));
    }

    let stored = source.eval(READ).await?;
    let (local, session) = (keys(stored.get("local")), keys(stored.get("session")));
    page.eval(&format!("({WRITE})({stored})")).await?;

    // One host's ports share its cookies already: only another host's
    // (localhost and 127.0.0.1) are copied.
    let same_host = sites::host_of(&from_url) == sites::host_of(&to_url);
    let mut cookies = Vec::new();
    if !same_host {
        let got = source.call("Network.getCookies", json!({ "urls": [from_url] })).await?;
        let copies: Vec<Value> = got
            .get("cookies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?;
                cookies.push(name.to_string());
                let path = c.get("path").and_then(Value::as_str).unwrap_or("/");
                let mut copy = json!({
                    "name": name,
                    "value": c.get("value")?,
                    "url": format!("{to_origin}{path}"),
                    "path": path,
                    "secure": c.get("secure").cloned().unwrap_or(Value::Bool(false)),
                    "httpOnly": c.get("httpOnly").cloned().unwrap_or(Value::Bool(false)),
                });
                if let Some(same_site) = c.get("sameSite") {
                    copy["sameSite"] = same_site.clone();
                }
                // A cookie that ends with the browser has no date to keep.
                if c.get("session").and_then(Value::as_bool) == Some(false) {
                    copy["expires"] = c.get("expires").cloned().unwrap_or(Value::Null);
                }
                Some(copy)
            })
            .collect();
        if !copies.is_empty() {
            page.call("Network.setCookies", json!({ "cookies": copies })).await?;
        }
    }

    browser.record(app, task, &format!("Copied the sign-in from {from_origin}"), None);
    let reload = args.get("reload").and_then(Value::as_bool).unwrap_or(true);
    if reload {
        page.call("Page.reload", json!({})).await?;
    }
    let cookies = if same_host {
        "cookies: none needed, the two share this host's".to_string()
    } else {
        listed("cookie", &cookies)
    };
    Ok(format!(
        "Copied the sign-in from {from_origin} into {to_origin}: {}, {}, {cookies}.{}",
        listed("local storage item", &local),
        listed("session storage item", &session),
        if reload { " Reloaded the page." } else { "" },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve};
    use crate::browser::tools::call;
    use tauri::Manager;

    /// A page that signs in the way a single-page app does: a token kept in
    /// both storages and a cookie.
    const SIGNED_IN: &str = r#"<html><head><title>Signed in</title></head><body><script>
        localStorage.setItem('token', 'tok-123');
        sessionStorage.setItem('state', 'st-9');
        document.cookie = 'sid=ck-7; path=/';
    </script><p>Welcome</p></body></html>"#;

    /// The same app on another port: signed in only with all three.
    const APP: &str = r#"<html><head><title>App</title></head><body><script>
        const signedIn = localStorage.getItem('token') === 'tok-123'
            && sessionStorage.getItem('state') === 'st-9'
            && document.cookie.includes('sid=ck-7');
        document.title = signedIn ? 'Dashboard' : 'Signed out';
    </script></body></html>"#;

    fn text(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect()
    }

    #[test]
    fn only_this_machine_s_pages_have_an_origin_to_copy_between() {
        assert_eq!(local_origin("http://localhost:4200/login?x=1").as_deref(), Some("http://localhost:4200"));
        assert_eq!(local_origin("http://127.0.0.1:4210/").as_deref(), Some("http://127.0.0.1:4210"));
        assert_eq!(local_origin("https://example.com/"), None);
        assert_eq!(local_origin("http://localhost@example.com/"), None);
        assert_eq!(local_origin("http://example.com@localhost/"), None, "no userinfo in an origin");
        assert_eq!(local_origin("about:blank"), None);
    }

    #[test]
    fn what_was_copied_is_listed_by_name_and_counted() {
        assert_eq!(listed("cookie", &[]), "no cookies");
        assert_eq!(listed("cookie", &["sid".into()]), "1 cookie (sid)");
        let many: Vec<String> = (0..25).map(|i| format!("k{i}")).collect();
        let shown = listed("item", &many);
        assert!(shown.starts_with("25 items (k0, k1,") && shown.ends_with("k19, …)"), "{shown}");
    }

    /// Against a real Chrome, BRW-21: an app signed in on one port of this
    /// machine is signed in on another, with its storage and its cookies,
    /// and the agent is never told a value.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_is_copied_to_another_port_without_the_agent_seeing_it() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));
        let signed_in = serve(SIGNED_IN).await;
        // Another host too, so the cookie has to be copied.
        let other = serve(APP).await.replace("localhost", "127.0.0.1");

        tool("browser_navigate", json!({ "url": format!("{signed_in}/") })).await.unwrap();
        let opened = text(&tool("browser_new_tab", json!({ "url": format!("{other}/") })).await.unwrap());
        assert!(opened.contains("Page: Signed out"), "{opened}");

        let own = tool("browser_copy_session", json!({ "from_tab": 2 })).await.unwrap_err();
        assert!(own.to_string().contains("that is the active tab"), "{own}");

        let copied = text(&tool("browser_copy_session", json!({ "from_tab": 1 })).await.unwrap());
        assert!(
            copied.contains("1 local storage item (token), 1 session storage item (state), 1 cookie (sid)"),
            "{copied}"
        );
        assert!(copied.contains("Page: Dashboard"), "signed in after the reload: {copied}");
        for value in ["tok-123", "st-9", "ck-7"] {
            assert!(!copied.contains(value), "{value} told to the agent: {copied}");
        }
        let state = app.state::<AppState>();
        state.browser.shutdown(std::time::Duration::from_secs(3));
        state.ptys.shutdown(std::time::Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
