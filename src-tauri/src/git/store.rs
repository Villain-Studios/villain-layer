//! The app's own copy of each repository, which every task worktree belongs
//! to.
//!
//! Worktrees used to be cut from the user's clone, and so lived inside it:
//! its `.git/worktrees/` held their registration, its branches their
//! commits. Re-cloning that clone cut seven task folders off at once (every
//! file still there, git unable to read any of them), and deleting a branch,
//! `git worktree prune` or `gc` in it reached into tasks the same way. A copy
//! the app alone uses keeps the user's clone the user's.
//!
//! The copy is `git clone --bare --local`, which hard-links the object files
//! rather than copying them. A hard link outlives the file it was made from,
//! so the copy costs little disk and survives the clone being deleted.

use std::path::{Path, PathBuf};

use super::run;
use crate::error::{Error, Result};

/// Make `store`, a private bare copy of `source`, fetching from where
/// `source` fetches.
pub fn create_store(source: &Path, store: &Path) -> Result<()> {
    let parent = store
        .parent()
        .ok_or_else(|| Error::Git(format!("{} has no parent folder", store.display())))?;
    std::fs::create_dir_all(parent)?;
    let (from, to) = (source.to_string_lossy(), store.to_string_lossy());
    run(parent, &["clone", "--bare", "--local", "--quiet", "--", &from, &to])?;

    let made = (|| {
        // A bare clone takes branches and tags only. What `source` knows of
        // origin comes along from disk, so bases and the branch picker work
        // before the first fetch reaches the network.
        let _ = run(
            store,
            &["fetch", "--quiet", "--no-tags", "--", &from, "+refs/remotes/origin/*:refs/remotes/origin/*"],
        );
        // Fetches and pushes go where the user's clone sends them, not to
        // the clone itself; a clone with no origin keeps the path.
        if let Ok(url) = run(source, &["config", "--get", "remote.origin.url"]) {
            run(store, &["config", "remote.origin.url", url.trim()])?;
        }
        run(store, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"])?;
        // A bare repository keeps no reflogs by default. They are what gets a
        // lost commit back.
        run(store, &["config", "core.logAllRefUpdates", "true"])?;
        copy_local_config(source, store)
    })();
    if made.is_err() {
        let _ = std::fs::remove_dir_all(store);
    }
    made
}

/// Whether `store` is a copy this app made of the repository at `source`:
/// a bare repository fetching from the same place. Lets a second build, or
/// a repo added again, reuse the copy instead of making another.
/// Also how Locate and Sync tell that a folder is still the same repository.
pub fn is_store_of(store: &Path, source: &Path) -> bool {
    let Some(fetches) = super::origin_url(store) else {
        return false;
    };
    // A clone with no origin is one its copy fetches from directly.
    super::is_bare(store)
        && (super::origin_url(source).is_some_and(|url| super::same_remote(&url, &fetches))
            || Path::new(&fetches) == source)
}

/// The user's settings for this repository, carried into the copy: a work
/// email, commit signing, an ssh command, a hooks path. Worktrees of the
/// clone used to get them for free, and a commit signed as the wrong person
/// is not something to find out from review. What describes the clone
/// itself rather than the user stays behind.
pub fn copy_local_config(from: &Path, to: &Path) -> Result<()> {
    let listed = run(from, &["config", "--local", "--null", "--list"])?;
    let mut seen: Vec<String> = Vec::new();
    for entry in listed.split('\0').filter(|e| !e.is_empty()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        if !carried(key) {
            continue;
        }
        // The first value replaces whatever the clone wrote; the rest are
        // added, for keys that hold several (`url.<x>.insteadOf`).
        let first = !seen.iter().any(|k| k == key);
        let flag = if first { "--replace-all" } else { "--add" };
        run(to, &["config", flag, "--", key, value])?;
        if first {
            seen.push(key.to_string());
        }
    }
    Ok(())
}

fn carried(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    let clone_only = ["remote.", "branch.", "submodule.", "extensions.", "worktree."];
    !(clone_only.iter().any(|p| k.starts_with(p))
        || matches!(
            k.as_str(),
            "core.bare" | "core.repositoryformatversion" | "core.worktree" | "core.logallrefupdates"
        ))
}

/// The registration folder a worktree's `.git` link names, if the link is
/// there and so is the folder.
fn registration(wt: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(wt.join(".git")).ok()?;
    let admin = wt.join(text.strip_prefix("gitdir:")?.trim());
    admin.is_dir().then_some(admin)
}

/// The repository a registration belongs to.
fn common_dir(admin: &Path) -> Option<PathBuf> {
    let rel = std::fs::read_to_string(admin.join("commondir")).ok()?;
    std::fs::canonicalize(admin.join(rel.trim())).ok()
}

/// The repository a worktree is registered in: the app's copy, or for one
/// not moved yet, the user's clone (its `.git`). None when it is not linked.
pub fn owner(wt: &Path) -> Option<PathBuf> {
    registration(wt).as_deref().and_then(common_dir)
}

/// Whether `wt` is already registered in `store`.
pub fn belongs_to(wt: &Path, store: &Path) -> bool {
    match (registration(wt).as_deref().and_then(common_dir), std::fs::canonicalize(store)) {
        (Some(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Move a worktree's registration from the clone it was cut from into
/// `store`, leaving every file where it is.
///
/// The branch comes across first, so commits nobody pushed are not left
/// behind in the old clone. The index is copied rather than rebuilt, so
/// what was staged stays staged. Refused mid-merge or mid-rebase: that
/// state lives in the old registration, and is not worth moving half way.
pub fn adopt_worktree(store: &Path, wt: &Path) -> Result<()> {
    if belongs_to(wt, store) {
        return Ok(());
    }
    let old = registration(wt).ok_or_else(|| {
        Error::Git(format!("{} is not linked to any repository", wt.display()))
    })?;
    let source = common_dir(&old)
        .ok_or_else(|| Error::Git(format!("cannot tell which repository {} came from", wt.display())))?;
    if super::in_progress(wt).is_some() {
        return Err(Error::Git(format!(
            "{} is in the middle of a merge or rebase; finish or abort it first",
            wt.display()
        )));
    }
    let branch = run(wt, &["symbolic-ref", "-q", "--short", "HEAD"])
        .map(|b| b.trim().to_string())
        .map_err(|_| Error::Git(format!("{} is not on a branch", wt.display())))?;
    super::check_names(&branch, "HEAD")?;
    copy_branch(&source, store, &branch)?;

    register_in_place(store, wt, &branch, Some(&old))?;
    // The clone it came from would still count the branch as checked out,
    // and refuse to delete or check it out, until this goes.
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Copy `branch` from `source` into `store`, with where it pushes to.
fn copy_branch(source: &Path, store: &Path, branch: &str) -> Result<()> {
    let from = source.to_string_lossy();
    run(
        store,
        &["fetch", "--quiet", "--no-tags", "--", &from, &format!("+refs/heads/{branch}:refs/heads/{branch}")],
    )?;
    copy_upstream(source, store, branch);
    Ok(())
}

/// Where `branch` pushes to, if it was pushed from `source`.
fn copy_upstream(source: &Path, store: &Path, branch: &str) {
    for key in ["remote", "merge"] {
        let name = format!("branch.{branch}.{key}");
        if let Ok(v) = run(source, &["config", "--get", &name]) {
            let _ = run(store, &["config", &name, v.trim()]);
        }
    }
}

/// Bring `branch` over from the user's clone when only the clone has it:
/// a branch started there, before or after the copy was made. A task on
/// it then goes on from the user's commits, as it did when worktrees were
/// cut from the clone itself; without this it quietly started again from
/// the base. The copy's own branch, when it has one, is the task's and
/// wins.
pub fn take_branch_from_clone(store: &Path, clone: &Path, branch: &str) -> Result<()> {
    super::check_names(branch, "HEAD")?;
    if super::branch_exists(store, branch) || !super::branch_exists(clone, branch) {
        return Ok(());
    }
    copy_branch(clone, store, branch)
}

/// Link a worktree whose registration is gone back into `store`, at the
/// commit it was last seen on, leaving every file where it is.
///
/// For a folder cut off by its clone being deleted or cloned again. The
/// branch is made at `head` unless `store` already has it. Nothing staged
/// survives — that lived in the lost registration — so the index is
/// rebuilt from the commit and every change reads as unstaged.
pub fn relink_worktree(store: &Path, wt: &Path, branch: &str, head: &str) -> Result<()> {
    if registration(wt).is_some() {
        return Err(Error::Git(format!("{} is still linked; nothing to repair", wt.display())));
    }
    super::check_names(branch, "HEAD")?;
    super::commit_id(head)?;
    if !super::branch_exists(store, branch) {
        run(store, &["cat-file", "-e", &format!("{head}^{{commit}}")])
            .map_err(|_| Error::Git(format!("the app's copy does not have commit {head}")))?;
        run(store, &["branch", "--", branch, head])?;
    }
    register_in_place(store, wt, branch, None)?;
    run(wt, &["reset", "--quiet"])?;
    Ok(())
}

/// Whether `wt` holds a repository of its own rather than a link to one.
pub fn is_own_clone(wt: &Path) -> bool {
    wt.join(".git").is_dir()
}

/// Link a task folder that has become a clone of its own back into
/// `store`, on `branch`, leaving every file where it is (TASK-12).
///
/// Something cloned the repository again in place of a task's worktree.
/// Git could still read the folder, but the app's copy no longer knew it,
/// and every launch reported it as "not linked to any repository". Done
/// only when it loses nothing, since the clone's own `.git` goes: its
/// branch comes into the copy first, and anything the copy could not keep
/// (staging, a stash, another branch's commits, a merge under way) leaves
/// the folder as it is, with the reason.
pub fn reclaim_clone(store: &Path, wt: &Path, branch: &str) -> Result<()> {
    let refuse = |why: String| -> Result<()> {
        Err(Error::Git(format!("{} is a clone of its own, and was left as it is: {why}", wt.display())))
    };
    if !is_own_clone(wt) {
        return Err(Error::Git(format!("{} is not a clone of its own", wt.display())));
    }
    super::check_names(branch, "HEAD")?;
    match (super::origin_url(wt), super::origin_url(store)) {
        (Some(a), Some(b)) if super::same_remote(&a, &b) => {}
        _ => return refuse("it does not fetch from where the app's copy does".into()),
    }
    if super::in_progress(wt).is_some() {
        return refuse("a merge or rebase is under way".into());
    }
    let on = run(wt, &["symbolic-ref", "-q", "--short", "HEAD"]).map(|b| b.trim().to_string()).unwrap_or_default();
    if on != branch {
        let on = if on.is_empty() { "no branch".to_string() } else { on };
        return refuse(format!("it is on {on}, not the task's branch {branch}"));
    }
    if super::status(wt)?.staged > 0 {
        return refuse("something is staged; commit or unstage it".into());
    }
    if run(wt, &["rev-parse", "-q", "--verify", "refs/stash"]).is_ok() {
        return refuse("it has a stash".into());
    }
    // Its other branches go with its `.git`, so each must be in the copy already.
    let tips = run(wt, &["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads"])?;
    for (name, sha) in tips.lines().filter_map(|l| l.split_once(' ')) {
        let kept = name == branch
            || (run(store, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
                && run(store, &["for-each-ref", "--count=1", "--contains", sha, "refs"])
                    .is_ok_and(|o| !o.trim().is_empty()));
        if !kept {
            return refuse(format!("its branch {name} has commits the app's copy does not"));
        }
    }

    // The branch comes across first, and only as a fast-forward of the
    // copy's: commits made in the folder before it was cloned again stay.
    let taken = format!("refs/villain-reclaiming/{branch}");
    let from = wt.to_string_lossy();
    run(store, &["fetch", "--quiet", "--no-tags", "--", &from, &format!("+refs/heads/{branch}:{taken}")])?;
    let moved = (|| -> Result<()> {
        let new = run(store, &["rev-parse", "--verify", &taken])?.trim().to_string();
        let local = format!("refs/heads/{branch}");
        match run(store, &["rev-parse", "-q", "--verify", &local]) {
            Ok(old) if !super::is_ancestor(store, old.trim(), &new) => {
                refuse(format!("the app's copy has commits on {branch} that it does not"))
            }
            Ok(old) => run(store, &["update-ref", &local, &new, old.trim()]).map(|_| ()),
            Err(_) => run(store, &["branch", "--", branch, &new]).map(|_| ()),
        }
    })();
    let _ = run(store, &["update-ref", "-d", &taken]);
    moved?;
    copy_upstream(wt, store, branch);

    // The copy still lists the folder from before, holding the branch there.
    if let Some(stale) = registration_for(store, wt) {
        std::fs::remove_dir_all(stale)?;
    }
    // Moved out of the folder rather than deleted until the link is in
    // place, so a failure can put it back, and a crash leaves nothing in
    // the worktree to be committed (DISK-1).
    let own = wt.join(".git");
    let aside_dir = store.join("villain-replaced");
    std::fs::create_dir_all(&aside_dir)?;
    let aside = aside_dir.join(uuid::Uuid::new_v4().to_string());
    std::fs::rename(&own, &aside)?;
    if let Err(e) = register_in_place(store, wt, branch, None) {
        let _ = std::fs::rename(&aside, &own);
        return Err(e);
    }
    // Nothing was staged, so an index rebuilt from the commit is the one it had.
    run(wt, &["reset", "--quiet"])?;
    let _ = std::fs::remove_dir_all(&aside);
    let _ = std::fs::remove_dir(&aside_dir);
    Ok(())
}

/// The registration `store` still keeps for the folder `wt`, if any.
fn registration_for(store: &Path, wt: &Path) -> Option<PathBuf> {
    let want = std::fs::canonicalize(wt).ok()?;
    std::fs::read_dir(store.join("worktrees")).ok()?.flatten().map(|e| e.path()).find(|admin| {
        std::fs::read_to_string(admin.join("gitdir"))
            .ok()
            .and_then(|g| Path::new(g.trim()).parent().and_then(|d| std::fs::canonicalize(d).ok()))
            .is_some_and(|d| d == want)
    })
}

/// Register `wt` in `store` on `branch` without checking anything out, and
/// point the two at each other. The files in `wt` are never touched.
fn register_in_place(store: &Path, wt: &Path, branch: &str, carry: Option<&Path>) -> Result<()> {
    let folder = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let parent = wt.parent().map(folder).unwrap_or_default();
    let name = format!("{parent}-{}", folder(wt))
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>();

    // `git worktree add` names the registration after the folder it makes,
    // so make an empty one with the name wanted, somewhere out of the way.
    let staging = store.join("villain-adopting");
    let mut tmp = staging.join(&name);
    let mut n = 2;
    while tmp.exists() || store.join("worktrees").join(folder(&tmp)).exists() {
        tmp = staging.join(format!("{name}-{n}"));
        n += 1;
    }
    std::fs::create_dir_all(&staging)?;
    let tmp_s = tmp.to_string_lossy().to_string();
    run(store, &["worktree", "add", "--no-checkout", "--quiet", &tmp_s, branch])?;

    let linked = (|| -> Result<()> {
        let link = std::fs::read_to_string(tmp.join(".git"))?;
        let admin = PathBuf::from(
            link.strip_prefix("gitdir:")
                .ok_or_else(|| Error::Git("unexpected worktree link".into()))?
                .trim(),
        );
        if let Some(old) = carry {
            for file in ["index", "logs/HEAD"] {
                if old.join(file).is_file() {
                    if let Some(dir) = admin.join(file).parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::copy(old.join(file), admin.join(file))?;
                }
            }
        }
        std::fs::write(admin.join("gitdir"), format!("{}\n", wt.join(".git").display()))?;
        // The switch itself: one rename, so a git running in the folder at
        // that moment reads one link or the other, never half of one.
        let next = wt.join(".git.villain-next");
        std::fs::write(&next, &link)?;
        std::fs::rename(&next, wt.join(".git"))?;
        Ok(())
    })();

    let _ = std::fs::remove_file(tmp.join(".git"));
    let _ = std::fs::remove_dir(&tmp);
    let _ = std::fs::remove_dir(&staging);
    if linked.is_err() {
        // Nothing points at the new registration yet; let git forget it.
        let _ = run(store, &["worktree", "prune"]);
    }
    linked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A remote, and the user's clone of it with its own identity set only
    /// in the clone — the setting a copy most needs to keep.
    fn user_clone(root: &Path) -> (PathBuf, PathBuf) {
        let remote = root.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        run(&remote, &["init", "-q", "-b", "main", "--bare"]).unwrap();
        let clone = root.join("clone");
        run(root, &["clone", "-q", remote.to_str().unwrap(), clone.to_str().unwrap()]).unwrap();
        run(&clone, &["config", "user.email", "me@work.example"]).unwrap();
        run(&clone, &["config", "user.name", "Me"]).unwrap();
        std::fs::write(clone.join("a.txt"), "one\n").unwrap();
        run(&clone, &["add", "-A"]).unwrap();
        run(&clone, &["commit", "-qm", "init"]).unwrap();
        run(&clone, &["push", "-q", "-u", "origin", "main"]).unwrap();
        (remote, clone)
    }

    fn head(dir: &Path) -> String {
        run(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    #[test]
    fn a_copy_fetches_from_the_remote_and_outlives_the_clone() {
        let root = sandbox();
        let (remote, clone) = user_clone(&root);
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        assert!(is_store_of(&store, &clone));
        let url = run(&store, &["config", "--get", "remote.origin.url"]).unwrap();
        assert_eq!(url.trim(), remote.to_str().unwrap(), "fetches go to the remote, not the clone");
        assert!(super::super::branch_exists(&store, "main"));
        assert!(run(&store, &["rev-parse", "--verify", "refs/remotes/origin/main"]).is_ok());

        let wt = root.join("task/clone");
        super::super::add_worktree(&store, &wt, "task", "main").unwrap();
        std::fs::remove_dir_all(&clone).unwrap();
        std::fs::write(wt.join("a.txt"), "two\n").unwrap();
        run(&wt, &["commit", "-qam", "work"]).unwrap();
        let author = run(&wt, &["log", "-1", "--format=%ae"]).unwrap();
        assert_eq!(author.trim(), "me@work.example", "the clone's own identity came across");
        assert_eq!(super::super::status(&wt).unwrap().branch, "task");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_worktree_moves_into_the_copy_with_its_commits_and_its_staging() {
        let root = sandbox();
        let (_remote, clone) = user_clone(&root);
        let wt = root.join("task/clone");
        super::super::add_worktree(&clone, &wt, "task", "main").unwrap();
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();

        // Made after the copy, and never pushed: only the clone has it.
        std::fs::write(wt.join("a.txt"), "committed\n").unwrap();
        run(&wt, &["commit", "-qam", "unpushed"]).unwrap();
        let unpushed = head(&wt);
        std::fs::write(wt.join("staged.txt"), "s\n").unwrap();
        run(&wt, &["add", "staged.txt"]).unwrap();
        std::fs::write(wt.join("a.txt"), "edited\n").unwrap();
        std::fs::write(wt.join("new.txt"), "n\n").unwrap();

        adopt_worktree(&store, &wt).unwrap();
        assert!(belongs_to(&wt, &store));
        assert_eq!(head(&wt), unpushed, "the unpushed commit came across");
        let st = super::super::status(&wt).unwrap();
        assert_eq!((st.staged, st.unstaged, st.untracked), (1, 1, 1), "staging kept as it was");

        // The clone lets go of the branch, and can be deleted outright.
        run(&clone, &["branch", "-D", "task"]).unwrap();
        std::fs::remove_dir_all(&clone).unwrap();
        assert_eq!(super::super::status(&wt).unwrap().staged, 1);
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "edited\n");
        assert!(adopt_worktree(&store, &wt).is_ok(), "adopting twice changes nothing");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_cut_off_from_its_clone_is_relinked_at_its_last_commit() {
        let root = sandbox();
        let (_remote, clone) = user_clone(&root);
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        let wt = root.join("task/clone");
        super::super::add_worktree(&clone, &wt, "task", "main").unwrap();
        let last = head(&wt);
        std::fs::write(wt.join("a.txt"), "uncommitted\n").unwrap();

        // The clone is cloned again: the registration goes, the folder stays.
        std::fs::remove_dir_all(clone.join(".git/worktrees")).unwrap();
        assert!(super::super::unlinked(&wt).is_some());

        relink_worktree(&store, &wt, "task", &last).unwrap();
        assert!(super::super::unlinked(&wt).is_none());
        assert_eq!(head(&wt), last);
        let st = super::super::status(&wt).unwrap();
        assert_eq!((st.branch.as_str(), st.unstaged), ("task", 1), "the edit reads as an edit");
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "uncommitted\n");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_branch_started_in_the_clone_after_the_copy_goes_on_in_a_task() {
        let root = sandbox();
        let (_remote, clone) = user_clone(&root);
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        run(&clone, &["switch", "-q", "-c", "ACME-1"]).unwrap();
        std::fs::write(clone.join("a.txt"), "started by hand\n").unwrap();
        run(&clone, &["commit", "-qam", "started"]).unwrap();
        let started = head(&clone);

        take_branch_from_clone(&store, &clone, "ACME-1").unwrap();
        let wt = root.join("task/clone");
        super::super::add_worktree(&store, &wt, "ACME-1", "main").unwrap();
        assert_eq!(head(&wt), started, "the task goes on from the clone's commit");

        // Once the copy has it, the task's own commits are what count.
        std::fs::write(wt.join("a.txt"), "the task's work\n").unwrap();
        run(&wt, &["commit", "-qam", "task"]).unwrap();
        let task = head(&wt);
        take_branch_from_clone(&store, &clone, "ACME-1").unwrap();
        assert_eq!(head(&wt), task);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_worktree_mid_merge_is_left_where_it_is() {
        let root = sandbox();
        let (_remote, clone) = user_clone(&root);
        let wt = root.join("task/clone");
        super::super::add_worktree(&clone, &wt, "task", "main").unwrap();
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        let git_dir = run(&wt, &["rev-parse", "--git-dir"]).unwrap();
        let merge_head = wt.join(git_dir.trim()).join("MERGE_HEAD");
        std::fs::write(&merge_head, format!("{}\n", head(&wt))).unwrap();
        assert!(adopt_worktree(&store, &wt).is_err());
        assert!(!belongs_to(&wt, &store));
        std::fs::remove_dir_all(&root).ok();
    }

    /// A task's worktree of the copy, with one commit pushed.
    fn pushed_task(root: &Path, store: &Path) -> PathBuf {
        let wt = root.join("task/clone");
        super::super::add_worktree(store, &wt, "task", "main").unwrap();
        std::fs::write(wt.join("a.txt"), "pushed\n").unwrap();
        run(&wt, &["commit", "-qam", "pushed"]).unwrap();
        run(&wt, &["push", "-q", "origin", "task"]).unwrap();
        wt
    }

    /// The folder replaced by a fresh clone of the remote, on the same branch.
    fn clone_in_place(root: &Path, remote: &Path, wt: &Path) {
        std::fs::remove_dir_all(wt).unwrap();
        run(root, &["clone", "-q", "-b", "task", remote.to_str().unwrap(), wt.to_str().unwrap()]).unwrap();
        run(wt, &["config", "user.email", "me@work.example"]).unwrap();
        run(wt, &["config", "user.name", "Me"]).unwrap();
    }

    #[test]
    fn a_task_folder_cloned_again_in_place_is_linked_back_with_its_commits_and_edits() {
        let root = sandbox();
        let (remote, clone) = user_clone(&root);
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        let wt = pushed_task(&root, &store);
        clone_in_place(&root, &remote, &wt);
        // Worked on there: a commit never pushed, an edit, a new file.
        std::fs::write(wt.join("a.txt"), "unpushed\n").unwrap();
        run(&wt, &["commit", "-qam", "unpushed"]).unwrap();
        let last = head(&wt);
        std::fs::write(wt.join("a.txt"), "edited\n").unwrap();
        std::fs::write(wt.join("new.txt"), "new\n").unwrap();
        assert!(is_own_clone(&wt) && !belongs_to(&wt, &store));

        reclaim_clone(&store, &wt, "task").unwrap();
        assert!(!is_own_clone(&wt) && belongs_to(&wt, &store));
        assert_eq!(head(&wt), last);
        assert_eq!(run(&store, &["rev-parse", "refs/heads/task"]).unwrap().trim(), last, "the unpushed commit is in the copy");
        let st = super::super::status(&wt).unwrap();
        assert_eq!((st.branch.as_str(), st.staged, st.unstaged, st.untracked), ("task", 0, 1, 1));
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "edited\n");
        let listed = run(&store, &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(listed.lines().filter(|l| l.starts_with("worktree ")).count(), 2, "the copy, and the folder once");
        assert!(!store.join("villain-replaced").exists(), "the old .git is gone once the link holds");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_cloned_again_is_left_alone_while_linking_it_would_lose_something() {
        let root = sandbox();
        let (remote, clone) = user_clone(&root);
        let store = root.join("store/clone.git");
        create_store(&clone, &store).unwrap();
        let wt = pushed_task(&root, &store);
        // Committed in the worktree before it was cloned again, never pushed.
        std::fs::write(wt.join("a.txt"), "only in the copy\n").unwrap();
        run(&wt, &["commit", "-qam", "only in the copy"]).unwrap();
        let kept = head(&wt);
        clone_in_place(&root, &remote, &wt);
        let refused = |why: &str| {
            let e = reclaim_clone(&store, &wt, "task").unwrap_err().to_string();
            assert!(e.contains(why), "{e}");
            assert!(is_own_clone(&wt), "left as it is");
        };

        run(&wt, &["switch", "-q", "-c", "other"]).unwrap();
        refused("not the task's branch task");
        run(&wt, &["switch", "-q", "task"]).unwrap();

        std::fs::write(wt.join("b.txt"), "staged\n").unwrap();
        run(&wt, &["add", "b.txt"]).unwrap();
        refused("something is staged");
        run(&wt, &["reset", "-q"]).unwrap();

        run(&wt, &["stash", "push", "-q", "-u"]).unwrap();
        refused("it has a stash");
        run(&wt, &["stash", "pop", "-q"]).unwrap();

        run(&wt, &["switch", "-q", "other"]).unwrap();
        run(&wt, &["commit", "-q", "--allow-empty", "-m", "only on other"]).unwrap();
        run(&wt, &["switch", "-q", "task"]).unwrap();
        refused("its branch other has commits the app's copy does not");
        run(&wt, &["branch", "-q", "-D", "other"]).unwrap();

        refused("the app's copy has commits on task that it does not");
        assert_eq!(run(&store, &["rev-parse", "refs/heads/task"]).unwrap().trim(), kept, "the copy's commit stays");
        assert!(run(&store, &["rev-parse", "-q", "--verify", "refs/villain-reclaiming/task"]).is_err());
        assert_eq!(std::fs::read_to_string(wt.join("b.txt")).unwrap(), "staged\n");
        std::fs::remove_dir_all(&root).ok();
    }
}
