//! Someone else's pull request, checked out to review it (REV-10).
//!
//! Its commits come from `refs/pull/<n>/head` on origin, which GitHub keeps
//! for every pull request, including one from a fork. Its own branch name
//! would not do: a fork's branch is not on origin at all, and a branch of
//! the same name there is someone else's code. The worktree's branch is the
//! app's own name for it, and is never pushed.

use std::path::Path;

use super::run;
use crate::error::{Error, Result};

/// Where the copy keeps a pull request's head, apart from its branches and
/// from `origin/*`, which `fetch --prune` would take away.
fn pull_ref(number: u64) -> String {
    format!("refs/villain/pull/{number}")
}

/// Fetch pull request `number`'s head from origin and say which commit it is.
fn fetch_pull(dir: &Path, number: u64) -> Result<String> {
    let at = pull_ref(number);
    run(dir, &["fetch", "--quiet", "origin", &format!("+refs/pull/{number}/head:{at}")])
        .map_err(|e| Error::Git(format!("could not fetch pull request #{number}: {e}")))?;
    Ok(run(dir, &["rev-parse", "--verify", "--quiet", &format!("{at}^{{commit}}")])?.trim().to_string())
}

/// Make `branch` in `store` at pull request `number`'s head, fetched now,
/// and return that commit. A branch of that name left by an earlier review
/// is moved; one checked out in a worktree is refused by git, and that is
/// an error rather than a second worktree on it.
pub fn take_pull(store: &Path, number: u64, branch: &str) -> Result<String> {
    super::check_names(branch, "HEAD")?;
    let sha = fetch_pull(store, number)?;
    run(store, &["branch", "--force", "--", branch, &sha])?;
    Ok(sha)
}

/// Move the review worktree `wt` to pull request `number`'s head now, and
/// return it with the branch point against `base`, fetched too: what the
/// pull request's diff on GitHub is measured from. `taken` is the commit it
/// was last moved to.
///
/// Refused when moving would lose anything: edits to tracked files, an
/// update under way, or commits made here since. A head the author rewrote
/// (a force-push) is followed like any other; only `taken` was ours.
pub fn follow_pull(wt: &Path, number: u64, base: &str, taken: &str) -> Result<(String, String)> {
    super::check_names("HEAD", base)?;
    let status = super::status(wt)?;
    if status.staged + status.unstaged + status.conflicted > 0 {
        return Err(Error::Git(
            "this review has uncommitted edits to tracked files; commit or discard them first".into(),
        ));
    }
    if super::in_progress(wt).is_some() {
        return Err(Error::Git("a merge or rebase is under way here; finish or abandon it first".into()));
    }
    if status.head != taken {
        return Err(Error::Git(
            "this review has commits of its own since the pull request was taken; they would be lost".into(),
        ));
    }
    let sha = fetch_pull(wt, number)?;
    if sha != taken {
        // `--keep`, not `--hard`: it refuses rather than overwrite a file it
        // did not expect, should something change between the check above
        // and here.
        run(wt, &["reset", "--quiet", "--keep", &sha])?;
    }
    // Best-effort, as for a new task: a stale base still gives a point.
    let _ = super::fetch_tracking(wt, base);
    Ok((sha.clone(), super::baseline(wt, base, None)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) -> String {
        run(dir, args).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A bare origin, a seed clone that pushes to it, and the app's copy:
    /// pull request 7's head is published as GitHub does, as
    /// `refs/pull/7/head`, and the branch it came from is not on origin.
    fn origin_with_pull() -> (PathBuf, PathBuf, PathBuf, String) {
        let root = std::env::temp_dir().join(format!("vl-review-{}", uuid::Uuid::new_v4()));
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let seed = root.join("seed");
        git(&root, &["clone", "-q", origin.to_str().unwrap(), seed.to_str().unwrap()]);
        git(&seed, &["config", "user.email", "test@villain.local"]);
        git(&seed, &["config", "user.name", "Test"]);
        commit(&seed, "a.txt", "one\n");
        git(&seed, &["push", "-q", "origin", "main"]);
        git(&seed, &["switch", "-q", "-c", "their-fork-branch"]);
        let head = commit(&seed, "b.txt", "two\n");
        git(&seed, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]);
        let store = root.join("store.git");
        git(&root, &["clone", "-q", "--bare", origin.to_str().unwrap(), store.to_str().unwrap()]);
        (root, seed, store, head)
    }

    fn republish(seed: &Path) -> String {
        let head = git(seed, &["rev-parse", "HEAD"]);
        git(seed, &["push", "-q", "--force", "origin", "HEAD:refs/pull/7/head"]);
        head
    }

    #[test]
    fn a_pull_request_from_a_fork_is_taken_from_its_pull_ref() {
        let (root, _seed, store, head) = origin_with_pull();
        assert_eq!(take_pull(&store, 7, "review/pr-7").unwrap(), head);
        assert_eq!(git(&store, &["rev-parse", "review/pr-7"]), head);
        assert!(!crate::git::branch_exists(&store, "their-fork-branch"), "its own branch is not on origin");
        assert!(take_pull(&store, 8, "review/pr-8").is_err(), "a pull request that is not there is an error");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_review_follows_new_and_rewritten_heads_but_never_loses_your_edits() {
        let (root, seed, store, head) = origin_with_pull();
        take_pull(&store, 7, "review/pr-7").unwrap();
        let wt = root.join("wt");
        git(&store, &["worktree", "add", "-q", wt.to_str().unwrap(), "review/pr-7"]);
        git(&wt, &["config", "user.email", "test@villain.local"]);
        git(&wt, &["config", "user.name", "Test"]);

        // The author pushes again.
        commit(&seed, "c.txt", "three\n");
        let pushed = republish(&seed);
        assert_eq!(follow_pull(&wt, 7, "main", &head).unwrap().0, pushed);
        assert_eq!(git(&wt, &["rev-parse", "HEAD"]), pushed);

        // And then rewrites it.
        git(&seed, &["reset", "-q", "--hard", &head]);
        let rewritten = commit(&seed, "d.txt", "four\n");
        republish(&seed);
        let (sha, point) = follow_pull(&wt, 7, "main", &pushed).unwrap();
        assert_eq!(sha, rewritten);
        assert_eq!(point, git(&seed, &["rev-parse", "main"]), "measured from where it left main");

        // An edit of yours stops it, and stays.
        std::fs::write(wt.join("a.txt"), "mine\n").unwrap();
        assert!(follow_pull(&wt, 7, "main", &rewritten).unwrap_err().to_string().contains("uncommitted"));
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "mine\n");

        // So does a commit of yours.
        git(&wt, &["commit", "-qam", "mine"]);
        assert!(follow_pull(&wt, 7, "main", &rewritten).unwrap_err().to_string().contains("commits of its own"));
        std::fs::remove_dir_all(root).ok();
    }
}
