//! The catalogue of coding-agent CLIs we know how to launch, and how each one
//! wants its opening prompt.

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
}

pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        id: "claude",
        name: "Claude Code",
        program: "claude",
        base_args: &[],
        prompt: PromptMode::Positional,
    },
    AgentDef {
        id: "codex",
        name: "Codex",
        program: "codex",
        base_args: &[],
        prompt: PromptMode::Positional,
    },
    AgentDef {
        id: "gemini",
        name: "Gemini CLI",
        program: "gemini",
        base_args: &[],
        prompt: PromptMode::Flag("-i"),
    },
    AgentDef {
        id: "cursor",
        name: "Cursor CLI",
        program: "cursor-agent",
        base_args: &[],
        prompt: PromptMode::Positional,
    },
    AgentDef {
        id: "opencode",
        name: "OpenCode",
        program: "opencode",
        base_args: &[],
        prompt: PromptMode::Typed,
    },
    AgentDef {
        id: "amp",
        name: "Amp",
        program: "amp",
        base_args: &[],
        prompt: PromptMode::Typed,
    },
    AgentDef {
        id: "aider",
        name: "Aider",
        program: "aider",
        base_args: &[],
        prompt: PromptMode::Typed,
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
