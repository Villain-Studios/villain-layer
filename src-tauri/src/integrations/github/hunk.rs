//! The code a review thread is on, cut from the hunk GitHub sends with it.
//!
//! Without it the picker showed `file.scss:20` and a comment about "this
//! selector", and which selector meant opening GitHub (PR-10). An outdated
//! thread's code is not in the file any more, so the hunk is the only copy.

use serde::{Deserialize, Serialize};

/// One line of the code under a thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeLine {
    /// Its number on the side the comment is on; none for a line that is
    /// only on the other side.
    pub n: Option<u64>,
    /// `+`, `-` or a space, as in the diff.
    pub op: String,
    pub text: String,
}

/// Lines above a one-line comment, as GitHub shows them.
const CONTEXT: u64 = 3;
/// A comment can span a whole new file. Its end is where it points.
const MAX_LINES: usize = 40;

/// The lines `start..=end` of `hunk`, numbered on the old side when `left`.
///
/// The hunk is the one the comment was written against, so these are the
/// thread's *original* line numbers: after a push has moved the code, the
/// current ones no longer match it.
pub fn excerpt(hunk: &str, start: Option<u64>, end: u64, left: bool) -> Vec<CodeLine> {
    let mut lines = hunk.lines();
    let Some((mut old, mut new)) = lines.next().and_then(header) else {
        return Vec::new();
    };
    let from = start.unwrap_or(end.saturating_sub(CONTEXT)).min(end);
    let mut out: Vec<CodeLine> = Vec::new();
    for raw in lines {
        // "\ No newline at end of file" belongs to the line before it.
        if raw.starts_with('\\') {
            continue;
        }
        let mut chars = raw.chars();
        let op = chars.next().unwrap_or(' ');
        let text = chars.as_str().to_string();
        let (o, n) = match op {
            '+' => (None, Some(new)),
            '-' => (Some(old), None),
            _ => (Some(old), Some(new)),
        };
        old += o.is_some() as u64;
        new += n.is_some() as u64;
        let at = if left { o } else { n };
        match at {
            Some(k) if k > end => break,
            Some(k) if k < from => continue,
            None if out.is_empty() => continue,
            _ => {}
        }
        out.push(CodeLine { n: at, op: if op == '+' || op == '-' { op } else { ' ' }.to_string(), text });
    }
    // The other side's lines after the last one commented on are not part of it.
    while out.last().is_some_and(|c| c.n.is_none()) {
        out.pop();
    }
    let cut = out.len().saturating_sub(MAX_LINES);
    out.split_off(cut)
}

/// Where `@@ -12,7 +12,9 @@` starts on each side.
fn header(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.strip_prefix("@@ ")?.split(' ');
    let side = |p: Option<&str>, sign: char| -> Option<u64> {
        p?.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    Some((side(parts.next(), '-')?, side(parts.next(), '+')?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[CodeLine]) -> Vec<String> {
        lines.iter().map(|l| format!("{}{}", l.op, l.text)).collect()
    }

    #[test]
    fn a_range_comment_shows_exactly_the_lines_it_spans() {
        let hunk = "@@ -0,0 +1,6 @@\n+a {\n+}\n+\n+.id {\n+  font-size: 1px;\n+}";
        let got = excerpt(hunk, Some(4), 6, false);
        assert_eq!(text(&got), ["+.id {", "+  font-size: 1px;", "+}"]);
        assert_eq!(got[0].n, Some(4));
    }

    #[test]
    fn a_one_line_comment_comes_with_the_lines_above_it() {
        let hunk = "@@ -10,6 +10,6 @@ fn f() {\n one\n two\n-three\n+THREE\n four\n five";
        let got = excerpt(hunk, None, 14, false);
        // 11..=14 on the new side, with the removed line where it was.
        assert_eq!(text(&got), [" two", "-three", "+THREE", " four", " five"]);
        assert_eq!(got.iter().map(|l| l.n).collect::<Vec<_>>(), [Some(11), None, Some(12), Some(13), Some(14)]);
    }

    #[test]
    fn a_comment_on_a_removed_line_is_numbered_on_the_old_side() {
        let hunk = "@@ -10,3 +10,3 @@\n one\n-two\n+TWO\n three";
        let got = excerpt(hunk, Some(11), 11, true);
        assert_eq!(text(&got), ["-two"]);
    }

    #[test]
    fn a_hunk_that_cannot_be_read_gives_no_code() {
        assert!(excerpt("", None, 3, false).is_empty());
        assert!(excerpt("not a hunk\n+x", None, 1, false).is_empty());
    }

    #[test]
    fn a_long_range_keeps_the_end_it_points_at() {
        let body: String = (1..=100).map(|i| format!("\n+line {i}")).collect();
        let got = excerpt(&format!("@@ -0,0 +1,100 @@{body}"), Some(1), 100, false);
        assert_eq!(got.len(), MAX_LINES);
        assert_eq!(got.last().map(|l| l.n), Some(Some(100)));
    }
}
