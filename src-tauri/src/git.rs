//! Thin wrapper over the `git` CLI. The CLI is used rather than libgit2
//! because worktree semantics, hooks and credential helpers all come free.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        // The status poll runs beside agents committing in the same worktree.
        // Without this, `git status` refreshes the index as a side effect and
        // takes index.lock to do it, and an agent's own `git commit` landing
        // in that moment fails with "index.lock: File exists".
        .env("GIT_OPTIONAL_LOCKS", "0");
    // A GUI app's PATH is /usr/bin:/bin. Hooks run with git's environment, so
    // a husky or lint-staged hook that needs node — or git-lfs on checkout —
    // failed here while working in a terminal.
    if let Some(path) = crate::shellenv::path_if_ready() {
        cmd.env("PATH", path);
    }
    let out = cmd
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
    let _ = run(repo, &["fetch", "--quiet", "origin", base]);
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

    if branch_exists(repo, branch) {
        run(repo, &["worktree", "add", &path_s, branch])?;
        // An existing branch has its own history; what it forked from is the
        // best available answer, not wherever the base happens to be today.
        Ok(run(repo, &["merge-base", &start, branch])
            .or_else(|_| run(repo, &["rev-parse", branch]))
            .map(|s| s.trim().to_string())
            .unwrap_or_default())
    } else {
        run(repo, &["worktree", "add", "-b", branch, &path_s, &start])?;
        Ok(run(path, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default())
    }
}

/// Delete a local branch, for undoing one this app just created.
pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    run(repo, &["branch", "-D", "--", branch]).map(|_| ())
}

pub fn remove_worktree(repo: &Path, path: &str, force: bool) -> Result<()> {
    let mut args = vec!["worktree", "remove"];
    if force {
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
    let out = run(dir, &["status", "--porcelain=v2", "--branch"])?;
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
        // Only if this worktree actually has it; a rewritten history may not.
        if run(dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok() {
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
    let patch = run(dir, &["diff", "--no-color", &merge_base, "--", path]).unwrap_or_default();
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

/// Files changed in a single commit, with the same shape as `changed_files`.
pub fn commit_files(dir: &Path, sha: &str) -> Result<Vec<ChangedFile>> {
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
    // `--pretty=format:` drops the commit header so the UI gets a bare patch,
    // the same shape `file_diff` returns for working-tree changes.
    let patch = run(dir, &["show", "--no-color", "--pretty=format:", sha, "--", path])?;
    Ok(patch)
}

pub fn commit_all(dir: &Path, message: &str) -> Result<String> {
    run(dir, &["add", "-A"])?;
    run(dir, &["commit", "-m", message])?;
    Ok(run(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

pub fn push(dir: &Path, branch: &str) -> Result<String> {
    run(dir, &["push", "-u", "origin", branch])
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
}
