//! What an agent on a loop is sent when its checks fail (LOOP-6), and how
//! its rounds are said.

use super::{CheckResult, Target};

/// The end of a failed check's output the agent is sent (LOOP-6).
const TAIL_LINES: usize = 150;
const TAIL_BYTES: usize = 12 * 1024;

pub(super) fn count(rounds: u32) -> String {
    format!("{rounds} round{}", if rounds == 1 { "" } else { "s" })
}

/// The end of a check's output, as the agent is sent it (LOOP-6): no
/// colour codes, the last `TAIL_LINES` lines, at most `TAIL_BYTES`.
pub(super) fn tail(output: &str) -> String {
    let plain = crate::pty::strip_ansi(output);
    let lines: Vec<&str> = plain.lines().map(str::trim_end).collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    let mut text = lines[start..].join("\n");
    if text.len() > TAIL_BYTES {
        let mut cut = text.len() - TAIL_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text = format!("…\n{}", &text[cut..]);
    } else if start > 0 {
        text = format!("…\n{text}");
    }
    text.trim().to_string()
}

/// What the agent is sent when the checks fail (LOOP-6).
///
/// Each repository is named by its full path as well as its folder: an
/// agent started inside one repository, told only `web/`, went looking
/// for a `web/` folder inside it. And it says the user asked for this: a
/// real agent told at the start to change nothing held to that, and the
/// failures alone did not tell it the user wanted them fixed.
pub(super) fn failures(results: &[CheckResult], targets: &[Target], round: u32, rounds: u32) -> String {
    let mut text = format!(
        "The user put you on a loop: each time you end a turn, Villain Layer runs this task's checks, and \
         they fail now (round {round} of {rounds}).\n\n\
         Make them pass, then end your turn: they run again when you do. If a failure is not yours to fix, \
         or you need something from the user, say so and end your turn without changing anything, and the loop \
         waits for them.\n"
    );
    let dir = |repo: &str| targets.iter().find(|t| t.repo == repo).map(|t| t.dir.display().to_string()).unwrap_or_default();
    for r in results {
        if r.passed {
            text.push_str(&format!("\n`{}/` ({}): `{}` passes.\n", r.repo, dir(&r.repo), r.command));
            continue;
        }
        let code = r.code.map(|c| c.to_string()).unwrap_or_else(|| "without a code".into());
        let fence = fence_for(&r.output);
        text.push_str(&format!(
            "\n## `{}/`: `{}` exited {code}\n\nRun in `{}`.\n\n{fence}text\n{}\n{fence}\n",
            r.repo,
            r.command,
            dir(&r.repo),
            r.output
        ));
    }
    text
}

/// A code fence longer than any run of backticks in `text`, so a log
/// holding one cannot end it early.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_log_is_cut_to_its_end_without_its_colours() {
        let log: String = (0..400).map(|i| format!("\u{1b}[31mline {i}\u{1b}[0m\n")).collect();
        let t = tail(&log);
        assert!(t.starts_with("…\n"));
        assert!(t.ends_with("line 399"));
        assert!(!t.contains('\u{1b}'));
        assert_eq!(t.lines().count(), TAIL_LINES + 1);
        let wide: String = (0..100).map(|_| format!("{}\n", "é".repeat(200))).collect();
        assert!(tail(&wide).len() <= TAIL_BYTES + "…\n".len());
    }

    #[test]
    fn the_failures_name_the_round_each_command_where_it_ran_and_a_fence_no_log_can_close() {
        let targets = vec![
            Target { repo: "api".into(), dir: "/tasks/ACME-1/api".into(), command: "cargo test".into() },
            Target { repo: "web".into(), dir: "/tasks/ACME-1/web".into(), command: "bun run check".into() },
        ];
        let results = vec![
            CheckResult { repo: "api".into(), command: "cargo test".into(), passed: true, code: Some(0), secs: 1.0, output: String::new() },
            CheckResult {
                repo: "web".into(),
                command: "bun run check".into(),
                passed: false,
                code: Some(1),
                secs: 2.0,
                output: "error: ```` in a log".into(),
            },
        ];
        let text = failures(&results, &targets, 2, 5);
        assert!(text.starts_with("The user put you on a loop"));
        assert!(text.contains("(round 2 of 5)"));
        assert!(text.contains("`api/` (/tasks/ACME-1/api): `cargo test` passes."));
        assert!(text.contains(
            "## `web/`: `bun run check` exited 1\n\nRun in `/tasks/ACME-1/web`.\n\n`````text\nerror: ```` in a log\n`````\n"
        ));
    }
}
