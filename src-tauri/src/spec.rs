//! A spec's three files and what is read from them (§19): the requirements
//! (or, for a bug, its analysis), the design, and the tasks. Nothing here
//! touches the disk or git; `commands/spec.rs` decides where a spec lives,
//! and commits it.

use serde::{Deserialize, Serialize};

/// One of a spec's files, in the order they are drafted and approved
/// (SPEC-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Requirements,
    Design,
    Tasks,
}

impl Part {
    pub const ALL: [Part; 3] = [Part::Requirements, Part::Design, Part::Tasks];

    /// Its file in the spec folder. A bug's requirements are its analysis.
    pub fn file(self, kind: Kind) -> &'static str {
        match (self, kind) {
            (Part::Requirements, Kind::Feature) => "requirements.md",
            (Part::Requirements, Kind::Bugfix) => "bugfix.md",
            (Part::Design, _) => "design.md",
            (Part::Tasks, _) => "tasks.md",
        }
    }

    /// What it is called in a commit message or a prompt.
    pub fn word(self, kind: Kind) -> &'static str {
        match (self, kind) {
            (Part::Requirements, Kind::Feature) => "requirements",
            (Part::Requirements, Kind::Bugfix) => "bug analysis",
            (Part::Design, _) => "design",
            (Part::Tasks, _) => "tasks",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// A feature, or a bug: which file holds the requirements (SPEC-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Feature,
    Bugfix,
}

/// One requirement: `R-2` and what it says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Requirement {
    pub id: String,
    pub text: String,
}

/// One step of `tasks.md`: its number, whether it is ticked, and the
/// requirements of this repository it serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub number: u32,
    pub text: String,
    pub done: bool,
    pub serves: Vec<String>,
}

/// The folder a task's spec is kept in, under the repository's spec folder:
/// its branch, made one path segment (SPEC-1).
pub fn folder_name(branch: &str) -> String {
    let name: String = branch
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect();
    let name = name.trim_matches(|c| c == '.' || c == '-').to_string();
    if name.is_empty() { "spec".into() } else { name }
}

/// A repository's spec folder as the user set it: relative, inside the
/// repository, and no option to git. `None` for the default.
pub fn valid_folder(folder: &str) -> Result<Option<String>, String> {
    let folder = folder.trim().trim_matches('/');
    if folder.is_empty() || folder == "specs" {
        return Ok(None);
    }
    let bad = folder.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with('-') || s == ".git")
        || folder.contains('\\');
    if bad {
        return Err(format!("`{folder}` is not a folder inside the repository"));
    }
    Ok(Some(folder.to_string()))
}

/// The requirements of a spec: the list items under its Requirements
/// heading, or a bug's Expected and Unchanged behaviour, up to the next
/// heading (SPEC-1). An item's `R-n` is its id; one without is numbered by
/// its place. A spec from before, with "Acceptance criteria" and `AC-n`,
/// reads the same. A checkbox is not part of the text, and an indented line
/// goes with the item above.
pub fn requirements(spec: &str) -> Vec<Requirement> {
    let mut found: Vec<Requirement> = Vec::new();
    let mut inside = false;
    for (line, trimmed) in outside_fences(spec) {
        if trimmed.starts_with('#') {
            let heading = trimmed.to_ascii_lowercase();
            inside = ["requirements", "acceptance criteria", "expected behavio", "unchanged behavio"]
                .iter()
                .any(|h| heading.contains(h));
            continue;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        let indent = line.len() - trimmed.len();
        match list_item(trimmed).filter(|_| indent < 2) {
            Some(item) => {
                let item = checkbox(item).map(|(_, rest)| rest).unwrap_or(item);
                let (id, text) = match own_id(item) {
                    Some((id, rest)) => (id, rest),
                    None => (format!("R-{}", found.len() + 1), item),
                };
                found.push(Requirement { id, text: text.trim().to_string() });
            }
            None => {
                // A wrapped or nested line belongs to the item above it.
                if let Some(last) = found.last_mut() {
                    let more = list_item(trimmed).unwrap_or(trimmed).trim();
                    if !more.is_empty() {
                        last.text.push(' ');
                        last.text.push_str(more);
                    }
                }
            }
        }
    }
    found.retain(|r| !r.text.is_empty());
    found
}

/// The open questions of a requirements file that have an answer, as
/// (question, answer) (SPEC-1, SPEC-19): a list item under the Open
/// questions heading, and under it an `Answer:` line, or a line starting
/// `--`, which is how people answered before the tab had a place for it.
/// The items under a question are the answers drafted for it, not its
/// answer. `src/lib/questions.ts` reads the same shape for the tab, which
/// needs the unanswered ones too; the prompts need only these.
pub fn answered(spec: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, Option<String>)> = Vec::new();
    let mut inside = false;
    for (line, trimmed) in outside_fences(spec) {
        if trimmed.starts_with('#') {
            inside = trimmed.to_ascii_lowercase().contains("open question");
            continue;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        if let Some(answer) = answer_line(trimmed) {
            if let Some(last) = found.last_mut() {
                last.1 = Some(answer);
            }
            continue;
        }
        let indent = line.len() - trimmed.len();
        match list_item(trimmed) {
            Some(item) if indent < 2 => found.push((question_text(item).trim().to_string(), None)),
            Some(_) => {}
            None => {
                if let Some(last) = found.last_mut() {
                    last.0.push(' ');
                    last.0.push_str(trimmed.trim());
                }
            }
        }
    }
    found.into_iter().filter_map(|(q, a)| Some((q, a.filter(|a| !a.is_empty())?))).collect()
}

/// `Answer: yes` (bullet and bold allowed) or `-- yes` as "yes".
fn answer_line(trimmed: &str) -> Option<String> {
    let dashed = trimmed.starts_with("--");
    let rest = if dashed { trimmed.trim_start_matches('-').trim_start() } else { list_item(trimmed).unwrap_or(trimmed) };
    let bare = rest.trim_start_matches('*');
    if bare.get(..6).is_some_and(|w| w.eq_ignore_ascii_case("answer")) {
        if let Some(answer) = bare[6..].trim_start_matches('*').trim_start().strip_prefix(':') {
            return Some(answer.trim_start_matches('*').trim().to_string());
        }
    }
    dashed.then(|| rest.trim().to_string())
}

/// A question's text without its `Q-2:`.
fn question_text(item: &str) -> &str {
    let bare = item.trim_start_matches('*');
    let Some(rest) = bare.strip_prefix("Q-").or_else(|| bare.strip_prefix("q-")) else {
        return item;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return item;
    }
    rest[digits..].trim_start_matches('*').trim_start().trim_start_matches([':', '.', '-', '—', '–']).trim_start()
}

/// The steps of `tasks.md`: every list item with a checkbox, outside code
/// blocks. A step's number is the one written after its checkbox, or its
/// place.
pub fn steps(tasks: &str) -> Vec<Step> {
    let mut found: Vec<Step> = Vec::new();
    for (line, trimmed) in outside_fences(tasks) {
        if line.len() - trimmed.len() >= 2 {
            continue;
        }
        let Some((done, rest)) = list_item(trimmed).and_then(checkbox) else {
            continue;
        };
        let (number, text) = step_number(rest).unwrap_or((found.len() as u32 + 1, rest));
        found.push(Step { number, text: text.trim().to_string(), done, serves: serves(text) });
    }
    found
}

/// `tasks` with step `number` ticked (or unticked). None when there is no
/// such step.
pub fn tick(tasks: &str, number: u32, done: bool) -> Option<String> {
    let mut place = 0;
    let mut hit = false;
    let mut fence = false;
    let mut out: Vec<String> = Vec::new();
    for line in tasks.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fence = !fence;
        }
        let step = (!fence && line.len() - trimmed.len() < 2)
            .then(|| list_item(trimmed).and_then(checkbox))
            .flatten();
        if let Some((_, rest)) = step {
            place += 1;
            let n = step_number(rest).map(|(n, _)| n).unwrap_or(place);
            if n == number && !hit {
                hit = true;
                let at = line.find('[').unwrap_or(0);
                let mark = if done { "[x]" } else { "[ ]" };
                out.push(format!("{}{mark}{}", &line[..at], &line[at + 3..]));
                continue;
            }
        }
        out.push(line.to_string());
    }
    hit.then(|| out.join("\n"))
}

/// What a step whose requirements are all gone is marked with, for the user
/// to remove: never removed by itself (SPEC-7).
pub const ORPHANED: &str = "— its requirements are gone: remove this step?";

/// A redrafted `tasks.md`, made to keep what was done (SPEC-7): a ticked
/// step of `old` stays ticked in `new`, or is put back at the end if the
/// draft left it out, and a step serving only requirements that are no
/// longer there is marked.
pub fn sync_steps(old: &str, new: &str, requirements: &[Requirement]) -> String {
    let fresh = steps(new);
    let mut out = new.trim_end().to_string();
    for done in steps(old).into_iter().filter(|s| s.done) {
        let same = |s: &Step| normal(&s.text) == normal(&done.text);
        match fresh.iter().find(|s| same(s)) {
            Some(s) if !s.done => out = tick(&out, s.number, true).unwrap_or(out),
            Some(_) => {}
            None => out.push_str(&format!("\n- [x] {}. {}", done.number, done.text)),
        }
    }
    let ids: Vec<&str> = requirements.iter().map(|r| r.id.as_str()).collect();
    let orphans: Vec<Step> = steps(&out)
        .into_iter()
        .filter(|s| !s.done && !s.serves.is_empty() && s.serves.iter().all(|id| !ids.contains(&id.as_str())))
        .filter(|s| !s.text.contains(ORPHANED))
        .collect();
    for step in orphans {
        out = mark(&out, &step, ORPHANED);
    }
    out.push('\n');
    out
}

fn mark(tasks: &str, step: &Step, note: &str) -> String {
    let mut done = false;
    tasks
        .split('\n')
        .map(|line| {
            if !done && line.contains(&step.text) && list_item(line.trim_start()).and_then(checkbox).is_some() {
                done = true;
                format!("{} {note}", line.trim_end())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn normal(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Each line with its indentation cut, skipping code blocks and their fences.
fn outside_fences(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut fence = false;
    text.lines().filter_map(move |line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fence = !fence;
            return None;
        }
        (!fence).then_some((line, trimmed))
    })
}

/// The text of a list item, without its bullet or number.
fn list_item(line: &str) -> Option<&str> {
    let rest = if let Some(rest) = line.strip_prefix(['-', '*', '+']) {
        rest
    } else {
        let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        line[digits..].strip_prefix(['.', ')'])?
    };
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    Some(rest.trim_start())
}

/// `[x] rest` as (true, "rest").
fn checkbox(item: &str) -> Option<(bool, &str)> {
    for (b, done) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
        if let Some(rest) = item.strip_prefix(b) {
            return Some((done, rest.trim_start()));
        }
    }
    None
}

/// `3. text` as (3, "text").
fn step_number(item: &str) -> Option<(u32, &str)> {
    let digits = item.len() - item.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let number = item[..digits].parse().ok()?;
    let rest = item[digits..].strip_prefix(['.', ')', ':'])?;
    Some((number, rest.trim_start()))
}

/// `R-3: text` as ("R-3", "text"). The id may be bold, and followed by a
/// colon, a full stop or a dash. `AC-n`, from a spec before requirements had
/// ids of their own, counts too.
fn own_id(item: &str) -> Option<(String, &str)> {
    let bare = item.trim_start_matches('*');
    let (prefix, rest) = ["R-", "r-", "AC-", "ac-"]
        .iter()
        .find_map(|p| bare.strip_prefix(p).map(|rest| (p.to_ascii_uppercase(), rest)))?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let id = format!("{prefix}{}", &rest[..digits]);
    let after = rest[digits..]
        .trim_start_matches('*')
        .trim_start()
        .trim_start_matches([':', '.', '-', '—', '–'])
        .trim_start_matches('*');
    Some((id, after.trim_start()))
}

/// The requirements a step names in its last parentheses, `(R-1, R-2)`.
/// Another repository's (`web R-2`) are not this one's, and left out.
fn serves(text: &str) -> Vec<String> {
    let Some(open) = text.rfind('(') else {
        return Vec::new();
    };
    let inner = text[open + 1..].split(')').next().unwrap_or("");
    inner
        .split(',')
        .map(str::trim)
        .filter_map(|entry| own_id(entry).filter(|(_, rest)| rest.is_empty()).map(|(id, _)| id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIREMENTS: &str = "## Goal\nRefunds work for split payments.\n\n\
        ## Requirements\n\
        - R-1: WHEN a two-card payment is refunded THE SYSTEM SHALL return money to both cards.\n\
        - **R-2** — WHEN a refund is made THE SYSTEM SHALL never return more\n  than was paid.\n\
        - [ ] R-3. WHEN one card paid THE SYSTEM SHALL refund as before.\n\n\
        ## Out of scope\n- Partial refunds.\n";

    #[test]
    fn requirements_are_read_with_their_own_ids() {
        let ids: Vec<(String, String)> = requirements(REQUIREMENTS).into_iter().map(|r| (r.id, r.text)).collect();
        assert_eq!(ids[0].0, "R-1");
        assert_eq!(ids[1], ("R-2".into(), "WHEN a refund is made THE SYSTEM SHALL never return more than was paid.".into()));
        assert_eq!(ids[2], ("R-3".into(), "WHEN one card paid THE SYSTEM SHALL refund as before.".into()));
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn a_bugs_expected_and_unchanged_behaviour_are_its_requirements() {
        let bug = "## Current behaviour\n- It loops.\n## Expected behaviour\n- R-1: WHEN … SHALL stop.\n\
                   ## Unchanged behaviour\n- R-2: WHEN … SHALL CONTINUE TO log in.\n## Open questions\n- None.\n";
        let ids: Vec<String> = requirements(bug).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["R-1", "R-2"]);
    }

    #[test]
    fn a_spec_from_before_reads_its_acceptance_criteria_as_requirements() {
        let old = "## Acceptance criteria\n- [ ] AC-1: Login works.\n- Logout works.\n```\n- AC-9: not this\n```\n";
        let ids: Vec<String> = requirements(old).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["AC-1", "R-2"]);
    }

    const TASKS: &str = "## Tasks\n\n- [ ] 1. Add the limiter (R-1, R-2)\n- [x] 2. Call it from login (R-1, web R-3)\n  - a note under it\n- [ ] Write the tests\n";

    #[test]
    fn steps_are_read_with_their_numbers_ticks_and_own_requirements() {
        let s = steps(TASKS);
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].number, s[0].done, s[0].serves.clone()), (1, false, vec!["R-1".to_string(), "R-2".to_string()]));
        assert_eq!((s[1].number, s[1].done, s[1].serves.clone()), (2, true, vec!["R-1".to_string()]));
        assert_eq!((s[2].number, s[2].text.as_str()), (3, "Write the tests"));
    }

    #[test]
    fn ticking_a_step_changes_its_box_and_nothing_else() {
        let ticked = tick(TASKS, 3, true).unwrap();
        assert!(ticked.contains("- [x] Write the tests"));
        assert!(ticked.contains("- [ ] 1. Add the limiter"));
        assert_eq!(ticked.len(), TASKS.len());
        let unticked = tick(TASKS, 2, false).unwrap();
        assert!(unticked.contains("- [ ] 2. Call it from login"));
        assert!(tick(TASKS, 9, true).is_none());
    }

    #[test]
    fn a_redraft_keeps_what_was_done_and_marks_steps_whose_requirements_went() {
        let old = "- [x] 1. Add the limiter (R-1)\n- [x] 2. Wire up metrics (R-4)\n- [ ] 3. Docs (R-2)\n";
        let new = "- [ ] 1. Add the limiter (R-1)\n- [ ] 2. Return 429 (R-2)\n- [ ] 3. Log it (R-9)\n";
        let reqs = vec![
            Requirement { id: "R-1".into(), text: String::new() },
            Requirement { id: "R-2".into(), text: String::new() },
        ];
        let synced = sync_steps(old, new, &reqs);
        let s = steps(&synced);
        assert!(s[0].done, "a step done before is still done");
        assert!(!s[1].done);
        assert!(s[2].text.contains(ORPHANED), "{synced}");
        assert!(s.iter().any(|x| x.done && x.text.starts_with("Wire up metrics")), "dropped work is put back: {synced}");
        // Synced again, nothing is marked twice.
        assert_eq!(sync_steps(old, &synced, &reqs).matches(ORPHANED).count(), 1);
    }

    #[test]
    fn answered_questions_are_read_with_their_answers_and_not_their_suggestions() {
        let spec = "## Requirements\n- R-1: WHEN x THE SYSTEM SHALL y.\n\n## Open questions\n\
                    - Q-1: Should staff lift the limit?\n  - Yes, from the admin page\n  - No\n  Answer: No\n\
                    - Q-2: How long is the window?\n  - An hour\n  - A day\n\
                    - **Q-3**: Say when to try again?\n-- yes\n\
                    - Which mail\n  provider?\n  - **Answer:** the current one\n";
        assert_eq!(
            answered(spec),
            vec![
                ("Should staff lift the limit?".to_string(), "No".to_string()),
                ("Say when to try again?".to_string(), "yes".to_string()),
                ("Which mail provider?".to_string(), "the current one".to_string()),
            ]
        );
        // `-- Answer: x` is one answer, not a dash and the word.
        assert_eq!(answered("## Open questions\n- Q-1: Ship first?\n-- Answer: \"<1 g\"\n")[0].1, "\"<1 g\"");
        // Nothing answered, nothing asked, or no such heading.
        assert!(answered("## Open questions\n- Q-1: Ship first?\n  Answer:\n").is_empty());
        assert!(answered("## Open questions\nNone.\n").is_empty());
        assert!(answered("## Requirements\n- R-1: x\n  Answer: y\n").is_empty());
    }

    #[test]
    fn a_branch_becomes_one_folder_name() {
        assert_eq!(folder_name("feature/ACME-12 refunds"), "feature-ACME-12-refunds");
        assert_eq!(folder_name("../.."), "spec");
    }

    #[test]
    fn a_spec_folder_must_stay_inside_the_repository() {
        assert_eq!(valid_folder("specs/"), Ok(None));
        assert_eq!(valid_folder("docs/specs"), Ok(Some("docs/specs".into())));
        assert!(valid_folder("../elsewhere").is_err());
        assert!(valid_folder("-x").is_err());
        assert!(valid_folder(".git/x").is_err());
    }
}
