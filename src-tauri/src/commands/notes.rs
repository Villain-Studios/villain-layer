//! Repo notes as the agents' tools and the Repos view see them (MEM-1…7):
//! each with what changed since it was last checked. All blocking: finding
//! a repository's key and measuring a note both run git.

use serde::Serialize;
use tauri::AppHandle;

use crate::config::{Project, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::notes::{New, Note};

use super::AppState;

/// A note, and how likely it is to be out of date (MEM-3).
#[derive(Debug, Clone, Serialize)]
pub struct RepoNote {
    #[serde(flatten)]
    pub note: Note,
    /// Files under its paths that changed on the default branch since it
    /// was last checked. None when that cannot be told: it names no paths,
    /// its repository is not registered, or the commit is unknown here.
    pub changed: Option<Vec<String>>,
}

/// What a repository's notes are kept by: where it fetches from, spelled
/// one way (MEM-7), or its clone's path when it fetches from nowhere.
pub(crate) fn repo_key(project: &Project) -> String {
    git::origin_url(&project.repo())
        .map(|url| git::remote_key(&url))
        .unwrap_or_else(|| project.path.clone())
}

/// The registered repository whose notes these are, if it still is one.
fn project_of_key(state: &AppState, key: &str) -> Option<Project> {
    state.config.read().projects.into_iter().find(|p| repo_key(p) == key)
}

fn tip(project: &Project) -> Option<String> {
    git::default_tip(&project.repo(), &project.default_branch)
}

fn measured(project: Option<&Project>, note: Note) -> RepoNote {
    let changed = match (project, &note.checked_commit) {
        (Some(p), Some(since)) if !note.paths.is_empty() => tip(p)
            .and_then(|now| git::changed_between(&p.repo(), since, &now, &note.paths).ok()),
        _ => None,
    };
    RepoNote { note, changed }
}

/// A registered repository by its name, the way agents name them.
pub(crate) fn project_named(state: &AppState, name: &str) -> Result<Project> {
    let projects = state.config.read().projects;
    projects
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
        .cloned()
        .ok_or_else(|| {
            Error::NotFound(format!(
                "no repository named {name}; known: {}",
                projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ))
        })
}

/// A repository's notes, the most recently checked first, each measured.
pub(crate) fn notes_of(state: &AppState, project: &Project) -> Vec<RepoNote> {
    state
        .notes
        .of(&repo_key(project))
        .into_iter()
        .map(|n| measured(Some(project), n))
        .collect()
}

/// Remember something about `project`, from `source`.
pub(crate) fn remember_inner(
    state: &AppState,
    project: &Project,
    text: &str,
    paths: Vec<String>,
    source: String,
) -> Result<Note> {
    state.notes.add(New {
        repo: repo_key(project),
        text: text.to_string(),
        paths,
        source,
        commit: tip(project),
    })
}

/// Say a note still holds, as of its repository's default branch now.
pub(crate) fn check_note_inner(state: &AppState, id: &str) -> Result<RepoNote> {
    let project = project_of_key(state, &state.notes.get(id)?.repo);
    let note = state.notes.check(id, project.as_ref().and_then(tip))?;
    Ok(measured(project.as_ref(), note))
}

pub(crate) fn forget_note_inner(state: &AppState, id: &str) -> Result<Note> {
    state.notes.forget(id)
}

fn edit_note_inner(state: &AppState, id: &str, text: &str, paths: &[String]) -> Result<Note> {
    let project = project_of_key(state, &state.notes.get(id)?.repo);
    state.notes.edit(id, text, paths, project.as_ref().and_then(tip))
}

/// One registered repository's notes, for the Repos view.
#[derive(Debug, Serialize)]
pub struct ProjectNotes {
    pub project_id: String,
    pub notes: Vec<RepoNote>,
}

/// Every registered repository's notes, each measured (MEM-6).
#[tauri::command]
pub async fn list_repo_notes(app: AppHandle) -> Result<Vec<ProjectNotes>> {
    super::blocking(app, |state| {
        Ok(state
            .config
            .read()
            .projects
            .iter()
            .map(|p| ProjectNotes { project_id: p.id.clone(), notes: notes_of(state, p) })
            .collect())
    })
    .await
}

/// A note written by hand in the Repos view.
#[tauri::command]
pub async fn add_repo_note(app: AppHandle, project_id: String, text: String, paths: Vec<String>) -> Result<()> {
    super::blocking(app, move |state| {
        let project = state.config.project(&project_id)?;
        remember_inner(state, &project, &text, paths, "you".into()).map(|_| ())
    })
    .await
}

#[tauri::command]
pub async fn edit_repo_note(app: AppHandle, id: String, text: String, paths: Vec<String>) -> Result<()> {
    super::blocking(app, move |state| edit_note_inner(state, &id, &text, &paths).map(|_| ())).await
}

#[tauri::command]
pub async fn check_repo_note(app: AppHandle, id: String) -> Result<()> {
    super::blocking(app, move |state| check_note_inner(state, &id).map(|_| ())).await
}

#[tauri::command]
pub async fn delete_repo_note(app: AppHandle, id: String) -> Result<()> {
    super::blocking(app, move |state| forget_note_inner(state, &id).map(|_| ())).await
}

/// "today", "3 days ago", "5 months ago": how old a note is, in words an
/// agent reads without doing arithmetic on timestamps.
pub(crate) fn age(then_ms: i64, now_ms: i64) -> String {
    let days = (now_ms - then_ms).max(0) / 86_400_000;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        d if d < 60 => format!("{d} days ago"),
        d if d < 730 => format!("{} months ago", d / 30),
        d => format!("{} years ago", d / 365),
    }
}

/// A note as an agent is handed it by `repo_notes` and `check_note`.
pub(crate) fn note_for_agent(n: &RepoNote, now_ms: i64) -> serde_json::Value {
    serde_json::json!({
        "id": n.note.id,
        "note": n.note.text,
        "paths": n.note.paths,
        "from": n.note.source,
        "written": age(n.note.written_at, now_ms),
        "last_checked": age(n.note.checked_at, now_ms),
        "changed_since_checked": match &n.changed {
            Some(files) => serde_json::json!(files),
            None => serde_json::json!("unknown: it names no paths, or its commit is not known here"),
        },
    })
}

/// Where an agent's note came from, as the Repos view shows it.
pub(crate) fn agent_source(agent: Option<&str>, task: Option<&Task>) -> String {
    let agent = agent.map(str::trim).filter(|a| !a.is_empty()).unwrap_or("an agent");
    let agent: String = agent.chars().take(60).collect();
    match task {
        Some(t) => format!("{agent} in {}", t.name),
        None => agent,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::AppConfig;

    fn git(dir: &Path, args: &[&str]) -> String {
        crate::git::run_for_tests(dir, args).expect("git")
    }

    /// A remote and a clone of it registered as `api`, with a `db/` folder.
    fn setup() -> (PathBuf, PathBuf, AppState) {
        let root = std::env::temp_dir().join(format!("vl-notes-cmd-{}", uuid::Uuid::new_v4()));
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "-b", "main", "--bare"]);
        let clone = root.join("api");
        git(&root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]);
        git(&clone, &["config", "user.email", "test@villain.local"]);
        git(&clone, &["config", "user.name", "Test"]);
        std::fs::create_dir_all(clone.join("db")).unwrap();
        std::fs::write(clone.join("db/schema.sql"), "create table a;\n").unwrap();
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-qm", "init"]);
        git(&clone, &["push", "-q", "-u", "origin", "main"]);

        let mut cfg = AppConfig::default();
        cfg.projects.push(Project {
            id: "p1".into(),
            name: "api".into(),
            path: clone.to_string_lossy().to_string(),
            default_branch: "main".into(),
            group: None,
            store: None,
            update_by: None,
            check: None,
        });
        let state = super::super::repos::tests::state(&root, cfg);
        (root, clone, state)
    }

    fn push_change(clone: &Path, file: &str) {
        std::fs::write(clone.join(file), uuid::Uuid::new_v4().to_string()).unwrap();
        git(clone, &["add", "-A"]);
        git(clone, &["commit", "-qm", "change"]);
        git(clone, &["push", "-q", "origin", "main"]);
    }

    #[test]
    fn a_note_says_which_of_its_files_changed_since_it_was_checked() {
        let (_root, clone, state) = setup();
        let api = project_named(&state, "API").expect("found by name, any case");
        let note = remember_inner(&state, &api, "migrations run from db/", vec!["db".into()], "you".into())
            .expect("remembered");

        push_change(&clone, "a.txt");
        assert_eq!(notes_of(&state, &api)[0].changed, Some(vec![]), "nothing it is about changed");

        push_change(&clone, "db/schema.sql");
        assert_eq!(notes_of(&state, &api)[0].changed, Some(vec!["db/schema.sql".to_string()]));

        let checked = check_note_inner(&state, &note.id).expect("checked");
        assert_eq!(checked.changed, Some(vec![]), "checking it starts the count again");
    }

    #[test]
    fn notes_outlive_the_repos_registration() {
        let (_root, clone, state) = setup();
        let api = project_named(&state, "api").expect("registered");
        remember_inner(&state, &api, "tests need `make db`", vec![], "you".into()).expect("remembered");

        // Removed, and added again under a new id and another name.
        let again = Project { id: "p2".into(), name: "api-again".into(), ..api.clone() };
        state.config.update(|c| c.projects = vec![again.clone()]).expect("saved");
        let notes = notes_of(&state, &again);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].changed, None, "a note naming no paths has only its age");
        assert!(repo_key(&again).ends_with("remote"), "kept by where it fetches from: {}", repo_key(&again));
        let _ = clone;
    }

    #[test]
    fn an_age_reads_as_words() {
        let day = 86_400_000;
        assert_eq!(age(0, day / 2), "today");
        assert_eq!(age(0, 3 * day), "3 days ago");
        assert_eq!(age(0, 150 * day), "5 months ago");
        assert_eq!(age(0, 800 * day), "2 years ago");
    }

    #[test]
    fn a_note_says_where_it_came_from() {
        assert_eq!(agent_source(Some("Claude Code"), None), "Claude Code");
        assert_eq!(agent_source(None, None), "an agent");
    }
}
