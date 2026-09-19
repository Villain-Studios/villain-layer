//! Persisted, non-secret application state.
//!
//! A **task** is one unit of work — usually one Jira ticket. It owns one
//! **checkout** per repository it touches, all on the same branch name, laid
//! out as sibling directories under the task root so an agent started there
//! can see every repo at once. Tokens live in `secrets`, never here.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::error::{Error, Result};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Absolute path to the main repository checkout.
    pub path: String,
    pub default_branch: String,
    /// "frontend", "backend", ... One group per repo; None means ungrouped.
    #[serde(default)]
    pub group: Option<String>,
}

/// A named combination of repositories, for ticket shapes that recur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSet {
    pub id: String,
    pub name: String,
    pub project_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    Component,
    Label,
}

/// "Tickets with component Payments touch these repos." User-authored, so it
/// outranks anything inferred from history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRule {
    pub id: String,
    pub kind: MatchKind,
    /// The component or label name, matched case-insensitively.
    pub value: String,
    pub project_ids: Vec<String>,
}

/// One ticket's worth of work, spanning one or more repositories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    /// Directory the checkouts live under, and the cwd for a task-wide agent.
    pub root: String,
    /// Shared across every repo, so sibling PRs are findable by branch alone.
    pub branch: String,
    #[serde(default)]
    pub issue_key: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One repository's worktree within a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkout {
    pub id: String,
    pub task_id: String,
    pub project_id: String,
    pub path: String,
    /// The branch this worktree was cut from, usually the repo's default.
    pub base: String,
    /// The commit this worktree was branched from.
    ///
    /// The diff is "what this branch did", and that only has a fixed meaning
    /// against a fixed point. Comparing to the base *branch* moves as the base
    /// moves, and credits this branch with everything its history picked up
    /// from anywhere else.
    #[serde(default)]
    pub base_commit: Option<String>,
}

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubConfig {
    /// https://api.github.com, or https://ghe.example.com/api/v3 for Enterprise.
    pub api_url: String,
    /// Web base, used for building links.
    pub web_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    pub channel: String,
    /// Master switch. Off means the app posts nothing at all, without having to
    /// disconnect and re-enter the token.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// An agent pane exiting.
    #[serde(default = "yes")]
    pub notify_on_done: bool,
    /// Pull requests opened for a task.
    #[serde(default = "yes")]
    pub notify_on_prs: bool,
    /// The `slack_post` MCP tool. Agents post unattended, so this is separate
    /// from the app's own notifications.
    #[serde(default = "yes")]
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

fn yes() -> bool {
    true
}

/// The v1 shape: one workspace was one worktree in one repo. Migrated on load.
#[derive(Debug, Clone, Deserialize)]
pub struct LegacyWorkspace {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub branch: String,
    pub path: String,
    pub base: String,
    #[serde(default)]
    pub issue_key: Option<String>,
    #[serde(default)]
    pub issue_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A pane that was open when the app last closed, so it can be put back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPane {
    pub id: String,
    pub task_id: String,
    #[serde(default)]
    pub checkout_id: Option<String>,
    /// "agent" or "shell".
    pub kind: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Where it was running. Task panes derive this from their checkout, but a
    /// chat belongs to no task and its folder is the only thing that ties it
    /// back to its saved conversation.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Presentation preferences. Terminal text scales separately from the chrome,
/// because xterm measures its own cell grid and a page zoom would fight it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPrefs {
    pub scale: f32,
    pub terminal_font_size: u16,
    /// Put back the panes that were open when the app last closed, resuming
    /// each agent's conversation where its CLI can.
    #[serde(default = "yes")]
    pub restore_panes: bool,
    /// Answer Claude Code's workspace-trust dialog for folders this app made,
    /// so a new task or chat starts working instead of waiting on a question.
    #[serde(default = "yes")]
    pub trust_agent_dirs: bool,
    /// Move the ticket into progress when a task is started for it, so the
    /// board and this app do not disagree about what is being worked on.
    #[serde(default = "yes")]
    pub sync_jira_status: bool,
    /// Let agents read the output of terminals in this app.
    ///
    /// Off by default, and deliberately: a shell's scrollback holds whatever
    /// has been typed and whatever printed, which can include tokens echoed by
    /// a failing script or a remote URL with credentials in it.
    #[serde(default)]
    pub agents_read_panes: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            scale: 1.0,
            terminal_font_size: 13,
            restore_panes: true,
            trust_agent_dirs: true,
            sync_jira_status: true,
            agents_read_panes: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub checkouts: Vec<Checkout>,
    /// Read-only remnant of v1; drained by `migrate`.
    #[serde(default, skip_serializing)]
    pub workspaces: Vec<LegacyWorkspace>,
    /// Where task directories are created. Defaults to ~/.villain-worktrees.
    #[serde(default)]
    pub worktree_root: Option<String>,
    /// Repos last used, keyed "epic:ACME-12" / "project:ACME". Prefills the picker.
    #[serde(default)]
    pub last_repos: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub repo_sets: Vec<RepoSet>,
    #[serde(default)]
    pub repo_rules: Vec<RepoRule>,
    #[serde(default)]
    pub ui: UiPrefs,
    /// Messages the app posted to Slack, so it can retract its own litter.
    /// Only a bot can delete a bot's messages, so nobody else can clear these.
    #[serde(default)]
    pub slack_posted: Vec<crate::integrations::slack::Posted>,
    #[serde(default)]
    pub saved_panes: Vec<SavedPane>,
    #[serde(default)]
    pub jira: Option<JiraConfig>,
    #[serde(default)]
    pub github: Option<GithubConfig>,
    #[serde(default)]
    pub slack: Option<SlackConfig>,
}

impl AppConfig {
    /// Fold v1 workspaces into single-checkout tasks, keeping their ids so
    /// selection and any running panes survive the upgrade.
    fn migrate(&mut self) -> bool {
        if self.workspaces.is_empty() {
            let bumped = self.version != SCHEMA_VERSION;
            self.version = SCHEMA_VERSION;
            return bumped;
        }

        for w in std::mem::take(&mut self.workspaces) {
            if self.tasks.iter().any(|t| t.id == w.id) {
                continue;
            }
            self.tasks.push(Task {
                id: w.id.clone(),
                name: w.name,
                // The old layout had no task directory; the lone checkout is it.
                root: w.path.clone(),
                branch: w.branch,
                issue_key: w.issue_key,
                issue_url: w.issue_url,
                created_at: w.created_at,
            });
            self.checkouts.push(Checkout {
                id: uuid::Uuid::new_v4().to_string(),
                task_id: w.id,
                project_id: w.project_id,
                path: w.path,
                base: w.base,
                // Nothing recorded the branch point back then; the diff falls
                // back to the merge base for these.
                base_commit: None,
            });
        }
        self.version = SCHEMA_VERSION;
        true
    }
}

/// Default location for task directories when the user has not chosen one.
pub fn default_worktree_root() -> PathBuf {
    #[allow(deprecated)]
    let home = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
    home.join(".villain-worktrees")
}

pub struct ConfigStore {
    path: PathBuf,
    inner: RwLock<AppConfig>,
}

impl ConfigStore {
    pub fn load(app: &AppHandle) -> Result<Self> {
        let dir = app
            .path()
            .app_config_dir()
            .map_err(|e| Error::Other(format!("no config dir: {e}")))?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.json");

        let mut inner: AppConfig = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            AppConfig::default()
        };

        let changed = inner.migrate();

        let store = Self {
            path,
            inner: RwLock::new(inner),
        };
        if changed {
            let snapshot = store.read();
            store.persist(&snapshot)?;
        }
        Ok(store)
    }

    pub fn read(&self) -> AppConfig {
        self.inner.read().clone()
    }

    /// Mutate the config and write it back to disk atomically.
    pub fn update<T>(&self, f: impl FnOnce(&mut AppConfig) -> T) -> Result<T> {
        let (out, snapshot) = {
            let mut guard = self.inner.write();
            let out = f(&mut guard);
            (out, guard.clone())
        };
        self.persist(&snapshot)?;
        Ok(out)
    }

    fn persist(&self, cfg: &AppConfig) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(cfg)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn worktree_root(&self) -> PathBuf {
        self.inner
            .read()
            .worktree_root
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(default_worktree_root)
    }

    pub fn project(&self, id: &str) -> Result<Project> {
        self.inner
            .read()
            .projects
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("project {id}")))
    }

    pub fn task(&self, id: &str) -> Result<Task> {
        self.inner
            .read()
            .tasks
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("task {id}")))
    }

    pub fn checkout(&self, id: &str) -> Result<Checkout> {
        self.inner
            .read()
            .checkouts
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("checkout {id}")))
    }

    pub fn checkouts_of(&self, task_id: &str) -> Vec<Checkout> {
        self.inner
            .read()
            .checkouts
            .iter()
            .filter(|c| c.task_id == task_id)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config written by v1, before tasks could span repositories.
    const V1: &str = r#"{
      "projects": [
        {"id": "p1", "name": "api", "path": "/repos/api", "default_branch": "main"}
      ],
      "workspaces": [
        {
          "id": "w1", "project_id": "p1", "name": "ACME-1 fix login",
          "branch": "acme-1-fix-login", "path": "/wt/acme-1-fix-login",
          "base": "main", "issue_key": "ACME-1",
          "issue_url": "https://x.atlassian.net/browse/ACME-1",
          "created_at": "2026-09-01T10:00:00Z"
        }
      ]
    }"#;

    #[test]
    fn migrates_v1_workspaces_into_single_repo_tasks() {
        let mut cfg: AppConfig = serde_json::from_str(V1).unwrap();
        assert!(cfg.migrate());

        assert_eq!(cfg.version, SCHEMA_VERSION);
        assert!(cfg.workspaces.is_empty());
        assert_eq!(cfg.tasks.len(), 1);
        assert_eq!(cfg.checkouts.len(), 1);

        let task = &cfg.tasks[0];
        // The id carries over so running panes and the current selection survive.
        assert_eq!(task.id, "w1");
        assert_eq!(task.branch, "acme-1-fix-login");
        assert_eq!(task.issue_key.as_deref(), Some("ACME-1"));

        let checkout = &cfg.checkouts[0];
        assert_eq!(checkout.task_id, "w1");
        assert_eq!(checkout.project_id, "p1");
        assert_eq!(checkout.path, "/wt/acme-1-fix-login");
        assert_eq!(checkout.base, "main");
    }

    #[test]
    fn migration_is_idempotent_and_drops_legacy_on_write() {
        let mut cfg: AppConfig = serde_json::from_str(V1).unwrap();
        cfg.migrate();
        let after_first = serde_json::to_string(&cfg).unwrap();

        // A second pass must not duplicate the task.
        assert!(!cfg.migrate());
        assert_eq!(cfg.tasks.len(), 1);

        // Legacy workspaces are never written back out.
        assert!(!after_first.contains("workspaces"));

        // And re-reading what we wrote is a no-op.
        let mut round_tripped: AppConfig = serde_json::from_str(&after_first).unwrap();
        assert!(!round_tripped.migrate());
        assert_eq!(round_tripped.tasks.len(), 1);
        assert_eq!(round_tripped.checkouts.len(), 1);
    }

    #[test]
    fn empty_config_is_stamped_with_the_current_version() {
        let mut cfg = AppConfig::default();
        assert!(cfg.migrate());
        assert_eq!(cfg.version, SCHEMA_VERSION);
        assert!(!cfg.migrate());
    }
}
