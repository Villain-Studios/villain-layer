//! Committing some files and no others: a spec the user approved (SPEC-6),
//! in a worktree where an agent may be halfway through its own change.

use std::path::Path;

use super::{in_progress, run};
use crate::error::{Error, Result};

/// Commit what is under `paths` (relative to `dir`), added, changed or
/// removed, and nothing else: whatever else is staged stays staged, and is
/// not in the commit. None when there was nothing to commit there.
pub fn commit_only(dir: &Path, paths: &[&str], message: &str) -> Result<Option<String>> {
    if paths.is_empty() {
        return Ok(None);
    }
    if let Some(busy) = in_progress(dir) {
        return Err(Error::Git(format!(
            "a {} is in progress here — resolve it or abandon it before committing",
            busy.word()
        )));
    }
    let mut add = vec!["add", "-A", "--"];
    add.extend_from_slice(paths);
    run(dir, &add)?;
    // Nothing of these paths changed: committing would fail on it, or with
    // `--allow-empty` say something that is not so.
    let mut status = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    status.extend_from_slice(paths);
    if run(dir, &status)?.trim().is_empty() {
        return Ok(None);
    }
    // `--only` takes these paths as they are and leaves the rest of the
    // index alone, so an agent's staged work is neither committed nor
    // unstaged.
    let mut commit = vec!["commit", "--only", "-m", message, "--"];
    commit.extend_from_slice(paths);
    run(dir, &commit)?;
    Ok(Some(run(dir, &["rev-parse", "HEAD"])?.trim().to_string()))
}

/// The commit `dir` is on.
pub fn head_commit(dir: &Path) -> Result<String> {
    Ok(run(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-only-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]).unwrap();
        run(&dir, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&dir, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&dir, &["add", "-A"]).unwrap();
        run(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    #[test]
    fn only_the_given_files_are_committed_and_an_agents_staged_work_stays_staged() {
        let dir = repo();
        // An agent's work: one change staged, one not.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        run(&dir, &["add", "a.txt"]).unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        std::fs::create_dir_all(dir.join("specs/T-1")).unwrap();
        std::fs::write(dir.join("specs/T-1/requirements.md"), "## Goal\n").unwrap();

        let sha = commit_only(&dir, &["specs/T-1"], "Approve the requirements for T-1.").unwrap();
        assert!(sha.is_some());
        let files = run(&dir, &["show", "--name-only", "--pretty=format:", "HEAD"]).unwrap();
        assert_eq!(files.trim(), "specs/T-1/requirements.md");
        let status = run(&dir, &["status", "--porcelain"]).unwrap();
        assert!(status.contains("M  a.txt"), "the staged change is still staged: {status}");
        assert!(status.contains("?? b.txt"), "{status}");

        // Nothing new: no commit.
        assert_eq!(commit_only(&dir, &["specs/T-1"], "again").unwrap(), None);

        // Removed: the removal is committed.
        std::fs::remove_file(dir.join("specs/T-1/requirements.md")).unwrap();
        assert!(commit_only(&dir, &["specs/T-1"], "Remove it.").unwrap().is_some());
        let files = run(&dir, &["show", "--name-status", "--pretty=format:", "HEAD"]).unwrap();
        assert_eq!(files.trim(), "D\tspecs/T-1/requirements.md");
        std::fs::remove_dir_all(&dir).ok();
    }
}
