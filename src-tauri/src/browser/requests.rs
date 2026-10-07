//! Agents asking for sites, until the user answers (BRW-11).

use serde::Serialize;
use tauri::{AppHandle, Runtime};

use super::{changed, Browser};

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
