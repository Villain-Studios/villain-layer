//! The conversations each CLI keeps on disk, by working directory: what
//! can be resumed, and where.

use serde::Serialize;

use super::{find, SessionStore, AGENTS};

/// A conversation this agent could pick up again in `cwd`.
#[derive(Debug, Clone, Serialize)]
pub struct Resumable {
    pub agent_id: String,
    pub name: String,
    pub sessions: usize,
    /// Unix seconds of the most recent transcript, for "last active" in the UI.
    pub last_active: Option<i64>,
}

/// Claude Code and friends key their transcripts by working directory, with
/// every non-alphanumeric character turned into a dash.
fn slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Where `agent_id` keeps its transcripts for `cwd`, if it keeps them by folder.
pub(crate) fn session_dir(agent_id: &str, cwd: &str) -> Option<std::path::PathBuf> {
    #[allow(deprecated)]
    let home = std::env::home_dir()?;
    let SessionStore::SlugUnderHome { dir, .. } = find(agent_id)?.session_store?;
    Some(home.join(dir).join(slug(cwd)))
}

/// Which agents have something to resume in this directory.
///
/// Read from each CLI's own transcript store rather than remembered by the
/// app, so it stays true even for sessions the app did not start.
pub fn resumable(cwd: &str) -> Vec<Resumable> {
    #[allow(deprecated)]
    let home = match std::env::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    AGENTS
        .iter()
        .filter(|a| a.resume_args.is_some())
        .filter_map(|a| {
            let SessionStore::SlugUnderHome { dir, ext } = a.session_store?;
            let store = home.join(dir).join(slug(cwd));

            let mut newest: Option<i64> = None;
            let mut count = 0usize;
            for entry in std::fs::read_dir(&store).ok()?.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) != Some(ext) || !continuable(&entry.path()) {
                    continue;
                }
                count += 1;
                if let Ok(secs) = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64)
                {
                    newest = Some(newest.map_or(secs, |n: i64| n.max(secs)));
                }
            }
            (count > 0).then(|| Resumable {
                agent_id: a.id.to_string(),
                name: a.name.to_string(),
                sessions: count,
                last_active: newest,
            })
        })
        .collect()
}

/// Whether a saved session is a conversation `--continue` picks up. Claude
/// Code marks each entry with how it was started, and skips one-shot runs
/// (`claude -p`, `entrypoint: sdk-cli`). The app's own PR description draft
/// is one, run in the task folder: counted, it made the task folder look
/// like where the conversation was, the agent was restarted there, and
/// Claude said "No conversation found to continue" while the real one sat
/// in the repo's folder. The first entry that says is enough; a transcript
/// that never says is an older CLI's, and counts.
fn continuable(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = Vec::with_capacity(16 * 1024);
    if std::fs::File::open(path).and_then(|f| f.take(16 * 1024).read_to_end(&mut head)).is_err() {
        return true;
    }
    let head = String::from_utf8_lossy(&head);
    let Some(at) = head.find("\"entrypoint\":\"") else { return true };
    !head[at + "\"entrypoint\":\"".len()..].starts_with("sdk")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_one_shot_run_is_not_a_conversation_to_resume() {
        let dir = std::env::temp_dir().join(format!("vl-sessions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let draft = write("a.jsonl", "{\"type\":\"queue-operation\"}\n{\"entrypoint\":\"sdk-cli\",\"cwd\":\"/t\"}\n");
        let chat = write("b.jsonl", "{\"type\":\"summary\"}\n{\"entrypoint\":\"cli\",\"cwd\":\"/t/api\"}\n");
        let old = write("c.jsonl", "{\"type\":\"user\",\"cwd\":\"/t\"}\n");
        assert!(!continuable(&draft), "a claude -p run");
        assert!(continuable(&chat));
        assert!(continuable(&old), "an older CLI that never said");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slugs_a_working_directory_the_way_the_clis_do() {
        // Matches the directories Claude Code actually creates.
        assert_eq!(
            slug("/Users/you/code/villain-layer"),
            "-Users-you-code-villain-layer"
        );
        // A dot becomes a dash too, which is why hidden folders double up.
        assert_eq!(
            slug("/Users/you/.villain-worktrees/ACME-123/api"),
            "-Users-you--villain-worktrees-ACME-123-api"
        );
    }

    #[test]
    fn a_directory_with_no_history_offers_nothing() {
        assert!(resumable("/nonexistent/path/that/has/never/been/used").is_empty());
    }
}
