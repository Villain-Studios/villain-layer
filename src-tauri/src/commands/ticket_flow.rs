//! Tickets that follow the work (TKT-8). A ticket moves to its project's
//! review status once one of its task's pull requests is ready for review,
//! and to its after-merge status once every one has merged. Each move
//! happens once per task, when the task gets there, and only to a status
//! chosen for the project: in Jira, "Review" and "In Progress" are both
//! "in progress", and a status's name is not the app's to assume.
//!
//! Before this the app moved a ticket only when work started and when a
//! task was finished, so tickets sat In Progress with their pull requests
//! merged long before.

use serde::Serialize;
use tauri::State;

use crate::config::{FlowStatus, StartTo, Task, TicketFlow};
use crate::error::{Error, Result};
use crate::integrations::jira::{Jira, ProjectStatus};

use super::github::CheckoutPr;
use super::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Review,
    Merged,
}

impl Stage {
    fn key(self) -> &'static str {
        match self {
            Stage::Review => "review",
            Stage::Merged => "merged",
        }
    }

    fn parse(s: &str) -> Option<Stage> {
        match s {
            "review" => Some(Stage::Review),
            "merged" => Some(Stage::Merged),
            _ => None,
        }
    }
}

/// Where a task's pull requests have got to. A draft is not ready for
/// review. Merged means every pull request that was not given up on has
/// merged, with nothing left over: work committed since, or in a repo that
/// never had one, is not done.
pub fn stage(rows: &[CheckoutPr]) -> Option<Stage> {
    let live: Vec<_> = rows
        .iter()
        .filter_map(|r| r.pr.as_ref())
        .filter(|p| p.state == "open" || p.merged)
        .collect();
    if live.is_empty() {
        return None;
    }
    if live.iter().all(|p| p.merged) && rows.iter().all(|r| r.changed == 0) {
        return Some(Stage::Merged);
    }
    live.iter().any(|p| p.state == "open" && !p.draft).then_some(Stage::Review)
}

/// The stage a task has reached that its ticket has not been moved for.
fn due(task: &Task, reached: Option<Stage>) -> Option<Stage> {
    let reached = reached?;
    let moved = task.ticket_stage.as_deref().and_then(Stage::parse);
    moved.is_none_or(|m| reached > m).then_some(reached)
}

/// What became of one task's ticket in a sweep, for the UI to say.
#[derive(Debug, Clone, Serialize)]
pub struct TicketMove {
    pub key: String,
    /// "review" or "merged".
    pub stage: &'static str,
    /// The status it was moved to.
    pub moved_to: Option<String>,
    /// No status is chosen for this stage in the ticket's project yet.
    pub unchosen: bool,
    pub error: Option<String>,
}

/// Move `task`'s ticket if its pull requests have reached a stage it has
/// not been moved for. None when there is nothing to say.
pub(crate) async fn follow(state: &AppState, client: &Jira, task: &Task, rows: &[CheckoutPr]) -> Option<TicketMove> {
    let key = task.issue_key.clone()?;
    let stage = due(task, stage(rows))?;
    let project = key.split('-').next()?.to_string();
    let target = state
        .config
        .read()
        .jira
        .and_then(|j| j.flow.get(&project).cloned())
        .and_then(|f| if stage == Stage::Review { f.review } else { f.merged });
    let mut out = TicketMove { key: key.clone(), stage: stage.key(), moved_to: None, unchosen: false, error: None };
    let Some(target) = target else {
        out.unchosen = true;
        return Some(out);
    };
    match move_to(client, &key, &target, stage == Stage::Review).await {
        Ok(moved) => out.moved_to = moved,
        // The network: tried again next sweep.
        Err(e @ Error::Http(_)) => {
            out.error = Some(e.to_string());
            return Some(out);
        }
        Err(e) => out.error = Some(e.to_string()),
    }
    // Once, whatever Jira said: a workflow that will not make the move is
    // said once, not every ninety seconds, and a ticket moved on by hand
    // after this stays where it was put.
    let id = task.id.clone();
    let _ = state.config.update(|c| {
        if let Some(t) = c.tasks.iter_mut().find(|t| t.id == id) {
            t.ticket_stage = Some(stage.key().into());
        }
    });
    (out.moved_to.is_some() || out.error.is_some()).then_some(out)
}

/// Move `key` to `target`, unless it is there already, or, with
/// `not_from_done`, closed: review is never a way back out of done.
async fn move_to(client: &Jira, key: &str, target: &FlowStatus, not_from_done: bool) -> Result<Option<String>> {
    let issue = client.issue(key).await?;
    if issue.status_id == target.id || (issue.status_category == "done" && not_from_done) {
        return Ok(None);
    }
    let transitions = client.transitions(key).await?;
    let way = transitions.iter().find(|t| t.to_id == target.id).ok_or_else(|| {
        Error::Other(format!(
            "{key} is {}, and its workflow has no way from there to {}. Move it in Jira.",
            issue.status, target.name
        ))
    })?;
    client.transition(key, &way.id).await?;
    Ok(Some(target.name.clone()))
}

/// Where `flow` sends a ticket when work starts. A project with nothing
/// chosen goes as the old switch in General said, so a ticket of one whose
/// user had turned it off still stays put.
pub(crate) fn start_to(flow: Option<&TicketFlow>, sync_jira_status: bool) -> StartTo {
    match flow.and_then(|f| f.started.clone()) {
        Some(to) => to,
        None if sync_jira_status => StartTo::FirstInProgress,
        None => StartTo::Leave,
    }
}

/// Move a ticket as its project says when work starts on it (TKT-1), and
/// say where to, when it moved.
///
/// Starting work in two places and telling Jira about neither is how a board
/// ends up disagreeing with the app: the ticket reads Open while a branch,
/// a worktree and an agent are all running against it. Best effort — a
/// workflow that will not allow the move, or an account that may not make it,
/// is not a reason to undo a task that was created successfully.
pub(crate) async fn started(state: &AppState, key: &str) -> Option<String> {
    let to = {
        let cfg = state.config.read();
        let project = key.split('-').next().unwrap_or_default();
        start_to(cfg.jira.as_ref().and_then(|j| j.flow.get(project)), cfg.ui.sync_jira_status)
    };
    let (client, _) = super::jira_client(state).ok()?;
    let moved = match to {
        StartTo::Leave => return None,
        StartTo::FirstInProgress => client.start_progress(key).await,
        // Out of done too: work starting on a closed ticket opens it again.
        StartTo::Status { id, name } => move_to(&client, key, &FlowStatus { id, name }, false).await,
    };
    match moved {
        Ok(moved) => moved,
        Err(e) => {
            eprintln!("could not move {key} as work started: {e}");
            None
        }
    }
}

/// The statuses a Jira project's tickets can be in, to choose from.
#[tauri::command]
pub async fn jira_project_statuses(state: State<'_, AppState>, project_key: String) -> Result<Vec<ProjectStatus>> {
    let (client, _) = super::jira_client(&state)?;
    client.project_statuses(&project_key).await
}

/// Choose where a project's tickets go at `stage` ("review" or "merged"),
/// or with None, stop moving them then.
#[tauri::command]
pub fn set_ticket_flow(
    state: State<AppState>,
    project_key: String,
    stage: String,
    status: Option<FlowStatus>,
) -> Result<()> {
    let stage = Stage::parse(&stage).ok_or_else(|| Error::Other(format!("no such stage: {stage}")))?;
    state.config.update(|c| {
        let Some(jira) = c.jira.as_mut() else {
            return Err(Error::NotConfigured("Jira"));
        };
        let flow = jira.flow.entry(project_key.clone()).or_default();
        match stage {
            Stage::Review => flow.review = status.clone(),
            Stage::Merged => flow.merged = status.clone(),
        }
        Ok(())
    })?
}

/// Choose where a project's tickets go when work on them starts (TKT-1).
#[tauri::command]
pub fn set_ticket_start(state: State<AppState>, project_key: String, to: StartTo) -> Result<()> {
    state.config.update(|c| {
        let Some(jira) = c.jira.as_mut() else {
            return Err(Error::NotConfigured("Jira"));
        };
        jira.flow.entry(project_key.clone()).or_default().started = Some(to.clone());
        Ok(())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::github::PullRequest;

    #[test]
    fn work_starting_goes_where_the_project_says_and_else_as_the_old_switch_did() {
        let status = StartTo::Status { id: "3".into(), name: "In Progress".into() };
        let chosen = TicketFlow { started: Some(status.clone()), ..Default::default() };
        assert_eq!(start_to(Some(&chosen), false), status);
        let left = TicketFlow { started: Some(StartTo::Leave), ..Default::default() };
        assert_eq!(start_to(Some(&left), true), StartTo::Leave);
        assert_eq!(start_to(Some(&TicketFlow::default()), true), StartTo::FirstInProgress);
        assert_eq!(start_to(None, false), StartTo::Leave, "switched off in General, before it could be chosen here");
    }

    #[test]
    fn a_start_choice_is_kept_as_a_tagged_object_and_an_old_flow_still_reads() {
        let json = serde_json::to_string(&StartTo::Status { id: "3".into(), name: "In Progress".into() }).unwrap();
        assert_eq!(json, r#"{"to":"status","id":"3","name":"In Progress"}"#);
        let back: StartTo = serde_json::from_str(r#"{"to":"first_in_progress"}"#).unwrap();
        assert_eq!(back, StartTo::FirstInProgress);
        let old: TicketFlow = serde_json::from_str(r#"{"review":null,"merged":null}"#).unwrap();
        assert!(old.started.is_none());
    }

    fn row(state: &str, draft: bool, merged: bool, changed: usize) -> CheckoutPr {
        CheckoutPr {
            checkout_id: "c".into(),
            repo: "api".into(),
            pr: (!state.is_empty()).then(|| PullRequest {
                number: 1,
                title: "t".into(),
                state: state.into(),
                draft,
                author: "me".into(),
                head: "ACME-1".into(),
                head_sha: "abc".into(),
                base: "main".into(),
                url: String::new(),
                mergeable_state: None,
                merged,
                comments: 0,
                review_comments: 0,
            }),
            checks: Vec::new(),
            reviews: Vec::new(),
            past: Vec::new(),
            verdict: "none".into(),
            base: "main".into(),
            changed,
            error: None,
        }
    }

    fn task(moved: Option<&str>) -> Task {
        Task {
            id: "t".into(),
            name: "ACME-1".into(),
            root: "/t".into(),
            branch: "ACME-1".into(),
            issue_key: Some("ACME-1".into()),
            issue_url: None,
            created_at: chrono::Utc::now(),
            ticket_stage: moved.map(String::from),
            chat: None,
            review: None,
            browser_url: None,
            browser: None,
        }
    }

    #[test]
    fn a_draft_is_not_ready_for_review_and_merged_means_nothing_left() {
        assert_eq!(stage(&[row("open", true, false, 3)]), None, "a draft");
        assert_eq!(stage(&[row("open", false, false, 3)]), Some(Stage::Review));
        assert_eq!(stage(&[row("open", true, false, 1), row("open", false, false, 2)]), Some(Stage::Review));
        assert_eq!(stage(&[row("closed", false, true, 0), row("closed", false, true, 0)]), Some(Stage::Merged));
        assert_eq!(stage(&[row("closed", false, true, 0), row("open", false, false, 2)]), Some(Stage::Review), "one still open");
        assert_eq!(stage(&[row("closed", false, true, 0), row("", false, false, 4)]), None, "a repo with work and no PR");
        assert_eq!(stage(&[row("closed", false, false, 2)]), None, "closed without merging is given up on");
    }

    #[test]
    fn each_stage_moves_the_ticket_once_and_never_back() {
        let merged = Some(Stage::Merged);
        let review = Some(Stage::Review);
        assert_eq!(due(&task(None), review), review);
        assert_eq!(due(&task(Some("review")), review), None, "moved for this already");
        assert_eq!(due(&task(Some("review")), merged), merged);
        assert_eq!(due(&task(Some("merged")), review), None, "a PR opened again after merging does not send it back");
        assert_eq!(due(&task(None), None), None);
    }
}
