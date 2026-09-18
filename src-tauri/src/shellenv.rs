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

        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env
    })
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
