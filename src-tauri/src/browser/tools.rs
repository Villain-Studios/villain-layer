//! The browser tools agents get through the app's MCP server (§12, §18).
//!
//! Each acts on the calling agent's own task's tab (BRW-2), and only on a
//! page from a site agents may use (BRW-3). An action answers with the page
//! as it is afterwards, so the agent sees what its action did without
//! asking again.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use super::{sites, snapshot, Browser, Page};
use crate::commands::{AppState, CHAT_TASK_ID};
use crate::error::{Error, Result};
use crate::mcp::{arg, bool_prop, required, str_prop, tool};

pub fn owns(name: &str) -> bool {
    name.starts_with("browser_")
}

fn ref_prop() -> Value {
    str_prop("The element's ref from the page outline, e.g. e12")
}

pub fn list() -> Vec<Value> {
    vec![
        tool(
            "browser_navigate",
            "Open a page in your task's browser tab and get its outline. The tab is shared \
             with the user, who can watch it and use it. Only this machine's pages \
             (localhost and the like, http unless you say https) and the sites the user \
             allowed are open to you; browser_request_site asks for another. Use it to \
             try what you built: the dev server, a page, a form.",
            json!({ "url": str_prop("The address, e.g. localhost:3000/login") }),
            vec!["url"],
        ),
        tool(
            "browser_back",
            "Go back one page in your task's browser tab, and get the outline of the page \
             it went back to.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_snapshot",
            "The page in your task's browser tab as an outline: headings, text, fields, \
             buttons and links, each element you can act on marked [ref=e12]. Read this \
             rather than a screenshot to find what to click or type into.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_click",
            "Click an element, scrolling it into view first. Real mouse input, as a person's \
             click. Answers with the page as it is afterwards.",
            json!({
                "ref": ref_prop(),
                "double": bool_prop("Double-click"),
                "right": bool_prop("Right-click"),
            }),
            vec!["ref"],
        ),
        tool(
            "browser_hover",
            "Move the mouse over an element, for menus and tooltips that open on hover.",
            json!({ "ref": ref_prop() }),
            vec!["ref"],
        ),
        tool(
            "browser_type",
            "Type text into a field, as typed at its caret. Answers with the page as it is \
             afterwards. Never type the user's passwords or other secrets: ask the user to \
             type those into the browser themselves.",
            json!({
                "ref": ref_prop(),
                "text": str_prop("What to type"),
                "clear": bool_prop("Replace what the field holds instead of adding to it"),
                "submit": bool_prop("Press Enter afterwards"),
            }),
            vec!["ref", "text"],
        ),
        tool(
            "browser_press_key",
            "Press a key in the page, at whatever has focus: a key's name (Enter, Tab, \
             Escape, Backspace, ArrowDown, PageDown, …) or one character, after any \
             modifiers joined by + (Shift+Tab, Meta+a).",
            json!({ "key": str_prop("The key, e.g. Enter or Shift+Tab") }),
            vec!["key"],
        ),
        tool(
            "browser_select",
            "Choose options in a <select> list, by their value or their text. Answers with \
             the options now chosen.",
            json!({
                "ref": ref_prop(),
                "values": { "type": "array", "items": { "type": "string" }, "description": "The options to choose" },
            }),
            vec!["ref", "values"],
        ),
        tool(
            "browser_scroll",
            "Scroll the page, or the scrolling part under an element, by a number of pixels \
             (negative is up). Clicking and typing scroll on their own; this is for \
             reading further down a long page or a list that loads more as it scrolls.",
            json!({
                "pixels": { "type": "number", "description": "How far down; negative scrolls up. A screen is about 700" },
                "ref": str_prop("Scroll where this element is, e.g. a list in a sidebar. Default: the middle of the page"),
            }),
            vec!["pixels"],
        ),
        tool(
            "browser_wait",
            "Wait until some text shows on the page, or for a number of seconds, at most 30. \
             For what loads after a click without a new page: a search's results, a save's \
             confirmation.",
            json!({
                "text": str_prop("Wait until the page shows this"),
                "seconds": { "type": "number", "description": "How long to wait at most; default 5" },
            }),
            vec![],
        ),
        tool(
            "browser_screenshot",
            "An image of what the tab shows now. For how the page looks: layout, colours, \
             what is cut off. To find what to act on, browser_snapshot is cheaper and \
             names the elements.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_console",
            "The last messages the page wrote to its console, and its uncaught errors, \
             oldest first.",
            json!({ "clear": bool_prop("Empty the list afterwards, to see only what comes next") }),
            vec![],
        ),
        tool(
            "browser_tabs",
            "Your task's browser tabs, numbered, with which one is active. Everything else \
             acts on the active tab. A link that opens a new tab opens it next to its own \
             and makes it active.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_new_tab",
            "Open a page in a new tab, next to the active one, and make it active. The same \
             sites as browser_navigate.",
            json!({ "url": str_prop("The address, e.g. localhost:3000/admin") }),
            vec!["url"],
        ),
        tool(
            "browser_switch_tab",
            "Make another tab the active one, by its number from browser_tabs. The user \
             sees the same tab you do.",
            json!({ "tab": { "type": "integer", "description": "The tab's number, from 1" } }),
            vec!["tab"],
        ),
        tool(
            "browser_close_tab",
            "Close a tab, the active one unless you give its number. Closing a tab a page \
             opened makes the tab that opened it active again. The last tab is left open, \
             empty.",
            json!({ "tab": { "type": "integer", "description": "The tab's number, from 1; default the active one" } }),
            vec![],
        ),
        tool(
            "browser_dialog",
            "Answer the alert, confirm or prompt the page opened: accept is OK, false is \
             Cancel. The page does nothing else until it is answered.",
            json!({
                "accept": bool_prop("true for OK, false for Cancel"),
                "text": str_prop("What to type into a prompt before accepting it"),
            }),
            vec!["accept"],
        ),
        tool(
            "browser_request_site",
            "Ask the user to let agents use a site in the browser, saying what for. The user \
             answers in the app; this waits up to two minutes and says what they chose. Ask \
             only for what the task needs, and once: asking again waits on the same question. \
             A site covers its subdomains.",
            json!({
                "site": str_prop("The site, e.g. github.com, or a URL on it"),
                "reason": str_prop("What you need it for, in a sentence the user reads before answering"),
            }),
            vec!["site", "reason"],
        ),
        tool(
            "browser_evaluate",
            "Run JavaScript in the page and get its value back, as JSON. An expression, or \
             an async one: await works. For what the outline does not show: a value in \
             the app's state, local storage, a computed style.",
            json!({ "expression": str_prop("The JavaScript, e.g. document.querySelectorAll('li').length") }),
            vec!["expression"],
        ),
    ]
}

pub(super) fn text(t: impl Into<String>) -> Vec<Value> {
    vec![json!({ "type": "text", "text": t.into() })]
}

/// The calling agent's task, its pane and what it is: its tab is the one
/// it uses (BRW-2).
fn task_of(state: &AppState, caller: Option<&str>) -> Result<(String, String, String)> {
    let pane = caller.ok_or_else(|| {
        Error::Other("only an agent the app started has a browser tab: it is found by the pane the agent runs in".into())
    })?;
    let info = state
        .ptys
        .info(pane)
        .map_err(|_| Error::Other("this agent's pane is not one the app knows, so it has no browser tab".into()))?;
    if info.task_id == CHAT_TASK_ID {
        return Err(Error::Other("a chat has no browser: the browser is per task, for the agents working on it".into()));
    }
    let agent = info
        .agent_id
        .as_deref()
        .and_then(crate::agents::find)
        .map_or_else(|| "An agent".to_string(), |a| a.name.to_string());
    Ok((info.task_id, pane.to_string(), agent))
}

/// The refusal for a page on a site agents may not use.
fn not_allowed(url: &str) -> Error {
    let site = sites::host_of(url).unwrap_or_else(|| url.chars().take(80).collect());
    Error::Other(format!(
        "{site} is not a site agents may use: only this machine's pages (localhost) and the \
         sites the user allowed are. Ask with browser_request_site, saying what you need it for."
    ))
}

/// The page, if agents may use it as it is now.
async fn allowed_page(page: &Page, state: &AppState) -> Result<(String, String)> {
    // A page with a dialog open answers nothing until it is answered: its
    // address as last heard, instead of asking it.
    let heard = || state.browser.state_of(&page.task).map(|s| (s.0, s.1)).unwrap_or_default();
    let (url, title) = match state.browser.dialog(&page.task) {
        Some(_) => heard(),
        None => match page.location().await? {
            // Chrome's error page for one that could not load: the page is
            // the address that failed, which the tab keeps.
            (u, _) if u.starts_with("chrome-error:") => heard(),
            here => here,
        },
    };
    let sites = state.config.read().browser.sites;
    if !sites::allowed(&url, &sites) {
        return Err(not_allowed(&url));
    }
    Ok((url, title))
}

/// What an action did, then the page as it is now: the one shown, which is
/// a window the page opened if the action opened one (BRW-14).
async fn report<R: Runtime>(app: &AppHandle<R>, state: &AppState, task: &str, did: &str) -> Result<Vec<Value>> {
    let page = &state.browser.page(app, task).await?;
    let (url, title) = match allowed_page(page, state).await {
        Ok(at) => at,
        // The action itself happened; where it led is what agents may not read.
        Err(e) => return Ok(text(format!("{did}\n\nThe tab is now somewhere you cannot read: {e}"))),
    };
    if let Some(d) = state.browser.dialog(&page.task) {
        return Ok(text(format!("{did}\n\nPage: {title}\nURL: {url}\n\n{}", d.describe())));
    }
    let outline = page.outline().await?;
    let tabs = state.browser.tabs(task);
    let tabs = if tabs.len() > 1 { format!("{}\n", super::tabs::tab_list(&tabs, state)) } else { String::new() };
    Ok(text(format!("{did}\n\nPage: {title}\nURL: {url}\n{tabs}\n{outline}")))
}

/// How the panel names an element an agent acted on: its label, not its
/// value, which for a password field is the password.
async fn label(page: &Page, n: i64) -> String {
    page.call_on(n, super::page::LABEL, vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|l| !l.is_empty())
        .map(|l| format!("“{l}”"))
        .unwrap_or_else(|| format!("e{n}"))
}

fn node(args: &Value) -> Result<i64> {
    let r = required(args, "ref")?;
    snapshot::node_of(r).ok_or_else(|| Error::Other(format!("{r} is not a ref: use one from the outline, like e12")))
}

pub async fn call<R: Runtime>(app: &AppHandle<R>, name: &str, args: Value, caller: Option<&str>) -> Result<Vec<Value>> {
    let state = app.state::<AppState>();
    let (task, pane, agent) = task_of(&state, caller)?;
    let browser: &Browser = &state.browser;
    // A question for the user, not the browser: nothing to start for it.
    if name == "browser_request_site" {
        return super::requests::request_site(app, &state, &task, &args).await;
    }
    // Reads too: what the user types while signing in is theirs.
    if browser.held(&task) {
        return Err(Error::Other(super::control::HELD.into()));
    }
    // The tab is the agent's until this call ends (BRW-15).
    let _driving = browser.drive(app, &task, &pane, &agent);
    let page = browser.page(app, &task).await?;
    // The page does nothing else until its dialog is answered, and a call
    // into it would wait as long. Reading the outline still says so.
    // Tabs are the browser's, not the page's: another one can be opened or
    // switched to while this one waits.
    if !matches!(
        name,
        "browser_dialog" | "browser_snapshot" | "browser_console" | "browser_tabs" | "browser_new_tab"
            | "browser_switch_tab" | "browser_close_tab"
    ) {
        if let Some(d) = browser.dialog(&task) {
            return Err(Error::Other(format!("Nothing was done. {}", d.describe())));
        }
    }

    match name {
        "browser_navigate" => {
            let url = sites::complete(required(&args, "url")?);
            if !sites::allowed(&url, &state.config.read().browser.sites) {
                return Err(not_allowed(&url));
            }
            browser.record(app, &task, &format!("Opened {url}"), None);
            page.navigate(&url).await?;
            page.settle(browser).await;
            report(app, &state, &task, &format!("Opened {url}.")).await
        }

        "browser_back" => {
            browser.record(app, &task, "Went back", None);
            let went = page.back().await?;
            if !went {
                return report(app, &state, &task, "There is no page to go back to.").await;
            }
            page.settle(browser).await;
            report(app, &state, &task, "Went back.").await
        }

        "browser_snapshot" => {
            browser.record(app, &task, "Read the page", None);
            report(app, &state, &task, "").await.map(|mut c| {
            if let Some(t) = c[0].get("text").and_then(Value::as_str) {
                c[0]["text"] = json!(t.trim_start());
            }
            c
        })
        }

        "browser_click" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let at = page.center(n).await?;
            let right = args.get("right").and_then(Value::as_bool).unwrap_or(false);
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            let what = if double { "Double-clicked" } else if right { "Right-clicked" } else { "Clicked" };
            browser.record(app, &task, &format!("{what} {}", label(&page, n).await), Some(at));
            browser.until_dialog(page.click(at, if right { "right" } else { "left" }, if double { 2 } else { 1 })).await?;
            page.settle(browser).await;
            report(app, &state, &task, &format!("Clicked e{n}.")).await
        }

        "browser_hover" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let at = page.center(n).await?;
            browser.record(app, &task, &format!("Pointed at {}", label(&page, n).await), Some(at));
            browser.until_dialog(page.hover(at)).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            report(app, &state, &task, &format!("The mouse is over e{n}.")).await
        }

        "browser_type" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let typed = args.get("text").and_then(Value::as_str).unwrap_or_default();
            let at = page.center(n).await.ok();
            browser.record(app, &task, &format!("Typed into {}", label(&page, n).await), at);
            let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
            let submit = args.get("submit").and_then(Value::as_bool).unwrap_or(false);
            browser
                .until_dialog(async {
                    page.focus(n).await?;
                    if clear {
                        page.call_on(n, super::page::SELECT_ALL, vec![]).await?;
                        page.key("Backspace").await?;
                    }
                    if !typed.is_empty() {
                        page.insert_text(typed).await?;
                    }
                    if submit {
                        page.key("Enter").await?;
                    }
                    Ok(())
                })
                .await?;
            page.settle(browser).await;
            report(app, &state, &task, &format!("Typed into e{n}.")).await
        }

        "browser_press_key" => {
            allowed_page(&page, &state).await?;
            let key = required(&args, "key")?;
            browser.record(app, &task, &format!("Pressed {key}"), None);
            browser.until_dialog(page.key(key)).await?;
            page.settle(browser).await;
            report(app, &state, &task, &format!("Pressed {key}.")).await
        }

        "browser_select" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let values: Vec<Value> = args.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
            browser.record(app, &task, &format!("Chose an option in {}", label(&page, n).await), page.center(n).await.ok());
            match browser.until_dialog(page.call_on(n, super::page::SELECT_OPTIONS, vec![Value::Array(values)])).await? {
                Some(chosen) => Ok(text(format!("Chosen in e{n}: {chosen}"))),
                None => report(app, &state, &task, &format!("Chose in e{n}.")).await,
            }
        }

        "browser_scroll" => {
            allowed_page(&page, &state).await?;
            let dy = args.get("pixels").and_then(Value::as_f64).unwrap_or(700.0);
            let at = match arg(&args, "ref") {
                Some(_) => page.center(node(&args)?).await?,
                None => {
                    let v = browser.viewport(&task);
                    (v.width as f64 / 2.0, v.height as f64 / 2.0)
                }
            };
            browser.record(app, &task, &format!("Scrolled {} {} pixels", if dy < 0.0 { "up" } else { "down" }, dy.abs()), Some(at));
            browser.until_dialog(page.wheel(at, dy)).await?;
            tokio::time::sleep(Duration::from_millis(400)).await;
            report(app, &state, &task, &format!("Scrolled {dy} pixels.")).await
        }

        "browser_wait" => {
            allowed_page(&page, &state).await?;
            let seconds = args.get("seconds").and_then(Value::as_f64).unwrap_or(5.0).clamp(0.0, 30.0);
            let deadline = Instant::now() + Duration::from_secs_f64(seconds);
            browser.record(app, &task, "Waiting on the page", None);
            let did = match arg(&args, "text") {
                Some(wanted) => {
                    let probe = format!("document.body ? document.body.innerText.includes({}) : false", json!(wanted));
                    loop {
                        if page.eval(&probe).await.ok() == Some(Value::Bool(true)) {
                            break format!("The page shows \"{wanted}\".");
                        }
                        if Instant::now() >= deadline {
                            break format!("The page did not show \"{wanted}\" within {seconds} seconds.");
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
                None => {
                    tokio::time::sleep_until(deadline.into()).await;
                    format!("Waited {seconds} seconds.")
                }
            };
            report(app, &state, &task, &did).await
        }

        "browser_screenshot" => {
            let (url, _) = allowed_page(&page, &state).await?;
            browser.record(app, &task, "Took a screenshot", None);
            let data = page.screenshot(browser.viewport(&task)).await?;
            Ok(vec![
                json!({ "type": "image", "data": data, "mimeType": "image/jpeg" }),
                json!({ "type": "text", "text": format!("The tab, at {url}.") }),
            ])
        }

        "browser_console" => {
            allowed_page(&page, &state).await?;
            let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
            browser.record(app, &task, "Read the console", None);
            let lines = browser.console(&task, clear);
            Ok(text(if lines.is_empty() { "The console is empty.".to_string() } else { lines.join("\n") }))
        }

        "browser_evaluate" => {
            allowed_page(&page, &state).await?;
            browser.record(app, &task, "Ran JavaScript in the page", None);
            let Some(value) = browser.until_dialog(page.eval(required(&args, "expression")?)).await? else {
                return report(app, &state, &task, "Ran it.").await;
            };
            let shown = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            Ok(text(shown.chars().take(40_000).collect::<String>()))
        }

        "browser_tabs" => Ok(text(super::tabs::tab_list(&browser.tabs(&task), &state))),

        "browser_new_tab" => {
            let url = sites::complete(required(&args, "url")?);
            if !sites::allowed(&url, &state.config.read().browser.sites) {
                return Err(not_allowed(&url));
            }
            browser.record(app, &task, &format!("Opened a new tab at {url}"), None);
            let page = browser.new_tab(app, &task, Some(&url)).await?;
            page.settle(browser).await;
            report(app, &state, &task, &format!("Opened {url} in a new tab.")).await
        }

        "browser_switch_tab" => {
            let tab = super::tabs::tab_numbered(&browser.tabs(&task), &args)?;
            let name = browser.switch_tab(app, &task, &tab.id)?;
            let shown = if sites::allowed(&tab.url, &state.config.read().browser.sites) { name } else { "a tab".into() };
            browser.record(app, &task, &format!("Switched to “{shown}”"), None);
            report(app, &state, &task, &format!("Switched to “{shown}”.")).await
        }

        "browser_close_tab" => {
            let tab = super::tabs::tab_numbered(&browser.tabs(&task), &args)?;
            browser.record(app, &task, "Closed a tab", None);
            browser.close_tab(app, &task, &tab.id).await?;
            report(app, &state, &task, "Closed the tab.").await
        }

        "browser_dialog" => {
            let accept = args
                .get("accept")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error::Other("accept is required: true for OK, false for Cancel".into()))?;
            let d = browser.answer_dialog(&task, accept, arg(&args, "text")).await?;
            let what = if accept { "Accepted" } else { "Dismissed" };
            browser.record(app, &task, &format!("{what} the page's {}", d.kind), None);
            page.settle(browser).await;
            report(app, &state, &task, &format!("{what} the {} dialog.", d.kind)).await
        }

        other => Err(Error::NotFound(format!("tool {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_in_task, app_with_task, serve, HOME};

    fn texts(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
    }

    /// The ref the outline gives the line that holds `what`.
    fn ref_of(outline: &str, what: &str) -> String {
        let line = outline.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in\n{outline}"));
        let at = line.find("[ref=").unwrap() + 5;
        line[at..].split(']').next().unwrap().to_string()
    }

    /// An agent asks for a site and waits; the user's Allow adds it to the
    /// list and is the agent's answer. Asking again while it waits is the
    /// same question, not a second one, and a site already allowed is not
    /// asked about at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_site_an_agent_asks_for_is_allowed_only_by_the_user() {
        let (app, root, pane) = app_in_task();
        // No banner from a test: the mock app has no window, so it reads as away.
        app.state::<AppState>().config.update(|c| c.ui.notify_waiting_agents = false).unwrap();
        let handle = app.handle().clone();
        let ask = |pane: String| {
            let handle = handle.clone();
            tokio::spawn(async move {
                let args = json!({ "site": "https://Docs.Example.com/guide", "reason": "Read the API guide" });
                texts(&call(&handle, "browser_request_site", args, Some(&pane)).await.unwrap())
            })
        };
        let first = ask(pane.clone());
        let second = ask(pane.clone());
        let state = app.state::<AppState>();
        let mut asked = Vec::new();
        for _ in 0..100 {
            asked = state.browser.requests("t1");
            if !asked.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let asked_now = state.browser.requests("t1");
        assert_eq!(asked_now.len(), 1, "one question, however often it is asked");
        assert_eq!((asked[0].site.as_str(), asked[0].reason.as_str()), ("docs.example.com", "Read the API guide"));
        assert!(state.messages.list().iter().any(|m| m.title == "An agent asks to use docs.example.com in its browser"));

        state.config.update(|c| c.browser.sites.push("docs.example.com".into())).unwrap();
        state.browser.answer(app.handle(), asked[0].id, true);
        for answer in [first.await.unwrap(), second.await.unwrap()] {
            assert_eq!(answer, "The user allowed docs.example.com. browser_navigate there now.");
        }
        assert!(state.browser.requests("t1").is_empty());

        let again = texts(&call(app.handle(), "browser_request_site", json!({ "site": "docs.example.com", "reason": "again" }), Some(&pane)).await.unwrap());
        assert_eq!(again, "Agents may already use docs.example.com.");

        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole way, against a real Chrome: an agent in a task opens this
    /// machine's page, reads it, types, chooses, clicks with real input, sees
    /// the console, goes back, takes a screenshot, and is refused a page on
    /// the internet. Skipped where no Chrome is installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_uses_its_task_s_tab_on_this_machine_and_nothing_else() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(HOME).await;
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));

        let opened = texts(&tool("browser_navigate", json!({ "url": format!("{base}/") })).await.unwrap());
        assert!(opened.contains("Page: Home"), "{opened}");
        assert!(opened.contains("heading \"Hello\""), "{opened}");

        let field = ref_of(&opened, "textbox \"User name\"");
        let typed = texts(&tool("browser_type", json!({ "ref": field, "text": "ada" })).await.unwrap());
        assert!(typed.contains(&format!("textbox \"User name\" [ref={field}] [focused]: \"ada\"\n- combobox")), "{typed}");

        let size = ref_of(&opened, "combobox \"Size\"");
        let chosen = texts(&tool("browser_select", json!({ "ref": size, "values": ["Large"] })).await.unwrap());
        assert!(chosen.contains("Large"), "{chosen}");

        let save = ref_of(&opened, "button \"Save\"");
        let clicked = texts(&tool("browser_click", json!({ "ref": save })).await.unwrap());
        assert!(clicked.contains("Page: Saved true"), "a click is real input: {clicked}");
        let did = state.browser.view("t1").last_action.unwrap();
        assert_eq!(did.text, "Clicked “Save”", "the panel names it as a person would");
        assert!(did.x.is_some() && did.y.is_some());
        let console = texts(&tool("browser_console", json!({})).await.unwrap());
        assert!(console.contains("[log] saved ada"), "{console}");

        let link = ref_of(&opened, "link \"Next page\"");
        let next = texts(&tool("browser_click", json!({ "ref": link })).await.unwrap());
        assert!(next.contains("The second page"), "{next}");
        let back = texts(&tool("browser_back", json!({})).await.unwrap());
        assert!(back.contains("heading \"Hello\""), "{back}");

        let shot = tool("browser_screenshot", json!({})).await.unwrap();
        assert_eq!(shot[0]["type"], "image");
        assert!(shot[0]["data"].as_str().unwrap().len() > 1000);

        let refused = tool("browser_navigate", json!({ "url": "https://example.com" })).await.unwrap_err();
        assert!(refused.to_string().contains("example.com is not a site agents may use"), "{refused}");

        assert_eq!(
            state.config.read().tasks[0].browser.clone().map(|b| b.urls),
            Some(vec![format!("{base}/")]),
            "the tab's page is kept for next time"
        );

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
