//! A GUI app launched from Finder inherits a bare PATH, so `claude`, `gemini`
//! and friends are invisible. Ask the user's login shell what it really has.

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

static ENV: OnceLock<HashMap<String, String>> = OnceLock::new();

pub fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

/// The environment of an interactive login shell, falling back to our own.
pub fn user_env() -> &'static HashMap<String, String> {
    ENV.get_or_init(|| {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        // `env -0` keeps values containing newlines intact.
        if let Ok(out) = Command::new(login_shell())
            .args(["-ilc", "env -0"])
            .output()
        {
            if out.status.success() {
                for chunk in out.stdout.split(|b| *b == 0) {
                    if chunk.is_empty() {
                        continue;
                    }
                    let s = String::from_utf8_lossy(chunk);
                    if let Some((k, v)) = s.split_once('=') {
                        env.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }

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
