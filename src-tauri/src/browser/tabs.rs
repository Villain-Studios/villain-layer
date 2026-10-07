//! A task's tabs (BRW-16, BRW-17): one or more, one of them active, shared
//! by the user and the task's agents. Everything that acts on "the task's
//! tab" acts on the active one.

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use super::{changed, make_tab, sites, AgentAction, Browser, Page, Tab};
use crate::commands::AppState;
use crate::config::SavedTabs;
use crate::error::{Error, Result};

/// A task's tabs, and what an agent last did in any of them.
pub(super) struct Tabs {
    pub(super) list: Vec<Tab>,
    pub(super) active: usize,
    pub(super) last_action: Option<AgentAction>,
}

impl Tabs {
    pub(super) fn new(list: Vec<Tab>, active: usize) -> Self {
        let active = active.min(list.len().saturating_sub(1));
        Self { list, active, last_action: None }
    }

    pub(super) fn active(&self) -> Option<&Tab> {
        self.list.get(self.active)
    }

    pub(super) fn active_mut(&mut self) -> Option<&mut Tab> {
        self.list.get_mut(self.active)
    }

    pub(super) fn position(&self, target: &str) -> Option<usize> {
        self.list.iter().position(|t| t.target == target)
    }

    /// Put `tab` next to the tab `after` (or last), and make it active: a
    /// link that opens a tab opens it beside the one it was clicked in.
    pub(super) fn open_next_to(&mut self, tab: Tab, after: Option<&str>) {
        let at = after.and_then(|t| self.position(t)).map_or(self.list.len(), |i| i + 1);
        self.list.insert(at, tab);
        self.active = at;
    }

    /// Take a tab out. If it was the active one, the tab that opened it is
    /// active again if it is still open (a sign-in window closing returns to
    /// its page), or else the one before it.
    pub(super) fn remove(&mut self, target: &str) -> Option<Tab> {
        let at = self.position(target)?;
        let tab = self.list.remove(at);
        if self.list.is_empty() {
            self.active = 0;
        } else if at == self.active {
            let opener = tab.opener.as_deref().and_then(|o| self.position(o));
            self.active = opener.unwrap_or(at.saturating_sub(1)).min(self.list.len() - 1);
        } else if at < self.active {
            self.active -= 1;
        }
        Some(tab)
    }

    fn saved(&self) -> SavedTabs {
        SavedTabs {
            urls: self.list.iter().map(|t| if t.url.is_empty() { "about:blank".into() } else { t.url.clone() }).collect(),
            active: self.active,
        }
    }
}

/// A tab's name (BRW-17): its page's title, else its site, else "New tab".
pub fn name(title: &str, url: &str) -> String {
    let title = title.trim();
    // Chrome titles a page without one by its address, less the scheme.
    let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
    let untitled = title.is_empty() || title == url || title.trim_end_matches('/') == bare.trim_end_matches('/');
    if !untitled {
        return title.to_string();
    }
    sites::host_of(url).unwrap_or_else(|| "New tab".to_string())
}

/// One tab, as the panel and the agents see it.
#[derive(Clone, Debug, Serialize)]
pub struct TabInfo {
    /// Chrome's id for the page: what the panel switches and closes by.
    pub id: String,
    pub name: String,
    pub url: String,
    pub active: bool,
    pub loading: bool,
}

impl Browser {
    /// The task's tabs, in order.
    pub fn tabs(&self, task: &str) -> Vec<TabInfo> {
        let inner = self.inner.lock();
        let Some(tabs) = inner.tabs.get(task) else { return Vec::new() };
        tabs.list
            .iter()
            .enumerate()
            .map(|(i, t)| TabInfo {
                id: t.target.clone(),
                name: name(&t.title, &t.url),
                url: t.url.clone(),
                active: i == tabs.active,
                loading: t.loading,
            })
            .collect()
    }

    /// A new tab next to the active one, made active, at `url` if given.
    pub async fn new_tab<R: Runtime>(&self, app: &AppHandle<R>, task: &str, url: Option<&str>) -> Result<Page> {
        // The task's browser first: a task with none gets its kept tabs
        // back before this one joins them.
        let current = self.page(app, task).await?;
        let (cdp, user_agent, after, viewport) = {
            let inner = self.inner.lock();
            let r = inner.running.as_ref().ok_or_else(|| Error::Other("the browser closed".into()))?;
            let active = inner.active(task);
            (
                r.cdp.clone(),
                r.user_agent.clone(),
                active.map(|t| t.target.clone()),
                active.map(|t| t.viewport).unwrap_or_default(),
            )
        };
        drop(current);
        let tab = make_tab(&cdp, user_agent.as_deref(), viewport).await?;
        let page = Page::new(cdp, tab.session.clone(), task.to_string());
        {
            let mut inner = self.inner.lock();
            let tabs = inner.tabs.get_mut(task).ok_or_else(|| Error::Other("the browser closed".into()))?;
            tabs.open_next_to(tab, after.as_deref());
        }
        if let Some(url) = url {
            page.navigate(url).await?;
        }
        self.save(app, task);
        changed(app, task);
        Ok(page)
    }

    /// Make the tab `id` the active one; its name.
    pub fn switch_tab<R: Runtime>(&self, app: &AppHandle<R>, task: &str, id: &str) -> Result<String> {
        let name = {
            let mut inner = self.inner.lock();
            let tabs = inner.tabs.get_mut(task).ok_or_else(|| Error::Other("this task's browser is not open".into()))?;
            let at = tabs.position(id).ok_or_else(|| Error::Other("that tab is closed".into()))?;
            tabs.active = at;
            name(&tabs.list[at].title, &tabs.list[at].url)
        };
        self.save(app, task);
        changed(app, task);
        Ok(name)
    }

    /// Close the tab `id`. The last one is left open and empty instead: a
    /// task's browser always has a tab (BRW-16).
    pub async fn close_tab<R: Runtime>(&self, app: &AppHandle<R>, task: &str, id: &str) -> Result<()> {
        let (cdp, closed, last) = {
            let mut inner = self.inner.lock();
            let cdp = inner.running.as_ref().map(|r| r.cdp.clone()).ok_or_else(|| Error::Other("the browser closed".into()))?;
            let tabs = inner.tabs.get_mut(task).ok_or_else(|| Error::Other("this task's browser is not open".into()))?;
            let at = tabs.position(id).ok_or_else(|| Error::Other("that tab is closed".into()))?;
            if tabs.list.len() == 1 {
                (cdp, None, Some(tabs.list[at].session.clone()))
            } else {
                (cdp, tabs.remove(id), None)
            }
        };
        match (closed, last) {
            (Some(tab), _) => cdp.send("Target.closeTarget", json!({ "targetId": tab.target }), None),
            (None, Some(session)) => {
                cdp.call("Page.navigate", json!({ "url": "about:blank" }), Some(&session)).await?;
            }
            (None, None) => {}
        }
        self.save(app, task);
        changed(app, task);
        Ok(())
    }

    /// Keep the task's tabs for next time (BRW-6). Off the lock: the config
    /// is written to disk.
    pub(super) fn save<R: Runtime>(&self, app: &AppHandle<R>, task: &str) {
        let Some(saved) = self.inner.lock().tabs.get(task).map(Tabs::saved) else { return };
        let _ = app.state::<AppState>().config.update(|c| {
            if let Some(t) = c.tasks.iter_mut().find(|t| t.id == task) {
                t.browser = Some(saved);
                t.browser_url = None;
            }
        });
    }
}

/// The task's tabs, numbered as the tab tools take them (BRW-16). A tab on
/// a site agents may not use is listed by its site alone: its title is the
/// page's own text.
pub(super) fn tab_list(tabs: &[TabInfo], state: &AppState) -> String {
    let sites = state.config.read().browser.sites;
    let lines: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let shown = if sites::allowed(&t.url, &sites) || t.url.is_empty() {
                format!("{} — {}", t.name, t.url)
            } else {
                format!("{} (a site agents may not use)", sites::host_of(&t.url).unwrap_or_else(|| t.url.clone()))
            };
            format!("  {}. {shown}{}", i + 1, if t.active { " (active)" } else { "" })
        })
        .collect();
    format!("Tabs:\n{}", lines.join("\n"))
}

/// The tab an agent named by its number, or the active one.
pub(super) fn tab_numbered(tabs: &[TabInfo], args: &Value) -> Result<TabInfo> {
    match args.get("tab").and_then(Value::as_u64) {
        None => tabs.iter().find(|t| t.active).cloned().ok_or_else(|| Error::Other("no tab is open".into())),
        Some(n) => tabs.get((n as usize).wrapping_sub(1)).cloned().ok_or_else(|| {
            Error::Other(format!("there is no tab {n}: there are {} (browser_tabs lists them)", tabs.len()))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn tab(id: &str, opener: Option<&str>) -> Tab {
        Tab {
            target: id.into(),
            session: format!("s-{id}"),
            url: format!("http://localhost/{id}"),
            title: String::new(),
            loading: false,
            viewport: Default::default(),
            console: VecDeque::new(),
            dialog: None,
            opener: opener.map(str::to_string),
            secret: None,
        }
    }

    fn ids(t: &Tabs) -> Vec<&str> {
        t.list.iter().map(|t| t.target.as_str()).collect()
    }

    #[test]
    fn a_tab_a_page_opens_goes_beside_it_and_its_closing_returns_there() {
        let mut t = Tabs::new(vec![tab("a", None), tab("b", None), tab("c", None)], 0);
        t.open_next_to(tab("popup", Some("a")), Some("a"));
        assert_eq!(ids(&t), ["a", "popup", "b", "c"]);
        assert_eq!(t.active().unwrap().target, "popup");

        t.active = 2;
        t.open_next_to(tab("link", Some("b")), Some("b"));
        assert_eq!(ids(&t), ["a", "popup", "b", "link", "c"]);
        t.remove("link");
        assert_eq!(t.active().unwrap().target, "b", "back to the tab that opened it");

        t.active = 1;
        t.remove("a");
        t.remove("popup");
        assert_eq!(t.active().unwrap().target, "b", "its opener gone, the tab before it, or the first");
    }

    #[test]
    fn closing_a_tab_before_the_active_one_keeps_the_same_tab_active() {
        let mut t = Tabs::new(vec![tab("a", None), tab("b", None), tab("c", None)], 2);
        t.remove("a");
        assert_eq!(t.active().unwrap().target, "c");
        t.remove("c");
        assert_eq!(t.active().unwrap().target, "b");
    }

    #[test]
    fn a_task_from_before_tabs_comes_back_with_its_one_page_as_one_tab() {
        let task: crate::config::Task = serde_json::from_value(serde_json::json!({
            "id": "t", "name": "t", "root": "/t", "branch": "t", "created_at": "2026-10-01T00:00:00Z",
            "browser_url": "http://localhost:3000/"
        }))
        .unwrap();
        assert_eq!(task.saved_tabs(), SavedTabs { urls: vec!["http://localhost:3000/".into()], active: 0 });
        let written = serde_json::to_value(&task).unwrap();
        assert!(written.get("browser_url").is_none(), "never written again");
    }

    #[test]
    fn a_tab_is_named_by_its_title_then_its_site() {
        assert_eq!(name("Admin", "http://localhost:3000/admin"), "Admin");
        assert_eq!(name("", "http://localhost:3000/admin"), "localhost");
        assert_eq!(name("localhost:3000/admin", "http://localhost:3000/admin"), "localhost");
        assert_eq!(name("", "about:blank"), "New tab");
    }
}
