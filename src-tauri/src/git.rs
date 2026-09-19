//! Thin wrapper over the `git` CLI. The CLI is used rather than libgit2
//! because worktree semantics, hooks and credential helpers all come free.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    run(
        dir,
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    )
    .is_ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub locked: bool,
}

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

/// Create a worktree at `path`. Creates `branch` from `base` when it does not
/// already exist, otherwise checks the existing branch out.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path_s = path.to_string_lossy().to_string();

    if branch_exists(repo, branch) {
        run(repo, &["worktree", "add", &path_s, branch])?;
    } else {
        run(repo, &["worktree", "add", "-b", branch, &path_s, base])?;
    }
    Ok(())
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
        } else if line.starts_with("u ") {
            s.conflicted += 1;
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
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
    /// "staged" | "unstaged" | "untracked" | "committed"
    pub origin: String,
}

/// Every file that differs from `base`, including uncommitted and untracked work.
pub fn changed_files(dir: &Path, base: &str) -> Result<Vec<ChangedFile>> {
    let mut files: Vec<ChangedFile> = Vec::new();

    let merge_base = run(dir, &["merge-base", "HEAD", base])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| base.to_string());

    // Committed on this branch since the baseline, plus everything in the tree.
    let numstat = run(dir, &["diff", "--numstat", &merge_base])?;
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let (a, d, path) = (parts.next(), parts.next(), parts.next());
        let (Some(a), Some(d), Some(path)) = (a, d, path) else {
            continue;
        };
        let binary = a == "-" || d == "-";
        files.push(ChangedFile {
            path: path.to_string(),
            additions: a.parse().unwrap_or(0),
            deletions: d.parse().unwrap_or(0),
            binary,
            origin: "tracked".into(),
        });
    }

    for path in run(dir, &["ls-files", "--others", "--exclude-standard"])?.lines() {
        if path.is_empty() {
            continue;
        }
        let added = std::fs::read_to_string(dir.join(path))
            .map(|c| c.lines().count() as u32)
            .unwrap_or(0);
        files.push(ChangedFile {
            path: path.to_string(),
            additions: added,
            deletions: 0,
            binary: false,
            origin: "untracked".into(),
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Unified patch for one file, against the baseline. Untracked files are
/// rendered as an all-additions patch so the review UI has one code path.
pub fn file_diff(dir: &Path, base: &str, path: &str) -> Result<String> {
    let merge_base = run(dir, &["merge-base", "HEAD", base])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| base.to_string());

    // One call covers modified, staged, renamed and deleted files. Branching on
    // "is it in the index" instead would send deleted files down the untracked
    // path, where reading them from disk fails.
    let patch = run(dir, &["diff", "--no-color", &merge_base, "--", path]).unwrap_or_default();
    if !patch.trim().is_empty() {
        return Ok(patch);
    }

    // Nothing from git means it is untracked; synthesise the addition patch.
    let content = std::fs::read_to_string(dir.join(path))?;
    let lines = content.lines().count();
    let mut out = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n");
    for line in content.lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
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

        let files = changed_files(&wt, "main").unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "b.txt"]);

        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.additions, a.deletions), (2, 1));
        assert_eq!(files.iter().find(|f| f.path == "b.txt").unwrap().origin, "untracked");

        // Tracked files diff through git; untracked ones are synthesised.
        assert!(file_diff(&wt, "main", "a.txt").unwrap().contains("+four"));
        assert!(file_diff(&wt, "main", "b.txt").unwrap().contains("+new file"));

        let st = status(&wt).unwrap();
        assert_eq!(st.branch, "feature/y");
        assert_eq!((st.unstaged, st.untracked), (1, 1));

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
        assert_eq!(changed_files(&task_root.join("api"), "main").unwrap().len(), 1);
        assert!(changed_files(&task_root.join("web"), "main").unwrap().is_empty());

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
        let files = changed_files(&wt, "main").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");

        // ...so asking for its patch must work, not error on a missing file.
        let patch = file_diff(&wt, "main", "a.txt").unwrap();
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
