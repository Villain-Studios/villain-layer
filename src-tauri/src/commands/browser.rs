//! The Browser panel's commands: a task's tab on screen, and the user's own
//! use of it (§18).

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Manager};

use super::AppState;
use crate::browser::input::BrowserInput;
use crate::browser::{sites, AgentAction, Dialog, SiteRequest, Viewport};
use crate::error::{Error, Result};

/// A task's tab, as the panel shows it.
#[derive(Debug, Serialize)]
pub struct BrowserView {
    /// Whether a Chrome is installed to run (BRW-1).
    pub chrome: bool,
    /// The tab's session; a new one means the tab was made again.
    pub tab: Option<String>,
    pub url: String,
    pub title: String,
    pub loading: bool,
    /// Whether agents may use the page as it is now (BRW-3).
    pub agents_may: bool,
    pub last_action: Option<AgentAction>,
    /// Sites the task's agents asked for, waiting on the user (BRW-11).
    pub requests: Vec<SiteRequest>,
    /// A dialog the page opened, waiting on an answer (BRW-13).
    pub dialog: Option<Dialog>,
    /// The user has taken the tab over, and agents wait (BRW-12).
    pub held: bool,
    /// What is shown is a window the page opened (BRW-14).
    pub window: bool,
}

/// The task's tab, as last heard. Makes nothing: a task whose panel was
/// never opened and whose agents never used the browser has no tab.
#[tauri::command]
pub async fn browser_view(app: AppHandle, task_id: String) -> Result<BrowserView> {
    // Looking for Chrome reads the disk.
    let chrome = super::off_runtime(crate::browser::chrome_installed).await?;
    let state = app.state::<AppState>();
    let view = state.browser.view(&task_id);
    let sites = state.config.read().browser.sites;
    Ok(BrowserView {
        chrome,
        agents_may: view.tab.is_some() && sites::allowed(&view.url, &sites),
        tab: view.tab,
        url: view.url,
        title: view.title,
        loading: view.loading,
        last_action: view.last_action,
        requests: state.browser.requests(&task_id),
        dialog: view.dialog,
        held: view.held,
        window: view.window,
    })
}

/// Open the task's tab, starting the browser if it is not running, and go
/// to `url` if one is given. The user may go anywhere on the web: BRW-3 is
/// about what agents may read, and it is checked when they do.
#[tauri::command]
pub async fn browser_open(app: AppHandle, task_id: String, url: Option<String>) -> Result<()> {
    let state = app.state::<AppState>();
    let page = state.browser.page(&app, &task_id).await?;
    if let Some(url) = url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        page.navigate(&address(url)?).await?;
    }
    Ok(())
}

/// What the address bar's text means: an address, with `http://` for this
/// machine and `https://` elsewhere when it has none, or else a search.
fn address(typed: &str) -> Result<String> {
    if typed.contains("://") {
        return match sites::host_of(typed) {
            Some(_) => Ok(typed.to_string()),
            None => Err(Error::Other("only http and https pages open here".into())),
        };
    }
    let first = typed.split(['/', '?', '#']).next().unwrap_or_default();
    let host = first.rsplit_once(':').map_or(first, |(h, _)| h);
    if !typed.contains(' ') && (host.contains('.') || sites::is_local(host)) {
        return Ok(sites::complete(typed));
    }
    let q: String = typed
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Ok(format!("https://duckduckgo.com/?q={q}"))
}

/// Back, forward or reload, from the panel's buttons.
#[tauri::command]
pub async fn browser_go(app: AppHandle, task_id: String, to: String) -> Result<()> {
    let state = app.state::<AppState>();
    let page = state.browser.page(&app, &task_id).await?;
    match to.as_str() {
        "back" => page.back().await.map(|_| ()),
        "forward" => page.forward().await.map(|_| ()),
        "reload" => page.call("Page.reload", serde_json::json!({})).await.map(|_| ()),
        other => Err(Error::Other(format!("cannot go {other}"))),
    }
}

/// Show the task's tab in the panel: sized to it, its frames sent to
/// `frames` while it is on screen (BRW-9).
#[tauri::command]
pub async fn browser_watch(
    app: AppHandle,
    task_id: String,
    width: u32,
    height: u32,
    scale: f64,
    frames: Channel<InvokeResponseBody>,
) -> Result<()> {
    let viewport = Viewport {
        // A page laid out in a sliver is not one anybody can use; Chrome
        // refuses a zero size.
        width: width.clamp(200, 4000),
        height: height.clamp(150, 4000),
        scale: if scale.is_finite() { scale.clamp(1.0, 3.0) } else { 1.0 },
    };
    let state = app.state::<AppState>();
    state.browser.watch(&app, &task_id, viewport, frames).await
}

/// The panel closed, or shows another task.
#[tauri::command]
pub fn browser_unwatch(app: AppHandle, task_id: String) {
    app.state::<AppState>().browser.unwatch(&task_id);
}

/// The user's mouse, wheel or keys in the panel (BRW-9).
#[tauri::command]
pub fn browser_input(app: AppHandle, task_id: String, input: BrowserInput) -> Result<()> {
    app.state::<AppState>().browser.input(&task_id, &input)
}

/// The text selected in the page, for ⌘C in the panel: a headless Chrome
/// copies to a clipboard of its own, not the Mac's.
#[tauri::command]
pub async fn browser_copy(app: AppHandle, task_id: String) -> Result<String> {
    let state = app.state::<AppState>();
    let page = state.browser.page(&app, &task_id).await?;
    let v = page
        .eval(
            "(() => { const a = document.activeElement;
               if (a && typeof a.selectionStart === 'number' && a.type !== 'password')
                 return a.value.slice(a.selectionStart, a.selectionEnd);
               return String(window.getSelection() || ''); })()",
        )
        .await?;
    Ok(v.as_str().unwrap_or_default().to_string())
}

/// Take the task's tab from its agents, or hand it back (BRW-12).
#[tauri::command]
pub fn browser_hold(app: AppHandle, task_id: String, held: bool) {
    app.state::<AppState>().browser.hold(&app, &task_id, held);
}

/// Close the window a page opened, showing the page under it (BRW-14).
#[tauri::command]
pub fn browser_close_window(app: AppHandle, task_id: String) {
    app.state::<AppState>().browser.close_window(&task_id);
}

/// Answer the dialog the page opened, from the panel (BRW-13).
#[tauri::command]
pub async fn browser_dialog(app: AppHandle, task_id: String, accept: bool, text: Option<String>) -> Result<()> {
    let state = app.state::<AppState>();
    state.browser.answer_dialog(&task_id, accept, text.as_deref()).await.map(|_| ())
}

/// The user's answer to an agent's request for a site (BRW-11). Allowed,
/// the site joins the list before the agent hears, so its next call works.
#[tauri::command]
pub async fn browser_answer_site(app: AppHandle, id: u64, allow: bool) -> Result<()> {
    let state = app.state::<AppState>();
    let site = state
        .browser
        .requests_all()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.site)
        .ok_or_else(|| Error::NotFound("that request was already answered".into()))?;
    if allow {
        super::blocking(app.clone(), move |state| add_site(state, &site)).await?;
    }
    state.browser.answer(&app, id, allow);
    Ok(())
}

fn add_site(state: &AppState, site: &str) -> Result<()> {
    state.config.update(|c| {
        if !c.browser.sites.iter().any(|s| s == site) {
            c.browser.sites.push(site.to_string());
            c.browser.sites.sort();
        }
    })
}

/// Settings → Browser: the sites agents may use besides this machine's
/// (BRW-3), as the user listed them. Each is kept as its host; what is not
/// a site is dropped. Returns the list as kept.
#[tauri::command]
pub async fn set_browser_sites(app: AppHandle, sites: Vec<String>) -> Result<Vec<String>> {
    let kept = super::blocking(app.clone(), move |state| {
        let mut kept: Vec<String> = sites.iter().filter_map(|s| sites::normalize(s)).collect();
        kept.sort();
        kept.dedup();
        let out = kept.clone();
        state.config.update(|c| c.browser.sites = kept)?;
        Ok(out)
    })
    .await?;
    // Whether agents may use the page a panel shows can have changed.
    let _ = tauri::Emitter::emit(&app, "browser:changed", "");
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_bar_opens_addresses_and_searches_for_anything_else() {
        assert_eq!(address("localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(address("example.com/a").unwrap(), "https://example.com/a");
        assert_eq!(address("http://example.com").unwrap(), "http://example.com");
        assert_eq!(address("tauri channel raw").unwrap(), "https://duckduckgo.com/?q=tauri+channel+raw");
        assert_eq!(address("a&b").unwrap(), "https://duckduckgo.com/?q=a%26b");
        assert!(address("file:///etc/passwd").is_err());
    }
}
