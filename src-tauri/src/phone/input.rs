//! What a phone may type, and where (PHONE-7).
//!
//! Typing into a terminal is running commands on this Mac, so the phone gets
//! the least that answers an agent: a line of text, and the keys its
//! questions and menus take. Into agents only. A shell is a prompt with
//! nobody's permission between it and the machine.

use crate::pty::{PaneInfo, PaneKind, MAX_TYPED};

/// The keys the phone's key bar sends, as the bytes a terminal sends.
///
/// Arrows are the normal-mode ones. The agent TUIs read both forms; a
/// program that only takes the application-mode ones would need the phone
/// to follow the mode, which nothing here does.
pub fn key(name: &str) -> Option<&'static str> {
    Some(match name {
        "enter" => "\r",
        "esc" => "\u{1b}",
        "up" => "\u{1b}[A",
        "down" => "\u{1b}[B",
        "tab" => "\t",
        // Claude Code's mode switch (plan, accept edits).
        "shift_tab" => "\u{1b}[Z",
        "ctrl_c" => "\u{3}",
        "1" => "1",
        "2" => "2",
        "3" => "3",
        _ => return None,
    })
}

/// Why this pane does not take typing from the phone, if it does not.
pub fn refused(typing: bool, pane: &PaneInfo) -> Option<&'static str> {
    if !typing {
        return Some("Typing from the phone is off. Turn it on in the app's Settings, under Phone.");
    }
    if pane.kind != PaneKind::Agent {
        return Some("Only agents take typing from the phone, not shells.");
    }
    if !pane.running {
        return Some("This agent has exited.");
    }
    None
}

/// A line as typed on a phone, made fit to type: one line, as the agent's
/// prompt would take it, and no longer than a terminal keeps (PANE-11).
pub fn line(text: &str) -> Result<String, String> {
    let one: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let one = one.trim().to_string();
    if one.is_empty() {
        return Err("Nothing to send.".into());
    }
    if one.len() > MAX_TYPED {
        return Err(format!(
            "{} bytes is more than a terminal takes typed ({MAX_TYPED}). Send it in parts.",
            one.len()
        ));
    }
    Ok(one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::Activity;

    fn pane(kind: PaneKind, running: bool) -> PaneInfo {
        let now = chrono::Utc::now();
        PaneInfo {
            id: "p".into(),
            task_id: "t".into(),
            checkout_id: None,
            kind,
            title: "Claude Code".into(),
            agent_id: Some("claude".into()),
            cwd: "/tmp".into(),
            running,
            exit_code: None,
            started_at: now,
            last_output_at: now,
            notice: None,
            activity: Activity::Asking,
            activity_since: now,
            topic: None,
            acp: false,
            loop_said: None,
        }
    }

    #[test]
    fn only_a_running_agent_takes_typing_and_only_when_it_is_on() {
        assert_eq!(refused(true, &pane(PaneKind::Agent, true)), None);
        assert!(refused(false, &pane(PaneKind::Agent, true)).is_some());
        assert!(refused(true, &pane(PaneKind::Shell, true)).is_some());
        assert!(refused(true, &pane(PaneKind::Agent, false)).is_some());
    }

    #[test]
    fn a_line_from_the_phone_is_one_line_with_no_terminal_controls_in_it() {
        assert_eq!(line("  fix the test\nand push  ").unwrap(), "fix the test and push");
        assert_eq!(line("go\u{1b}[2J\u{3}").unwrap(), "go[2J", "no escapes or interrupts ride along");
        assert!(line(" \n ").is_err());
        assert!(line(&"x".repeat(MAX_TYPED + 1)).is_err());
        assert_eq!(line(&"é".repeat(MAX_TYPED / 2)).unwrap().len(), MAX_TYPED);
    }

    #[test]
    fn the_key_bar_sends_what_a_keyboard_would() {
        assert_eq!(key("enter"), Some("\r"));
        assert_eq!(key("esc"), Some("\u{1b}"));
        assert_eq!(key("ctrl_c"), Some("\u{3}"));
        assert_eq!(key("up"), Some("\u{1b}[A"));
        assert_eq!(key("shift_tab"), Some("\u{1b}[Z"));
        assert_eq!(key("rm -rf /"), None);
        assert_eq!(key("4"), None);
    }
}
