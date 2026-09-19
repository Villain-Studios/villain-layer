//! The catalogue of coding-agent CLIs we know how to launch, and how each one
//! wants its opening prompt.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::shellenv;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// `agent "prompt"`
    Positional,
    /// `agent <flag> "prompt"`
    Flag(&'static str),
    /// No prompt argument; type it into the TUI after it starts.
    Typed,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentDef {
    pub id: &'static str,
    pub name: &'static str,
    pub program: &'static str,
    pub base_args: &'static [&'static str],
    pub prompt: PromptMode,
    /// How to pick the previous conversation back up in the same directory.
    /// None means the CLI has no such flag, so a restart starts fresh.
    pub resume_args: Option<&'static [&'static str]>,
    /// Where the CLI keeps its transcripts, so the app can tell whether there
    /// is anything to resume before offering to.
    pub session_store: Option<SessionStore>,
    /// Flag for pointing the agent at an MCP config file outside its working
    /// directory. Needed when the cwd is a git worktree, where dropping a
    /// generated `.mcp.json` would show up as an untracked change.
    pub mcp_config_flag: Option<&'static str>,
}

/// Accept Claude Code's workspace-trust dialog for a directory up front.
///
/// Claude Code asks whether you trust a folder the first time it starts there,
/// and nothing runs until it is answered. That is a sensible question to ask a
/// person opening an unfamiliar checkout; it is noise when the app just made
/// the folder itself, from the user's own repository, for the task they asked
/// for. So the app answers it, for its own folders only.
///
/// The record lives in `~/.claude.json`, which Claude Code owns, so this is
/// deliberately conservative: it never creates the file, never touches it if
/// it cannot be parsed, sets exactly one key, and writes through a temporary
/// file so a crash mid-write cannot leave Claude Code without a config.
///
/// Returns whether anything was written.
pub fn pretrust(agent_id: &str, dir: &Path) -> bool {
    if agent_id != "claude" {
        return false;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    trust_in(&PathBuf::from(home).join(".claude.json"), dir)
}

/// The part of [`pretrust`] that does not depend on where home is.
fn trust_in(config: &Path, dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(config) else {
        return false;
    };
    let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    if !root.is_object() {
        return false;
    }

    let key = dir.to_string_lossy().to_string();
    let projects = root
        .as_object_mut()
        .and_then(|o| {
            o.entry("projects")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        });
    let Some(projects) = projects else {
        return false;
    };

    let entry = projects
        .entry(key)
        .or_insert_with(|| serde_json::json!({}));
    let Some(entry) = entry.as_object_mut() else {
        return false;
    };
    if entry.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true)) {
        return false;
    }
    entry.insert("hasTrustDialogAccepted".into(), serde_json::Value::Bool(true));

    // Pretty, because that is how Claude Code writes it: a compact rewrite
    // would flatten a 2,500-line file the user may well read themselves.
    let Ok(out) = serde_json::to_string_pretty(&root) else {
        return false;
    };
    // Same directory, so the rename is atomic on the same filesystem.
    let tmp = config.with_extension("json.villain-tmp");
    if std::fs::write(&tmp, out).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, &config).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// How to find an agent's saved conversations for a working directory.
#[derive(Debug, Clone, Copy)]
pub enum SessionStore {
    /// `~/<dir>/<cwd with non-alphanumerics replaced by dashes>/*.<ext>`
    SlugUnderHome { dir: &'static str, ext: &'static str },
}

pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        id: "claude",
        name: "Claude Code",
        program: "claude",
        base_args: &[],
        prompt: PromptMode::Positional,
        mcp_config_flag: Some("--mcp-config"),
        resume_args: Some(&["--continue"]),
        session_store: Some(SessionStore::SlugUnderHome {
            dir: ".claude/projects",
            ext: "jsonl",
        }),
    },
    AgentDef {
        id: "codex",
        name: "Codex",
        program: "codex",
        base_args: &[],
        prompt: PromptMode::Positional,
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "gemini",
        name: "Gemini CLI",
        program: "gemini",
        base_args: &[],
        prompt: PromptMode::Flag("-i"),
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "cursor",
        name: "Cursor CLI",
        program: "cursor-agent",
        base_args: &[],
        prompt: PromptMode::Positional,
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "opencode",
        name: "OpenCode",
        program: "opencode",
        base_args: &[],
        prompt: PromptMode::Typed,
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "amp",
        name: "Amp",
        program: "amp",
        base_args: &[],
        prompt: PromptMode::Typed,
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "aider",
        name: "Aider",
        program: "aider",
        base_args: &[],
        prompt: PromptMode::Typed,
        mcp_config_flag: None,
        resume_args: None,
        session_store: None,
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    pub id: String,
    pub name: String,
    pub program: String,
    pub installed: bool,
    pub path: Option<String>,
}

pub fn find(id: &str) -> Option<&'static AgentDef> {
    AGENTS.iter().find(|a| a.id == id)
}

/// The catalogue annotated with what is actually on the user's PATH.
pub fn available() -> Vec<AgentStatus> {
    AGENTS
        .iter()
        .map(|a| {
            let path = shellenv::which(a.program);
            AgentStatus {
                id: a.id.to_string(),
                name: a.name.to_string(),
                program: a.program.to_string(),
                installed: path.is_some(),
                path,
            }
        })
        .collect()
}

/// Argv and any text that must be typed in after start-up.
pub fn launch_args(def: &AgentDef, prompt: Option<&str>) -> (Vec<String>, Option<String>) {
    let mut args: Vec<String> = def.base_args.iter().map(|s| s.to_string()).collect();
    let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) else {
        return (args, None);
    };

    match def.prompt {
        PromptMode::Positional => {
            args.push(prompt.to_string());
            (args, None)
        }
        PromptMode::Flag(flag) => {
            args.push(flag.to_string());
            args.push(prompt.to_string());
            (args, None)
        }
        PromptMode::Typed => (args, Some(prompt.to_string())),
    }
}

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
                if entry.path().extension().and_then(|e| e.to_str()) != Some(ext) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn only_agents_that_can_resume_are_considered() {
        let claude = find("claude").unwrap();
        assert!(claude.resume_args.is_some());
        assert!(claude.session_store.is_some());

        // Nothing else claims support until it has been verified.
        for a in AGENTS.iter().filter(|a| a.id != "claude") {
            assert!(a.resume_args.is_none(), "{} should not claim resume", a.id);
        }
    }

    #[test]
    fn a_directory_with_no_history_offers_nothing() {
        assert!(resumable("/nonexistent/path/that/has/never/been/used").is_empty());
    }

    /// Its own sandbox per test: these write files, and a shared path would
    /// let one failure poison the next run.
    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("villain-trust-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accepting_for_a_folder_leaves_the_rest_of_the_config_alone() {
        let dir = sandbox("adds");
        let config = dir.join(".claude.json");
        std::fs::write(
            &config,
            r#"{"autoUpdates":true,"projects":{"/other":{"hasTrustDialogAccepted":true,"lastCost":7}}}"#,
        )
        .unwrap();

        assert!(trust_in(&config, Path::new("/work/new-task")));

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["projects"]["/work/new-task"]["hasTrustDialogAccepted"], true);
        // Everything already there survives, untouched.
        assert_eq!(v["autoUpdates"], true);
        assert_eq!(v["projects"]["/other"]["lastCost"], 7);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_already_accepted_is_not_rewritten() {
        let dir = sandbox("noop");
        let config = dir.join(".claude.json");
        let before = r#"{"projects":{"/work":{"hasTrustDialogAccepted":true}}}"#;
        std::fs::write(&config, before).unwrap();

        assert!(!trust_in(&config, Path::new("/work")));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_we_cannot_read_is_left_exactly_as_it_was() {
        let dir = sandbox("hostile");

        // Claude Code owns this file: if it is missing, do not invent one.
        assert!(!trust_in(&dir.join("absent.json"), Path::new("/work")));
        assert!(!dir.join("absent.json").exists());

        // And if it is there but not JSON, do not overwrite it.
        let broken = dir.join("broken.json");
        std::fs::write(&broken, "{not json at all").unwrap();
        assert!(!trust_in(&broken, Path::new("/work")));
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{not json at all");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_other_cli_keeps_its_state_in_that_file() {
        assert!(!pretrust("codex", Path::new("/work")));
        assert!(!pretrust("gemini", Path::new("/work")));
    }
}
