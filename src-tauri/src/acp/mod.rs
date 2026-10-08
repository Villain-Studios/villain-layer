//! Agents over the Agent Client Protocol (ACP-*, agentclientprotocol.com).
//!
//! An ACP agent is a subprocess the app speaks JSON-RPC to over its stdin and
//! stdout, one message a line. It has no terminal: the app keeps the
//! conversation (`conversation.rs`) and the window draws it. The pane around
//! it is an ordinary one (`pty.rs`, `pty/acp.rs`), so everything that lists,
//! counts, stops, restores or reads panes treats it like any other. What it
//! is doing comes from the protocol itself (ACP-2): a prompt in flight is
//! working, an open permission question is asking, a turn's end is done.
//!
//! `Conn` is the protocol, and runs nothing: it is handed the agent's lines
//! and gives back what to send, which is what lets the tests drive it with
//! no process at all. `start` puts a process behind it, with a thread each
//! to write, read, keep the end of stderr and wait for the exit, as a
//! terminal pane has. Nothing here runs on the async runtime or the main
//! thread.

mod conn;
mod conversation;
mod entry;
mod process;
mod read;
mod user;

use serde::Serialize;

pub use conn::Conn;
pub use entry::{Changes, Command, Setting, Usage};
pub use process::start;

use crate::pty::Activity;

/// What the app's MCP server is called, where it is, and the token for it:
/// given to the agent in `session/new` (ACP-3).
pub struct Mcp {
    pub name: String,
    pub url: String,
    pub token: String,
}

/// Which conversation a launch picks up (ACP-9).
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    New,
    /// The newest the agent has for the folder.
    Newest,
    /// One by the id the agent gave it.
    Id(String),
}

/// Told the conversation's id once the agent gives it: the pane's id, then
/// the conversation's.
pub type OnSession = Box<dyn Fn(&str, &str) + Send + Sync>;

/// What to open once an ACP agent is running: the process itself is the
/// pane's to start (`start`).
pub struct Launch {
    pub mcp: Option<Mcp>,
    pub pick: Pick,
    /// The opening prompt, sent once the conversation is open.
    pub prompt: Option<String>,
    pub on_session: Option<OnSession>,
}

/// What the connection tells the pane it runs in. `pty/acp.rs` is the one
/// that matters; the tests have their own.
pub trait Pane: Send + Sync {
    fn report(&self, activity: Activity);
    /// More of the conversation's text copy (ACP-7).
    fn print(&self, text: &str);
    /// The conversation changed: the window is told, if it is looking.
    fn changed(&self);
    fn topic(&self, topic: String);
    fn session(&self, id: &str);
    /// A turn failed with this: it may be a usage limit (STATE-6).
    fn failed(&self, message: &str);
    fn exited(&self, code: Option<i32>);
}

/// What the window draws an ACP pane from.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    #[serde(flatten)]
    pub changes: Changes,
    /// The agent's own name and version, once it has said.
    pub agent: Option<String>,
    /// The conversation is open and takes prompts.
    pub ready: bool,
    /// A turn is running.
    pub busy: bool,
    /// Prompts waiting for the running turn to end.
    pub queued: usize,
    pub settings: Vec<Setting>,
    pub commands: Vec<Command>,
    pub usage: Option<Usage>,
    pub exited: bool,
}
