//! What a push of a task's branch would send (PR-11): the commits GitHub
//! does not have yet, and the lines they add.

use std::path::Path;

use super::{baseline, commits_since, is_ancestor, remote_tip, run, CommitInfo};
use crate::error::Result;

/// The commit a push would send from: the branch on origin, when this one
/// has grown from it; else its branch point, for a branch never pushed or
/// one rebased since, where everything goes again.
pub fn outgoing_from(dir: &Path, branch: &str, base: &str, base_commit: Option<&str>) -> String {
    match remote_tip(dir, branch) {
        Some(tip) if is_ancestor(dir, &tip, "HEAD") => tip,
        _ => baseline(dir, base, base_commit),
    }
}

/// The commits after `from`, newest first, as the Diff tab's picker lists
/// them: first parent only, so a merge of the base is one commit.
pub fn outgoing_commits(dir: &Path, base: &str, from: &str) -> Result<Vec<CommitInfo>> {
    commits_since(dir, base, Some(from))
}

/// The committed change from `from` to HEAD with no context lines: what a
/// push sends, and nothing uncommitted, which it does not.
pub fn outgoing_patch(dir: &Path, from: &str) -> Result<String> {
    super::commit_id(from)?;
    // Prefixes spelled out, as in `diff_patch`: the user's config may
    // change or drop them, and the reader looks for `+++ b/`.
    run(
        dir,
        &["diff", "--no-color", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "-U0", from, "HEAD"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        run(dir, args).unwrap().trim().to_string()
    }

    #[test]
    fn a_push_sends_what_origin_lacks_and_never_what_is_uncommitted() {
        let root = std::env::temp_dir().join(format!("vl-outgoing-{}", uuid::Uuid::new_v4()));
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let wt = root.join("wt");
        git(&root, &["clone", "-q", origin.to_str().unwrap(), wt.to_str().unwrap()]);
        git(&wt, &["config", "user.email", "test@villain.local"]);
        git(&wt, &["config", "user.name", "Test"]);
        std::fs::write(wt.join("a.txt"), "one\n").unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "init"]);
        git(&wt, &["push", "-q", "origin", "main"]);
        let point = git(&wt, &["rev-parse", "HEAD"]);
        git(&wt, &["switch", "-q", "-c", "feature"]);

        std::fs::write(wt.join("a.txt"), "one\ntwo\n").unwrap();
        git(&wt, &["commit", "-qam", "two"]);
        // Never pushed: everything since the branch point goes.
        let from = outgoing_from(&wt, "feature", "main", Some(&point));
        assert_eq!(from, point);
        git(&wt, &["push", "-q", "-u", "origin", "feature"]);

        std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&wt, &["commit", "-qam", "three"]);
        std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\nuncommitted\n").unwrap();
        let from = outgoing_from(&wt, "feature", "main", Some(&point));
        let subjects: Vec<_> = outgoing_commits(&wt, "main", &from).unwrap().into_iter().map(|c| c.subject).collect();
        assert_eq!(subjects, vec!["three"], "only what origin does not have");
        let patch = outgoing_patch(&wt, &from).unwrap();
        assert!(patch.contains("+three"));
        assert!(!patch.contains("uncommitted"), "a push sends commits, not the worktree");
        std::fs::remove_dir_all(&root).ok();
    }
}
