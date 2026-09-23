//! A GUI app launched from Finder inherits a bare PATH, so `claude`, `gemini`
//! and friends are invisible. Ask the user's login shell what it really has.

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();

pub fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

/// Printed just before the environment, so whatever the startup files print
/// first — a greeting, a fortune, a warning — is not read as part of it.
/// Glued onto the first record, it cost that variable, and when that was PATH
/// every agent read as not installed.
const MARK: &str = "__VILLAIN_LAYER_ENV__";

/// How long the startup files get. Every pane waits on this answer, and one
/// rc file that waits for input or a network share held all of them.
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Not the user's environment but where the scrape happened to run, or the
/// app's own for one pane — none of them should follow a new pane in.
fn inherited_by_accident(key: &str) -> bool {
    matches!(key, "PWD" | "OLDPWD" | "SHLVL" | "_")
        // A dev build started from a pane of the release one carried its
        // pane id and tokens into every terminal it opened.
        || key.starts_with("VILLAIN_")
}

/// What an interactive login shell prints for `env -0` after [`MARK`], or
/// None if it did not answer in time.
fn ask_login_shell() -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = Command::new(login_shell())
        .args(["-ilc", &format!("printf '\\0{MARK}\\0'; env -0")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read beside the wait: something the rc file started in the background
    // can hold the pipe open after the shell has gone, and reading to the end
    // would then never end.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let deadline = std::time::Instant::now() + SHELL_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("villain-layer: the login shell did not answer in time; using the app's own environment");
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    rx.recv_timeout(std::time::Duration::from_secs(1)).ok()
}

/// The records of `env -0` after the marker.
fn parse_env(out: &[u8]) -> Vec<(String, String)> {
    let mut records = out.split(|b| *b == 0);
    if !records.any(|r| r == MARK.as_bytes()) {
        return Vec::new();
    }
    records
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let s = String::from_utf8_lossy(r);
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// The environment of an interactive login shell, falling back to our own.
pub fn user_env() -> &'static HashMap<String, String> {
    ENV.get_or_init(|| {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        if let Some(out) = ask_login_shell() {
            env.extend(parse_env(&out));
        }
        env.retain(|k, _| !inherited_by_accident(k));

        // The scrape above is not a TTY (`zsh -ilc` pipes stdout), so shells
        // and tools often export TERM=dumb, FORCE_COLOR=0 or NO_COLOR into what
        // we just copied. A pane is a real terminal — those must not follow the
        // child in, or every chalk/Ink CLI (Claude Code, Copilot, …) renders
        // monochrome.
        env.remove("NO_COLOR");
        env.remove("FORCE_COLOR");
        if env.get("CLICOLOR").is_some_and(|v| v == "0") {
            env.remove("CLICOLOR");
        }
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env
    })
}

/// The login shell's PATH if it has already been asked for, without waiting.
///
/// For git, which runs from the first moment of a launch: a poll should not
/// sit behind a 300ms interactive shell, but a commit hook that needs node or
/// a Homebrew tool should find it once the answer is in.
pub fn path_if_ready() -> Option<&'static str> {
    ENV.get().and_then(|env| env.get("PATH")).map(String::as_str)
}

pub fn path() -> String {
    user_env()
        .get("PATH")
        .cloned()
        .unwrap_or_else(|| "/usr/bin:/bin".into())
}

/// Is `program` runnable with the user's real PATH?
pub fn which(program: &str) -> Option<String> {
    for dir in path().split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = std::path::Path::new(dir).join(program);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_startup_files_print_is_not_read_as_the_environment() {
        let out = format!("Welcome back!\nPATH=/wrong\0{MARK}\0PATH=/opt/bin:/usr/bin\0MULTI=a\nb\0\0");
        let env = parse_env(out.as_bytes());
        assert_eq!(env, [("PATH".into(), "/opt/bin:/usr/bin".into()), ("MULTI".into(), "a\nb".into())]);
        assert!(parse_env(b"PATH=/usr/bin\0").is_empty(), "no marker, no answer");
        assert!(inherited_by_accident("VILLAIN_HOOK_TOKEN"));
        assert!(inherited_by_accident("PWD"));
        assert!(!inherited_by_accident("PATH"));
    }

    #[test]
    fn a_pane_env_looks_like_a_real_terminal() {
        let env = user_env();
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert_eq!(env.get("COLORTERM").map(String::as_str), Some("truecolor"));
        assert!(!env.contains_key("NO_COLOR"));
        // FORCE_COLOR=0 is the usual poison from a non-TTY scrape; any other
        // value the user set on purpose is fine, but zero must not survive.
        assert_ne!(env.get("FORCE_COLOR").map(String::as_str), Some("0"));
    }
}
