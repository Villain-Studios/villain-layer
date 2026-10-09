//! What a merged pull request has landed (UPD-6).
//!
//! A merge does not move the recorded branch point by itself, and everything
//! measured from it went on counting the merged work as the branch's own: the
//! Diff view, the PR panel, a follow-up PR's description, the agent's history
//! — and opening PRs, which opened a second one for a repo already merged.
//! So once a pull request has merged, its head becomes the branch point — or,
//! when the head is not on the branch here, the newest commit it has the
//! change of (`git/landed.rs`).

use std::path::Path;

use crate::config::Checkout;
use crate::error::Result;
use crate::git;
use crate::integrations::github::PullRequest;

use super::{off_runtime, AppState};

/// The newest merged pull request of a branch. An open one after it, or one
/// closed unmerged, takes nothing back.
pub(crate) fn landed(prs: &[PullRequest]) -> Option<&PullRequest> {
    prs.iter().find(|p| p.merged)
}

/// Move a checkout's branch point up to what a merged pull request landed,
/// when UPD-6 allows it. Where it moved to, if it did: anything already
/// measured from the old point is stale.
pub(crate) async fn advance(state: &AppState, checkout: &Checkout, head: &str) -> Result<Option<String>> {
    let point = checkout.base_commit.clone();
    // Once moved it is the point, and the sweep that runs this for every
    // merged repository asks git nothing more.
    if point.as_deref() == Some(head) || checkout.point_before_update.is_some() {
        return Ok(None);
    }
    let (dir, base, sha) = (checkout.path.clone(), checkout.base.clone(), head.to_string());
    let Some(to) = off_runtime(move || moves_to(Path::new(&dir), point.as_deref(), &base, &sha)).await? else {
        return Ok(None);
    };
    let moved = state.config.update(|c| {
        let Some(found) = c.checkouts.iter_mut().find(|c| c.id == checkout.id) else {
            return false;
        };
        // Moved by an update in the meantime, which knows better.
        if found.base_commit != checkout.base_commit || found.point_before_update.is_some() {
            return false;
        }
        found.base_commit = Some(to.clone());
        true
    })?;
    if moved {
        state.status_cache.lock().remove(&checkout.id);
    }
    Ok(moved.then_some(to))
}

/// Where the branch point may move for a merged pull request whose head is
/// `head`: to the head, when the branch grew from it; else, when the head is
/// not on the branch here (pushed to on GitHub since, or the branch rebased
/// here), to the newest commit since the point up to which the head has every
/// one, as it is or as the same change.
///
/// Forward only. Once the base is merged back in, or rebased onto, the point
/// is the base's tip, which has the merged work in it already; the PR's head
/// is behind that, and moving back to it would count the base's work again.
fn moves_to(dir: &Path, point: Option<&str>, base: &str, head: &str) -> Option<String> {
    if head.is_empty() {
        return None;
    }
    if git::is_ancestor(dir, head, "HEAD") {
        return point.is_none_or(|p| git::is_ancestor(dir, p, head)).then(|| head.to_string());
    }
    let from = git::baseline(dir, base, point);
    // Nothing since the point is nothing to move over, and no fetch for it.
    if git::is_ancestor(dir, "HEAD", &from) || !git::have_commit(dir, head) {
        return None;
    }
    git::landed_through(dir, &from, head).filter(|to| git::is_ancestor(dir, &from, to))
}

/// Whether a task's local branch has nothing left of its own (TASK-8): the
/// base on origin or its merged pull request's head has every commit on it,
/// or that head has the change of each one the base does not.
pub(crate) fn branch_landed(repo: &Path, branch: &str, base: &str, landed: Option<&str>) -> bool {
    let local = format!("refs/heads/{branch}");
    let base = format!("refs/remotes/origin/{base}");
    git::is_ancestor(repo, &local, &base)
        || landed.is_some_and(|sha| {
            git::is_ancestor(repo, &local, sha)
                || (git::have_commit(repo, sha) && git::all_landed(repo, &base, &local, sha))
        })
}

#[cfg(test)]
mod tests {
    use super::super::repos::tests::{commit, git};
    use super::*;
    use std::path::PathBuf;

    /// A repo on `task` cut from `main`, returning it and the branch point.
    fn branch() -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("vl-landed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "test@villain.local"]);
        git(&dir, &["config", "user.name", "Test"]);
        commit(&dir, "a.txt");
        git(&dir, &["checkout", "-q", "-b", "task"]);
        (dir.clone(), head(&dir))
    }

    fn head(dir: &Path) -> String {
        git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn pr(number: u64, state: &str, merged: bool) -> PullRequest {
        PullRequest {
            number,
            title: String::new(),
            state: state.into(),
            draft: false,
            author: String::new(),
            head: "task".into(),
            head_sha: format!("sha{number}"),
            base: "main".into(),
            url: String::new(),
            mergeable_state: None,
            merged,
            comments: 0,
            review_comments: 0,
        }
    }

    #[test]
    fn a_merged_pull_request_moves_the_branch_point_to_what_it_landed() {
        let (dir, point) = branch();
        commit(&dir, "infra.tf");
        let merged = head(&dir);
        assert_eq!(git::changed_count(&dir, "main", Some(&point), git::Scope::Branch), 1);

        assert_eq!(moves_to(&dir, Some(&point), "main", &merged), Some(merged.clone()));
        assert_eq!(git::changed_count(&dir, "main", Some(&merged), git::Scope::Branch), 0);

        commit(&dir, "more.tf");
        assert_eq!(moves_to(&dir, Some(&point), "main", &merged), Some(merged.clone()), "work after the merge keeps it");
        assert_eq!(git::changed_count(&dir, "main", Some(&merged), git::Scope::Branch), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Squash-merged, then the base merged back in: the point is the base's
    /// tip, which already holds the squashed work.
    #[test]
    fn the_branch_point_never_moves_back_behind_a_merged_in_base() {
        let (dir, _) = branch();
        commit(&dir, "infra.tf");
        let merged = head(&dir);
        git(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "squashed.tf");
        let tip = head(&dir);
        git(&dir, &["checkout", "-q", "task"]);
        git(&dir, &["merge", "-q", "--no-ff", "--no-edit", "main"]);

        assert_eq!(moves_to(&dir, Some(&tip), "main", &merged), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_head_the_branch_did_not_grow_from_is_not_a_branch_point() {
        let (dir, point) = branch();
        commit(&dir, "infra.tf");
        let rewritten = head(&dir);
        git(&dir, &["reset", "-q", "--hard", &point]);
        commit(&dir, "other.tf");

        assert_eq!(moves_to(&dir, Some(&point), "main", &rewritten), None);
        assert_eq!(moves_to(&dir, Some(&point), "main", "0123456789abcdef0123456789abcdef01234567"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// DT-21035. The work was pushed and a pull request opened; Update from
    /// base then rebased it here, and that was never pushed. On GitHub,
    /// "Update branch" merged the base in instead, and that is what merged.
    /// Every commit had landed, yet the panel said "has work since" and
    /// Finish kept the branch.
    #[test]
    fn a_branch_rebased_here_after_github_updated_its_pull_request_has_landed() {
        let root = std::env::temp_dir().join(format!("vl-landed-{}", uuid::Uuid::new_v4()));
        let origin = root.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);
        let clone = |to: &Path| {
            git(&root, &["clone", "-q", origin.to_str().unwrap(), to.to_str().unwrap()]);
            git(to, &["config", "user.email", "test@villain.local"]);
            git(to, &["config", "user.name", "Test"]);
        };
        let (here, github) = (root.join("here"), root.join("github"));
        clone(&here);
        commit(&here, "a.txt");
        git(&here, &["push", "-q", "origin", "main"]);
        git(&here, &["checkout", "-q", "-b", "task"]);
        commit(&here, "fix.java");
        git(&here, &["push", "-q", "origin", "task"]);

        clone(&github);
        commit(&github, "theirs.java");
        git(&github, &["push", "-q", "origin", "main"]);
        git(&github, &["checkout", "-q", "task"]);
        git(&github, &["merge", "-q", "--no-ff", "--no-edit", "main"]);
        git(&github, &["push", "-q", "origin", "task"]);
        let merged = head(&github);

        git(&here, &["fetch", "-q", "origin", "main"]);
        git(&here, &["rebase", "-q", "origin/main"]);
        let point = git(&here, &["rev-parse", "origin/main"]).trim().to_string();
        assert_eq!(git::changed_count(&here, "main", Some(&point), git::Scope::Branch), 1);
        assert!(!branch_landed(&here, "task", "main", None), "no pull request, nothing landed");

        let to = moves_to(&here, Some(&point), "main", &merged);
        assert_eq!(to, Some(head(&here)));
        assert_eq!(git::changed_count(&here, "main", to.as_deref(), git::Scope::Branch), 0);
        assert!(branch_landed(&here, "task", "main", Some(&merged)));

        commit(&here, "after.java");
        assert!(!branch_landed(&here, "task", "main", Some(&merged)), "work after it is kept");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_branch_point_is_recorded_once_and_not_mid_update() {
        let (dir, point) = branch();
        commit(&dir, "infra.tf");
        let merged = head(&dir);
        let checkout = Checkout {
            id: "c1".into(),
            task_id: "t1".into(),
            project_id: "p1".into(),
            path: dir.to_string_lossy().to_string(),
            base: "main".into(),
            base_commit: Some(point.clone()),
            push_lease: None,
            point_before_update: None,
            last_head: None,
        };
        let mut cfg = crate::config::AppConfig::default();
        cfg.checkouts.push(checkout.clone());
        let root = dir.join(".state");
        let state = super::super::repos::tests::state(&root, cfg);
        let advance = |c: &Checkout| tauri::async_runtime::block_on(advance(&state, c, &merged)).unwrap().is_some();

        let updating = Checkout { point_before_update: Some(point.clone()), ..checkout.clone() };
        assert!(!advance(&updating), "an unfinished update owns the point");
        assert!(advance(&checkout));
        let now = state.config.checkout("c1").unwrap();
        assert_eq!(now.base_commit.as_deref(), Some(merged.as_str()));
        assert!(!advance(&now), "already there");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_newest_merged_pull_request_is_what_landed() {
        let prs = [pr(4, "open", false), pr(3, "closed", false), pr(2, "closed", true), pr(1, "closed", true)];
        assert_eq!(landed(&prs).map(|p| p.number), Some(2));
        assert!(landed(&[pr(1, "closed", false)]).is_none());
    }
}
