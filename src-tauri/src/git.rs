//! Thin wrapper over the `git` CLI. The CLI is used rather than libgit2
//! because worktree semantics, hooks and credential helpers all come free.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        // The status poll runs beside agents committing in the same worktree.
        // Without this, `git status` refreshes the index as a side effect and
        // takes index.lock to do it, and an agent's own `git commit` landing
        // in that moment fails with "index.lock: File exists".
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Nobody is at a terminal to answer. A dev build started from one let
        // a fetch ask for a password there, and the thread waited on it.
        .env("GIT_TERMINAL_PROMPT", "0");
    // A GUI app's PATH is /usr/bin:/bin. Hooks run with git's environment, so
    // a husky or lint-staged hook that needs node — or git-lfs on checkout —
    // failed here while working in a terminal.
    if let Some(path) = crate::shellenv::path_if_ready() {
        cmd.env("PATH", path);
    }
    cmd
}

/// Private on purpose: a hand-written argument list elsewhere skips the flags
/// this file adds against the user's own git config. Add a named function.
fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = command(dir, args)
        .output()
        .map_err(|e| {
            // The common cause is a missing working directory, not a missing
            // git. Saying "failed to run git: No such file or directory" sends
            // people looking for the wrong problem.
            if !dir.is_dir() {
                Error::Git(format!("{} no longer exists", dir.display()))
            } else {
                Error::Git(format!("failed to run git in {}: {e}", dir.display()))
            }
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(Error::Git(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr
        }));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Why a worktree folder that is still on disk is not one git can use, when
/// the link its `.git` file names is gone.
///
/// Deleting or re-cloning the main repository takes its `.git/worktrees/`
/// with it. Every git command in the folder then fails with "not a git
/// repository", and the Diff view read that as "No changes yet": seven task
/// folders looked empty while all their files were still there. No
/// subprocess, so it can run on every poll.
pub fn unlinked(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".git")).ok()?;
    let target = dir.join(text.strip_prefix("gitdir:")?.trim());
    (!target.exists()).then(|| {
        format!(
            "Git no longer knows this worktree: {} is gone, usually because the repository was deleted or cloned again",
            target.display()
        )
    })
}

/// For other modules' test fixtures, which need a real repository to stand on.
#[cfg(test)]
pub fn run_for_tests(dir: &Path, args: &[&str]) -> Result<String> {
    run(dir, args)
}

pub fn repo_root(dir: &Path) -> Result<String> {
    Ok(run(dir, &["rev-parse", "--show-toplevel"])?.trim().to_string())
}

pub fn current_branch(dir: &Path) -> Result<String> {
    Ok(run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string())
}

/// The repo's default branch, best-effort: origin/HEAD, then main, then master.
pub fn default_branch(dir: &Path) -> String {
    if let Ok(out) = run(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(name) = out.trim().strip_prefix("origin/") {
            return name.to_string();
        }
    }
    for candidate in ["main", "master"] {
        if run(dir, &["rev-parse", "--verify", "--quiet", candidate]).is_ok() {
            return candidate.to_string();
        }
    }
    current_branch(dir).unwrap_or_else(|_| "main".into())
}

/// The branches a pull request could be opened against, newest first.
///
/// Read from the worktree rather than asked of GitHub: it is instant, it works
/// with no network, and a remote-tracking ref is exactly what a base has to
/// name anyway. A branch pushed by somebody else since the last fetch will not
/// be in here, which is why the field that uses this still takes anything
/// typed into it.
pub fn remote_branches(dir: &Path) -> Result<Vec<String>> {
    let out = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=-committerdate",
            "refs/remotes/origin",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("origin/"))
        .filter(|b| *b != "HEAD")
        .map(str::to_string)
        .collect())
}

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    run(
        dir,
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    )
    .is_ok()
}

#[cfg(test)]
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub locked: bool,
}

/// `git worktree list`, parsed. Only the tests read it back today.
#[cfg(test)]
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
            });
        } else if let Some(e) = cur.as_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                e.head = Some(head.to_string());
            } else if let Some(branch) = line.strip_prefix("branch ") {
                e.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
            } else if line.starts_with("locked") {
                e.locked = true;
            }
        }
    }
    if let Some(e) = cur {
        entries.push(e);
    }
    Ok(entries)
}

/// A start-point that `git worktree add -b` can resolve.
///
/// Prefer the remote-tracking tip over a local branch of the same name: the
/// main checkout on disk is often weeks behind `origin/main`, and cutting a
/// new worktree from that local tip is how new tasks quietly start outdated.
/// Falls back to the local name when there is no remote (tests, fresh clones
/// that have never fetched, repos with no `origin`).
fn resolve_base(repo: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    if run(repo, &["rev-parse", "--verify", "--quiet", &remote]).is_ok() {
        return remote;
    }
    if run(repo, &["rev-parse", "--verify", "--quiet", base]).is_ok() {
        return base.to_string();
    }
    // Last resort: hand git whatever was asked and let it explain the miss.
    base.to_string()
}

/// Bring `origin/<base>` up to date before we cut from it.
///
/// Best-effort: offline laptops and repos without an `origin` still create
/// the worktree from whatever tip they already have. A failure here is not a
/// reason to refuse the task — starting slightly stale beats not starting.
fn fetch_base(repo: &Path, base: &str) {
    let _ = fetch_tracking(repo, base);
}

/// Fetch `origin/<name>` itself, whatever the clone's configured refspec.
///
/// `fetch origin <name>` writes only FETCH_HEAD in a `--single-branch` clone,
/// so the tracking ref stayed where it was: Update from base said "up to
/// date" against a stale base, and a task branch never got the tracking ref a
/// push lease is read from.
fn fetch_tracking(repo: &Path, name: &str) -> Result<String> {
    run(
        repo,
        &["fetch", "--quiet", "origin", &format!("+refs/heads/{name}:refs/remotes/origin/{name}")],
    )
}

/// Fetch the task branch before rewriting it, and forget it when the remote
/// no longer has it.
///
/// A PR merged with "delete branch" leaves `origin/<branch>` behind in this
/// clone, and a failed fetch does not remove it: the stale tip became the
/// lease, and every push after the rebase was refused as "someone else
/// pushed". Asked with `ls-remote`, whose exit status — 2 for no such ref —
/// says so without reading a message that may be in another language.
pub fn fetch_branch(dir: &Path, branch: &str) {
    if branch.trim().is_empty() || branch.starts_with('-') || fetch_tracking(dir, branch).is_ok() {
        return;
    }
    let gone = command(dir, &["ls-remote", "--exit-code", "--heads", "origin", &format!("refs/heads/{branch}")])
        .output()
        .is_ok_and(|o| o.status.code() == Some(2));
    if gone {
        let _ = run(dir, &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")]);
    }
}

/// Both names reach git where it still reads options: `fetch origin <base>`
/// took `--upload-pack=<command>` as one and ran the command. The base can
/// come from an agent through the MCP server, so this is the door, not a
/// typo check.
fn check_names(branch: &str, base: &str) -> Result<()> {
    for (what, name) in [("branch", branch), ("base", base)] {
        if name.trim().is_empty() || name.starts_with('-') {
            return Err(Error::Git(format!("{name:?} is not a usable {what} name")));
        }
    }
    Ok(())
}

/// `fetch_base` for several repositories at once.
///
/// A task spanning repos fetched each base in turn, a network round trip
/// apiece, before its worktree could be made — so a four-repo task waited on
/// four fetches in a row. They are independent and mostly waiting on the
/// network, so they overlap.
pub fn fetch_bases(targets: &[(std::path::PathBuf, String)]) {
    std::thread::scope(|scope| {
        for chunk in targets.chunks(8) {
            let handles: Vec<_> = chunk
                .iter()
                .filter(|(_, base)| !base.trim().is_empty() && !base.starts_with('-'))
                .map(|(repo, base)| scope.spawn(move || fetch_base(repo, base)))
                .collect();
            for h in handles {
                let _ = h.join();
            }
        }
    });
}

/// Create a worktree at `path`. Creates `branch` from `base` when it does not
/// already exist, otherwise checks the existing branch out.
/// Returns the commit the worktree starts at, so the diff has a fixed point.
pub fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<String> {
    check_names(branch, base)?;
    fetch_base(repo, base);
    add_worktree_fetched(repo, path, branch, base)
}

/// `add_worktree` for a base already brought up to date by `fetch_bases`.
pub fn add_worktree_fetched(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<String> {
    check_names(branch, base)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_s = path.to_string_lossy().to_string();
    let start = resolve_base(repo, base);
    // A post-checkout hook that fails — git-lfs without lfs, a husky install
    // step — fails the add but leaves the worktree registered and on disk.
    // Left there, the rollback could not delete the branch ("used by
    // worktree") and every retry failed with "already checked out".
    let existed = path.exists();
    let add = |args: &[&str]| {
        run(repo, args).inspect_err(|_| {
            if !existed && path.exists() {
                let _ = run(repo, &["worktree", "remove", "--force", &path_s]);
                let _ = run(repo, &["worktree", "prune"]);
            }
        })
    };

    if branch_exists(repo, branch) {
        add(&["worktree", "add", &path_s, branch])?;
        // An existing branch has its own history; what it forked from is the
        // best available answer, not wherever the base happens to be today.
        Ok(run(repo, &["merge-base", &start, branch])
            .or_else(|_| run(repo, &["rev-parse", branch]))
            .map(|s| s.trim().to_string())
            .unwrap_or_default())
    } else {
        add(&["worktree", "add", "-b", branch, &path_s, &start])?;
        Ok(run(path, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default())
    }
}

/// Whether `a` is `b` or behind it. False when either is unknown here.
pub fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    !a.starts_with('-') && !b.starts_with('-') && run(dir, &["merge-base", "--is-ancestor", a, b]).is_ok()
}

/// Delete a local branch, for undoing one this app just created.
pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    run(repo, &["branch", "-D", "--", branch]).map(|_| ())
}

pub fn remove_worktree(repo: &Path, path: &str, force: bool) -> Result<()> {
    // git's own "is it clean" check honours `status.showUntrackedFiles=no`,
    // and with that set it removed a worktree holding new files the agent had
    // not committed — silently, without --force.
    let mut args = vec!["-c", "status.showUntrackedFiles=normal", "worktree", "remove"];
    // A worktree with submodules is always refused without --force, however
    // clean. Our own status counts a submodule's changes, so when it finds
    // none, forcing takes nothing that git would have kept.
    let clean_with_submodules = !force
        && Path::new(path).join(".gitmodules").is_file()
        && status(Path::new(path)).is_ok_and(|s| s.dirty_files == 0);
    if force || clean_with_submodules {
        args.push("--force");
    }
    args.push(path);
    run(repo, &args)?;
    let _ = run(repo, &["worktree", "prune"]);
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    /// Distinct paths with any uncommitted change — what the sidebar badge
    /// wants. Cheaper than a second `git diff`/`ls-files` pass on every poll.
    pub dirty_files: u32,
}

pub fn status(dir: &Path) -> Result<WorktreeStatus> {
    // Spelled out, since the user's config applies: with
    // `status.showUntrackedFiles=no` a worktree holding only new files read
    // as clean, and was removed with them.
    let out = run(
        dir,
        &["status", "--porcelain=v2", "--branch", "--untracked-files=normal", "--ignore-submodules=none"],
    )?;
    let mut s = WorktreeStatus {
        branch: String::new(),
        ahead: 0,
        behind: 0,
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicted: 0,
        dirty_files: 0,
    };

    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            s.branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for tok in rest.split_whitespace() {
                let n: u32 = tok[1..].parse().unwrap_or(0);
                match tok.as_bytes().first() {
                    Some(b'+') => s.ahead = n,
                    Some(b'-') => s.behind = n,
                    _ => {}
                }
            }
        } else if line.starts_with("? ") {
            s.untracked += 1;
            s.dirty_files += 1;
        } else if line.starts_with("u ") {
            s.conflicted += 1;
            s.dirty_files += 1;
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            s.dirty_files += 1;
            // Field 2 is the two-character XY staged/unstaged code.
            if let Some(xy) = line.split_whitespace().nth(1) {
                let b = xy.as_bytes();
                if b.first() != Some(&b'.') {
                    s.staged += 1;
                }
                if b.get(1) != Some(&b'.') {
                    s.unstaged += 1;
                }
            }
        }
    }
    Ok(s)
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedFile {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    /// "tracked" for anything git already knows about, "untracked" otherwise.
    pub origin: String,
}

/// Untracked files larger than this are listed but not read for a line count.
const UNTRACKED_COUNT_LIMIT: u64 = 4 * 1024 * 1024;

/// Every file that differs from `base`, including uncommitted and untracked work.
pub fn changed_files(
    dir: &Path,
    base: &str,
    base_commit: Option<&str>,
    scope: Scope,
) -> Result<Vec<ChangedFile>> {
    let mut files: Vec<ChangedFile> = Vec::new();

    let merge_base = compare_against(dir, base, base_commit, scope);

    // Committed on this branch since the baseline, plus everything in the tree.
    let numstat = run(dir, &["diff", "--numstat", "-z", &merge_base])?;
    files.extend(parse_numstat(&numstat));

    for path in run(dir, &["ls-files", "-z", "--others", "--exclude-standard"])?.split('\0') {
        if path.is_empty() {
            continue;
        }
        // Counting lines means reading the whole file, and this runs on every
        // refresh of the list. An untracked dump or archive of tens of
        // megabytes is not something anyone reviews line by line, so past a
        // few megabytes it is listed as binary and not read at all.
        let added = untracked_text(&dir.join(path)).map(|c| c.lines().count() as u32);
        files.push(ChangedFile {
            path: path.to_string(),
            additions: added.unwrap_or(0),
            deletions: 0,
            binary: added.is_none(),
            origin: "untracked".into(),
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// An untracked file's contents, if it is text worth showing as a diff.
///
/// None for anything too big to read on every refresh, and for anything that
/// is not text — the way git itself decides, by a NUL near the start. A small
/// PNG used to be read as a string, fail, and leave the previous file's patch
/// on screen beside an error.
fn untracked_text(path: &Path) -> Option<String> {
    let small = std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() <= UNTRACKED_COUNT_LIMIT)
        .unwrap_or(false);
    if !small {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// How many files differ from the baseline, committed or not.
///
/// The same set `changed_files` lists, counted without building it: no file
/// is opened, which is what makes this safe to call for every repository of
/// every task on the pull-request watch's timer.
pub fn changed_count(
    dir: &Path,
    base: &str,
    base_commit: Option<&str>,
    scope: Scope,
) -> usize {
    let merge_base = compare_against(dir, base, base_commit, scope);
    changed_count_from(dir, &merge_base)
}

/// `changed_count` measured from any revision: files that differ from it,
/// committed or not, plus untracked ones.
pub fn changed_count_from(dir: &Path, rev: &str) -> usize {
    let tracked = run(dir, &["diff", "--name-only", rev])
        .map(|o| o.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    let untracked = run(dir, &["ls-files", "--others", "--exclude-standard"])
        .map(|o| o.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    tracked + untracked
}

/// Whether `rev` names a commit this repository has.
pub fn has_commit(dir: &Path, rev: &str) -> bool {
    !rev.is_empty() && !rev.starts_with('-') && run(dir, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_ok()
}

/// What the diff is measured against.
///
/// Two honest answers to "what changed", and which one is wanted depends on
/// why you are looking. Uncommitted is what you are holding right now and is
/// what `git status` shows. Branch is everything since the worktree was made,
/// which is what a reviewer eventually sees — and is only meaningful when the
/// branch was actually cut for this work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Everything not yet committed, plus untracked files.
    #[default]
    Uncommitted,
    /// Everything since the branch point, committed or not.
    Branch,
}

/// The revision a scope compares against: HEAD for uncommitted work, the
/// branch point for the branch as a whole.
pub fn compare_against(
    dir: &Path,
    base: &str,
    base_commit: Option<&str>,
    scope: Scope,
) -> String {
    match scope {
        Scope::Uncommitted => "HEAD".to_string(),
        Scope::Branch => baseline(dir, base, base_commit),
    }
}

/// The commit a branch is measured against.
///
/// A recorded branch point is used as-is: it is the one answer that does not
/// change when the base branch moves, when history is rebased onto something
/// else, or when a merge brings in work from elsewhere — all of which
/// otherwise turn up in the diff as though this branch had done them.
///
/// Without one — worktrees made before this was recorded — fall back to the
/// merge base with the base branch, preferring the remote's copy.
pub fn baseline(dir: &Path, base: &str, base_commit: Option<&str>) -> String {
    if let Some(commit) = base_commit.map(str::trim).filter(|c| !c.is_empty()) {
        // Only if the branch actually grew from it. A rewritten history may
        // not have it at all; and a merge of the base that was then aborted
        // leaves the point it was heading for recorded but never reached —
        // measured from there, the diff ran backwards through the base's own
        // work. The same one call as asking whether the commit exists.
        if run(dir, &["merge-base", "--is-ancestor", commit, "HEAD"]).is_ok() {
            return commit.to_string();
        }
    }
    run(dir, &["merge-base", &format!("origin/{base}"), "HEAD"])
        .or_else(|_| run(dir, &["merge-base", base, "HEAD"]))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| base.to_string())
}

/// What a branch amounts to, so a diff can say plainly what it is showing.
#[derive(Debug, Clone, Serialize)]
pub struct BranchFacts {
    /// The commit the branch is measured from, abbreviated.
    pub baseline: String,
    /// Whether that is the branch point this worktree recorded when it was
    /// made, or a merge base worked out afterwards. The two part company as
    /// soon as the base branch moves, and which one is in use decides whether
    /// the file list can be trusted to be this branch's own work.
    pub baseline_recorded: bool,
    /// Commits on this branch since that point.
    pub commits: u32,
    /// Commits not yet on the remote branch.
    pub unpushed: u32,
    /// Whether the branch exists on the remote at all.
    pub has_remote: bool,
}

/// Measure a branch against its base and its remote.
///
/// Every number here is one `git` can answer directly; none of it is inferred
/// from the file list, which is what makes it safe to print beside one.
pub fn branch_facts(
    dir: &Path,
    branch: &str,
    base: &str,
    base_commit: Option<&str>,
) -> BranchFacts {
    let point = baseline(dir, base, base_commit);
    // `baseline` falls back when the recorded commit is not in this worktree,
    // so believing `base_commit` alone would overstate what is known.
    let recorded = base_commit
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .is_some_and(|c| c == point);

    let count = |range: &str| {
        run(dir, &["rev-list", "--count", range])
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    };

    let remote = format!("origin/{branch}");
    let has_remote = run(dir, &["rev-parse", "--verify", "--quiet", &remote]).is_ok();

    BranchFacts {
        commits: count(&format!("{point}..HEAD")),
        unpushed: if has_remote { count(&format!("{remote}..HEAD")) } else { 0 },
        has_remote,
        baseline: run(dir, &["rev-parse", "--short", &point])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| point.chars().take(8).collect()),
        baseline_recorded: recorded,
    }
}

/// Unified patch for one file, against the baseline. Untracked files are
/// rendered as an all-additions patch so the review UI has one code path.
pub fn file_diff(
    dir: &Path,
    base: &str,
    base_commit: Option<&str>,
    scope: Scope,
    path: &str,
) -> Result<String> {
    let merge_base = compare_against(dir, base, base_commit, scope);

    // One call covers modified, staged, renamed and deleted files. Branching on
    // "is it in the index" instead would send deleted files down the untracked
    // path, where reading them from disk fails.
    // `--no-ext-diff`: a global `diff.external` (difftastic, say) answered with
    // its own rendering and no hunks, and the Diff view showed nothing.
    let patch = run(dir, &["diff", "--no-color", "--no-ext-diff", &merge_base, "--", path]).unwrap_or_default();
    if !patch.trim().is_empty() {
        return Ok(patch);
    }

    // Nothing from git means it is untracked; synthesise the addition patch.
    let full = dir.join(path);
    if !full.exists() {
        return Err(Error::Git(format!("{path} is no longer in the worktree")));
    }
    let content = untracked_text(&full).ok_or_else(|| {
        Error::Git(format!("{path} is binary or too large to show as a diff"))
    })?;
    let lines = content.lines().count();
    let mut out = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n");
    for line in content.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// One commit on the branch, for the Diff view's commit picker.
#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    pub subject: String,
}

/// Commits made on this task's branch since the baseline, newest first.
///
/// `--first-parent` is required: without it, every merge of the base branch
/// into this one dumps that branch's history into the picker, which is how a
/// two-commit task ends up listing a hundred commits from elsewhere. Cap keeps
/// a runaway still from drowning the menu.
pub fn commits_since(
    dir: &Path,
    base: &str,
    base_commit: Option<&str>,
) -> Result<Vec<CommitInfo>> {
    let point = baseline(dir, base, base_commit);
    let out = run(
        dir,
        &[
            "log",
            "--first-parent",
            "--format=%H\t%h\t%s",
            "--no-decorate",
            "-n",
            "100",
            &format!("{point}..HEAD"),
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let sha = parts.next()?.to_string();
            let short = parts.next()?.to_string();
            let subject = parts.next()?.to_string();
            if sha.is_empty() {
                return None;
            }
            Some(CommitInfo { sha, short, subject })
        })
        .collect())
}

/// `git diff --stat` from `from` to the worktree, for a summary of the change.
pub fn diff_stat(dir: &Path, from: &str) -> Result<String> {
    run(dir, &["diff", "--no-color", "--no-ext-diff", "--stat", from])
}

/// The patch from `from` to the worktree, leaving out the `excluded` pathspecs
/// (`:!*.lock`). `--no-ext-diff` for the same reason as `file_diff`.
pub fn diff_patch(dir: &Path, from: &str, excluded: &[&str]) -> Result<String> {
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", from];
    if !excluded.is_empty() {
        args.extend_from_slice(&["--", "."]);
        args.extend_from_slice(excluded);
    }
    run(dir, &args)
}

/// Forget worktrees whose folders were removed by hand.
pub fn prune_worktrees(repo: &Path) -> Result<()> {
    run(repo, &["worktree", "prune"]).map(|_| ())
}

/// Files changed in a single commit, with the same shape as `changed_files`.
/// A commit id from the UI, which reaches git where options are still read.
fn commit_id(sha: &str) -> Result<&str> {
    if sha.is_empty() || sha.starts_with('-') {
        return Err(Error::Git(format!("{sha:?} is not a commit")));
    }
    Ok(sha)
}

pub fn commit_files(dir: &Path, sha: &str) -> Result<Vec<ChangedFile>> {
    let sha = commit_id(sha)?;
    // Empty format: we only want the numstat body, not the commit header.
    let numstat = run(
        dir,
        &["show", "--numstat", "-z", "--format=", "--diff-filter=AMDR", sha],
    )?;
    let mut files = parse_numstat(&numstat);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// `--numstat -z` output as changed files, each under the path it has now.
///
/// Read with `-z` because without it git rewrites the paths for a terminal:
/// a rename becomes `src/{old => new}/x.rs` and a name outside ASCII comes
/// back quoted and escaped, `"caf\303\251.txt"`. Neither is a path, so the
/// Diff view could list the file but not open it, and a note on it sent the
/// agent a name that does not exist. With `-z` a rename is its own two
/// fields, old then new, and every name is exactly as it is on disk.
fn parse_numstat(out: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut fields = out.split('\0');
    while let Some(record) = fields.next() {
        // `show` can lead with the newline its empty header ends in.
        let record = record.trim_start_matches('\n');
        let mut parts = record.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            // A rename: the next two fields are where it came from and where
            // it went. The Diff view wants the new one.
            let _from = fields.next();
            fields.next().unwrap_or_default()
        } else {
            path
        };
        if path.is_empty() {
            continue;
        }
        files.push(ChangedFile {
            path: path.to_string(),
            additions: a.parse().unwrap_or(0),
            deletions: d.parse().unwrap_or(0),
            binary: a == "-" || d == "-",
            origin: "tracked".into(),
        });
    }
    files
}

/// Unified patch for one file as it changed in `sha`.
pub fn commit_file_diff(dir: &Path, sha: &str, path: &str) -> Result<String> {
    let sha = commit_id(sha)?;
    // `--pretty=format:` drops the commit header so the UI gets a bare patch,
    // the same shape `file_diff` returns for working-tree changes.
    let patch = run(dir, &["show", "--no-color", "--pretty=format:", sha, "--", path])?;
    Ok(patch)
}

pub fn commit_all(dir: &Path, message: &str) -> Result<String> {
    // `add -A` stages conflict markers as resolved, and the commit then
    // concluded the merge with them in it, or added a commit to the rebase.
    if let Some(busy) = in_progress(dir) {
        return Err(Error::Git(format!(
            "a {} is in progress here — resolve it or abandon it before committing",
            busy.word()
        )));
    }
    run(dir, &["add", "-A"])?;
    run(dir, &["commit", "-m", message])?;
    Ok(run(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Push the branch, replacing the remote one only if it is still `lease`.
///
/// A rebased branch cannot be pushed any other way, and `--force` would
/// replace whatever is there — including commits someone else pushed after
/// the rebase. With a lease git refuses unless the remote branch is still
/// exactly what the rebase was measured against.
///
/// Returns whether the remote branch was replaced rather than added to.
pub fn push(dir: &Path, branch: &str, lease: Option<&str>) -> Result<bool> {
    // Mid-rebase the branch still points at its old tip, so the push went
    // through as a no-op and spent the lease the finished rebase needed.
    if let Some(busy) = in_progress(dir) {
        return Err(Error::Git(format!(
            "a {} is in progress here — finish or abandon it before pushing",
            busy.word()
        )));
    }
    // A lease the branch still contains protects nothing: the rebase it came
    // from was abandoned, or its result was merged over since. Sent anyway it
    // was refused the moment anyone else pushed, however the branch had
    // caught up with them. A plain push is refused by git itself if the
    // remote has something this branch does not.
    let lease = lease.filter(|sha| !is_ancestor(dir, sha, &format!("refs/heads/{branch}")));
    let replaced = lease.is_some();
    match lease {
        Some(sha) => run(
            dir,
            &["push", &format!("--force-with-lease={branch}:{sha}"), "-u", "origin", branch],
        )
        .map_err(|e| match e {
            Error::Git(msg) if msg.contains("stale info") => Error::Git(format!(
                "{msg}\nThe remote branch has moved since it was rebased here — someone else \
                 pushed to it. Fetch and look at what they pushed before replacing it."
            )),
            other => other,
        }),
        None => run(dir, &["push", "-u", "origin", branch]),
    }
    .map(|_| replaced)
}

/// The commit `origin/<branch>` points at, as this clone last heard.
pub fn remote_tip(dir: &Path, branch: &str) -> Option<String> {
    run(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{branch}")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// How a branch takes in what its base has done since it was cut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateBy {
    /// Merge the base in. History is kept, so nothing has to be force pushed
    /// — what GitHub's own "Update branch" does.
    #[default]
    Merge,
    /// Replay the branch's commits on the base. A straight line, as teams that
    /// only rebase want it — at the price of rewriting what was pushed.
    Rebase,
}

impl UpdateBy {
    fn word(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }
}

/// Where a branch stands after being brought up to date with its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Updated {
    /// The base had nothing this branch lacked.
    UpToDate,
    /// The base's commits are in, this many of them — merged or rebased onto.
    Applied { commits: u32 },
    /// Stopped on these files. The merge or rebase is left in progress, for an
    /// agent or a person to resolve: undoing it here would throw away the only
    /// state from which the conflict can be seen.
    Conflicts(Vec<String>),
}

/// Which update, if any, is half done in this worktree.
pub fn in_progress(dir: &Path) -> Option<UpdateBy> {
    // A merge leaves MERGE_HEAD; a rebase keeps its state in a directory.
    // `--git-path` finds both in a worktree, whose git dir is not `.git`
    // beside it — all three in one call, since every commit and push asks.
    let out = run(
        dir,
        &["rev-parse", "--git-path", "MERGE_HEAD", "--git-path", "rebase-merge", "--git-path", "rebase-apply"],
    )
    .ok()?;
    let paths: Vec<PathBuf> = out
        .lines()
        .map(|p| {
            let p = PathBuf::from(p.trim());
            if p.is_absolute() { p } else { dir.join(p) }
        })
        .collect();
    match paths.as_slice() {
        [merge, ..] if merge.is_file() => Some(UpdateBy::Merge),
        [_, rest @ ..] if rest.iter().any(|p| p.is_dir()) => Some(UpdateBy::Rebase),
        _ => None,
    }
}

/// Files with unresolved conflicts, as they are named on disk.
pub fn conflicted_files(dir: &Path) -> Vec<String> {
    run(dir, &["diff", "--name-only", "-z", "--diff-filter=U"])
        .map(|o| o.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Bring the newest tip of the base into this worktree's branch, by merging it
/// in or by rebasing onto it.
///
/// Returns the base commit taken in, which becomes the branch's recorded
/// point. Left at the old one, everything the base did since would be counted
/// as this branch's work.
///
/// Uncommitted edits to tracked files are refused rather than carried
/// through: a conflict on top of them cannot be untangled from them, and an
/// abort would take them too. Untracked files are left to git, which refuses
/// by itself if the update would overwrite one.
///
/// `lease` is where the remote branch was at the last rebase not yet pushed.
pub fn update_from_base(
    dir: &Path,
    expected: &str,
    base: &str,
    by: UpdateBy,
    lease: Option<&str>,
) -> Result<(Updated, String)> {
    if let Some(busy) = in_progress(dir) {
        return Err(Error::Git(format!(
            "a {} is already in progress here — finish or abandon it first",
            busy.word()
        )));
    }
    let branch = current_branch(dir)?;
    // Something switched the worktree — an agent, a detached checkout. The
    // update would rewrite that branch while the lease and the push went to
    // the task's.
    if branch != expected {
        return Err(Error::Git(if branch == "HEAD" {
            format!("this worktree is not on a branch — check out {expected} first")
        } else {
            format!("this worktree is on {branch}, not {expected} — check {expected} out first")
        }));
    }
    check_names(&branch, base)?;
    let st = status(dir)?;
    let edited = st.staged.max(st.unstaged) + st.conflicted;
    if edited > 0 {
        return Err(Error::Git(format!(
            "{edited} uncommitted change{} — commit {} first",
            if edited == 1 { "" } else { "s" },
            if edited == 1 { "it" } else { "them" },
        )));
    }

    let from = resolve_base(dir, base);
    let target = run(dir, &["rev-parse", "--verify", &format!("{from}^{{commit}}")])?
        .trim()
        .to_string();
    let count = |range: String| {
        run(dir, &["rev-list", "--count", &range])
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    };
    let commits = count(format!("HEAD..{target}"));
    if commits == 0 {
        return Ok((Updated::UpToDate, target));
    }

    let done = match by {
        // Named for the branch, not the commit: `git merge <sha>` writes
        // "Merge commit 'a1b2c3'" into history, which says nothing to anyone
        // later.
        //
        // `--ff` spelled out, because the user's own config applies here too:
        // with `merge.ff = only` set globally — a common guard against
        // accidental merges — every update of a branch with work on it failed
        // with "Not possible to fast-forward".
        UpdateBy::Merge => {
            let message = format!("Merge {from} into {branch}");
            run(dir, &["merge", "--ff", "--no-edit", "-m", &message, &target])
        }
        UpdateBy::Rebase => {
            // Commits on the remote branch that are not here are someone
            // else's work. Rebasing without them and pushing the result would
            // replace the branch with one that does not have them.
            //
            // Unless the remote is still where the last rebase found it: then
            // those commits are this branch's own, from before that rebase,
            // and bringing them in would put every commit in twice.
            if let Some(remote) = remote_tip(dir, &branch).filter(|r| Some(r.as_str()) != lease) {
                let theirs = count(format!("HEAD..{remote}"));
                if theirs > 0 {
                    return Err(Error::Git(format!(
                        "origin/{branch} has {theirs} commit{} this worktree does not — \
                         bring {} in first, or the rebased branch would push over {}",
                        if theirs == 1 { "" } else { "s" },
                        if theirs == 1 { "it" } else { "them" },
                        if theirs == 1 { "it" } else { "them" },
                    )));
                }
            }
            // As `--ff` above, and by config because every git reads it: a
            // global `rebase.autoSquash` reorders the commits being replayed,
            // and `rebase.updateRefs` moves other branches along with this one.
            run(
                dir,
                &[
                    "-c", "rebase.autoSquash=false",
                    "-c", "rebase.autoStash=false",
                    "-c", "rebase.updateRefs=false",
                    "rebase", &target,
                ],
            )
        }
    };

    match done {
        Ok(_) => Ok((Updated::Applied { commits }, target)),
        Err(e) => {
            let files = conflicted_files(dir);
            if !files.is_empty() {
                return Ok((Updated::Conflicts(files), target));
            }
            // Anything else — a hook that refused, an untracked file in the
            // way — must not leave the worktree half updated.
            let _ = abort_update(dir);
            Err(e)
        }
    }
}

/// Give up on a merge or rebase in progress, putting the branch back as it was.
pub fn abort_update(dir: &Path) -> Result<()> {
    match in_progress(dir) {
        Some(by) => run(dir, &[by.word(), "--abort"]).map(|_| ()),
        None => Err(Error::Git("no merge or rebase is in progress here".into())),
    }
}

/// `owner/repo` parsed from the origin remote.
///
/// Handles the three shapes that turn up in practice, including the SSH form
/// with an explicit port that GitHub Enterprise installs often use:
///   git@host:acme/web.git
///   https://host/acme/web.git
///   ssh://git@host:2222/acme/web.git
pub fn origin_slug(dir: &Path) -> Result<(String, String)> {
    let url = run(dir, &["remote", "get-url", "origin"])?.trim().to_string();

    // Normalise scp-style `host:path` into `host/path` so one split works for
    // everything. A `//` right after the colon means it was a real scheme.
    let normalised = match url.split_once("://") {
        Some((_, rest)) => rest.to_string(),
        None => match url.split_once(':') {
            Some((host, path)) => format!("{host}/{path}"),
            None => url.clone(),
        },
    };

    let segments: Vec<&str> = normalised
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    // The first segment is the host (possibly with userinfo and a port); the
    // last two are always owner and repo.
    if segments.len() >= 3 {
        let repo = segments[segments.len() - 1];
        let owner = segments[segments.len() - 2];
        return Ok((owner.to_string(), repo.to_string()));
    }

    Err(Error::Git(format!("cannot parse owner/repo from {url}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A private directory for one test, cleaned up by the caller.
    fn sandbox() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A throwaway repo with one commit on `main`, inside its own sandbox.
    fn fixture() -> PathBuf {
        let dir = sandbox().join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]).unwrap();
        run(&dir, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&dir, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(&dir, &["add", "-A"]).unwrap();
        run(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    #[test]
    fn detects_default_branch_and_root() {
        let repo = fixture();
        assert_eq!(default_branch(&repo), "main");
        assert_eq!(
            std::fs::canonicalize(repo_root(&repo).unwrap()).unwrap(),
            std::fs::canonicalize(&repo).unwrap()
        );
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn worktree_round_trip() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");

        add_worktree(&repo, &wt, "feature/x", "main").unwrap();
        assert!(wt.join("a.txt").exists());
        assert_eq!(current_branch(&wt).unwrap(), "feature/x");

        let listed = list_worktrees(&repo).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|e| e.branch.as_deref() == Some("feature/x")));

        remove_worktree(&repo, &wt.to_string_lossy(), true).unwrap();
        assert_eq!(list_worktrees(&repo).unwrap().len(), 1);
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    /// The bug: a local `main` that nobody had pulled recently was preferred
    /// over `origin/main`, so every new task started days or weeks behind.
    #[test]
    fn new_worktree_starts_at_the_newest_remote_base() {
        let root = sandbox();
        let bare = root.join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        run(&bare, &["init", "-q", "-b", "main", "--bare"]).unwrap();

        let seed = root.join("seed");
        run(&root, &["clone", "-q", bare.to_str().unwrap(), seed.to_str().unwrap()]).unwrap();
        run(&seed, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&seed, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(seed.join("a.txt"), "old\n").unwrap();
        run(&seed, &["add", "-A"]).unwrap();
        run(&seed, &["commit", "-qm", "old tip"]).unwrap();
        run(&seed, &["push", "-q", "-u", "origin", "main"]).unwrap();

        let local = root.join("local");
        run(&root, &["clone", "-q", bare.to_str().unwrap(), local.to_str().unwrap()]).unwrap();
        run(&local, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&local, &["config", "user.name", "Test"]).unwrap();
        let stale = run(&local, &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        // Advance origin/main without updating the local clone's main tip.
        std::fs::write(seed.join("a.txt"), "new\n").unwrap();
        run(&seed, &["commit", "-am", "new tip"]).unwrap();
        run(&seed, &["push", "-q"]).unwrap();
        let newest = run(&seed, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert_ne!(stale, newest);

        let wt = root.join("wt");
        let point = add_worktree(&local, &wt, "feature/fresh", "main").unwrap();
        assert_eq!(point, newest);
        assert_eq!(
            run(&wt, &["rev-parse", "HEAD"]).unwrap().trim(),
            newest.as_str()
        );
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "new\n");

        // The same through the parallel fetch a multi-repo task uses.
        std::fs::write(seed.join("a.txt"), "newer\n").unwrap();
        run(&seed, &["commit", "-am", "newer tip"]).unwrap();
        run(&seed, &["push", "-q"]).unwrap();
        let newer = run(&seed, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        fetch_bases(&[(local.clone(), "main".to_string())]);
        let wt2 = root.join("wt2");
        let point = add_worktree_fetched(&local, &wt2, "feature/fresher", "main").unwrap();
        assert_eq!(point, newer);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_committed_and_untracked_changes() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "feature/y", "main").unwrap();

        // One committed edit, one uncommitted edit, one brand new file.
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        commit_all(&wt, "edit a").unwrap();
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        std::fs::write(wt.join("b.txt"), "new file\n").unwrap();

        let files = changed_files(&wt, "main", None, Scope::Branch).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);

        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.additions, a.deletions), (2, 1));
        assert_eq!(files.iter().find(|f| f.path == "b.txt").unwrap().origin, "untracked");

        // Tracked files diff through git; untracked ones are synthesised.
        assert!(file_diff(&wt, "main", None, Scope::Branch, "a.txt").unwrap().contains("+four"));
        assert!(file_diff(&wt, "main", None, Scope::Branch, "b.txt").unwrap().contains("+new file"));

        let st = status(&wt).unwrap();
        assert_eq!(st.branch, "feature/y");
        assert_eq!((st.unstaged, st.untracked), (1, 1));

        remove_worktree(&repo, &wt.to_string_lossy(), true).ok();
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn lists_and_diffs_a_single_commit() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "feature/commits", "main").unwrap();

        std::fs::write(wt.join("a.txt"), "one\n").unwrap();
        commit_all(&wt, "first").unwrap();
        std::fs::write(wt.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(wt.join("b.txt"), "new\n").unwrap();
        let sha = commit_all(&wt, "second").unwrap();

        let log = commits_since(&wt, "main", None).unwrap();
        assert!(log.iter().any(|c| c.sha == sha));
        assert!(log.iter().any(|c| c.subject == "second"));

        let files = commit_files(&wt, &sha).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);

        let patch = commit_file_diff(&wt, &sha, "b.txt").unwrap();
        assert!(patch.contains("+new"));

        remove_worktree(&repo, &wt.to_string_lossy(), true).ok();
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn renames_resolve_to_the_new_path() {
        let parsed = parse_numstat("1\t0\tplain.txt\x000\t0\t\0src/old.txt\0src/new.txt\0-\t-\tpic.png\0");
        let paths: Vec<_> = parsed.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt", "src/new.txt", "pic.png"]);
        assert!(parsed[2].binary);
        // `show` opens with the newline its empty header ends in.
        assert_eq!(parse_numstat("\n3\t1\ta.rs\0")[0].path, "a.rs");
    }

    #[test]
    fn renamed_and_non_ascii_files_keep_paths_that_open() {
        let repo = fixture();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/old.txt"), (1..50).map(|n| format!("{n}\n")).collect::<String>()).unwrap();
        run(&repo, &["add", "-A"]).unwrap();
        run(&repo, &["commit", "-qm", "old"]).unwrap();
        run(&repo, &["mv", "src/old.txt", "src/new.txt"]).unwrap();
        std::fs::write(repo.join("café.txt"), "z\n").unwrap();

        let files = changed_files(&repo, "main", None, Scope::Uncommitted).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/new.txt"), "{paths:?}");
        assert!(paths.contains(&"café.txt"), "{paths:?}");
        for f in &files {
            file_diff(&repo, "main", None, Scope::Uncommitted, &f.path)
                .unwrap_or_else(|e| panic!("{} did not open: {e}", f.path));
        }

        run(&repo, &["commit", "-qm", "move"]).unwrap();
        let sha = run(&repo, &["rev-parse", "HEAD"]).unwrap();
        let moved = commit_files(&repo, sha.trim()).unwrap();
        assert_eq!(moved.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["src/new.txt"]);
    }

    #[test]
    fn commit_picker_skips_history_brought_in_by_a_merge() {
        let repo = fixture();
        // Commits on main after the branch was cut — without --first-parent
        // these would show up on the feature branch's log after a merge.
        for i in 1..=5 {
            std::fs::write(repo.join("main.txt"), format!("m{i}\n")).unwrap();
            run(&repo, &["add", "main.txt"]).unwrap();
            run(&repo, &["commit", "-m", &format!("main-{i}")]).unwrap();
        }
        let wt = repo.parent().unwrap().join("wt");
        let point = add_worktree(&repo, &wt, "feature/merge", "main").unwrap();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "feature work").unwrap();

        // More main commits, then merge them into the feature branch.
        for i in 6..=10 {
            std::fs::write(repo.join("main.txt"), format!("m{i}\n")).unwrap();
            run(&repo, &["add", "main.txt"]).unwrap();
            run(&repo, &["commit", "-m", &format!("main-{i}")]).unwrap();
        }
        run(&wt, &["merge", "--no-ff", "main", "-m", "merge main"]).unwrap();

        let log = commits_since(&wt, "main", Some(&point)).unwrap();
        let subjects: Vec<&str> = log.iter().map(|c| c.subject.as_str()).collect();
        assert!(subjects.contains(&"feature work"));
        assert!(subjects.contains(&"merge main"));
        assert!(
            !subjects.iter().any(|s| s.starts_with("main-")),
            "picker listed main's history: {subjects:?}"
        );

        remove_worktree(&repo, &wt.to_string_lossy(), true).ok();
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    /// One task, one branch name, several repositories: the branch is created
    /// independently in each and the worktrees sit side by side.
    #[test]
    fn same_branch_across_several_repos() {
        let api = fixture();
        let web = fixture();
        let task_root = sandbox().join("task");

        for repo in [&api, &web] {
            let name = if repo == &api { "api" } else { "web" };
            add_worktree(repo, &task_root.join(name), "acme-9-shared", "main").unwrap();
        }

        assert_eq!(current_branch(&task_root.join("api")).unwrap(), "acme-9-shared");
        assert_eq!(current_branch(&task_root.join("web")).unwrap(), "acme-9-shared");

        // Each repo tracks only its own worktree.
        assert_eq!(list_worktrees(&api).unwrap().len(), 2);
        assert_eq!(list_worktrees(&web).unwrap().len(), 2);

        // Changes in one repo are invisible to the other's diff.
        std::fs::write(task_root.join("api/a.txt"), "changed\n").unwrap();
        assert_eq!(changed_files(&task_root.join("api"), "main", None, Scope::Branch).unwrap().len(), 1);
        assert!(changed_files(&task_root.join("web"), "main", None, Scope::Branch).unwrap().is_empty());

        remove_worktree(&api, &task_root.join("api").to_string_lossy(), true).ok();
        remove_worktree(&web, &task_root.join("web").to_string_lossy(), true).ok();
        std::fs::remove_dir_all(api.parent().unwrap()).ok();
        std::fs::remove_dir_all(web.parent().unwrap()).ok();
        std::fs::remove_dir_all(task_root.parent().unwrap()).ok();
    }

    #[test]
    fn diffs_a_file_the_agent_deleted() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "feature/del", "main").unwrap();

        run(&wt, &["rm", "-q", "a.txt"]).unwrap();

        // It shows up as changed...
        let files = changed_files(&wt, "main", None, Scope::Branch).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");

        // ...so asking for its patch must work, not error on a missing file.
        let patch = file_diff(&wt, "main", None, Scope::Branch, "a.txt").unwrap();
        assert!(patch.contains("-one"), "expected a deletion patch, got: {patch}");

        remove_worktree(&repo, &wt.to_string_lossy(), true).ok();
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    /// The bug behind orphaned worktrees: git refuses to remove a worktree that
    /// has untracked files, and the failure used to be swallowed.
    #[test]
    fn removing_a_dirty_worktree_needs_force() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "feature/dirty", "main").unwrap();

        // An untracked file is enough; it does not take uncommitted edits.
        std::fs::write(wt.join("scratch.log"), "noise\n").unwrap();

        let refused = remove_worktree(&repo, &wt.to_string_lossy(), false);
        assert!(refused.is_err(), "expected git to refuse while the worktree is dirty");
        assert!(wt.exists(), "the worktree must still be on disk after a refusal");
        assert_eq!(list_worktrees(&repo).unwrap().len(), 2);

        remove_worktree(&repo, &wt.to_string_lossy(), true).unwrap();
        assert!(!wt.exists());
        assert_eq!(list_worktrees(&repo).unwrap().len(), 1);

        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    /// The repository re-cloned under its worktrees: the folders stay, their
    /// link does not, and nothing in git will say why.
    #[test]
    fn a_worktree_whose_repository_forgot_it_says_so() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "feature/x", "main").unwrap();
        assert_eq!(unlinked(&wt), None, "a healthy worktree");
        assert_eq!(unlinked(&repo), None, "a plain clone has a .git folder, not a link");

        std::fs::remove_dir_all(repo.join(".git/worktrees")).unwrap();
        let why = unlinked(&wt).expect("the link is gone");
        assert!(why.contains(".git/worktrees"), "names what is missing: {why}");
        assert!(status(&wt).is_err(), "and git itself cannot read it");
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn a_deleted_worktree_says_so() {
        let gone = std::env::temp_dir().join(format!("vl-gone-{}", uuid::Uuid::new_v4()));
        let err = status(&gone).unwrap_err().to_string();
        assert!(
            err.contains("no longer exists"),
            "expected a message about the missing directory, got: {err}"
        );
    }

    #[test]
    fn parses_origin_slug_from_ssh_url_with_a_port() {
        let repo = fixture();
        run(&repo, &["remote", "add", "origin", "ssh://git@ghe.example.com:2222/acme/web.git"])
            .unwrap();
        assert_eq!(origin_slug(&repo).unwrap(), ("acme".into(), "web".into()));
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }

    #[test]
    fn parses_origin_slug_from_both_url_styles() {
        let repo = fixture();
        run(&repo, &["remote", "add", "origin", "git@github.com:acme/web.git"]).unwrap();
        assert_eq!(origin_slug(&repo).unwrap(), ("acme".into(), "web".into()));

        run(&repo, &["remote", "set-url", "origin", "https://ghe.example.com/acme/web.git"]).unwrap();
        assert_eq!(origin_slug(&repo).unwrap(), ("acme".into(), "web".into()));
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }
    /// A clone of a bare remote, and a worktree of it on `feature`, so the
    /// base can move on the remote while the branch stays where it was.
    fn remote_and_worktree() -> (PathBuf, PathBuf, PathBuf, String) {
        let root = sandbox();
        let bare = root.join("remote.git");
        std::fs::create_dir_all(&bare).unwrap();
        run(&bare, &["init", "-q", "-b", "main", "--bare"]).unwrap();
        let seed = root.join("seed");
        run(&root, &["clone", "-q", bare.to_str().unwrap(), seed.to_str().unwrap()]).unwrap();
        run(&seed, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&seed, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(seed.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(&seed, &["add", "-A"]).unwrap();
        run(&seed, &["commit", "-qm", "init"]).unwrap();
        run(&seed, &["push", "-q", "-u", "origin", "main"]).unwrap();

        let local = root.join("local");
        run(&root, &["clone", "-q", bare.to_str().unwrap(), local.to_str().unwrap()]).unwrap();
        run(&local, &["config", "user.email", "test@villain.local"]).unwrap();
        run(&local, &["config", "user.name", "Test"]).unwrap();
        let wt = root.join("wt");
        let point = add_worktree(&local, &wt, "feature", "main").unwrap();
        (root, seed, wt, point)
    }

    fn push_to_main(seed: &Path, file: &str, text: &str) {
        std::fs::write(seed.join(file), text).unwrap();
        run(seed, &["add", "-A"]).unwrap();
        run(seed, &["commit", "-qm", &format!("main: {file}")]).unwrap();
        run(seed, &["push", "-q"]).unwrap();
    }

    #[test]
    fn updating_merges_the_newest_base_and_moves_the_branch_point() {
        let (root, seed, wt, point) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "feature work").unwrap();
        assert_eq!(update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap().0, Updated::UpToDate);

        push_to_main(&seed, "main.txt", "m\n");
        push_to_main(&seed, "main2.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);
        let (outcome, target) = update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap();
        assert_eq!(outcome, Updated::Applied { commits: 2 });
        assert!(wt.join("main2.txt").exists());
        let subject = run(&wt, &["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(subject.trim(), "Merge origin/main into feature");

        // From the old point the base's two files look like this branch's;
        // from the one returned, only the feature's own file does.
        let from_old = changed_files(&wt, "main", Some(&point), Scope::Branch).unwrap();
        assert_eq!(from_old.len(), 3);
        let from_new = changed_files(&wt, "main", Some(&target), Scope::Branch).unwrap();
        assert_eq!(from_new.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["feat.txt"]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_conflict_is_left_to_resolve_and_can_be_abandoned() {
        let (root, seed, wt, point) = remote_and_worktree();
        std::fs::write(wt.join("a.txt"), "one\nTWO (feature)\nthree\n").unwrap();
        commit_all(&wt, "feature edit").unwrap();
        push_to_main(&seed, "a.txt", "one\nTWO (main)\nthree\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        let (outcome, target) = update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap();
        assert_eq!(outcome, Updated::Conflicts(vec!["a.txt".into()]));
        assert_eq!(in_progress(&wt), Some(UpdateBy::Merge));
        // Asked again while it is still in progress: refused, not stacked.
        assert!(update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).is_err());

        // Recorded as the target but never reached: not a branch point.
        assert_ne!(baseline(&wt, "main", Some(&target)), target);

        abort_update(&wt).unwrap();
        assert_eq!(in_progress(&wt), None);
        assert_eq!(baseline(&wt, "main", Some(&target)), point);
        assert!(abort_update(&wt).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn uncommitted_edits_are_refused_but_untracked_files_are_not() {
        let (root, seed, wt, _) = remote_and_worktree();
        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        std::fs::write(wt.join("a.txt"), "edited\n").unwrap();
        let err = update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap_err().to_string();
        assert!(err.contains("1 uncommitted change"), "{err}");
        assert_eq!(in_progress(&wt), None);

        run(&wt, &["checkout", "--", "a.txt"]).unwrap();
        std::fs::write(wt.join("notes.txt"), "scratch\n").unwrap();
        assert_eq!(update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap().0, Updated::Applied { commits: 1 });
        std::fs::remove_dir_all(root).ok();
    }
    /// Finishing a task: the worktree goes first, because git will not delete
    /// a branch that is checked out anywhere.
    #[test]
    fn a_branch_deletes_once_its_worktree_is_gone() {
        let repo = fixture();
        let wt = repo.parent().unwrap().join("wt");
        add_worktree(&repo, &wt, "acme-5-done", "main").unwrap();
        std::fs::write(wt.join("x.txt"), "x\n").unwrap();
        commit_all(&wt, "work that was squash-merged elsewhere").unwrap();

        let landed = run(&wt, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        assert!(is_ancestor(&repo, "refs/heads/acme-5-done", &landed));
        std::fs::write(wt.join("y.txt"), "after the merge\n").unwrap();
        commit_all(&wt, "work after the PR landed").unwrap();
        assert!(!is_ancestor(&repo, "refs/heads/acme-5-done", &landed));
        assert!(!is_ancestor(&repo, "refs/heads/acme-5-done", "0000000000000000000000000000000000000000"));

        assert!(delete_branch(&repo, "acme-5-done").is_err());
        remove_worktree(&repo, &wt.to_string_lossy(), false).unwrap();
        delete_branch(&repo, "acme-5-done").unwrap();
        assert!(!branch_exists(&repo, "acme-5-done"));
        std::fs::remove_dir_all(repo.parent().unwrap()).ok();
    }
    #[test]
    fn rebasing_replays_the_branch_on_the_newest_base() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "feature one").unwrap();
        std::fs::write(wt.join("feat2.txt"), "f\n").unwrap();
        commit_all(&wt, "feature two").unwrap();
        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        let (outcome, target) = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap();
        assert_eq!(outcome, Updated::Applied { commits: 1 });
        assert_eq!(current_branch(&wt).unwrap(), "feature");
        // A straight line: no merge commit, the branch's two on top of the base.
        let log = run(&wt, &["log", "--format=%s", &format!("{target}..HEAD")]).unwrap();
        assert_eq!(log.lines().collect::<Vec<_>>(), ["feature two", "feature one"]);
        assert!(is_ancestor(&wt, &target, "HEAD"));
        let files = changed_files(&wt, "main", Some(&target), Scope::Branch).unwrap();
        assert_eq!(files.len(), 2);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_rebase_conflict_waits_and_can_be_abandoned() {
        let (root, seed, wt, point) = remote_and_worktree();
        std::fs::write(wt.join("a.txt"), "one\nTWO (feature)\nthree\n").unwrap();
        commit_all(&wt, "feature edit").unwrap();
        let before = run(&wt, &["rev-parse", "HEAD"]).unwrap();
        push_to_main(&seed, "a.txt", "one\nTWO (main)\nthree\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        let (outcome, target) = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap();
        assert_eq!(outcome, Updated::Conflicts(vec!["a.txt".into()]));
        assert_eq!(in_progress(&wt), Some(UpdateBy::Rebase));
        let again = update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap_err().to_string();
        assert!(again.contains("rebase is already in progress"), "{again}");

        abort_update(&wt).unwrap();
        assert_eq!(in_progress(&wt), None);
        assert_eq!(run(&wt, &["rev-parse", "HEAD"]).unwrap(), before);
        assert_eq!(current_branch(&wt).unwrap(), "feature");
        assert_eq!(baseline(&wt, "main", Some(&target)), point);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_rebase_will_not_leave_someone_elses_pushed_commits_behind() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "mine").unwrap();
        push(&wt, "feature", None).unwrap();

        // A colleague adds to the branch; this worktree has not taken it in.
        run(&seed, &["fetch", "-q", "origin"]).unwrap();
        run(&seed, &["checkout", "-q", "-b", "feature", "origin/feature"]).unwrap();
        std::fs::write(seed.join("theirs.txt"), "t\n").unwrap();
        run(&seed, &["add", "-A"]).unwrap();
        run(&seed, &["commit", "-qm", "theirs"]).unwrap();
        run(&seed, &["push", "-q", "origin", "feature"]).unwrap();
        run(&seed, &["checkout", "-q", "main"]).unwrap();
        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into()), (wt.clone(), "feature".into())]);

        let err = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap_err().to_string();
        assert!(err.contains("origin/feature has 1 commit this worktree does not"), "{err}");
        assert_eq!(in_progress(&wt), None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_rebased_branch_pushes_only_over_what_it_was_rebased_against() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "mine").unwrap();
        push(&wt, "feature", None).unwrap();
        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        let lease = remote_tip(&wt, "feature").unwrap();
        update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap();
        // A plain push is refused: the history no longer follows on.
        assert!(push(&wt, "feature", None).is_err());
        // Leased on the wrong commit: refused, and says why.
        let stale = push(&wt, "feature", Some(&"0".repeat(40))).unwrap_err().to_string();
        assert!(stale.contains("someone else pushed"), "{stale}");
        push(&wt, "feature", Some(&lease)).unwrap();
        assert_eq!(remote_tip(&wt, "feature").unwrap(), run(&wt, &["rev-parse", "HEAD"]).unwrap().trim());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn new_files_count_whatever_the_users_config_says() {
        let (root, _seed, wt, _) = remote_and_worktree();
        run(&wt, &["config", "status.showUntrackedFiles", "no"]).unwrap();
        std::fs::write(wt.join("new.txt"), "not committed\n").unwrap();
        assert_eq!(status(&wt).unwrap().dirty_files, 1);
        // git's own check honours the setting too; told otherwise, it refuses.
        let repo = root.join("local");
        assert!(remove_worktree(&repo, &wt.to_string_lossy(), false).is_err());
        assert!(wt.join("new.txt").exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn nothing_is_committed_or_pushed_half_way_through_an_update() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("a.txt"), "one\nTWO (feature)\nthree\n").unwrap();
        commit_all(&wt, "feature edit").unwrap();
        push(&wt, "feature", None).unwrap();
        push_to_main(&seed, "a.txt", "one\nTWO (main)\nthree\n");
        fetch_bases(&[(wt.clone(), "main".into())]);
        let lease = remote_tip(&wt, "feature");

        let (outcome, _) = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap();
        assert!(matches!(outcome, Updated::Conflicts(_)));
        assert!(commit_all(&wt, "resolve").unwrap_err().to_string().contains("in progress"));
        assert!(push(&wt, "feature", lease.as_deref()).unwrap_err().to_string().contains("in progress"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_lease_the_branch_still_contains_is_not_sent() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "mine").unwrap();
        push(&wt, "feature", None).unwrap();
        let lease = remote_tip(&wt, "feature").unwrap();

        // The rebase was abandoned; since then someone pushed, and that was
        // merged in here. The old lease would be refused as stale.
        run(&seed, &["fetch", "-q", "origin"]).unwrap();
        run(&seed, &["checkout", "-q", "-b", "feature", "origin/feature"]).unwrap();
        std::fs::write(seed.join("theirs.txt"), "t\n").unwrap();
        run(&seed, &["add", "-A"]).unwrap();
        run(&seed, &["commit", "-qm", "theirs"]).unwrap();
        run(&seed, &["push", "-q", "origin", "feature"]).unwrap();
        fetch_branch(&wt, "feature");
        run(&wt, &["merge", "-q", "--ff", "--no-edit", "origin/feature"]).unwrap();
        std::fs::write(wt.join("more.txt"), "m\n").unwrap();
        commit_all(&wt, "more").unwrap();

        assert!(!push(&wt, "feature", Some(&lease)).unwrap(), "pushed plainly, replacing nothing");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_second_rebase_before_pushing_is_not_refused_by_the_first() {
        let (root, seed, wt, _) = remote_and_worktree();
        std::fs::write(wt.join("feat.txt"), "f\n").unwrap();
        commit_all(&wt, "mine").unwrap();
        push(&wt, "feature", None).unwrap();
        let lease = remote_tip(&wt, "feature").unwrap();

        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);
        update_from_base(&wt, "feature", "main", UpdateBy::Rebase, Some(&lease)).unwrap();
        push_to_main(&seed, "main2.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);

        // Without the lease its own pre-rebase commit reads as someone else's.
        assert!(update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).is_err());
        let (outcome, _) = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, Some(&lease)).unwrap();
        assert_eq!(outcome, Updated::Applied { commits: 1 });
        assert!(push(&wt, "feature", Some(&lease)).unwrap());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn an_update_is_refused_off_the_task_branch() {
        let (root, seed, wt, _) = remote_and_worktree();
        push_to_main(&seed, "main.txt", "m\n");
        fetch_bases(&[(wt.clone(), "main".into())]);
        run(&wt, &["checkout", "-q", "-b", "elsewhere"]).unwrap();
        let err = update_from_base(&wt, "feature", "main", UpdateBy::Rebase, None).unwrap_err().to_string();
        assert!(err.contains("on elsewhere, not feature"), "{err}");
        run(&wt, &["checkout", "-q", "--detach"]).unwrap();
        let err = update_from_base(&wt, "feature", "main", UpdateBy::Merge, None).unwrap_err().to_string();
        assert!(err.contains("not on a branch"), "{err}");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_branch_deleted_on_the_remote_is_forgotten_here() {
        let (root, _seed, wt, _) = remote_and_worktree();
        push(&wt, "feature", None).unwrap();
        assert!(remote_tip(&wt, "feature").is_some());
        // Merged with "delete branch": the push below is from elsewhere.
        run(&wt, &["push", "-q", "origin", "--delete", "feature"]).unwrap();
        run(&wt, &["update-ref", "refs/remotes/origin/feature", "HEAD"]).unwrap();
        fetch_branch(&wt, "feature");
        assert_eq!(remote_tip(&wt, "feature"), None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_single_branch_clone_still_tracks_what_is_fetched() {
        let (root, seed, _wt, _) = remote_and_worktree();
        let bare = root.join("remote.git");
        run(&seed, &["push", "-q", "origin", "main:develop"]).unwrap();
        let narrow = root.join("narrow");
        run(&root, &["clone", "-q", "--single-branch", "-b", "main", bare.to_str().unwrap(), narrow.to_str().unwrap()]).unwrap();
        fetch_bases(&[(narrow.clone(), "develop".into())]);
        assert!(remote_tip(&narrow, "develop").is_some());
        std::fs::remove_dir_all(root).ok();
    }
}
