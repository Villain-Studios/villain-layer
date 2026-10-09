//! Whether a worktree changed between two looks, for a loop (LOOP-4): an
//! agent that ended its turn without touching anything is waiting on
//! someone, and running its checks again would only send it back the
//! failures it already has.

use std::hash::{Hash, Hasher};
use std::path::Path;

use super::run;
use crate::error::Result;

/// A short fingerprint of everything a check could see: the commit, every
/// change against it (staged or not), and each untracked file by size and
/// modification time. Equal fingerprints mean nothing changed.
///
/// The untracked files are listed one by one (`--untracked-files=all`): a
/// new folder is one line in a plain status, and a file added inside it
/// later changed nothing there.
pub fn fingerprint(dir: &Path) -> Result<String> {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    run(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).unwrap_or_default().hash(&mut hash);
    run(dir, &["diff", "HEAD", "--no-ext-diff", "--no-textconv", "--binary"])?.hash(&mut hash);
    let untracked = run(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in untracked.split('\0').filter(|p| !p.is_empty()) {
        path.hash(&mut hash);
        if let Ok(meta) = std::fs::metadata(dir.join(path)) {
            meta.len().hash(&mut hash);
            meta.modified().ok().hash(&mut hash);
        }
    }
    Ok(format!("{:016x}", hash.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::run_for_tests;

    fn repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vl-snap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        run_for_tests(&dir, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run_for_tests(&dir, &["add", "a.txt"]).unwrap();
        run_for_tests(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "one"]).unwrap();
        dir
    }

    #[test]
    fn a_worktree_left_alone_keeps_its_fingerprint() {
        let dir = repo();
        assert_eq!(fingerprint(&dir).unwrap(), fingerprint(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edit_a_new_file_an_edit_to_it_and_a_commit_each_change_it() {
        let dir = repo();
        let mut seen = vec![fingerprint(&dir).unwrap()];
        let mut changed = |what: &str| {
            let now = fingerprint(&dir).unwrap();
            assert!(!seen.contains(&now), "{what} changed nothing");
            seen.push(now);
        };

        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        changed("an edit to a tracked file");
        std::fs::create_dir_all(dir.join("new")).unwrap();
        std::fs::write(dir.join("new/b.txt"), "b\n").unwrap();
        changed("a file in a new folder");
        std::fs::write(dir.join("new/c.txt"), "c\n").unwrap();
        changed("a second file in that folder");
        std::fs::write(dir.join("new/b.txt"), "longer\n").unwrap();
        changed("an edit to an untracked file");
        run_for_tests(&dir, &["add", "-A"]).unwrap();
        run_for_tests(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "two"]).unwrap();
        changed("a commit of it all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_ignored_file_does_not_count() {
        let dir = repo();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        let before = fingerprint(&dir).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/out.bin"), "built\n").unwrap();
        assert_eq!(before, fingerprint(&dir).unwrap(), "a build's output is not the agent's work");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
