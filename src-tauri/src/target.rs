//! What a click on a banner, a toast or a message opens (NOTE-4).
//!
//! A string on the wire, so a banner still sitting in Notification Center
//! from an older build opens something sensible: `src/lib/target.ts` routes
//! it, and falls back when what it names has gone.

use std::fmt;

use crate::commands::CHAT_TASK_ID;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    Work,
    Chat,
    Reviews,
    Tickets,
    Task(&'a str),
    /// A pane, with its task: pane ids are new every launch, and the task is
    /// where a click lands once this one is gone.
    Pane { task: &'a str, pane: &'a str },
    Ticket(&'a str),
    /// `owner/repo#number`, as `news::identity` writes it.
    Review(&'a str),
}

impl<'a> Target<'a> {
    /// A task's own place: a chat has no task to open, only the chats.
    pub fn task(task: &'a str) -> Self {
        if task == CHAT_TASK_ID { Target::Chat } else { Target::Task(task) }
    }
}

impl fmt::Display for Target<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Work => f.write_str("work"),
            Target::Chat => f.write_str("chat"),
            Target::Reviews => f.write_str("reviews"),
            Target::Tickets => f.write_str("tickets"),
            Target::Task(id) => write!(f, "task:{id}"),
            Target::Pane { task, pane } => write!(f, "pane:{task}:{pane}"),
            Target::Ticket(key) => write!(f, "ticket:{key}"),
            Target::Review(id) => write!(f, "review:{id}"),
        }
    }
}
