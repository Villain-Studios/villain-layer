//! What a terminal's screen says that its program does not report: a
//! window title, a question waiting on the user, a usage limit.

/// How much of the end of the output is searched for trust and limit phrases.
pub(super) const NOTICE_TAIL: usize = 4096;

/// Phrases a CLI prints when it is waiting on the user rather than working.
///
/// Every task is a brand-new directory, so the trust question is not a rare
/// edge case — it is the first thing an agent asks on every new task, and an
/// unanswered one looks exactly like an agent that has silently done nothing.
const TRUST_MARKERS: &[&str] = &[
    "do you trust the files in this folder",
    "trust the files in this directory",
    "do you trust this folder",
];

/// Phrases the agent CLIs print when they will not do any more work.
///
/// Best-effort and deliberately specific: matching a bare "rate limit" would
/// fire whenever an agent read code about rate limiting. A bare "usage limit"
/// was the same mistake — the prompt "add a usage limit to the API" is echoed
/// as it is typed — and this notice never clears, so it flagged the pane for
/// good and offered to hand off an agent that was fine.
const LIMIT_MARKERS: &[&str] = &[
    "usage limit reached",
    "reached your usage limit",
    "hit your usage limit",
    "usage limit exceeded",
    "rate limit reached",
    "rate limit exceeded",
    "quota exceeded",
    "resource_exhausted",
    "resource exhausted",
    "insufficient_quota",
    "out of credits",
    "credit balance is too low",
    "upgrade to continue",
];

/// How far back to look for the window title a CLI last set.
pub(super) const TITLE_TAIL: usize = 2048;

/// The last whole window title set in `tail` (OSC 0 or 2), if any.
///
/// Read from the tail rather than the chunk just read: a title can straddle
/// two reads, and then neither chunk holds all of it.
pub(super) fn last_title(tail: &[u8]) -> Option<String> {
    let mut found = None;
    let mut at = 0;
    while let Some(off) = tail[at..].windows(2).position(|w| w == b"\x1b]") {
        let start = at + off + 2;
        let body = &tail[start..];
        let Some(rest) = body.strip_prefix(b"0;").or_else(|| body.strip_prefix(b"2;")) else {
            at = start;
            continue;
        };
        // Ended by BEL or by ST; one with no end yet is still arriving.
        let Some(end) = rest.iter().position(|&b| b == 0x07 || b == 0x1b) else {
            break;
        };
        found = Some(String::from_utf8_lossy(&rest[..end]).to_string());
        at = start + 2 + end;
    }
    found
}

/// Which notice, if any, the end of a pane's output is showing.
///
/// Runs on every read, so it avoids the obvious version — a lossy UTF-8
/// decode and a Unicode lowercase of 4KB, two allocations a read. The phrases
/// are all ASCII, so folding ASCII and blanking everything else keeps every
/// match and lets `str::contains` do the searching. `scratch` is reused by
/// the caller between reads.
pub(super) fn notice_in(tail: &[u8], scratch: &mut Vec<u8>) -> Option<&'static str> {
    scratch.clear();
    scratch.extend(tail.iter().map(|b| if b.is_ascii() { b.to_ascii_lowercase() } else { b' ' }));
    let text = std::str::from_utf8(scratch).ok()?;
    if LIMIT_MARKERS.iter().any(|m| text.contains(m)) {
        Some("usage_limit")
    } else if TRUST_MARKERS.iter().any(|m| text.contains(m)) {
        Some("trust_prompt")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_whole_title_is_the_one_read() {
        let t = |b: &[u8]| last_title(b);
        assert_eq!(t(b"\x1b]0;\xe2\x97\x87  Ready (api)\x07drawn"), Some("◇  Ready (api)".into()));
        // Two in one read: the later wins. ST ends one as well as BEL does.
        assert_eq!(
            t(b"\x1b]2;one\x07text\x1b]0;two\x1b\\more"),
            Some("two".into())
        );
        // Still arriving: the whole one before it stands.
        assert_eq!(t(b"\x1b]0;done\x07\x1b]0;half"), Some("done".into()));
        // Other OSC sequences are not titles.
        assert_eq!(t(b"\x1b]10;?\x1b\\\x1b]9;hello\x07"), None);
        assert_eq!(t(b"plain"), None);
    }

    #[test]
    fn recognises_the_trust_question_every_new_worktree_triggers() {
        let hit = |s: &str| TRUST_MARKERS.iter().any(|m| s.to_lowercase().contains(m));
        assert!(hit("Do you trust the files in this folder?"));
        assert!(hit("  Do you trust this folder?  "));
        // Not the usage-limit wording, which is handled separately.
        assert!(!hit("You've reached your usage limit"));
    }

    #[test]
    fn a_notice_is_found_through_colour_and_unicode() {
        let mut scratch = Vec::new();
        let tui = "\u{1b}[1m╭─ Do you trust the files in this folder? ─╮\u{1b}[0m".as_bytes();
        assert_eq!(notice_in(tui, &mut scratch), Some("trust_prompt"));
        // A usage limit outranks a trust question still on screen.
        let both = b"Do you trust this folder?\n... You've reached your USAGE LIMIT";
        assert_eq!(notice_in(both, &mut scratch), Some("usage_limit"));
        // A tail cut through the middle of a character is still searchable.
        let cut = &"é usage limit reached".as_bytes()[1..];
        assert_eq!(notice_in(cut, &mut scratch), Some("usage_limit"));
        assert_eq!(notice_in(b"added a rate limiter", &mut scratch), None);
    }

    #[test]
    fn recognises_limit_messages_without_firing_on_prose() {
        let hit = |s: &str| LIMIT_MARKERS.iter().any(|m| s.to_lowercase().contains(m));

        assert!(hit("You've reached your usage limit. Resets at 3pm."));
        assert!(hit("Error: quota exceeded for this model"));
        assert!(hit("RESOURCE_EXHAUSTED"));
        assert!(hit("Your credit balance is too low"));

        assert!(hit("Claude usage limit reached. Your limit will reset at 5pm"));
        assert!(hit("You've hit your usage limit for GPT-5"));

        // Reading or writing code about rate limiting must not count.
        assert!(!hit("added a rate limiter to the gateway"));
        assert!(!hit("> add a usage limit to the orders API"));
        assert!(!hit("Do you trust the files in this folder?"));
        assert!(!hit("see docs/rate-limits.md for the policy"));
        assert!(!hit("fn check_quota(user: &User) -> bool"));
    }
}
