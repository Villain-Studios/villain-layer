//! What a conversation is made of, as the window is sent it (`AcpView` and
//! `AcpEntry` in `src/lib/types-acp.ts`).

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    /// What the user sent. `queued` while it waits for the turn before it to
    /// end (ACP-4); `dropped` when Stop ended that turn and it was never sent.
    User { text: String, queued: bool, dropped: bool },
    Agent { text: String },
    Thought { text: String },
    Tool {
        id: String,
        title: String,
        /// The protocol's `kind`: read, edit, execute, search, fetch, …
        tool: String,
        /// pending, in_progress, completed or failed.
        status: String,
        content: Vec<ToolContent>,
        /// `path` or `path:line`, absolute.
        locations: Vec<String>,
    },
    Plan { steps: Vec<Step> },
    /// A permission question (ACP-5). `answer` is the option chosen, or
    /// "cancelled"; None while it is open.
    Question { title: String, options: Vec<Choice>, answer: Option<String> },
    /// Something the app says about the conversation: an error, why it could
    /// not be picked up, how to sign in.
    Note { text: String, error: bool },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Diff { path: String, old: Option<String>, new: String },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub content: String,
    /// pending, in_progress or completed.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    /// allow_once, allow_always, reject_once or reject_always.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    /// Its place in the whole conversation, which stays the same when older
    /// entries are let go.
    pub index: usize,
    /// The conversation's revision when this entry last changed: the window
    /// asks for what changed after the last one it saw.
    pub rev: u64,
    #[serde(flatten)]
    pub body: Body,
}

/// A setting the agent offers for the session: its mode, its model (ACP-12).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Setting {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current: String,
    pub options: Vec<SettingOption>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SettingOption {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
}

/// A slash command the agent says it takes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

/// How full the context window is, as the agent last said.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Usage {
    pub used: u64,
    pub size: u64,
    /// "0.10 USD", where the agent says what it cost.
    pub cost: Option<String>,
}

/// The view the window asks for: the entries that changed after `since`,
/// or all of them when it has nothing or has fallen behind what is kept.
#[derive(Debug, Clone, Serialize)]
pub struct Changes {
    pub rev: u64,
    /// The first index still kept.
    pub base: usize,
    /// One past the last index.
    pub len: usize,
    /// True when `entries` is everything kept, to replace what the window has.
    pub reset: bool,
    pub entries: Vec<Entry>,
}
