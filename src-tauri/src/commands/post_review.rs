//! A review task's notes, posted to its pull request as one GitHub review
//! (REV-11).
//!
//! Reviewing here ended at the same place as before: copying each note into
//! GitHub by hand, line by line. The notes already say which file and line
//! they are on, so they go as line comments, with a body and a verdict, in
//! one review. A note GitHub cannot anchor (a line outside the pull
//! request's diff) goes into the body under its `file:line`, never lost.

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::error::{Error, Result};
use crate::git;
use crate::integrations::github::{review_event, LineComment, Posted};

use super::diff::ReviewComment;
use super::github::github_client;
use super::reviewer::{new_side_hunks, Hunks};
use super::{off_runtime, AppState};

#[derive(Debug, Serialize)]
pub struct PostedReview {
    /// The review on GitHub.
    pub url: String,
    /// Posted as line comments.
    pub inline: usize,
    /// Posted in the body, under their `file:line`.
    pub in_body: usize,
    /// GitHub refused the line comments, so every note went in the body.
    pub moved_all: bool,
}

/// Post a review task's notes as one review of its pull request: `verdict`
/// is "comment", "approve" or "request_changes". Asked for from a dialog
/// that shows exactly this; nothing reaches GitHub before it.
#[tauri::command]
pub async fn github_post_review(
    state: State<'_, AppState>,
    task_id: String,
    verdict: String,
    body: String,
    notes: Vec<ReviewComment>,
) -> Result<PostedReview> {
    let task = state.config.task(&task_id)?;
    let review = task
        .review
        .clone()
        .ok_or_else(|| Error::Other(format!("{} is not a review of a pull request", task.name)))?;
    let event = review_event(&verdict)?;
    let checkout = state
        .config
        .checkouts_of(&task_id)
        .into_iter()
        .next()
        .ok_or_else(|| Error::NotFound(format!("{} has no worktree", task.name)))?;
    let (client, _) = github_client(&state)?;

    let head = review.head_sha.clone();
    let hunks = off_runtime(move || {
        review_lines(&PathBuf::from(&checkout.path), &checkout.base, checkout.base_commit.as_deref(), &head)
    })
    .await??;

    let (inline, off) = split_notes(&notes, &hunks);
    let text = review_body(&body, &off);
    if inline.is_empty() && text.trim().is_empty() && event != "APPROVE" {
        return Err(Error::Other("there is nothing to post: write a summary or leave a note first".into()));
    }
    let posted = client.post_review(&review.repo, review.number, &review.head_sha, event, &text, &inline).await?;
    match posted {
        Posted::Review(url) => Ok(PostedReview { url, inline: inline.len(), in_body: off.len(), moved_all: false }),
        Posted::LinesRefused(_) => {
            // Lines the diff here has and GitHub's does not: its base moved,
            // say. The review still goes, with every note in its body.
            let all: Vec<_> = notes.iter().map(|n| (n.path.clone(), n.line, n.body.clone())).collect();
            let text = review_body(&body, &all);
            match client.post_review(&review.repo, review.number, &review.head_sha, event, &text, &[]).await? {
                Posted::Review(url) => Ok(PostedReview { url, inline: 0, in_body: all.len(), moved_all: true }),
                Posted::LinesRefused(why) => Err(Error::Other(format!("GitHub refused the review: {why}"))),
            }
        }
    }
}

/// The lines of the review's worktree a comment can be anchored on, as
/// GitHub's diff of the pull request has them. Refused when the worktree is
/// not the commit the review is of: its line numbers would be another
/// version's, and land on the wrong lines.
fn review_lines(dir: &std::path::Path, base: &str, base_commit: Option<&str>, head: &str) -> Result<Hunks> {
    let status = git::status(dir)?;
    if status.head != head {
        return Err(Error::Other(
            "this review is not at the commit it was taken at; take the latest, or drop your own commits".into(),
        ));
    }
    if status.staged + status.unstaged + status.conflicted > 0 {
        return Err(Error::Other(
            "this review has edits to tracked files, so its line numbers are not the pull request's; discard them first"
                .into(),
        ));
    }
    let from = git::baseline(dir, base, base_commit);
    Ok(new_side_hunks(&git::diff_patch(dir, &from, &[])?))
}

/// Notes on a line of the diff, as line comments; the rest as
/// `(path, line, text)`, for the body.
fn split_notes(notes: &[ReviewComment], hunks: &Hunks) -> (Vec<LineComment>, Vec<(String, u32, String)>) {
    let mut inline = Vec::new();
    let mut off = Vec::new();
    for n in notes {
        let anchored = hunks
            .get(&n.path)
            .is_some_and(|ranges| ranges.iter().any(|(a, b)| (*a..*b).contains(&n.line)));
        if anchored {
            inline.push(LineComment { path: n.path.clone(), line: n.line, body: n.body.clone() });
        } else {
            off.push((n.path.clone(), n.line, n.body.clone()));
        }
    }
    (inline, off)
}

/// The review's body: what you wrote, then the notes not on a diff line.
fn review_body(summary: &str, off: &[(String, u32, String)]) -> String {
    let mut text = summary.trim().to_string();
    if !off.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("On lines outside this diff:\n");
        for (path, line, body) in off {
            text.push_str(&format!("\n- `{path}:{line}`: {}", body.trim()));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(path: &str, line: u32, body: &str) -> ReviewComment {
        ReviewComment { path: path.into(), line, body: body.into(), code: None, repo: None }
    }

    #[test]
    fn a_note_off_the_diff_goes_in_the_body_under_its_line() {
        let hunks: Hunks = [("src/a.ts".to_string(), vec![(1, 5), (40, 43)])].into_iter().collect();
        let notes = [note("src/a.ts", 2, "Off by one."), note("src/a.ts", 20, "Unrelated."), note("src/b.ts", 1, "Elsewhere.")];
        let (inline, off) = split_notes(&notes, &hunks);
        assert_eq!(inline, vec![LineComment { path: "src/a.ts".into(), line: 2, body: "Off by one.".into() }]);
        assert_eq!(off.len(), 2);
        let body = review_body("Looks close.", &off);
        assert_eq!(body, "Looks close.\n\nOn lines outside this diff:\n\n- `src/a.ts:20`: Unrelated.\n- `src/b.ts:1`: Elsewhere.");
        assert_eq!(review_body("  ", &[]), "", "nothing written, nothing added");
    }

    #[test]
    fn a_review_whose_worktree_moved_is_not_posted_on_the_wrong_lines() {
        let dir = std::env::temp_dir().join(format!("vl-post-review-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| crate::git::run_for_tests(&dir, args).unwrap().trim().to_string();
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@villain.local"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let point = git(&["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(&["commit", "-qam", "two"]);
        let head = git(&["rev-parse", "HEAD"]);

        let hunks = review_lines(&dir, "main", Some(&point), &head).unwrap();
        assert_eq!(hunks["a.txt"], vec![(1, 3)]);
        assert!(review_lines(&dir, "main", Some(&point), &point).unwrap_err().to_string().contains("not at the commit"));
        std::fs::write(dir.join("a.txt"), "edited\n").unwrap();
        assert!(review_lines(&dir, "main", Some(&point), &head).unwrap_err().to_string().contains("edits"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
