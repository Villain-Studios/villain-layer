//! A pane's output read as text rather than drawn: for handing work to
//! another agent, and for a CI log.

/// Drop ANSI escapes so terminal output can be read as text.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters then a final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or ST
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Terminal history as readable text: a TUI redraws constantly, so identical
/// consecutive lines and blank runs are collapsed before taking the tail.
pub fn readable_tail(raw: &str, max_lines: usize) -> String {
    let plain = strip_ansi(raw);
    let mut lines: Vec<&str> = Vec::new();

    for line in plain.lines() {
        let line = line.trim_end();
        let blank = line.trim().is_empty();
        if blank && lines.last().is_some_and(|l: &&str| l.trim().is_empty()) {
            continue;
        }
        // A TUI repaints the same rows continually; one copy is enough.
        if lines.last() == Some(&line) {
            continue;
        }
        lines.push(line);
    }

    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_escapes_a_tui_emits() {
        let raw = "\u{1b}[32mgreen\u{1b}[0m plain\u{1b}[1;31mred\u{1b}[m";
        assert_eq!(strip_ansi(raw), "green plainred");

        // OSC title sequences end with BEL or ST.
        assert_eq!(strip_ansi("\u{1b}]0;a title\u{7}after"), "after");
        assert_eq!(strip_ansi("\u{1b}]0;t\u{1b}\\after"), "after");

        // Carriage returns are progress-bar redraw, not content.
        assert_eq!(strip_ansi("a\rb"), "ab");
    }

    #[test]
    fn collapses_the_repaints_and_keeps_the_tail() {
        // A TUI rewrites the same row over and over.
        let raw = "thinking\nthinking\nthinking\n\n\n\ndone\nfinal";
        assert_eq!(readable_tail(raw, 10), "thinking\n\ndone\nfinal");

        let many: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let tail = readable_tail(&many, 3);
        assert_eq!(tail, "line 47\nline 48\nline 49");
    }
}
