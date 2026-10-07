//! Agents asking for sites, until the user answers (BRW-11).

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use super::{changed, sites, Browser};
use crate::commands::AppState;
use crate::error::{Error, Result};
use crate::mcp::required;
use super::tools::text;

/// An agent asking for a site, until the user answers (BRW-11).
#[derive(Clone, Debug, Serialize)]
pub struct SiteRequest {
    pub id: u64,
    pub task: String,
    pub site: String,
    /// What the agent wants there, in its own words.
    pub reason: String,
    pub at: i64,
}

pub(super) struct Asked {
    pub(super) request: SiteRequest,
    /// The answer, for the tool call waiting on it.
    answer: tokio::sync::watch::Sender<Option<bool>>,
}

impl Browser {
    /// An agent's request for `site`, and the answer to wait on. The same
    /// request again, from the same task, waits on the one already asked:
    /// an agent that timed out and asks again is not a second question.
    /// True when it is new.
    pub fn ask<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        task: &str,
        site: &str,
        reason: &str,
    ) -> (SiteRequest, tokio::sync::watch::Receiver<Option<bool>>, bool) {
        let mut inner = self.inner.lock();
        if let Some(a) = inner.requests.iter().find(|a| a.request.task == task && a.request.site == site) {
            return (a.request.clone(), a.answer.subscribe(), false);
        }
        inner.next_request += 1;
        let request = SiteRequest {
            id: inner.next_request,
            task: task.to_string(),
            site: site.to_string(),
            reason: reason.chars().take(500).collect(),
            at: chrono::Utc::now().timestamp_millis(),
        };
        let (answer, waiting) = tokio::sync::watch::channel(None);
        inner.requests.push(Asked { request: request.clone(), answer });
        drop(inner);
        changed(app, task);
        (request, waiting, true)
    }

    /// The user's answer to a request; the request, gone from the list.
    pub fn answer<R: Runtime>(&self, app: &AppHandle<R>, id: u64, allow: bool) -> Option<SiteRequest> {
        let asked = {
            let mut inner = self.inner.lock();
            let at = inner.requests.iter().position(|a| a.request.id == id)?;
            inner.requests.remove(at)
        };
        let _ = asked.answer.send(Some(allow));
        changed(app, &asked.request.task);
        Some(asked.request)
    }

    /// Every request waiting on the user.
    pub fn requests_all(&self) -> Vec<SiteRequest> {
        self.inner.lock().requests.iter().map(|a| a.request.clone()).collect()
    }

    /// What the task's agents are waiting to hear about.
    pub fn requests(&self, task: &str) -> Vec<SiteRequest> {
        let inner = self.inner.lock();
        inner.requests.iter().filter(|a| a.request.task == task).map(|a| a.request.clone()).collect()
    }

}

/// How long `browser_request_site` waits on the user before saying there is
/// no answer yet.
const ANSWER_WAIT: Duration = Duration::from_secs(120);

/// Ask the user for a site, and wait for the answer (BRW-11).
pub(super) async fn request_site<R: Runtime>(app: &AppHandle<R>, state: &AppState, task: &str, args: &Value) -> Result<Vec<Value>> {
    let asked = required(args, "site")?;
    let site = sites::normalize(asked)
        .ok_or_else(|| Error::Other(format!("{asked} is not a site: give one like github.com")))?;
    let reason = required(args, "reason")?;
    if sites::allowed(&format!("https://{site}/"), &state.config.read().browser.sites) {
        return Ok(text(format!("Agents may already use {site}.")));
    }
    let (request, mut answer, new) = state.browser.ask(app, task, &site, reason);
    if new {
        tell_user(app, state, &request);
    }
    let said = tokio::time::timeout(ANSWER_WAIT, answer.wait_for(Option::is_some)).await;
    Ok(text(match said.ok().and_then(|r| r.ok().and_then(|a| *a)) {
        Some(true) => format!("The user allowed {site}. browser_navigate there now."),
        Some(false) => format!(
            "The user said no to {site}. Do not ask for it again in this task unless they say otherwise."
        ),
        None => format!(
            "No answer yet: the request for {site} stays in the task's Browser panel. Tell the user \
             what you need it for, and carry on with something else; browser_navigate there will \
             work once they allow it."
        ),
    }))
}

/// A request goes to the message center, and to a banner while the window
/// is away: the agent asking is waiting on the user, like one at a prompt.
fn tell_user<R: Runtime>(app: &AppHandle<R>, state: &AppState, r: &SiteRequest) {
    let target = crate::target::Target::Task(&r.task).to_string();
    let title = format!("An agent asks to use {} in its browser", r.site);
    crate::messages::record_all(app, vec![crate::messages::New {
        kind: crate::messages::Kind::Agent,
        level: crate::messages::Level::Info,
        title: title.clone(),
        body: r.reason.clone(),
        target: Some(target.clone()),
    }]);
    let away = !app.get_webview_window("main").and_then(|w| w.is_focused().ok()).unwrap_or(false);
    if away && state.config.read().ui.notify_waiting_agents {
        let _ = crate::commands::banner(app, title, r.reason.clone(), target);
    }
}
