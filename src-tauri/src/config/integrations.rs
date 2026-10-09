//! Where the integrations are, and how each is set: Jira, GitHub, Slack.
//! Their tokens are in the keychain (`secrets`), never here.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JiraConfig {
    /// The custom field this site keeps the Epic Link in, discovered on
    /// connect. Only company-managed projects still use one; modern Cloud puts
    /// the epic on `parent`, so None is both common and fine.
    #[serde(default)]
    pub epic_field: Option<String>,
    /// e.g. https://your-site.atlassian.net
    pub base_url: String,
    pub email: String,
    #[serde(default)]
    pub project_key: Option<String>,
    /// JQL used for the task list; falls back to a sensible default.
    #[serde(default)]
    pub jql: Option<String>,
    /// Where tickets go as the work moves, per Jira project (TKT-8). Chosen
    /// in Settings: "Review" and "In Progress" share a status category, and
    /// a status's name is not the app's to assume.
    #[serde(default)]
    pub flow: HashMap<String, TicketFlow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketFlow {
    /// Where a ticket goes when work on it starts (TKT-1). None until chosen,
    /// and then as `UiPrefs::sync_jira_status` says: the switch that was all
    /// there was before, in General, out of sight of the rest of this.
    #[serde(default)]
    pub started: Option<StartTo>,
    /// Where a ticket goes when its task's pull request is ready for review.
    #[serde(default)]
    pub review: Option<FlowStatus>,
    /// Where it goes once every pull request of its task has merged.
    #[serde(default)]
    pub merged: Option<FlowStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStatus {
    pub id: String,
    pub name: String,
}

/// Where a ticket goes when work on it starts (TKT-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum StartTo {
    /// The first status in Jira's "in progress" category its workflow
    /// allows from where it is, unless it is in that category already.
    FirstInProgress,
    /// Nowhere: the ticket stays where it is.
    Leave,
    /// This status, unless the ticket is there already.
    Status { id: String, name: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubConfig {
    /// https://api.github.com, or https://ghe.example.com/api/v3 for Enterprise.
    pub api_url: String,
    /// Web base, used for building links.
    pub web_url: String,
    /// Team whose review queue is listed beside your own. `@fe` or `org/fe`.
    /// Absent means only reviews requested of you. A name, not a constant:
    /// which team matters is a property of the install.
    #[serde(default)]
    pub review_team: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    pub channel: String,
    /// Master switch. Off means the app posts nothing at all, without having to
    /// disconnect and re-enter the token.
    #[serde(default = "super::yes")]
    pub enabled: bool,
    /// An agent pane exiting.
    #[serde(default = "super::yes")]
    pub notify_on_done: bool,
    /// Pull requests opened for a task.
    #[serde(default = "super::yes")]
    pub notify_on_prs: bool,
    /// The `slack_post` MCP tool. Agents post unattended, so this is separate
    /// from the app's own notifications.
    #[serde(default = "super::yes")]
    pub allow_agent_posts: bool,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            channel: String::new(),
            enabled: true,
            notify_on_done: true,
            notify_on_prs: true,
            allow_agent_posts: true,
        }
    }
}
