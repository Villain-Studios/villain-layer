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
             (localhost and the like, http unless you say https) are allowed. Use it to \
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
            "browser_evaluate",
            "Run JavaScript in the page and get its value back, as JSON. An expression, or \
             an async one: await works. For what the outline does not show: a value in \
             the app's state, local storage, a computed style.",
            json!({ "expression": str_prop("The JavaScript, e.g. document.querySelectorAll('li').length") }),
            vec!["expression"],
        ),
    ]
}

fn text(t: impl Into<String>) -> Vec<Value> {
    vec![json!({ "type": "text", "text": t.into() })]
}

/// The calling agent's task: its tab is the one it uses (BRW-2).
fn task_of(state: &AppState, caller: Option<&str>) -> Result<String> {
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
    Ok(info.task_id)
}

/// The refusal for a page on a site agents may not use.
fn not_allowed(url: &str) -> Error {
    let site = sites::host_of(url).unwrap_or_else(|| url.chars().take(80).collect());
    Error::Other(format!(
        "{site} is not a site agents may use: only this machine's pages (localhost) are. \
         Tell the user what you wanted there, and they can open it themselves."
    ))
}

/// The page, if agents may use it as it is now.
async fn allowed_page(page: &Page, state: &AppState) -> Result<(String, String)> {
    let (url, title) = page.location().await?;
    let sites = state.config.read().browser.sites;
    if !sites::allowed(&url, &sites) {
        return Err(not_allowed(&url));
    }
    Ok((url, title))
}

/// What an action did, then the page as it is now.
async fn report(page: &Page, state: &AppState, did: &str) -> Result<Vec<Value>> {
    let (url, title) = match allowed_page(page, state).await {
        Ok(at) => at,
        // The action itself happened; where it led is what agents may not read.
        Err(e) => return Ok(text(format!("{did}\n\nThe tab is now somewhere you cannot read: {e}"))),
    };
    let outline = page.outline().await?;
    Ok(text(format!("{did}\n\nPage: {title}\nURL: {url}\n\n{outline}")))
}

fn node(args: &Value) -> Result<i64> {
    let r = required(args, "ref")?;
    snapshot::node_of(r).ok_or_else(|| Error::Other(format!("{r} is not a ref: use one from the outline, like e12")))
}

pub async fn call<R: Runtime>(app: &AppHandle<R>, name: &str, args: Value, caller: Option<&str>) -> Result<Vec<Value>> {
    let state = app.state::<AppState>();
    let task = task_of(&state, caller)?;
    let browser: &Browser = &state.browser;
    let page = browser.page(app, &task).await?;

    match name {
        "browser_navigate" => {
            let url = sites::complete(required(&args, "url")?);
            if !sites::allowed(&url, &state.config.read().browser.sites) {
                return Err(not_allowed(&url));
            }
            page.navigate(&url).await?;
            page.settle(browser).await;
            report(&page, &state, &format!("Opened {url}.")).await
        }

        "browser_back" => {
            let went = page.back().await?;
            if !went {
                return report(&page, &state, "There is no page to go back to.").await;
            }
            page.settle(browser).await;
            report(&page, &state, "Went back.").await
        }

        "browser_snapshot" => report(&page, &state, "").await.map(|mut c| {
            if let Some(t) = c[0].get("text").and_then(Value::as_str) {
                c[0]["text"] = json!(t.trim_start());
            }
            c
        }),

        "browser_click" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let at = page.center(n).await?;
            let right = args.get("right").and_then(Value::as_bool).unwrap_or(false);
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            page.click(at, if right { "right" } else { "left" }, if double { 2 } else { 1 }).await?;
            page.settle(browser).await;
            report(&page, &state, &format!("Clicked e{n}.")).await
        }

        "browser_hover" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            page.hover(page.center(n).await?).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            report(&page, &state, &format!("The mouse is over e{n}.")).await
        }

        "browser_type" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let typed = args.get("text").and_then(Value::as_str).unwrap_or_default();
            page.focus(n).await?;
            if args.get("clear").and_then(Value::as_bool).unwrap_or(false) {
                page.call_on(n, SELECT_ALL, vec![]).await?;
                page.key("Backspace").await?;
            }
            if !typed.is_empty() {
                page.insert_text(typed).await?;
            }
            if args.get("submit").and_then(Value::as_bool).unwrap_or(false) {
                page.key("Enter").await?;
            }
            page.settle(browser).await;
            report(&page, &state, &format!("Typed into e{n}.")).await
        }

        "browser_press_key" => {
            allowed_page(&page, &state).await?;
            let key = required(&args, "key")?;
            page.key(key).await?;
            page.settle(browser).await;
            report(&page, &state, &format!("Pressed {key}.")).await
        }

        "browser_select" => {
            allowed_page(&page, &state).await?;
            let n = node(&args)?;
            let values: Vec<Value> = args.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
            let chosen = page.call_on(n, SELECT_OPTIONS, vec![Value::Array(values)]).await?;
            Ok(text(format!("Chosen in e{n}: {chosen}")))
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
            page.wheel(at, dy).await?;
            tokio::time::sleep(Duration::from_millis(400)).await;
            report(&page, &state, &format!("Scrolled {dy} pixels.")).await
        }

        "browser_wait" => {
            allowed_page(&page, &state).await?;
            let seconds = args.get("seconds").and_then(Value::as_f64).unwrap_or(5.0).clamp(0.0, 30.0);
            let deadline = Instant::now() + Duration::from_secs_f64(seconds);
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
            report(&page, &state, &did).await
        }

        "browser_screenshot" => {
            let (url, _) = allowed_page(&page, &state).await?;
            let data = page.screenshot(browser.viewport(&task)).await?;
            Ok(vec![
                json!({ "type": "image", "data": data, "mimeType": "image/jpeg" }),
                json!({ "type": "text", "text": format!("The tab, at {url}.") }),
            ])
        }

        "browser_console" => {
            allowed_page(&page, &state).await?;
            let clear = args.get("clear").and_then(Value::as_bool).unwrap_or(false);
            let lines = browser.console(&task, clear);
            Ok(text(if lines.is_empty() { "The console is empty.".to_string() } else { lines.join("\n") }))
        }

        "browser_evaluate" => {
            allowed_page(&page, &state).await?;
            let value = page.eval(required(&args, "expression")?).await?;
            let shown = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            Ok(text(shown.chars().take(40_000).collect::<String>()))
        }

        other => Err(Error::NotFound(format!("tool {other}"))),
    }
}

/// Select everything in a field, or in an editable element.
const SELECT_ALL: &str = "function () {
  if (typeof this.select === 'function') { this.select(); return; }
  const range = document.createRange();
  range.selectNodeContents(this);
  const s = window.getSelection();
  s.removeAllRanges();
  s.addRange(range);
}";

/// Choose a list's options by value or text, as a person's choice would:
/// with input and change events, which is what frameworks listen to.
///
/// Matched first and set after: in a single-choice list, unselecting the
/// chosen option makes the browser select the first one, which then read
/// as chosen.
const SELECT_OPTIONS: &str = "function (wanted) {
  if (!(this instanceof HTMLSelectElement)) throw new Error('that element is not a <select> list');
  const matches = [...this.options].filter((o) => wanted.includes(o.value) || wanted.includes(o.label.trim()));
  if (matches.length === 0) throw new Error('no option is called ' + wanted.join(' or '));
  if (this.multiple) for (const o of this.options) o.selected = matches.includes(o);
  else matches[0].selected = true;
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return [...this.selectedOptions].map((o) => o.label.trim());
}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ConfigStore, Task};
    use crate::pty::{PaneKind, SpawnOptions};

    const HOME: &str = r#"<html><head><title>Home</title></head><body>
        <h1>Hello</h1>
        <a href="/two">Next page</a>
        <input aria-label="User name">
        <select aria-label="Size"><option value="s">Small</option><option value="l">Large</option></select>
        <button onclick="console.log('saved', document.querySelector('input').value); document.title = 'Saved ' + event.isTrusted">Save</button>
    </body></html>"#;

    async fn serve() -> String {
        use axum::{response::Html, routing::get, Router};
        let router = Router::new()
            .route("/", get(|| async { Html(HOME) }))
            .route("/two", get(|| async { Html("<html><head><title>Two</title></head><body><p>The second page</p></body></html>") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://localhost:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move { axum::serve(listener, router).await });
        base
    }

    fn texts(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
    }

    /// The ref the outline gives the line that holds `what`.
    fn ref_of(outline: &str, what: &str) -> String {
        let line = outline.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in\n{outline}"));
        let at = line.find("[ref=").unwrap() + 5;
        line[at..].split(']').next().unwrap().to_string()
    }

    /// The whole way, against a real Chrome: an agent in a task opens this
    /// machine's page, reads it, types, chooses, clicks with real input, sees
    /// the console, goes back, takes a screenshot, and is refused a page on
    /// the internet. Skipped where no Chrome is installed.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_uses_its_task_s_tab_on_this_machine_and_nothing_else() {
        if super::super::chrome::find().is_none() {
            eprintln!("skipped: no Chrome installed");
            return;
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
        let state = app.state::<AppState>();
        let pane = state
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
        let base = serve().await;
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
            state.config.read().tasks[0].browser_url.as_deref(),
            Some(format!("{base}/").as_str()),
            "the tab's page is kept for next time"
        );

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }
}
