//! One-shot `claude -p` runs: a prompt in, a streamed answer out, with no
//! live agent session disturbed and nothing written or run.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::shellenv;

/// A cheap one-shot `claude -p` run: no tools, no MCP, streamed text deltas.
///
/// Shared by PR drafting and issue/epic description polish — both are
/// summarising jobs that must not disturb a working agent session.
pub(crate) fn oneshot_haiku(
    program: impl AsRef<std::path::Path>,
    cwd: impl AsRef<std::path::Path>,
    prompt: &str,
    on_delta: impl FnMut(&str),
) -> Result<String> {
    // Cheap and fast: this is a summarising job, not a reasoning one.
    // Everything it needs is in the prompt, and nothing in it is worth
    // thinking about first: the thinking block is dead time the reader
    // spends watching a spinner.
    oneshot(program, cwd, Oneshot { model: "haiku", read: false, think: false }, prompt, on_delta)
}

/// What a one-shot run may do beyond answering the prompt.
pub(crate) struct Oneshot<'a> {
    /// A model alias `claude --model` takes: "haiku", "sonnet", "opus".
    pub model: &'a str,
    /// Read the files under `cwd` (Read, Glob, Grep). Nothing that writes or
    /// runs is ever allowed.
    pub read: bool,
    pub think: bool,
}

/// A one-shot `claude -p` run with no MCP, streamed text deltas.
pub(crate) fn oneshot(
    program: impl AsRef<std::path::Path>,
    cwd: impl AsRef<std::path::Path>,
    how: Oneshot,
    prompt: &str,
    mut on_delta: impl FnMut(&str),
) -> Result<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let env = shellenv::user_env().clone();
    let mut command = Command::new(program.as_ref());
    command.args([
        "-p",
        "--model",
        how.model,
        // Connecting to MCP servers is the single largest part of a cold
        // start, and this run needs none of them.
        "--strict-mcp-config",
        // Streamed, so the description appears as it is written rather
        // than all at once at the end.
        "--output-format",
        "stream-json",
        "--include-partial-messages",
        "--verbose",
        // Without this it may go reading the repository and turn seconds
        // into minutes, where the prompt already holds what it needs.
        "--disallowed-tools",
        "Bash",
        "Edit",
        "Write",
        "NotebookEdit",
        "WebFetch",
        "WebSearch",
        // Subagents, under both the names Claude Code has given them.
        "Task",
        "Agent",
    ]);
    if !how.read {
        command.args(["Read", "Glob", "Grep"]);
    }
    command
        .current_dir(cwd.as_ref())
        .env_clear()
        .envs(&env);
    if !how.think {
        command.env("MAX_THINKING_TOKENS", "0");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(format!("could not run claude: {e}")))?;

    // Both pipes are pumped on their own threads. The prompt can be larger than
    // a pipe buffer, so writing it inline would block before claude has said
    // anything — and each side would then be waiting on the other.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Other("claude took no input".into()))?;
    let prompt = prompt.to_string();
    std::thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
    });

    let errors = child.stderr.take().map(|e| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(e), &mut buf);
            buf
        })
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Other("claude produced no output".into()))?;

    let mut streamed = String::new();
    let mut result = None;
    let mut reported = None;
    for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                let delta = &v["event"]["delta"];
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    continue;
                }
                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                    streamed.push_str(text);
                    on_delta(text);
                }
            }
            Some("result") => {
                let text = v.get("result").and_then(Value::as_str).unwrap_or_default();
                if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                    reported = Some(text.to_string());
                } else {
                    result = Some(text.to_string());
                }
            }
            _ => {}
        }
    }

    let status = child
        .wait()
        .map_err(|e| Error::Other(format!("claude did not finish: {e}")))?;
    let stderr = errors.and_then(|h| h.join().ok()).unwrap_or_default();

    if let Some(why) = reported {
        return Err(Error::Other(format!("claude: {}", why.trim())));
    }
    if !status.success() {
        let why = stderr.trim();
        return Err(Error::Other(if why.is_empty() {
            "claude stopped without an answer".into()
        } else {
            format!("claude: {why}")
        }));
    }
    // The final message is authoritative; the deltas are what was shown.
    Ok(result.unwrap_or(streamed).trim().to_string())
}
