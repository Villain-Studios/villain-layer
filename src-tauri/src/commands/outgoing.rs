//! What Open pull requests is about to push, looked at first (PR-11).
//!
//! A push used to be the first time anyone saw a branch whole, and what it
//! carried was found by the reviewers: a `console.log` left in, a test
//! focused with `.only`, a conflict marker committed with the merge. This
//! lists, per repo, the commits a push sends and what in their added lines
//! is usually left behind by accident. It is advice: nothing is refused.

use std::path::PathBuf;

use serde::Serialize;
use tauri::AppHandle;

use crate::error::Result;
use crate::git::{self, CommitInfo};

use super::AppState;

/// More than this many is a pattern in the codebase, not a slip.
const MAX_LEFTOVERS: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Leftover {
    pub path: String,
    pub line: u32,
    /// "debug", "focused test", "conflict marker" or "todo".
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct RepoOutgoing {
    pub checkout_id: String,
    pub repo: String,
    /// What a review of it has to have been of to still be current.
    pub head: String,
    pub commits: Vec<CommitInfo>,
    pub leftovers: Vec<Leftover>,
    /// More than `MAX_LEFTOVERS` were found.
    pub leftovers_more: bool,
    pub error: Option<String>,
}

/// Per repo, the commits a push would send and the leftovers they add.
#[tauri::command]
pub async fn task_outgoing(app: AppHandle, task_id: String) -> Result<Vec<RepoOutgoing>> {
    super::blocking(app, move |state| task_outgoing_inner(state, &task_id)).await
}

fn task_outgoing_inner(state: &AppState, task_id: &str) -> Result<Vec<RepoOutgoing>> {
    let task = state.config.task(task_id)?;
    Ok(state
        .config
        .checkouts_of(task_id)
        .into_iter()
        .map(|c| {
            let repo = state.config.project(&c.project_id).map(|p| p.name).unwrap_or_else(|_| "repo".into());
            let dir = PathBuf::from(&c.path);
            let mut row = RepoOutgoing {
                checkout_id: c.id.clone(),
                repo,
                head: String::new(),
                commits: Vec::new(),
                leftovers: Vec::new(),
                leftovers_more: false,
                error: None,
            };
            // A repo that cannot be read is a row saying so, not the end of
            // the list.
            let read = (|| -> Result<()> {
                row.head = git::status(&dir)?.head;
                let from = git::outgoing_from(&dir, &task.branch, &c.base, c.base_commit.as_deref());
                row.commits = git::outgoing_commits(&dir, &c.base, &from)?;
                let (found, more) = leftovers_in(&git::outgoing_patch(&dir, &from)?);
                row.leftovers = found;
                row.leftovers_more = more;
                Ok(())
            })();
            row.error = read.err().map(|e| e.to_string());
            row
        })
        .collect())
}

/// What in a patch's added lines is usually there by accident.
fn leftovers_in(patch: &str) -> (Vec<Leftover>, bool) {
    let mut found = Vec::new();
    let mut more = false;
    let mut file: Option<String> = None;
    let mut line = 0u32;
    for raw in patch.lines() {
        if raw.starts_with("diff ") {
            file = None;
        } else if let Some(name) = raw.strip_prefix("+++ ") {
            // Prose mentions `console.log` as often as code calls it.
            file = name.strip_prefix("b/").filter(|p| !is_prose(p)).map(str::to_string);
        } else if let Some(header) = raw.strip_prefix("@@ ") {
            line = header
                .split_whitespace()
                .find_map(|w| w.strip_prefix('+'))
                .and_then(|n| n.split(',').next()?.parse().ok())
                .unwrap_or(0);
        } else if let (Some(path), Some(added)) = (&file, raw.strip_prefix('+')) {
            if let Some(kind) = leftover(added) {
                if found.len() < MAX_LEFTOVERS {
                    found.push(Leftover {
                        path: path.clone(),
                        line,
                        kind: kind.into(),
                        text: added.trim().chars().take(160).collect(),
                    });
                } else {
                    more = true;
                }
            }
            line += 1;
        }
    }
    (found, more)
}

fn is_prose(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".mdx", ".txt", ".rst", ".adoc"].iter().any(|ext| lower.ends_with(ext))
}

/// The kind of leftover an added line is, if it is one.
fn leftover(added: &str) -> Option<&'static str> {
    let t = added.trim();
    if t.starts_with("<<<<<<< ") || t.starts_with(">>>>>>> ") || t == "=======" {
        return Some("conflict marker");
    }
    // Commented out, it is not going to run.
    if t.starts_with("//") || t.starts_with('#') || t.starts_with("/*") || t.starts_with('*') {
        return (t.contains("TODO") || t.contains("FIXME")).then_some("todo");
    }
    const DEBUG: &[&str] = &[
        "console.log(", "console.debug(", "dbg!(", "binding.pry", "byebug", "pdb.set_trace(", "breakpoint()", "var_dump(",
    ];
    if DEBUG.iter().any(|d| t.contains(d)) || t == "debugger" || t == "debugger;" {
        return Some("debug");
    }
    const FOCUSED: &[&str] = &["it.only(", "describe.only(", "test.only(", "fit(", "fdescribe(", "context.only("];
    if FOCUSED.iter().any(|f| t.starts_with(f) || t.contains(&format!(" {f}")) || t.contains(&format!(".{f}"))) {
        return Some("focused test");
    }
    (t.contains("TODO") || t.contains("FIXME")).then_some("todo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_usually_left_in_by_accident_is_found_on_its_line() {
        let patch = "\
diff --git a/src/auth.ts b/src/auth.ts
--- a/src/auth.ts
+++ b/src/auth.ts
@@ -10,0 +11,4 @@
+  console.log(\"token\", token);
+  const ok = check(token);
+  debugger;
+  // TODO: retry
diff --git a/src/auth.test.ts b/src/auth.test.ts
--- a/src/auth.test.ts
+++ b/src/auth.test.ts
@@ -3 +3 @@
-it(\"retries\", () => {
+it.only(\"retries\", () => {
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1,0 +2 @@
+Never leave a console.log( in.
diff --git a/src/merge.rs b/src/merge.rs
--- a/src/merge.rs
+++ b/src/merge.rs
@@ -7,0 +8,2 @@
+<<<<<<< HEAD
+    dbg!(x);
";
        let (found, more) = leftovers_in(patch);
        let got: Vec<_> = found.iter().map(|l| (l.path.as_str(), l.line, l.kind.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("src/auth.ts", 11, "debug"),
                ("src/auth.ts", 13, "debug"),
                ("src/auth.ts", 14, "todo"),
                ("src/auth.test.ts", 3, "focused test"),
                ("src/merge.rs", 8, "conflict marker"),
                ("src/merge.rs", 9, "debug"),
            ],
            "and nothing in prose, nor a removed line"
        );
        assert!(!more);
    }

    #[test]
    fn ordinary_code_is_not_a_leftover() {
        for line in ["const fit = (a) => a;", "logger.info(\"started\")", "describe(\"login\", () => {", "// it.only( is banned here"] {
            assert_eq!(leftover(line), None, "{line}");
        }
    }
}
