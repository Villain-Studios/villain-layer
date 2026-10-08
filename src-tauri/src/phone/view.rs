//! What the phone's overview shows (PHONE-5): every task, its panes and
//! their states, the ones that need you first.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::attention;
use crate::commands::CHAT_TASK_ID;
use crate::config::Task;
use crate::pty::{Activity, PaneInfo, PaneKind};

#[derive(Debug, Serialize)]
pub struct Overview {
    /// The name this phone paired under.
    pub device: String,
    /// Whether the phone may type into agents (PHONE-7).
    pub typing: bool,
    pub groups: Vec<Group>,
}

#[derive(Debug, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// The ticket, when the task has one.
    pub key: Option<String>,
    pub panes: Vec<PhonePane>,
}

#[derive(Debug, Serialize)]
pub struct PhonePane {
    pub id: String,
    /// What the conversation is about once the CLI says (PANE-12), its own
    /// title until then.
    pub name: String,
    pub kind: PaneKind,
    pub agent: Option<String>,
    pub activity: Activity,
    pub since: DateTime<Utc>,
    pub notice: Option<String>,
    pub running: bool,
    /// Why it needs you, by the dock count's own rule (STATE-4).
    pub waiting: Option<&'static str>,
}

/// The topic in place of the agent's name, keeping where it works after
/// " · ", as the window's `paneName` does.
fn name(info: &PaneInfo) -> String {
    let Some(topic) = &info.topic else {
        return info.title.clone();
    };
    match info.title.split(" · ").nth(1) {
        Some(scope) => format!("{topic} · {}", scope.trim_end_matches(" (resumed)")),
        None => topic.clone(),
    }
}

fn pane(info: PaneInfo) -> PhonePane {
    PhonePane {
        waiting: attention::waiting(&info),
        name: name(&info),
        id: info.id,
        kind: info.kind,
        agent: info.agent_id,
        activity: info.activity,
        since: info.activity_since,
        notice: info.notice,
        running: info.running,
    }
}

/// Tasks with something waiting first, then those with something running,
/// then the rest, newest first; chats as one group among them.
pub fn groups(tasks: &[Task], panes: Vec<PaneInfo>) -> Vec<Group> {
    let mut chat = Vec::new();
    let mut by_task: std::collections::HashMap<String, Vec<PhonePane>> = Default::default();
    for info in panes {
        if info.task_id == CHAT_TASK_ID {
            chat.push(pane(info));
        } else {
            by_task.entry(info.task_id.clone()).or_default().push(pane(info));
        }
    }

    let mut newest: Vec<&Task> = tasks.iter().collect();
    newest.sort_by_key(|t| std::cmp::Reverse(t.created_at));
    let mut out: Vec<Group> = newest
        .into_iter()
        .map(|t| Group {
            id: t.id.clone(),
            name: t.name.clone(),
            key: t.issue_key.clone(),
            panes: by_task.remove(&t.id).unwrap_or_default(),
        })
        .collect();
    if !chat.is_empty() {
        out.push(Group { id: CHAT_TASK_ID.into(), name: "Chat".into(), key: None, panes: chat });
    }
    // Stable, so newest first holds within each rank.
    out.sort_by_key(|g| {
        if g.panes.iter().any(|p| p.waiting.is_some()) {
            0
        } else if g.panes.iter().any(|p| p.running) {
            1
        } else {
            2
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, age_mins: i64) -> Task {
        Task {
            id: id.into(),
            name: format!("Task {id}"),
            root: "/tmp".into(),
            branch: id.into(),
            issue_key: Some(format!("ACME-{id}")),
            issue_url: None,
            created_at: Utc::now() - chrono::TimeDelta::minutes(age_mins),
            ticket_stage: None,
            chat: None,
            review: None,
            browser_url: None,
            browser: None,
        }
    }

    fn info(id: &str, task: &str, activity: Activity, topic: Option<&str>) -> PaneInfo {
        let now = Utc::now();
        PaneInfo {
            id: id.into(),
            task_id: task.into(),
            checkout_id: None,
            kind: PaneKind::Agent,
            title: "Claude Code".into(),
            agent_id: Some("claude".into()),
            cwd: "/tmp".into(),
            running: true,
            exit_code: None,
            started_at: now,
            last_output_at: now,
            notice: None,
            activity,
            activity_since: now,
            topic: topic.map(str::to_string),
            acp: false,
        }
    }

    #[test]
    fn what_needs_you_comes_first_then_what_is_working_then_the_rest_newest_first() {
        let tasks = [task("old", 60), task("idle", 30), task("busy", 20), task("asking", 90), task("new", 1)];
        let panes = vec![
            info("p1", "busy", Activity::Working, Some("Fixing login")),
            info("p2", "asking", Activity::Asking, None),
            info("p3", "idle", Activity::Idle, None),
            info("c1", CHAT_TASK_ID, Activity::Idle, None),
        ];
        let order: Vec<String> = groups(&tasks, panes).into_iter().map(|g| g.id).collect();
        assert_eq!(order, ["asking", "busy", "idle", "chat", "new", "old"]);
    }

    #[test]
    fn a_pane_goes_by_its_topic_and_says_why_it_waits() {
        let tasks = [task("t", 1)];
        let mut scoped = info("p", "t", Activity::Asking, Some("Fixing login"));
        scoped.title = "Claude Code · api (resumed)".into();
        let g = groups(&tasks, vec![scoped, info("q", "t", Activity::Idle, None)]);
        let p = &g[0].panes[0];
        assert_eq!(p.name, "Fixing login · api");
        assert_eq!(g[0].panes[1].name, "Claude Code", "no topic yet: the agent's name");
        assert_eq!(p.waiting, Some("is asking for your permission"));
        assert_eq!(g[0].key.as_deref(), Some("ACME-t"));
    }
}
