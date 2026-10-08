//! A task's specs (§19): one per repository, three files each, committed on
//! the task's branch once approved.
//!
//! A spec used to be `SPEC.md` in the task folder: deleted with the task,
//! and never checked against the work it described. In the repository it
//! goes through review with the code and outlives the task, the branch and
//! the app.
//!
//! What is approved lives in the spec folder (the worktree's, or the app's
//! for a repository that keeps none). Drafts, which file is out of date, and
//! the last check live in the task folder until then, where no agent is
//! told to look (SPEC-6).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::{Checkout, Project, Task};
use crate::error::{Error, Result};
use crate::git;
use crate::spec::{self, Kind, Part, Requirement, Step};

use super::panes::agent_file_dir;
use super::task_context::write_task_context;
use super::AppState;

/// Where a spec was kept before it was kept in the repository (SPEC-17).
pub(crate) const LEGACY_FILE: &str = "SPEC.md";

/// The task folder's drafts, one folder per repository (SPEC-6).
pub(crate) const DRAFTS: &str = ".spec-drafts";

/// What the app keeps about a repository's spec beside its drafts.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Notes {
    #[serde(default)]
    kind: Kind,
    /// Approved files changed under: an earlier one was approved since (SPEC-7).
    #[serde(default)]
    stale: Vec<Part>,
    #[serde(default)]
    check: Option<Check>,
}

/// The last check against the spec (SPEC-15).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    /// The commit it was checked at.
    pub sha: String,
    pub at: i64,
    pub results: Vec<Verdict>,
}

/// One requirement's answer: `met`, `not met` or `unclear`, and what shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub id: String,
    pub verdict: String,
    pub evidence: String,
}

/// Where one repository's spec lives, for one task.
pub(crate) struct Home {
    pub checkout: Checkout,
    pub project: Project,
    /// The checkout's folder in the task: `api`.
    pub folder: String,
    /// Where the approved files are.
    pub dir: PathBuf,
    /// The spec folder relative to the worktree, when it is committed there.
    pub rel: Option<String>,
    /// Where drafts are kept: None for a task with no folder of its own.
    pub drafts: Option<PathBuf>,
}

impl Home {
    fn notes(&self) -> Notes {
        let read = |d: &PathBuf| std::fs::read_to_string(d.join("state.json")).ok();
        self.drafts
            .as_ref()
            .and_then(read)
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn keep(&self, notes: &Notes) -> Result<()> {
        let Some(dir) = &self.drafts else { return Ok(()) };
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(notes).map_err(|e| Error::Other(e.to_string()))?;
        std::fs::write(dir.join("state.json"), json)?;
        Ok(())
    }

    /// Feature or bug: which requirements file there is, or else what was
    /// chosen when the spec was started.
    pub fn kind(&self) -> Kind {
        if self.dir.join(Part::Requirements.file(Kind::Bugfix)).is_file() {
            Kind::Bugfix
        } else if self.dir.join(Part::Requirements.file(Kind::Feature)).is_file() {
            Kind::Feature
        } else {
            self.notes().kind
        }
    }

    /// An approved file's text.
    pub fn approved(&self, part: Part) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.join(part.file(self.kind()))).ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn draft(&self, part: Part) -> Option<String> {
        let text = std::fs::read_to_string(self.drafts.as_ref()?.join(part.file(Kind::Feature))).ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// What a later file is drafted from: the draft where there is one, else
    /// what was approved.
    pub fn latest(&self, part: Part) -> Option<String> {
        self.draft(part).or_else(|| self.approved(part))
    }

    pub fn save_draft(&self, part: Part, text: Option<&str>) -> Result<()> {
        let dir = self.drafts.as_ref().ok_or_else(no_folder)?;
        // Drafts go by the part's plain name: whether it is a bug is decided
        // when it is approved.
        let path = dir.join(part.file(Kind::Feature));
        match text.filter(|t| !t.trim().is_empty()) {
            Some(text) => {
                std::fs::create_dir_all(dir)?;
                std::fs::write(path, text)?;
            }
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        Ok(())
    }

    pub fn set_kind(&self, kind: Kind) -> Result<()> {
        let mut notes = self.notes();
        notes.kind = kind;
        self.keep(&notes)
    }

    pub fn set_check(&self, check: Check) -> Result<()> {
        let mut notes = self.notes();
        notes.check = Some(check);
        self.keep(&notes)
    }

    pub fn requirements(&self) -> Vec<Requirement> {
        self.approved(Part::Requirements).map(|t| spec::requirements(&t)).unwrap_or_default()
    }

    fn repo_name(&self) -> &str {
        &self.project.name
    }

    /// The spec folder as an agent or a person finds it.
    pub fn shown(&self) -> String {
        self.dir.display().to_string()
    }
}

fn no_folder() -> Error {
    Error::Other("this task has no folder of its own to keep spec drafts in".into())
}

/// Each repository's spec in `task`, in checkout order.
pub(crate) fn homes(state: &AppState, task: &Task) -> Vec<Home> {
    let task_dir = agent_file_dir(state, task);
    let name = spec::folder_name(&task.branch);
    state
        .config
        .checkouts_of(&task.id)
        .into_iter()
        .filter_map(|checkout| {
            let project = state.config.project(&checkout.project_id).ok()?;
            let folder = Path::new(&checkout.path).file_name()?.to_string_lossy().to_string();
            let (dir, rel) = if project.specs_in_app {
                let kept = state.config.folder().join("specs").join(spec::folder_name(&project.name)).join(&name);
                (kept, None)
            } else {
                let base = project.spec_folder.clone().unwrap_or_else(|| "specs".into());
                let rel = format!("{base}/{name}");
                (Path::new(&checkout.path).join(&rel), Some(rel))
            };
            let drafts = task_dir.as_ref().map(|d| d.join(DRAFTS).join(&folder));
            Some(Home { checkout, project, folder, dir, rel, drafts })
        })
        .collect()
}

fn home_of(state: &AppState, task: &Task, checkout_id: &str) -> Result<Home> {
    homes(state, task)
        .into_iter()
        .find(|h| h.checkout.id == checkout_id)
        .ok_or_else(|| Error::NotFound(format!("checkout {checkout_id}")))
}

/// A task's specs as the Spec tab shows them (SPEC-8).
#[derive(Debug, Serialize)]
pub struct TaskSpec {
    pub repos: Vec<RepoSpec>,
    /// A `SPEC.md` from before, to start the requirements from (SPEC-17).
    pub legacy: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RepoSpec {
    pub checkout_id: String,
    pub repo: String,
    pub folder: String,
    /// Where it is kept, and whether that is in the repository (SPEC-3).
    pub home: String,
    pub in_repo: bool,
    pub kind: Kind,
    pub parts: Vec<PartView>,
    pub requirements: Vec<Requirement>,
    pub steps: Vec<Step>,
    pub check: Option<CheckView>,
}

#[derive(Debug, Serialize)]
pub struct PartView {
    pub part: Part,
    pub file: String,
    pub approved: Option<String>,
    pub draft: Option<String>,
    /// Approved, but an earlier file was approved since (SPEC-7).
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct CheckView {
    #[serde(flatten)]
    pub check: Check,
    /// The branch has moved on since.
    pub stale: bool,
}

/// Runs git, for whether a check is still current.
fn view(state: &AppState, task: &Task) -> TaskSpec {
    let repos = homes(state, task)
        .into_iter()
        .map(|h| {
            let notes = h.notes();
            let kind = h.kind();
            let parts = Part::ALL
                .iter()
                .map(|&part| PartView {
                    part,
                    file: part.file(kind).to_string(),
                    approved: h.approved(part),
                    draft: h.draft(part),
                    stale: notes.stale.contains(&part),
                })
                .collect();
            let head = git::head_commit(Path::new(&h.checkout.path)).ok();
            RepoSpec {
                checkout_id: h.checkout.id.clone(),
                repo: h.project.name.clone(),
                folder: h.folder.clone(),
                home: h.rel.clone().map(|r| format!("{}/{r}", h.folder)).unwrap_or_else(|| h.shown()),
                in_repo: h.rel.is_some(),
                kind,
                parts,
                requirements: h.requirements(),
                steps: h.approved(Part::Tasks).map(|t| spec::steps(&t)).unwrap_or_default(),
                check: notes.check.map(|check| CheckView { stale: head.as_deref() != Some(check.sha.as_str()), check }),
            }
        })
        .collect();
    TaskSpec { repos, legacy: legacy(state, task) }
}

fn legacy(state: &AppState, task: &Task) -> Option<String> {
    let text = std::fs::read_to_string(agent_file_dir(state, task)?.join(LEGACY_FILE)).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

/// The task's specs.
#[tauri::command]
pub async fn read_spec(app: tauri::AppHandle, task_id: String) -> Result<TaskSpec> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        Ok(view(state, &task))
    })
    .await
}

/// Keep what the editor holds for one file, or with None let it go: kept in
/// the task folder, so quitting loses nothing, and seen by no agent (SPEC-6).
#[tauri::command]
pub async fn save_spec_draft(
    app: tauri::AppHandle,
    task_id: String,
    checkout_id: String,
    part: Part,
    text: Option<String>,
) -> Result<()> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        home_of(state, &task, &checkout_id)?.save_draft(part, text.as_deref())
    })
    .await
}

/// One file's approved text.
#[derive(Debug, Deserialize)]
pub struct PartText {
    pub part: Part,
    pub text: String,
}

/// Approve files of one repository's spec: written into its spec folder and,
/// in a repository that keeps specs, committed on the task branch with only
/// the spec's own files in the commit (SPEC-6). The files after them that
/// were approved before are out of date from now (SPEC-7). Empty text
/// removes a file.
#[tauri::command]
pub async fn approve_spec(
    app: tauri::AppHandle,
    task_id: String,
    checkout_id: String,
    texts: Vec<PartText>,
    kind: Option<Kind>,
) -> Result<TaskSpec> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        let home = home_of(state, &task, &checkout_id)?;
        approve(&home, &task, &texts, kind)?;
        // Every repository's requirements approved: the spec from before has
        // been carried over (SPEC-17).
        let all = homes(state, &task);
        if !all.is_empty() && all.iter().all(|h| h.approved(Part::Requirements).is_some()) {
            if let Some(dir) = agent_file_dir(state, &task) {
                let _ = std::fs::remove_file(dir.join(LEGACY_FILE));
            }
        }
        write_task_context(state, &task)?;
        Ok(view(state, &task))
    })
    .await
}

fn approve(home: &Home, task: &Task, texts: &[PartText], kind: Option<Kind>) -> Result<()> {
    if texts.is_empty() {
        return Ok(());
    }
    let before = home.kind();
    let kind = kind.unwrap_or(before);
    std::fs::create_dir_all(&home.dir)?;
    for t in texts {
        let path = home.dir.join(t.part.file(kind));
        let text = t.text.trim();
        if text.is_empty() {
            remove(&path)?;
        } else {
            crate::agents::replace_file(&path, format!("{text}\n").as_bytes(), None)?;
        }
        // A bug that turned out to be a feature, or the other way round,
        // keeps one requirements file, not both.
        if t.part == Part::Requirements && kind != before {
            remove(&home.dir.join(Part::Requirements.file(before)))?;
        }
    }
    if let Some(rel) = &home.rel {
        let words: Vec<&str> = texts.iter().map(|t| t.part.word(kind)).collect();
        let message = format!("Approve the {} for {}.", and_list(&words), task.issue_key.as_deref().unwrap_or(&task.branch));
        git::commit_only(Path::new(&home.checkout.path), &[rel.as_str()], &message)?;
    }
    let mut notes = home.notes();
    notes.kind = kind;
    let first = texts.iter().map(|t| t.part.index()).min().unwrap_or(0);
    let approved: Vec<Part> = texts.iter().map(|t| t.part).collect();
    notes.stale.retain(|p| !approved.contains(p));
    for part in Part::ALL.into_iter().filter(|p| p.index() > first && !approved.contains(p)) {
        if home.approved(part).is_some() && !notes.stale.contains(&part) {
            notes.stale.push(part);
        }
    }
    home.keep(&notes)?;
    for t in texts {
        home.save_draft(t.part, None)?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

fn and_list(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Tick (or untick) step `number` of a repository's `tasks.md`, for an
/// agent's `spec_task` (SPEC-13). Committed later, with the next approval
/// or before the pull requests are opened.
pub(crate) fn tick_step(state: &AppState, task_id: &str, repo: &str, number: u32, done: bool) -> Result<Step> {
    let task = state.config.task(task_id)?;
    let all = homes(state, &task);
    let home = all
        .iter()
        .find(|h| h.folder == repo || h.project.name == repo)
        .or_else(|| (all.len() == 1).then(|| &all[0]))
        .ok_or_else(|| Error::NotFound(format!("a spec for `{repo}` in this task")))?;
    let path = home.dir.join(Part::Tasks.file(Kind::Feature));
    let text = std::fs::read_to_string(&path)
        .map_err(|_| Error::NotFound(format!("an approved tasks.md for `{repo}`")))?;
    let ticked = spec::tick(&text, number, done).ok_or_else(|| Error::NotFound(format!("step {number}")))?;
    std::fs::write(&path, &ticked)?;
    spec::steps(&ticked)
        .into_iter()
        .find(|s| s.number == number)
        .ok_or_else(|| Error::NotFound(format!("step {number}")))
}

/// Commit what agents ticked since the last approval, before a push takes
/// the branch to review (SPEC-13). Nothing for a repository that keeps no
/// specs, or with nothing ticked.
pub(crate) fn commit_ticks(state: &AppState, task: &Task, checkout_id: &str) -> Result<()> {
    let Ok(home) = home_of(state, task, checkout_id) else { return Ok(()) };
    let Some(rel) = &home.rel else { return Ok(()) };
    if !home.dir.is_dir() {
        return Ok(());
    }
    let message = format!("Tick the done steps of the spec for {}.", task.issue_key.as_deref().unwrap_or(&task.branch));
    git::commit_only(Path::new(&home.checkout.path), &[rel.as_str()], &message)?;
    Ok(())
}

/// The line an opening prompt gains when the task has a spec (SPEC-11):
/// every spec folder by its full path.
pub(crate) fn prompt_line(state: &AppState, task: &Task) -> Option<String> {
    let found: Vec<String> = homes(state, task)
        .iter()
        .filter(|h| h.approved(Part::Requirements).is_some())
        .map(|h| format!("`{}` for {}", h.shown(), h.folder))
        .collect();
    (!found.is_empty()).then(|| {
        format!(
            "The user's spec for this work is in {}: requirements, design and tasks. Its \
             requirements are what done means; read it before you start.",
            and_list(&found.iter().map(String::as_str).collect::<Vec<_>>())
        )
    })
}

/// Each repository's approved requirements, for the context file (SPEC-10):
/// its folder name, where its spec is, and the text.
pub(crate) fn approved_requirements(state: &AppState, task: &Task) -> Vec<(String, String, String)> {
    homes(state, task)
        .iter()
        .filter_map(|h| Some((h.folder.clone(), h.shown(), h.approved(Part::Requirements)?)))
        .collect()
}

/// What a drafted pull request description is told of the spec (SPEC-16):
/// for each repository with approved requirements, where its spec is in it
/// and each requirement with the last check's answer. Runs git, for whether
/// that check is still current. Empty without a spec.
pub(crate) fn pr_section(state: &AppState, task: &Task) -> String {
    let mut md = String::new();
    for h in homes(state, task) {
        let reqs = h.requirements();
        if reqs.is_empty() {
            continue;
        }
        let place = match &h.rel {
            Some(rel) => format!("`{rel}/` in the repository"),
            None => "kept outside the repository, so not linked".into(),
        };
        md.push_str(&format!("\n## Spec of `{}`: {place}\n\n", h.repo_name()));
        let check = h.notes().check;
        let head = git::head_commit(Path::new(&h.checkout.path)).ok();
        let stale = check.as_ref().is_some_and(|c| head.as_deref() != Some(c.sha.as_str()));
        for r in reqs {
            let answer = check
                .as_ref()
                .and_then(|c| c.results.iter().find(|v| v.id == r.id))
                .map(|v| format!("{}{}", v.verdict, if v.evidence.is_empty() { String::new() } else { format!(" ({})", v.evidence) }))
                .unwrap_or_else(|| "not checked".into());
            md.push_str(&format!("- {}: {} — {answer}\n", r.id, r.text));
        }
        match (&check, stale) {
            (None, _) => md.push_str("\nNo check against the spec was run.\n"),
            (Some(_), true) => md.push_str("\nThe check was run before the latest commits.\n"),
            _ => {}
        }
    }
    md
}

/// What Start agent asks of an agent on the spec (SPEC-12): its tasks, in
/// order, each marked done as it is finished.
#[tauri::command]
pub async fn spec_work_prompt(app: tauri::AppHandle, task_id: String, checkout_id: Option<String>) -> Result<String> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        let lists: Vec<String> = homes(state, &task)
            .iter()
            .filter(|h| checkout_id.as_ref().is_none_or(|id| *id == h.checkout.id))
            .filter(|h| h.approved(Part::Tasks).is_some())
            .map(|h| format!("- {} (repo `{}`)", h.dir.join("tasks.md").display(), h.folder))
            .collect();
        if lists.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "Work through the steps in\n{}\nin order. When you finish a step, call `spec_task` on the \
             `villain-layer` MCP server with task id `{}`, the repo, and the step's number, so it is \
             ticked off. If a step turns out wrong or the spec does not hold, stop and ask rather \
             than working around it.",
            lists.join("\n"),
            task.id
        ))
    })
    .await
}

/// Tell the task's running agents that the spec changed (SPEC-14): one line
/// each, naming the files. Returns how many were told.
#[tauri::command]
pub async fn tell_spec_change(app: tauri::AppHandle, task_id: String, checkout_id: String, parts: Vec<Part>) -> Result<usize> {
    super::blocking(app, move |state| {
        let task = state.config.task(&task_id)?;
        let home = home_of(state, &task, &checkout_id)?;
        let kind = home.kind();
        let files: Vec<String> = parts.iter().map(|p| home.dir.join(p.file(kind)).display().to_string()).collect();
        let line = format!(
            "The user changed the spec for this work: {}. Read it again, and check what you are doing still meets it.",
            and_list(&files.iter().map(String::as_str).collect::<Vec<_>>())
        );
        let mut told = 0;
        for pane in state.ptys.list(Some(&task.id)) {
            if pane.running && pane.kind == crate::pty::PaneKind::Agent {
                super::hand_over(state, &task, &pane.id, "SPEC_CHANGED.md", &line)?;
                told += 1;
            }
        }
        Ok(told)
    })
    .await
}

/// Where a repository keeps specs (SPEC-1, SPEC-3): committed in it, under
/// `folder`, or kept by the app.
#[tauri::command]
pub fn set_project_specs(state: State<AppState>, project_id: String, in_app: bool, folder: String) -> Result<()> {
    let folder = spec::valid_folder(&folder).map_err(Error::Other)?;
    state.config.update(|c| {
        if let Some(p) = c.projects.iter_mut().find(|p| p.id == project_id) {
            p.specs_in_app = in_app;
            p.spec_folder = folder;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(root: &Path, in_repo: bool) -> Home {
        let worktree = root.join("api");
        std::fs::create_dir_all(&worktree).unwrap();
        let rel = in_repo.then(|| "specs/ACME-12".to_string());
        let dir = match &rel {
            Some(r) => worktree.join(r),
            None => root.join("kept/ACME-12"),
        };
        Home {
            checkout: Checkout {
                id: "c1".into(),
                task_id: "t1".into(),
                project_id: "p1".into(),
                path: worktree.to_string_lossy().to_string(),
                base: "main".into(),
                base_commit: None,
                push_lease: None,
                point_before_update: None,
                last_head: None,
            },
            project: Project {
                id: "p1".into(),
                name: "api".into(),
                path: String::new(),
                default_branch: "main".into(),
                group: None,
                store: None,
                update_by: None,
                spec_folder: None,
                specs_in_app: !in_repo,
            },
            folder: "api".into(),
            dir,
            rel,
            drafts: Some(root.join(DRAFTS).join("api")),
        }
    }

    fn task() -> Task {
        Task {
            id: "t1".into(),
            name: "ACME-12 Refunds".into(),
            root: "/work/acme-12".into(),
            branch: "ACME-12".into(),
            issue_key: Some("ACME-12".into()),
            issue_url: None,
            created_at: chrono::Utc::now(),
            ticket_stage: None,
            chat: None,
            review: None,
            browser: None,
            browser_url: None,
        }
    }

    fn texts(parts: &[(Part, &str)]) -> Vec<PartText> {
        parts.iter().map(|(part, text)| PartText { part: *part, text: text.to_string() }).collect()
    }

    #[test]
    fn approving_later_files_and_then_an_earlier_one_marks_the_later_ones_out_of_date() {
        let root = std::env::temp_dir().join(format!("vl-spec-{}", uuid::Uuid::new_v4()));
        let h = home(&root, false);
        approve(&h, &task(), &texts(&[(Part::Requirements, "## Requirements\n- R-1: x"), (Part::Design, "d"), (Part::Tasks, "- [ ] 1. a (R-1)")]), None).unwrap();
        assert!(h.notes().stale.is_empty(), "approved together, nothing is out of date");
        assert_eq!(h.requirements().len(), 1);

        h.save_draft(Part::Requirements, Some("## Requirements\n- R-1: y")).unwrap();
        approve(&h, &task(), &texts(&[(Part::Requirements, "## Requirements\n- R-1: y")]), None).unwrap();
        assert_eq!(h.notes().stale, vec![Part::Design, Part::Tasks]);
        assert!(h.draft(Part::Requirements).is_none(), "an approved draft is let go");

        approve(&h, &task(), &texts(&[(Part::Design, "d2")]), None).unwrap();
        assert_eq!(h.notes().stale, vec![Part::Tasks]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_bug_keeps_its_requirements_in_bugfix_md_and_only_there() {
        let root = std::env::temp_dir().join(format!("vl-spec-{}", uuid::Uuid::new_v4()));
        let h = home(&root, false);
        approve(&h, &task(), &texts(&[(Part::Requirements, "## Requirements\n- R-1: x")]), None).unwrap();
        approve(&h, &task(), &texts(&[(Part::Requirements, "## Expected behaviour\n- R-1: x")]), Some(Kind::Bugfix)).unwrap();
        assert!(h.dir.join("bugfix.md").is_file());
        assert!(!h.dir.join("requirements.md").exists());
        assert_eq!(h.kind(), Kind::Bugfix);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn approving_in_a_repository_commits_the_spec_and_ticks_are_committed_later() {
        let root = std::env::temp_dir().join(format!("vl-spec-{}", uuid::Uuid::new_v4()));
        let h = home(&root, true);
        let wt = Path::new(&h.checkout.path);
        let git = |args: &[&str]| git::run_for_tests(wt, args).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@villain.local"], &["config", "user.name", "T"], &["commit", "-q", "--allow-empty", "-m", "init"]] {
            git(args);
        }
        approve(&h, &task(), &texts(&[(Part::Tasks, "- [ ] 1. Add it\n- [ ] 2. Test it")]), None).unwrap();
        let log = git(&["log", "--format=%s", "--name-only"]);
        assert!(log.starts_with("Approve the tasks for ACME-12.\n\nspecs/ACME-12/tasks.md"), "{log}");

        let path = h.dir.join("tasks.md");
        let ticked = spec::tick(&std::fs::read_to_string(&path).unwrap(), 2, true).unwrap();
        std::fs::write(&path, ticked).unwrap();
        git::commit_only(wt, &["specs/ACME-12"], "Tick.").unwrap();
        assert!(git(&["show", "HEAD:specs/ACME-12/tasks.md"]).contains("- [x] 2. Test it"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn commit_messages_name_every_file_approved() {
        assert_eq!(and_list(&["requirements", "design", "tasks"]), "requirements, design and tasks");
        assert_eq!(and_list(&["design"]), "design");
    }
}
