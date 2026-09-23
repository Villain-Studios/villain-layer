//! What Sync and Clean up do in git (REPO-7, REPO-8): keep the app's copies
//! fetched, bring the user's default branch forward, and find what tasks
//! left behind.
//!
//! Only two things here write to the user's clone: a fast-forward of its
//! default branch, and deleting a branch whose every commit the app's copy
//! holds. Everything else is the app's own.

use std::path::Path;

use super::{check_names, commit_id, is_ancestor, run};
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub locked: bool,
    /// Git's record of a worktree whose folder is gone.
    pub prunable: bool,
}

/// `git worktree list`, parsed. The first entry is the repository itself.
pub fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = run(repo, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;

    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(e) = cur.take() {
                entries.push(e);
            }
            cur = Some(WorktreeEntry {
                path: path.to_string(),
                branch: None,
                head: None,
                locked: false,
                prunable: false,
            });
        } else if let Some(e) = cur.as_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                e.head = Some(head.to_string());
            } else if let Some(branch) = line.strip_prefix("branch ") {
                e.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
            } else if line.starts_with("locked") {
                e.locked = true;
            } else if line.starts_with("prunable") {
                e.prunable = true;
            }
        }
    }
    if let Some(e) = cur {
        entries.push(e);
    }
    Ok(entries)
}

/// Where `origin` fetches from.
pub fn origin_url(repo: &Path) -> Option<String> {
    run(repo, &["config", "--get", "remote.origin.url"])
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// Whether two remote URLs name the same repository, however they are
/// spelled: `git@github.com:org/api.git`, `ssh://git@github.com/org/api` and
/// `https://github.com/org/api` are one repository cloned three ways, and a
/// clone made again over https is still the clone it was over ssh.
pub fn same_remote(a: &str, b: &str) -> bool {
    fn key(url: &str) -> String {
        let url = url.trim().trim_end_matches('/');
        let url = url.strip_suffix(".git").unwrap_or(url);
        let (host, path) = if let Some((_, rest)) = url.split_once("://") {
            rest.split_once('/').unwrap_or((rest, ""))
        } else if let Some(scp) = url.split_once(':').filter(|(host, _)| !host.contains('/')) {
            scp
        } else {
            // A path on disk.
            return url.to_string();
        };
        let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
        let host = host.split_once(':').map_or(host, |(h, _)| h);
        format!("{}/{}", host.to_ascii_lowercase(), path.trim_start_matches('/'))
    }
    key(a) == key(b)
}

pub fn is_bare(repo: &Path) -> bool {
    run(repo, &["rev-parse", "--is-bare-repository"]).is_ok_and(|o| o.trim() == "true")
}

/// When the app's copy last fetched, in seconds since the epoch. Read from
/// `FETCH_HEAD`, which every fetch rewrites, so it costs no git process.
pub fn fetched_at(store: &Path) -> Option<u64> {
    let modified = std::fs::metadata(store.join("FETCH_HEAD")).ok()?.modified().ok()?;
    modified.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn tip(repo: &Path, refname: &str) -> Option<String> {
    run(repo, &["rev-parse", "--verify", "--quiet", &format!("{refname}^{{commit}}")])
        .ok()
        .map(|s| s.trim().to_string())
}

fn count(repo: &Path, args: &[&str]) -> Result<usize> {
    let mut all = vec!["rev-list", "--count"];
    all.extend_from_slice(args);
    run(repo, &all)?
        .trim()
        .parse()
        .map_err(|_| Error::Git("rev-list gave no count".into()))
}

/// Fetch origin into the app's copy, and forget the branches origin deleted.
pub fn fetch_store(store: &Path) -> Result<()> {
    run(store, &["fetch", "--quiet", "--prune", "--no-recurse-submodules", "origin"]).map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    /// Commits on origin's branch that the clone's does not have.
    pub behind: usize,
    /// Commits on the clone's branch that origin's does not have.
    pub ahead: usize,
}

/// How the clone's `branch` stands against origin's. Counted in the app's
/// copy, which fetched last: the clone's own `origin/<branch>` is only as
/// fresh as the user's last fetch. The copy lacks the clone's commit only
/// when the clone committed on it since the copy was made, and then the
/// clone's own view is the best there is. None without such a branch.
pub fn standing(clone: &Path, store: &Path, branch: &str) -> Option<Standing> {
    check_names(branch, "HEAD").ok()?;
    let local = tip(clone, &format!("refs/heads/{branch}"))?;
    let theirs = format!("refs/remotes/origin/{branch}");
    let counted = |repo: &Path| -> Option<Standing> {
        let out = run(repo, &["rev-list", "--left-right", "--count", &format!("{local}...{theirs}")]).ok()?;
        let mut n = out.split_whitespace().map(|x| x.parse::<usize>().ok());
        Some(Standing { ahead: n.next()??, behind: n.next()?? })
    };
    counted(store).or_else(|| counted(clone))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forwarded {
    UpToDate,
    /// Moved forward this many commits.
    Moved(usize),
    /// The branch has this many commits origin's does not: the user's to
    /// deal with, however their team updates branches.
    OwnCommits(usize),
    /// Checked out with uncommitted changes to tracked files, or mid-merge.
    Busy,
    /// The clone has no such branch.
    NoBranch,
}

/// Fast-forward `branch` in the user's clone to origin's, as the app's copy
/// just fetched it. Never a merge or a rebase: a branch with commits of its
/// own is left alone, whatever the team's habit, so nothing here needs to
/// know it.
pub fn fast_forward(clone: &Path, store: &Path, branch: &str) -> Result<Forwarded> {
    check_names(branch, "HEAD")?;
    let theirs = format!("refs/remotes/origin/{branch}");
    // The clone's `origin/<branch>` first, from the copy rather than the
    // network: the same answer without a second trip, and the clone's own
    // view of origin then agrees with what it was moved to.
    let from = store.to_string_lossy();
    run(
        clone,
        &["fetch", "--quiet", "--no-tags", "--no-recurse-submodules", "--", &from, &format!("+{theirs}:{theirs}")],
    )?;
    let Some(ours) = tip(clone, &format!("refs/heads/{branch}")) else {
        return Ok(Forwarded::NoBranch);
    };
    let target = tip(clone, &theirs).ok_or_else(|| Error::Git(format!("origin has no {branch}")))?;
    if ours == target {
        return Ok(Forwarded::UpToDate);
    }
    if !is_ancestor(clone, &ours, &target) {
        return Ok(Forwarded::OwnCommits(count(clone, &[ours.as_str(), "--not", target.as_str()])?));
    }
    let moved = count(clone, &[target.as_str(), "--not", ours.as_str()])?;

    let here = list_worktrees(clone)?
        .into_iter()
        .find(|w| w.branch.as_deref() == Some(branch) && !w.prunable);
    match here {
        Some(w) => {
            let wt = Path::new(&w.path);
            let st = super::status(wt)?;
            if super::in_progress(wt).is_some() || st.staged + st.unstaged + st.conflicted > 0 {
                return Ok(Forwarded::Busy);
            }
            // Untracked files are left to git: it refuses a fast-forward
            // that would overwrite one, and says which.
            run(wt, &["merge", "--ff-only", "--quiet", &target])?;
        }
        None => {
            // Checked out nowhere, so no files move: only the branch. The
            // old value makes it a compare-and-swap.
            let name = format!("refs/heads/{branch}");
            run(clone, &["update-ref", "-m", "villain-layer: sync (fast-forward)", &name, &target, &ours])?;
        }
    }
    Ok(Forwarded::Moved(moved))
}

/// Every local branch, with its tip.
pub fn branch_tips(repo: &Path) -> Result<Vec<(String, String)>> {
    let out = run(repo, &["for-each-ref", "--format=%(refname)%00%(objectname)", "refs/heads"])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let (name, sha) = l.split_once('\0')?;
            Some((name.strip_prefix("refs/heads/")?.to_string(), sha.to_string()))
        })
        .collect())
}

/// How many commits reachable from `tip` are on no remote-tracking branch:
/// what deleting the branch would lose, as far as this repository knows.
pub fn only_here(repo: &Path, tip: &str) -> Result<usize> {
    commit_id(tip)?;
    count(repo, &[tip, "--not", "--remotes"])
}

/// Whether `repo` has commit `sha` on one of its branches or remote-tracking
/// branches, so a copy of it elsewhere can go without losing anything.
pub fn holds(repo: &Path, sha: &str) -> bool {
    commit_id(sha).is_ok()
        && run(repo, &["for-each-ref", "--count=1", "--contains", sha, "refs/heads", "refs/remotes"])
            .is_ok_and(|o| !o.trim().is_empty())
}

/// Delete `branch` if it is still at `tip`: a branch that moved since it was
/// judged safe to delete is judged again, not deleted.
pub fn delete_branch_at(repo: &Path, branch: &str, tip_was: &str) -> Result<()> {
    check_names(branch, "HEAD")?;
    commit_id(tip_was)?;
    if tip(repo, &format!("refs/heads/{branch}")).as_deref() != Some(tip_was) {
        return Err(Error::Git(format!("{branch} has moved since it was checked; check again")));
    }
    // `branch -D`, not `update-ref -d`: git itself refuses a branch checked
    // out in any worktree, and takes the branch's settings with it.
    run(repo, &["branch", "-D", "--", branch]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::super::{add_worktree, create_store};
    use super::*;
    use std::path::PathBuf;

    fn sandbox() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-upkeep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A remote, the user's clone of it, the app's copy of the clone, and a
    /// teammate's clone to push new work from.
    fn scene(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        run(&remote, &["init", "-q", "-b", "main", "--bare"]).unwrap();
        let clone = root.join("clone");
        let mate = root.join("mate");
        for dir in [&clone, &mate] {
            run(root, &["clone", "-q", remote.to_str().unwrap(), dir.to_str().unwrap()]).unwrap();
            run(dir, &["config", "user.email", "t@villain.local"]).unwrap();
            run(dir, &["config", "user.name", "Test"]).unwrap();
        }
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        run(&clone, &["add", "-A"]).unwrap();
        run(&clone, &["commit", "-qm", "init"]).unwrap();
        run(&clone, &["push", "-q", "-u", "origin", "main"]).unwrap();
        run(&mate, &["pull", "-q", "origin", "main"]).unwrap();
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        (clone, mate, store)
    }

    fn push_from(mate: &Path, file: &str) {
        std::fs::write(mate.join(file), "theirs\n").unwrap();
        run(mate, &["add", "-A"]).unwrap();
        run(mate, &["commit", "-qm", file]).unwrap();
        run(mate, &["push", "-q", "origin", "HEAD:main"]).unwrap();
    }

    fn head(dir: &Path, rev: &str) -> String {
        run(dir, &["rev-parse", rev]).unwrap().trim().to_string()
    }

    #[test]
    fn one_repository_cloned_over_ssh_and_https_is_the_same_remote() {
        let ssh = "git@github.com:Org/api.git";
        assert!(same_remote(ssh, "https://github.com/Org/api"));
        assert!(same_remote(ssh, "ssh://git@GitHub.com:22/Org/api.git/"));
        assert!(same_remote("/Users/me/code/api", "/Users/me/code/api/"));
        assert!(!same_remote(ssh, "git@github.com:Org/web.git"));
        assert!(!same_remote(ssh, "git@gitlab.com:Org/api.git"));
    }

    #[test]
    fn sync_fast_forwards_a_checked_out_main_and_says_how_far() {
        let root = sandbox();
        let (clone, mate, store) = scene(&root);
        push_from(&mate, "b.txt");
        push_from(&mate, "c.txt");
        fetch_store(&store).unwrap();
        assert_eq!(standing(&clone, &store, "main"), Some(Standing { behind: 2, ahead: 0 }));

        assert_eq!(fast_forward(&clone, &store, "main").unwrap(), Forwarded::Moved(2));
        assert_eq!(head(&clone, "HEAD"), head(&mate, "HEAD"));
        assert!(clone.join("c.txt").is_file(), "the files moved with the branch");
        assert_eq!(head(&clone, "origin/main"), head(&mate, "HEAD"), "the clone's view of origin agrees");
        assert_eq!(fast_forward(&clone, &store, "main").unwrap(), Forwarded::UpToDate);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_main_with_commits_of_its_own_is_left_for_the_user() {
        let root = sandbox();
        let (clone, mate, store) = scene(&root);
        std::fs::write(clone.join("mine.txt"), "mine\n").unwrap();
        run(&clone, &["add", "-A"]).unwrap();
        run(&clone, &["commit", "-qm", "mine"]).unwrap();
        let mine = head(&clone, "HEAD");
        push_from(&mate, "b.txt");
        fetch_store(&store).unwrap();

        assert_eq!(fast_forward(&clone, &store, "main").unwrap(), Forwarded::OwnCommits(1));
        assert_eq!(head(&clone, "HEAD"), mine, "not merged, not rebased");
        // The copy never saw the clone's commit, so this is counted in the
        // clone, against the origin/main the sync just brought it.
        assert_eq!(standing(&clone, &store, "main"), Some(Standing { behind: 1, ahead: 1 }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_main_with_uncommitted_edits_is_not_moved_under_them() {
        let root = sandbox();
        let (clone, mate, store) = scene(&root);
        let before = head(&clone, "HEAD");
        std::fs::write(clone.join("a.txt"), "editing\n").unwrap();
        push_from(&mate, "b.txt");
        fetch_store(&store).unwrap();

        assert_eq!(fast_forward(&clone, &store, "main").unwrap(), Forwarded::Busy);
        assert_eq!(head(&clone, "HEAD"), before);
        assert_eq!(std::fs::read_to_string(clone.join("a.txt")).unwrap(), "editing\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_main_not_checked_out_moves_without_touching_the_folder() {
        let root = sandbox();
        let (clone, mate, store) = scene(&root);
        run(&clone, &["switch", "-q", "-c", "feature"]).unwrap();
        std::fs::write(clone.join("a.txt"), "on feature, uncommitted\n").unwrap();
        push_from(&mate, "b.txt");
        fetch_store(&store).unwrap();

        assert_eq!(fast_forward(&clone, &store, "main").unwrap(), Forwarded::Moved(1));
        assert_eq!(head(&clone, "main"), head(&mate, "HEAD"));
        assert_eq!(run(&clone, &["branch", "--show-current"]).unwrap().trim(), "feature");
        assert!(!clone.join("b.txt").exists(), "the checked-out branch's files stay as they were");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_branch_is_only_safe_to_drop_once_its_commits_are_elsewhere() {
        let root = sandbox();
        let (clone, _mate, store) = scene(&root);
        let wt = root.join("task/clone");
        add_worktree(&store, &wt, "task", "main").unwrap();
        std::fs::write(wt.join("a.txt"), "work\n").unwrap();
        run(&wt, &["commit", "-qam", "work"]).unwrap();
        let work = head(&wt, "HEAD");

        assert_eq!(only_here(&store, &work).unwrap(), 1, "never pushed");
        assert!(holds(&store, &work));
        assert!(!holds(&clone, &work), "the clone never saw it");
        let (_, main) = branch_tips(&clone).unwrap().into_iter().find(|(b, _)| b == "main").unwrap();
        assert!(holds(&store, &main));

        assert!(delete_branch_at(&store, "task", &work).is_err(), "checked out in the task's worktree");
        run(&store, &["worktree", "remove", "--force", wt.to_str().unwrap()]).unwrap();
        assert!(delete_branch_at(&store, "task", &main).is_err(), "not where it was judged");
        delete_branch_at(&store, "task", &work).unwrap();
        assert!(!super::super::branch_exists(&store, "task"));
        std::fs::remove_dir_all(&root).ok();
    }
}
