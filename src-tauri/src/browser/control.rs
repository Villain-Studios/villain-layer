//! Who has a tab: the user, while they hold it (BRW-12), or a dialog the
//! page opened, until someone answers it (BRW-13).

use std::future::Future;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Runtime};

use super::{changed, Browser};
use crate::error::{Error, Result};

/// An alert, confirm or prompt the page opened. A headless Chrome draws
/// none, and the page's scripts stop until it is answered.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Dialog {
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub kind: String,
    pub message: String,
    /// What a prompt offers before anything is typed.
    pub default_prompt: String,
}

impl Dialog {
    pub(super) fn from_event(p: &Value) -> Self {
        let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        Self { kind: text("type"), message: text("message"), default_prompt: text("defaultPrompt") }
    }

    /// The dialog as an agent is told of it.
    pub fn describe(&self) -> String {
        let message: String = self.message.chars().take(500).collect();
        format!(
            "The page opened a {} dialog: \"{message}\". Its scripts wait until it is answered: \
             browser_dialog, accept true or false{}.",
            self.kind,
            if self.kind == "prompt" { ", with the text to give it" } else { "" }
        )
    }
}

/// The refusal while the user holds the tab.
pub const HELD: &str = "The user has taken over this task's browser, to sign in or to look \
     at something, and agents wait until they hand it back. Tell the user what you were about \
     to do there, and try again once they have.";

impl Browser {
    /// The user takes the task's tab, or hands it back (BRW-12).
    pub fn hold<R: Runtime>(&self, app: &AppHandle<R>, task: &str, held: bool) {
        {
            let mut inner = self.inner.lock();
            if held {
                inner.held.insert(task.to_string());
            } else {
                inner.held.remove(task);
            }
        }
        changed(app, task);
    }

    pub fn held(&self, task: &str) -> bool {
        self.inner.lock().held.contains(task)
    }

    /// The dialog open in the task's tab, if one is.
    pub fn dialog(&self, task: &str) -> Option<Dialog> {
        self.inner.lock().tabs.get(task).and_then(|t| t.dialog.clone())
    }

    /// Answer the task's open dialog: accept it (OK), or dismiss it
    /// (Cancel), with `text` for a prompt.
    pub async fn answer_dialog(&self, task: &str, accept: bool, text: Option<&str>) -> Result<Dialog> {
        let (cdp, session, dialog) = {
            let inner = self.inner.lock();
            let tab = inner.tabs.get(task);
            let dialog = tab.and_then(|t| t.dialog.clone()).ok_or_else(|| Error::Other("no dialog is open".into()))?;
            let cdp = inner.running.as_ref().map(|r| r.cdp.clone()).ok_or_else(|| Error::Other("the browser closed".into()))?;
            (cdp, tab.map(|t| t.session.clone()).unwrap_or_default(), dialog)
        };
        let mut params = json!({ "accept": accept });
        if let Some(t) = text {
            params["promptText"] = json!(t);
        }
        cdp.call("Page.handleJavaScriptDialog", params, Some(&session)).await?;
        // Closed now, whether or not Chrome's word of it has been read yet.
        if let Some(tab) = self.inner.lock().tabs.get_mut(task) {
            tab.dialog = None;
        }
        Ok(dialog)
    }

    /// Run an action on the page, but stop waiting on it if the page opens
    /// a dialog: Chrome answers an input event only once its handlers have
    /// run, and an `alert()` in one holds them until the dialog is answered.
    /// A click that opened one waited out the whole call timeout.
    pub async fn until_dialog<T>(&self, action: impl Future<Output = Result<T>>) -> Result<Option<T>> {
        let opened = self.dialog_opened.notified();
        tokio::select! {
            done = action => done.map(Some),
            _ = opened => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve};
    use crate::browser::tools::call;
    use crate::commands::AppState;
    use std::time::{Duration, Instant};
    use tauri::Manager;

    const ASKS: &str = r#"<html><head><title>Asks</title></head><body>
        <button onclick="document.title = confirm('Delete it?') ? 'Deleted' : 'Kept'">Delete</button>
    </body></html>"#;

    fn text(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect()
    }

    /// Against a real Chrome: a click that opens a confirm comes back at once
    /// saying so, not after the call's timeout; nothing else is done until it
    /// is answered; answering it lets the page go on. While the user holds
    /// the tab, agents are refused, reads included.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dialog_stops_the_agent_until_answered_and_a_held_tab_stops_it_altogether() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(ASKS).await;
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));

        let opened = text(&tool("browser_navigate", json!({ "url": format!("{base}/") })).await.unwrap());
        let at = opened.find("button \"Delete\" [ref=").unwrap() + "button \"Delete\" [ref=".len();
        let button = opened[at..].split(']').next().unwrap().to_string();

        let started = Instant::now();
        let clicked = text(&tool("browser_click", json!({ "ref": button })).await.unwrap());
        assert!(started.elapsed() < Duration::from_secs(10), "came back when the dialog opened");
        assert!(clicked.contains("The page opened a confirm dialog: \"Delete it?\""), "{clicked}");

        let refused = tool("browser_click", json!({ "ref": button })).await.unwrap_err();
        assert!(refused.to_string().starts_with("Nothing was done."), "{refused}");

        let answered = text(&tool("browser_dialog", json!({ "accept": false })).await.unwrap());
        assert!(answered.contains("Page: Kept"), "{answered}");
        assert_eq!(state.browser.dialog("t1"), None);

        state.browser.hold(app.handle(), "t1", true);
        let held = tool("browser_snapshot", json!({})).await.unwrap_err();
        assert_eq!(held.to_string(), HELD);
        state.browser.hold(app.handle(), "t1", false);
        assert!(text(&tool("browser_snapshot", json!({})).await.unwrap()).contains("Page: Kept"));

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_agent_is_told_what_the_dialog_says_and_how_to_answer_it() {
        let d = Dialog::from_event(&json!({ "type": "prompt", "message": "Your name?", "defaultPrompt": "ada" }));
        assert_eq!(d.default_prompt, "ada");
        assert_eq!(
            d.describe(),
            "The page opened a prompt dialog: \"Your name?\". Its scripts wait until it is answered: \
             browser_dialog, accept true or false, with the text to give it."
        );
    }
}
