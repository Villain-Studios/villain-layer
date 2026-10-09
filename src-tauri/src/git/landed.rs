//! What a merged pull request has of a branch (UPD-6, TASK-8).
//!
//! Its head is not always on the branch here. GitHub's "Update branch", or a
//! suggestion committed there, puts commits on the pull request that this
//! copy never takes in; and Update from base rebases the branch here after it
//! was pushed, so what merged is the same change under other ids. Asked only
//! whether one contains the other, every commit of such a branch looked
//! unlanded: the panel said "has work since" of a repo whose work had all
//! merged, and finishing the task kept its branch.

use std::collections::HashSet;
use std::path::Path;

use super::{commit_id, is_ancestor, run};

/// Have commit `sha` here, fetched from origin when it is not. Whether it is
/// here now.
///
/// By commit, not by branch: many repositories delete the branch on merge,
/// and the pull request's head is what merged either way.
pub fn have_commit(dir: &Path, sha: &str) -> bool {
    if commit_id(sha).is_err() {
        return false;
    }
    let object = format!("{sha}^{{commit}}");
    run(dir, &["cat-file", "-e", &object]).is_ok()
        || (run(dir, &["fetch", "--quiet", "--no-tags", "--no-recurse-submodules", "origin", sha]).is_ok()
            && run(dir, &["cat-file", "-e", &object]).is_ok())
}

/// The newest commit on HEAD's first-parent line after `from` up to which
/// `landed` has every one, as it is or as the same change. None when it
/// lacks the first, or there is none.
pub fn landed_through(dir: &Path, from: &str, landed: &str) -> Option<String> {
    marked(dir, from, "HEAD", landed)?
        .into_iter()
        .take_while(|(_, has)| *has)
        .last()
        .map(|(commit, _)| commit)
}

/// Whether `landed` has every commit on `tip`'s first-parent line after
/// `from`, as it is or as the same change.
pub fn all_landed(dir: &Path, from: &str, tip: &str, landed: &str) -> bool {
    marked(dir, from, tip, landed).is_some_and(|line| line.iter().all(|(_, has)| *has))
}

/// Each commit on `tip`'s first-parent line after `from`, oldest first, and
/// whether `landed` has it.
fn marked(dir: &Path, from: &str, tip: &str, landed: &str) -> Option<Vec<(String, bool)>> {
    for rev in [from, tip, landed] {
        commit_id(rev).ok()?;
    }
    let line = run(dir, &["rev-list", "--reverse", "--first-parent", "--parents", tip, &format!("^{from}")]).ok()?;
    if line.trim().is_empty() {
        return Some(Vec::new());
    }
    // `+` is a commit whose change `landed` lacks. One it has as it is is not
    // listed, and nor is a merge, which has no change of its own to compare.
    let cherry = run(dir, &["cherry", landed, tip, from]).ok()?;
    let lacks: HashSet<&str> = cherry.lines().filter_map(|l| l.strip_prefix("+ ")).collect();
    Some(
        line.lines()
            .filter_map(|l| {
                let mut ids = l.split_whitespace();
                let commit = ids.next()?;
                let merge = ids.count() > 1;
                let has = !lacks.contains(commit) && (!merge || is_ancestor(dir, commit, landed));
                Some((commit.to_string(), has))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str]) -> String {
        run(dir, args).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, file: &str) -> String {
        std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn clone(origin: &Path, to: &Path) {
        git(origin.parent().unwrap(), &["clone", "-q", origin.to_str().unwrap(), to.to_str().unwrap()]);
        git(to, &["config", "user.email", "test@villain.local"]);
        git(to, &["config", "user.name", "Test"]);
    }

    /// The work pushed and a pull request opened from it; then on GitHub,
    /// "Update branch" merged `main` into it, and it merged. Returns the
    /// sandbox, this copy (on `task`, never told of the merge), the branch
    /// point, and the head that merged.
    fn merged_after_update_branch() -> (PathBuf, PathBuf, String, String) {
        let root = std::env::temp_dir().join(format!("vl-git-landed-{}", uuid::Uuid::new_v4()));
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let here = root.join("here");
        clone(&origin, &here);
        commit(&here, "a.txt");
        git(&here, &["push", "-q", "origin", "main"]);
        let point = git(&here, &["rev-parse", "HEAD"]);
        git(&here, &["switch", "-q", "-c", "task"]);
        commit(&here, "fix.txt");
        git(&here, &["push", "-q", "origin", "task"]);

        let github = root.join("github");
        clone(&origin, &github);
        commit(&github, "theirs.txt");
        git(&github, &["push", "-q", "origin", "main"]);
        git(&github, &["switch", "-q", "task"]);
        git(&github, &["merge", "-q", "--no-ff", "--no-edit", "main"]);
        git(&github, &["push", "-q", "origin", "task"]);
        let head = git(&github, &["rev-parse", "HEAD"]);
        (root, here, point, head)
    }

    #[test]
    fn a_merged_head_this_copy_never_fetched_is_fetched_by_its_id() {
        let (root, here, _, head) = merged_after_update_branch();
        assert!(run(&here, &["cat-file", "-e", &format!("{head}^{{commit}}")]).is_err());
        assert!(have_commit(&here, &head));
        assert!(!have_commit(&here, "0123456789abcdef0123456789abcdef01234567"));
        assert!(!have_commit(&here, "--upload-pack=touch"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_branch_behind_what_merged_has_landed() {
        let (root, here, point, head) = merged_after_update_branch();
        assert!(have_commit(&here, &head));
        let tip = git(&here, &["rev-parse", "HEAD"]);
        assert_eq!(landed_through(&here, &point, &head), Some(tip));
        assert!(all_landed(&here, &point, "task", &head));
        std::fs::remove_dir_all(&root).ok();
    }

    /// DT-21035: Update from base rebased the branch here after it was
    /// pushed, and the rebase was never pushed.
    #[test]
    fn a_branch_rebased_here_after_it_was_pushed_has_landed() {
        let (root, here, _, head) = merged_after_update_branch();
        git(&here, &["fetch", "-q", "origin", "main"]);
        git(&here, &["rebase", "-q", "origin/main"]);
        let point = git(&here, &["rev-parse", "origin/main"]);
        let rebased = git(&here, &["rev-parse", "HEAD"]);
        assert!(have_commit(&here, &head));
        assert!(!is_ancestor(&here, "HEAD", &head), "the rebase is not on what merged");

        assert_eq!(landed_through(&here, &point, &head), Some(rebased.clone()));
        assert!(all_landed(&here, &point, "task", &head));

        commit(&here, "after.txt");
        assert_eq!(landed_through(&here, &point, &head), Some(rebased), "work after it is not");
        assert!(!all_landed(&here, &point, "task", &head));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_change_that_merged_is_not_mistaken_for_one_that_did_not() {
        let (root, here, point, head) = merged_after_update_branch();
        assert!(have_commit(&here, &head));
        git(&here, &["reset", "-q", "--hard", &point]);
        commit(&here, "other.txt");
        assert_eq!(landed_through(&here, &point, &head), None);
        assert!(!all_landed(&here, &point, "task", &head));

        // A merge of something else is not a change the pull request has.
        git(&here, &["reset", "-q", "--hard", &point]);
        git(&here, &["switch", "-q", "-c", "elsewhere"]);
        commit(&here, "elsewhere.txt");
        git(&here, &["switch", "-q", "task"]);
        git(&here, &["merge", "-q", "--no-ff", "--no-edit", "elsewhere"]);
        assert!(!all_landed(&here, &point, "task", &head));
        std::fs::remove_dir_all(&root).ok();
    }
}
