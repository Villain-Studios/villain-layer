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
    /// How the app plugs into this CLI for a launch.
    pub integration: Integration,
}

/// How the app plugs into an agent CLI for one launch: how the CLI says
/// whether it is working, asking or done, and how it reaches the app's own
/// MCP server — each done the way that CLI offers, for that launch only,
/// without the app touching the user's own configuration.
///
/// A CLI that cannot say what it is doing is not in the catalogue: from its
/// output alone its state is a guess, and a wrong one — "working" on an agent
/// two days idle — is worse than none. Taken out for that reason, and to put
/// back once they report: Cursor's CLI reads hooks only from fixed files in the
/// home folder and the repository, and has no event for asking. Codex (hooks,
/// enabled per launch with `-c`), Aider (`--notifications-command`, which says
/// only "stopped") and Amp (a plugin, no asking) were not installed to check
/// against, and a wrong flag stops a CLI from starting at all. Their launch
/// details are in the history of this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integration {
    /// Hooks in a settings file given with `--settings`; the server with
    /// `--mcp-config`.
    Claude,
    /// Claude Code's hooks and payloads, in a plugin given with
    /// `--plugin-dir`; the server with `--additional-mcp-config`.
    Copilot,
    /// A plugin forwarding its event bus, and the server, both added to
    /// `OPENCODE_CONFIG_CONTENT`.
    Opencode,
    /// Its window title says what it is doing. The server only in a folder of
    /// the app's own: Gemini CLI takes one from no flag, variable or file
    /// outside its project settings — it refuses a system settings file in a
    /// folder root does not own — and project settings in a worktree would be
    /// a stray file in the user's repository.
    Gemini,
}

/// Accept Claude Code's workspace-trust dialog for a directory up front, and
/// its approval of the app's own MCP server there.
///
/// Claude Code asks whether you trust a folder the first time it starts there,
/// and nothing runs until it is answered. That is a sensible question to ask a
/// person opening an unfamiliar checkout; it is noise when the app just made
/// the folder itself, from the user's own repository, for the task they asked
/// for. So the app answers it, for its own folders only. The same goes for
/// the `.mcp.json` the app writes into a task folder: Claude Code held the
/// server as "pending approval" in every new task until someone said yes.
/// Only the app's own server is approved — never anything else a folder's
/// `.mcp.json` names.
///
/// The record lives in `~/.claude.json`, which Claude Code owns, so this is
/// deliberately conservative: it never creates the file, never touches it if
/// it cannot be parsed, sets exactly those two keys, and writes through a
/// temporary file so a crash mid-write cannot leave Claude Code without a
/// config.
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
///
/// Claude Code rewrites this file all the time, and so could the app from
/// several threads at once — a restore, a spawn, an MCP start_work. One
/// shared temporary name let one writer's rename publish another's half
/// written file, and anything Claude wrote between the read and the rename
/// was lost. So: one writer at a time in this process, a temporary name of
/// its own, and a look just before the rename that nothing changed since the
/// read. The file keeps its permissions, and a symlink stays a symlink.
fn trust_in(config: &Path, dir: &Path) -> bool {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let target = std::fs::canonicalize(config).unwrap_or_else(|_| config.to_path_buf());
    let Some(name) = target.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return false;
    };
    for _ in 0..3 {
        let Ok(text) = std::fs::read_to_string(&target) else {
            return false;
        };
        let Some(out) = with_trust(&text, dir) else {
            return false;
        };
        // Same directory, so the rename is atomic on the same filesystem.
        let tmp = target.with_file_name(format!(".{name}.villain-{}.tmp", uuid::Uuid::new_v4()));
        if std::fs::write(&tmp, out).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        if let Ok(meta) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
        if std::fs::read_to_string(&target).ok().as_deref() != Some(text.as_str()) {
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        if std::fs::rename(&tmp, &target).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
        return true;
    }
    false
}

/// `text` with `dir` trusted and the app's server approved, or None when it
/// already is — or is not a config this should touch.
fn with_trust(text: &str, dir: &Path) -> Option<String> {
    let mut root = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if !root.is_object() {
        return None;
    }

    let key = dir.to_string_lossy().to_string();
    let projects = root.as_object_mut().and_then(|o| {
        o.entry("projects")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
    })?;
    let entry = projects
        .entry(key)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()?;
    let trusted = entry.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true));
    let approved = entry
        .get("enabledMcpjsonServers")
        .and_then(|v| v.as_array())
        .is_some_and(|a| a.iter().any(|n| n.as_str() == Some(crate::mcp::SERVER_NAME)));
    if trusted && approved {
        return None;
    }
    entry.insert("hasTrustDialogAccepted".into(), serde_json::Value::Bool(true));
    if !approved {
        let list = entry
            .entry("enabledMcpjsonServers")
            .or_insert_with(|| serde_json::json!([]));
        match list.as_array_mut() {
            Some(list) => list.push(crate::mcp::SERVER_NAME.into()),
            None => *list = serde_json::json!([crate::mcp::SERVER_NAME]),
        }
    }

    // Pretty, because that is how Claude Code writes it: a compact rewrite
    // would flatten a 2,500-line file the user may well read themselves.
    serde_json::to_string_pretty(&root).ok()
}

/// The shell command a hook runs: post the event to the app's own server,
/// naming the pane from the environment the app started it with — so the same
/// CLI started anywhere else, where those are unset, posts nothing. Always
/// exits 0, so a hook can never fail or block the agent's turn.
///
/// Only a notification is sent as the agent handed it over, since only there
/// does the payload say anything the name does not. The rest post their name
/// and throw the payload away: PostToolUse carries the tool's whole output,
/// megabytes for a big read, sent on every call — and past the server's 2MB
/// the post was refused and the event lost.
fn post_hook(event: &str) -> String {
    let body = if event == "Notification" {
        "--data-binary @-".to_string()
    } else {
        format!("-d '{{\"hook_event_name\":\"{event}\"}}'")
    };
    let drain = if event == "Notification" { "" } else { "cat >/dev/null; " };
    format!(
        "{drain}[ -n \"$VILLAIN_HOOK_URL\" ] && [ -n \"$VILLAIN_PANE\" ] && \
         curl -s -m 2 -o /dev/null -X POST \
         -H \"Authorization: Bearer $VILLAIN_HOOK_TOKEN\" \
         -H 'Content-Type: application/json' \
         {body} \"$VILLAIN_HOOK_URL/$VILLAIN_PANE\"; exit 0"
    )
}

/// The events that say what a Claude-style agent is doing.
///
/// PreToolUse is left out: it holds up every tool call until it returns, and
/// PostToolUse says the same thing a moment later without making anything
/// wait.
const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
];

/// The hooks that make Claude Code say what it is doing, as its settings file.
///
/// Its output cannot: it repaints its prompt every few seconds while it waits,
/// so an agent two days idle read as working.
pub fn claude_hook_settings() -> serde_json::Value {
    let report = |e: &str| {
        serde_json::json!([{
            "matcher": "*",
            "hooks": [{ "type": "command", "command": post_hook(e), "timeout": 5 }]
        }])
    };
    serde_json::json!({
        "hooks": HOOK_EVENTS
            .iter()
            .map(|e| (e.to_string(), report(e)))
            .collect::<serde_json::Map<_, _>>()
    })
}

/// The same hooks for GitHub Copilot CLI, in its own file format.
///
/// It takes Claude Code's event names, and with them sends Claude Code's
/// payloads, so one reading serves both. The file is its own shape all the
/// same: `bash`, not `command`, and a version.
pub fn copilot_hooks() -> serde_json::Value {
    let report = |e: &str| serde_json::json!([{ "type": "command", "bash": post_hook(e), "timeoutSec": 5 }]);
    serde_json::json!({
        "version": 1,
        "hooks": HOOK_EVENTS
            .iter()
            .map(|e| (e.to_string(), report(e)))
            .collect::<serde_json::Map<_, _>>()
    })
}

/// A plugin for OpenCode that forwards its own event bus, reduced to the one
/// word the app wants.
///
/// Every export must be a function or OpenCode rejects the whole file, so
/// nothing else is exported, and it imports nothing. The `event` handler is
/// never awaited, so posting cannot hold the agent up. A subagent's session
/// goes idle when its own piece is done, while the one you are talking to is
/// still at work, so child sessions are not reported on.
pub const OPENCODE_PLUGIN: &str = r#"// Written by Villain Layer, which reads it to tell whether this OpenCode is
// working, asking or done. Rewritten on every launch; edits here are lost.
export const VillainLayer = async () => {
  const url = process.env.VILLAIN_HOOK_URL;
  const pane = process.env.VILLAIN_PANE;
  if (!url || !pane) return {};
  const token = process.env.VILLAIN_HOOK_TOKEN ?? "";
  const say = (state) => {
    fetch(`${url}/${pane}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
      body: JSON.stringify({ state }),
    }).catch(() => {});
  };
  const children = new Set();
  say("started");
  return {
    event: async ({ event }) => {
      const p = event?.properties ?? {};
      if ((event.type === "session.created" || event.type === "session.updated") && p.info?.parentID) {
        children.add(p.info.id);
      }
      if (p.sessionID && children.has(p.sessionID)) return;
      switch (event.type) {
        case "session.status": {
          const t = p.status?.type;
          if (t === "busy" || t === "retry") say("working");
          else if (t === "idle") say("done");
          break;
        }
        case "session.idle":
          say("done");
          break;
        case "permission.asked":
        case "permission.v2.asked":
        case "question.asked":
        case "question.v2.asked":
          say("asking");
          break;
        case "permission.replied":
        case "permission.v2.replied":
        case "question.replied":
        case "question.v2.replied":
          say("working");
          break;
      }
    },
  };
};
"#;

/// The app's own MCP server, as a launch hands it to a CLI.
pub struct Mcp<'a> {
    pub url: &'a str,
    /// A `.mcp.json` naming the server, token and all, readable by the user
    /// alone.
    pub config_file: &'a Path,
}

/// What the app adds to a launch.
#[derive(Debug, Default, PartialEq)]
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Get a launch plugged in: write whatever the CLI loads, and say what to add
/// to its arguments and environment.
///
/// Files go in `dir`, the app's own folder — never a worktree, where they
/// would be untracked changes — except Gemini CLI's project settings, which
/// go in `own_folder` when the agent starts in a folder of the app's own that
/// is not a repository (a task root, a chat), and nowhere otherwise.
/// `opencode_config` is any `OPENCODE_CONFIG_CONTENT` the user already has:
/// what the app needs is added to it, not put in its place.
///
/// The token is read from `VILLAIN_MCP_TOKEN` wherever the CLI will expand a
/// variable, rather than written into another file.
pub fn prepare_launch(
    integration: Integration,
    dir: &Path,
    mcp: Option<&Mcp>,
    own_folder: Option<&Path>,
    opencode_config: Option<&str>,
) -> std::io::Result<Launch> {
    std::fs::create_dir_all(dir)?;
    let json = |v: serde_json::Value| serde_json::to_vec_pretty(&v).unwrap_or_default();
    let mut launch = Launch::default();
    match integration {
        Integration::Claude => {
            let path = dir.join("claude-hooks.json");
            replace_file(&path, &json(claude_hook_settings()), None)?;
            launch.args = vec!["--settings".into(), path.to_string_lossy().into()];
            if let Some(mcp) = mcp {
                // Always the flag, even where a `.mcp.json` sits in the
                // working directory: a task from the one-repo layout has no
                // folder of its own to put one in.
                launch.args.push("--mcp-config".into());
                launch.args.push(mcp.config_file.to_string_lossy().into());
            }
        }
        Integration::Copilot => {
            // A plugin is a folder: its manifest, and the hooks beside it. No
            // `$schema` in the manifest, or Copilot looks for the hooks
            // somewhere else.
            let plugin = dir.join("copilot-plugin");
            std::fs::create_dir_all(&plugin)?;
            replace_file(&plugin.join("plugin.json"), &json(serde_json::json!({ "name": crate::mcp::SERVER_NAME })), None)?;
            replace_file(&plugin.join("hooks.json"), &json(copilot_hooks()), None)?;
            launch.args = vec!["--plugin-dir".into(), plugin.to_string_lossy().into()];
            if let Some(mcp) = mcp {
                // It reads a workspace `.mcp.json` only at a repository's
                // root, so a task root's went unread. `@` marks a path.
                launch.args.push("--additional-mcp-config".into());
                launch.args.push(format!("@{}", mcp.config_file.to_string_lossy()));
            }
        }
        Integration::Opencode => {
            let path = dir.join("opencode-plugin.js");
            replace_file(&path, OPENCODE_PLUGIN.as_bytes(), None)?;
            let spec = format!("file://{}", path.to_string_lossy());
            launch.env = vec![(
                "OPENCODE_CONFIG_CONTENT".into(),
                opencode_config_with(opencode_config, &spec, mcp.map(|m| m.url)),
            )];
        }
        Integration::Gemini => {
            if let Some(folder) = own_folder {
                gemini_project_settings(folder, mcp.map(|m| m.url))?;
            }
        }
    }
    Ok(launch)
}

/// Gemini CLI's project settings for `folder`: the app's server, and the
/// context file the app writes, keeping whatever else is there. Gemini
/// expands `$VAR` in its settings, so the token stays in the environment.
///
/// It reads GEMINI.md and nothing else by default, so the task and chat
/// context the app writes as AGENTS.md went unread, and a Gemini agent
/// started knowing nothing of its ticket.
fn gemini_project_settings(folder: &Path, url: Option<&str>) -> std::io::Result<()> {
    let path = folder.join(".gemini").join("settings.json");
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(url) = url {
        let servers = root
            .as_object_mut()
            .map(|o| o.entry("mcpServers").or_insert_with(|| serde_json::json!({})));
        if let Some(servers) = servers.and_then(|s| s.as_object_mut()) {
            servers.insert(
                crate::mcp::SERVER_NAME.into(),
                serde_json::json!({
                    "httpUrl": url,
                    "headers": { "Authorization": "Bearer $VILLAIN_MCP_TOKEN" }
                }),
            );
        }
    }
    let context = root
        .as_object_mut()
        .map(|o| o.entry("context").or_insert_with(|| serde_json::json!({})));
    if let Some(context) = context.and_then(|c| c.as_object_mut()) {
        context.insert("fileName".into(), serde_json::json!(["AGENTS.md", "GEMINI.md"]));
    }
    std::fs::create_dir_all(folder.join(".gemini"))?;
    replace_file(&path, &serde_json::to_vec_pretty(&root).unwrap_or_default(), None)
}

/// Put `bytes` at `path` whole, or leave it be if it already holds them.
///
/// These files are shared by every pane, and were truncated and rewritten on
/// every launch: a Claude still starting beside the restore of eleven more
/// could read an empty `--settings` — no hooks, so a state read from output,
/// which for Claude is "working" forever. A reader now sees the old file or
/// the new one. `mode` is the new file's from the moment it exists.
pub(crate) fn replace_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|now| now == bytes) {
        return Ok(());
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let written = (|| {
        use std::io::Write;
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        open.open(&tmp)?.write_all(bytes)?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The user's `OPENCODE_CONFIG_CONTENT` with the plugin, and the app's server
/// when there is one, added. One that does not parse as a JSON object is left
/// alone rather than guessed at: losing the plugin costs a state dot, losing
/// their config costs their setup.
///
/// OpenCode fills `{env:NAME}` in from the environment, so the token is not
/// written into the variable.
fn opencode_config_with(existing: Option<&str>, spec: &str, mcp_url: Option<&str>) -> String {
    let mut root = match existing.map(str::trim).filter(|s| !s.is_empty()) {
        None => serde_json::json!({}),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) if v.is_object() => v,
            _ => return text.to_string(),
        },
    };
    let plugins = root
        .as_object_mut()
        .map(|o| o.entry("plugin").or_insert_with(|| serde_json::json!([])));
    if let Some(list) = plugins.and_then(|p| p.as_array_mut()) {
        if !list.iter().any(|p| p.as_str() == Some(spec)) {
            list.push(serde_json::Value::String(spec.to_string()));
        }
    }
    if let Some(url) = mcp_url {
        let servers = root
            .as_object_mut()
            .map(|o| o.entry("mcp").or_insert_with(|| serde_json::json!({})));
        if let Some(servers) = servers.and_then(|s| s.as_object_mut()) {
            servers.insert(
                crate::mcp::SERVER_NAME.into(),
                serde_json::json!({
                    "type": "remote",
                    "url": url,
                    "headers": { "Authorization": "Bearer {env:VILLAIN_MCP_TOKEN}" },
                    "enabled": true
                }),
            );
        }
    }
    root.to_string()
}

/// What a hook's post says the agent is doing now. None when it says nothing
/// new.
pub fn hook_activity(
    integration: Integration,
    payload: &serde_json::Value,
    current: Option<crate::pty::Activity>,
) -> Option<crate::pty::Activity> {
    use crate::pty::Activity;
    match integration {
        Integration::Claude | Integration::Copilot => claude_hook_activity(payload, current),
        Integration::Opencode => match payload.get("state").and_then(|s| s.as_str())? {
            // Loaded: at its prompt, with nothing new — unless an event from
            // the session already beat this post here.
            "started" => current.is_none().then_some(Activity::Idle),
            "working" => Some(Activity::Working),
            "asking" => Some(Activity::Asking),
            "done" => Some(Activity::Done),
            _ => None,
        },
        Integration::Gemini => None,
    }
}

/// What a Claude Code — or Copilot — hook event says the agent is doing.
///
/// `current` is what it said before. Hooks run side by side, and a session's
/// start can land after its first prompt: so it only counts when nothing has
/// been said yet. A notice that it is waiting for input only counts if it was
/// thought to be working — no Stop hook runs for an interrupted turn.
pub fn claude_hook_activity(
    payload: &serde_json::Value,
    current: Option<crate::pty::Activity>,
) -> Option<crate::pty::Activity> {
    use crate::pty::Activity;
    let event = payload.get("hook_event_name").and_then(|e| e.as_str())?;
    match event {
        // Started or resumed: sitting at its prompt with nothing new to show.
        "SessionStart" => current.is_none().then_some(Activity::Idle),
        // A tool finishing beside a question still open — parallel tools, a
        // subagent — is not the question answered. The key that answers it
        // is: `typed` turns asking into working.
        "PostToolUse" | "PostToolUseFailure" if current == Some(Activity::Asking) => None,
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => Some(Activity::Working),
        "Stop" => Some(Activity::Done),
        "Notification" => {
            let kind = payload.get("notification_type").and_then(|k| k.as_str());
            let message = payload
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let asking = match kind {
                Some(k) => matches!(k, "permission_prompt" | "elicitation_dialog"),
                // Older releases say only in words.
                None => message.contains("permission") || message.contains("approval"),
            };
            let waiting = kind == Some("idle_prompt") || message.contains("waiting for your input");
            if asking {
                Some(Activity::Asking)
            } else if waiting && current == Some(Activity::Working) {
                Some(Activity::Done)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// What Gemini CLI's window title says it is doing.
///
/// It keeps the title on its state — "◇  Ready", "✋  Action Required",
/// "✦  <what it is thinking>", "⏲  Working…" — from the same place its own
/// UI draws from, so it also covers a denied tool, an Esc or an API error,
/// which its hooks do not. Only the leading mark is read; the words after it
/// are the model's.
pub fn gemini_title_activity(title: &str) -> Option<crate::pty::Activity> {
    use crate::pty::Activity;
    match title.trim_start().chars().next()? {
        '◇' => Some(Activity::Done),
        '✋' => Some(Activity::Asking),
        '✦' | '⏲' => Some(Activity::Working),
        _ => None,
    }
}

/// The title reader for a CLI that reports through its title.
pub fn title_reader(integration: Integration) -> Option<fn(&str) -> Option<crate::pty::Activity>> {
    match integration {
        Integration::Gemini => Some(gemini_title_activity),
        _ => None,
    }
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
        integration: Integration::Claude,
        resume_args: Some(&["--continue"]),
        session_store: Some(SessionStore::SlugUnderHome {
            dir: ".claude/projects",
            ext: "jsonl",
        }),
    },
    AgentDef {
        id: "gemini",
        name: "Gemini CLI",
        program: "gemini",
        base_args: &[],
        prompt: PromptMode::Flag("-i"),
        integration: Integration::Gemini,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "opencode",
        name: "OpenCode",
        program: "opencode",
        base_args: &[],
        prompt: PromptMode::Typed,
        integration: Integration::Opencode,
        resume_args: None,
        session_store: None,
    },
    AgentDef {
        id: "copilot",
        name: "GitHub Copilot CLI",
        program: "copilot",
        base_args: &[],
        // `-i` starts the TUI and runs the prompt; `-p` would answer once and
        // exit, which is no use for a pane you are going to talk to.
        prompt: PromptMode::Flag("-i"),
        integration: Integration::Copilot,
        // It has `--continue`, but that resumes the most recent session
        // anywhere rather than the most recent one *here*, and its transcripts
        // are filed under a UUID with no directory in the name. Neither half
        // of a per-directory resume is available, so it is not offered.
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
        let before = r#"{"projects":{"/work":{"hasTrustDialogAccepted":true,"enabledMcpjsonServers":["villain-layer"]}}}"#;
        std::fs::write(&config, before).unwrap();

        assert!(!trust_in(&config, Path::new("/work")));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_linked_private_config_stays_linked_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = sandbox("link");
        let real = dir.join("dotfiles-claude.json");
        std::fs::write(&real, r#"{"projects":{}}"#).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = dir.join(".claude.json");
        std::os::unix::fs::symlink(&real, &config).unwrap();

        assert!(trust_in(&config, Path::new("/work")));
        assert!(std::fs::symlink_metadata(&config).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(v["projects"]["/work"]["hasTrustDialogAccepted"], true);
        // Nothing of the app's is left beside it.
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(left.len(), 2, "{left:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_apps_own_server_is_approved_beside_what_was() {
        let dir = sandbox("approve");
        let config = dir.join(".claude.json");
        std::fs::write(
            &config,
            r#"{"projects":{"/work":{"hasTrustDialogAccepted":true,"enabledMcpjsonServers":["theirs"],"disabledMcpjsonServers":["other"]}}}"#,
        )
        .unwrap();
        assert!(trust_in(&config, Path::new("/work")));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(v["projects"]["/work"]["enabledMcpjsonServers"], serde_json::json!(["theirs", "villain-layer"]));
        assert_eq!(v["projects"]["/work"]["disabledMcpjsonServers"], serde_json::json!(["other"]));
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
        assert!(!pretrust("gemini", Path::new("/work")));
        assert!(!pretrust("copilot", Path::new("/work")));
    }

    #[test]
    fn claude_hooks_say_what_the_agent_is_doing() {
        use crate::pty::Activity;
        let ev = |v: serde_json::Value, now| claude_hook_activity(&v, now);
        use serde_json::json as j;
        assert_eq!(ev(j!({"hook_event_name": "UserPromptSubmit"}), None), Some(Activity::Working));
        assert_eq!(ev(j!({"hook_event_name": "Stop"}), Some(Activity::Working)), Some(Activity::Done));
        assert_eq!(ev(j!({"hook_event_name": "SessionStart", "source": "resume"}), None), Some(Activity::Idle));
        assert_eq!(
            ev(j!({"hook_event_name": "Notification", "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}), Some(Activity::Working)),
            Some(Activity::Asking)
        );
        assert_eq!(
            ev(j!({"hook_event_name": "Notification", "message": "Claude needs your permission to use Bash"}), None),
            Some(Activity::Asking)
        );
        // Waiting for input is news only when it was thought to be working.
        let idle = j!({"hook_event_name": "Notification", "notification_type": "idle_prompt", "message": "Claude is waiting for your input"});
        assert_eq!(ev(idle.clone(), Some(Activity::Working)), Some(Activity::Done));
        assert_eq!(ev(idle, Some(Activity::Idle)), None);
        // Another tool finishing while a question is open does not answer it.
        assert_eq!(ev(j!({"hook_event_name": "PostToolUse"}), Some(Activity::Asking)), None);
        assert_eq!(ev(j!({"hook_event_name": "PostToolUse"}), Some(Activity::Working)), Some(Activity::Working));
        assert_eq!(ev(j!({"hook_event_name": "SubagentStop"}), None), None);
        assert_eq!(ev(j!({}), None), None);
    }

    #[test]
    fn every_hook_posts_for_its_pane_and_never_fails_the_turn() {
        let claude = claude_hook_settings();
        let copilot = copilot_hooks();
        for e in HOOK_EVENTS {
            let c = claude["hooks"][e][0]["hooks"][0]["command"].as_str().unwrap();
            let p = copilot["hooks"][e][0]["bash"].as_str().unwrap();
            for cmd in [c, p] {
                assert!(cmd.contains("$VILLAIN_HOOK_URL/$VILLAIN_PANE"), "{e}");
                assert!(cmd.ends_with("exit 0"), "{e}");
            }
        }
        assert!(claude["hooks"].get("PreToolUse").is_none());
        assert_eq!(copilot["version"], 1);
        // A tool's output stays where it is; a notification's words come along.
        let after_tool = claude["hooks"]["PostToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(!after_tool.contains("@-") && after_tool.contains(r#"{"hook_event_name":"PostToolUse"}"#));
        let notice = claude["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(notice.contains("--data-binary @-"));
    }

    #[test]
    fn a_session_start_that_lands_late_does_not_undo_the_prompt() {
        use crate::pty::Activity;
        let start = serde_json::json!({"hook_event_name": "SessionStart", "source": "new"});
        assert_eq!(claude_hook_activity(&start, None), Some(Activity::Idle));
        assert_eq!(claude_hook_activity(&start, Some(Activity::Working)), None);
    }

    #[test]
    fn opencode_says_one_word_and_the_plugin_is_added_to_the_users_config() {
        use crate::pty::Activity;
        let say = |w: &str, now| hook_activity(Integration::Opencode, &serde_json::json!({ "state": w }), now);
        assert_eq!(say("working", None), Some(Activity::Working));
        assert_eq!(say("asking", Some(Activity::Working)), Some(Activity::Asking));
        assert_eq!(say("done", Some(Activity::Working)), Some(Activity::Done));
        assert_eq!(say("started", None), Some(Activity::Idle));
        assert_eq!(say("started", Some(Activity::Working)), None);

        let spec = "file:///app/opencode-plugin.js";
        assert_eq!(opencode_config_with(None, spec, None), r#"{"plugin":["file:///app/opencode-plugin.js"]}"#);
        let theirs = r#"{"model":"x","plugin":["their-plugin"],"mcp":{"theirs":{"type":"local"}}}"#;
        let merged: serde_json::Value =
            serde_json::from_str(&opencode_config_with(Some(theirs), spec, Some("http://127.0.0.1:9/mcp"))).unwrap();
        assert_eq!(merged["model"], "x");
        assert_eq!(merged["plugin"], serde_json::json!(["their-plugin", spec]));
        // Their servers stay; ours is added, its token left to the environment.
        assert_eq!(merged["mcp"]["theirs"]["type"], "local");
        assert_eq!(merged["mcp"]["villain-layer"]["url"], "http://127.0.0.1:9/mcp");
        assert_eq!(merged["mcp"]["villain-layer"]["headers"]["Authorization"], "Bearer {env:VILLAIN_MCP_TOKEN}");
        // Twice is still once.
        let again = opencode_config_with(Some(&merged.to_string()), spec, None);
        assert_eq!(again.matches(spec).count(), 1);
        // Not ours to fix.
        assert_eq!(opencode_config_with(Some("not json"), spec, None), "not json");
    }

    #[test]
    fn the_plugin_exports_only_the_one_function() {
        // Anything else exported makes OpenCode refuse the whole file.
        assert_eq!(OPENCODE_PLUGIN.matches("export ").count(), 1);
        assert!(OPENCODE_PLUGIN.contains("export const VillainLayer = async"));
        assert!(!OPENCODE_PLUGIN.contains("import "));
    }

    #[test]
    fn gemini_is_read_from_the_mark_in_its_title() {
        use crate::pty::Activity;
        assert_eq!(gemini_title_activity("◇  Ready (villain-layer)      "), Some(Activity::Done));
        assert_eq!(gemini_title_activity("✋  Action Required (api)"), Some(Activity::Asking));
        assert_eq!(gemini_title_activity("✦  Reading the auth module"), Some(Activity::Working));
        assert_eq!(gemini_title_activity("⏲  Working…"), Some(Activity::Working));
        assert_eq!(gemini_title_activity("Gemini CLI"), None);
        assert_eq!(gemini_title_activity(""), None);
    }

    #[test]
    fn each_cli_is_handed_its_hooks_and_the_server_its_own_way() {
        let dir = std::env::temp_dir().join(format!("vl-launch-{}", uuid::Uuid::new_v4()));
        let own = dir.join("task-root");
        std::fs::create_dir_all(&own).unwrap();
        let config = dir.join(".mcp.json");
        let mcp = Mcp { url: "http://127.0.0.1:9/mcp", config_file: &config };
        let cfg = config.to_string_lossy().to_string();

        let c = prepare_launch(Integration::Claude, &dir, Some(&mcp), None, None).unwrap();
        assert_eq!(c.args[0], "--settings");
        assert!(std::path::Path::new(&c.args[1]).is_file());
        assert_eq!(c.args[2..], ["--mcp-config".to_string(), cfg.clone()]);

        let p = prepare_launch(Integration::Copilot, &dir, Some(&mcp), None, None).unwrap();
        assert_eq!(p.args[0], "--plugin-dir");
        assert!(std::path::Path::new(&p.args[1]).join("hooks.json").is_file());
        assert!(std::path::Path::new(&p.args[1]).join("plugin.json").is_file());
        assert_eq!(p.args[2..], ["--additional-mcp-config".to_string(), format!("@{cfg}")]);

        let o = prepare_launch(Integration::Opencode, &dir, Some(&mcp), None, None).unwrap();
        assert!(o.args.is_empty());
        assert_eq!(o.env[0].0, "OPENCODE_CONFIG_CONTENT");
        assert!(o.env[0].1.contains("villain-layer"));
        assert!(dir.join("opencode-plugin.js").is_file());

        // Gemini: in a folder of the app's own, beside what is already there.
        let settings = own.join(".gemini/settings.json");
        std::fs::create_dir_all(own.join(".gemini")).unwrap();
        std::fs::write(&settings, r#"{"ui":{"theme":"x"}}"#).unwrap();
        let g = prepare_launch(Integration::Gemini, &dir, Some(&mcp), Some(&own), None).unwrap();
        assert_eq!(g, Launch::default());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(v["ui"]["theme"], "x");
        assert_eq!(v["mcpServers"]["villain-layer"]["httpUrl"], "http://127.0.0.1:9/mcp");
        assert_eq!(v["mcpServers"]["villain-layer"]["headers"]["Authorization"], "Bearer $VILLAIN_MCP_TOKEN");
        // And it reads the context the app writes, which is not GEMINI.md.
        assert_eq!(v["context"]["fileName"], serde_json::json!(["AGENTS.md", "GEMINI.md"]));
        // In a worktree: nothing written at all.
        let wt = dir.join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        prepare_launch(Integration::Gemini, &dir, Some(&mcp), None, None).unwrap();
        assert!(!wt.join(".gemini").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
