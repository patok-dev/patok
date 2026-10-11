//! The task file: a Markdown checklist.
//!
//! Only lines beginning (after optional whitespace) with `- [ ] ` or `- [x] ` are tasks; every
//! other line is ignored by the parser and preserved on rewrite.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::utils::digits::is_ascii_digits;

/// Placeholder ID of a task line whose ID does not match the ID pattern.
pub const MALFORMED_ID: &str = "TASK";

/// The unchecked checkbox prefix that begins every task line, after optional whitespace.
pub const UNCHECKED: &str = "- [ ] ";

/// The checked checkbox prefix of a completed task line.
pub const CHECKED: &str = "- [x] ";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// The ID's prefix letter (`T` or `D`); `None` for a bare `1.1` ID, a malformed one, or an
    /// `H`-prefixed ID (existing task files contain `H` tasks).
    pub origin: Option<char>,
    pub description: String,
    pub done: bool,
    /// 1-based line number recorded at parse time.
    pub line: usize,
    /// The full original line, used as the last-resort address when completing the task.
    pub raw: String,
}

impl Task {
    pub fn is_malformed(&self) -> bool {
        self.id == MALFORMED_ID
    }

    /// Whether the task is not done and well-formed: the task the build loop
    /// picks next.
    pub fn is_pending(&self) -> bool {
        !self.done && !self.is_malformed()
    }

    /// The leading number of the task ID (`T1.2` -> `Some("1")`): the digits
    /// before the dot, shared by a batch-review group. `None` for malformed IDs.
    pub fn group_number(&self) -> Option<String> {
        group_number_of(&self.id)
    }

    /// The SHA-256 hex digest of the description: the description's identity
    /// for group membership, stable across restarts.
    pub fn description_hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.description.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Parses every task line in `text`, in file order.
pub fn parse(text: &str) -> Vec<Task> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| parse_line(line, i + 1))
        .collect()
}

/// The first pending, well-formed task in file order.
pub fn next_pending(tasks: &[Task]) -> Option<&Task> {
    tasks.iter().find(|t| t.is_pending())
}

/// The leading number of a task ID (`T1.2` -> `Some("1")`): the digits
/// before the dot, shared by a batch-review group. `None` for malformed IDs.
/// The free twin of [`Task::group_number`] for callers that hold only an ID.
pub fn group_number_of(task_id: &str) -> Option<String> {
    let (digits, _) = task_id.rsplit_once('.')?;
    let stripped = digits
        .strip_prefix(|c: char| c.is_alphabetic())
        .unwrap_or(digits);
    is_ascii_digits(stripped).then(|| stripped.to_string())
}

/// Returns `text` with `task` flipped to checked, or `None` when no matching unchecked line
/// exists (already checked, or the file changed too much). Tiers, in order (Part II, 1.5):
/// the recorded line number, the first unchecked line with the same `ID:` prefix, then an
/// unchecked line whose full text equals the original.
pub fn mark_done(text: &str, task: &Task) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let index = task_line(&lines, task)?;

    let mut out = String::with_capacity(text.len() + 1);
    for (i, line) in lines.iter().enumerate() {
        if i == index {
            out.push_str(&line.replacen("[ ]", "[x]", 1));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    Some(out)
}

/// The index of `task`'s unchecked line in `lines`, by the same tiers as [`mark_done`]:
/// the recorded line number, the first unchecked line with the same `ID:` prefix, then a
/// line whose full text equals the original. An already-checked line is not matched.
/// Public for the engine's progress rewrite, which shares the matching but not the rewrite.
pub fn task_line(lines: &[&str], task: &Task) -> Option<usize> {
    task.line
        .checked_sub(1)
        .filter(|&i| {
            lines
                .get(i)
                .is_some_and(|l| is_unchecked(l) && same_id(l, task))
        })
        .or_else(|| {
            (!task.is_malformed())
                .then(|| {
                    lines
                        .iter()
                        .position(|l| is_unchecked(l) && same_id(l, task))
                })
                .flatten()
        })
        .or_else(|| lines.iter().position(|l| is_unchecked(l) && *l == task.raw))
}

/// Splits a task line into its checked state and the text after the checkbox,
/// ignoring leading whitespace. `None` for a line that begins with neither
/// checkbox (uppercase `- [X]` is not a checkbox). Public for the engine's
/// task-file rewrites.
pub fn split_checkbox(line: &str) -> Option<(bool, &str)> {
    if let Some(rest) = line.trim_start().strip_prefix(UNCHECKED) {
        Some((false, rest))
    } else {
        line.trim_start()
            .strip_prefix(CHECKED)
            .map(|rest| (true, rest))
    }
}

/// Whether the line is an unchecked task line.
pub fn is_unchecked(line: &str) -> bool {
    line.trim_start().starts_with(UNCHECKED)
}

/// Whether the line's text after the checkbox starts with `<task id>:`. For malformed tasks the
/// recorded line is only trusted when its full text still matches the original.
pub fn same_id(line: &str, task: &Task) -> bool {
    if task.is_malformed() {
        return line == task.raw;
    }
    let rest = line.trim_start().strip_prefix(UNCHECKED).unwrap_or("");
    rest.strip_prefix(task.id.as_str())
        .is_some_and(|r| r.starts_with(':'))
}

fn parse_line(line: &str, number: usize) -> Option<Task> {
    let (done, rest) = split_checkbox(line)?;
    let (id, origin, description) = match split_id(rest) {
        Some((id, origin, description)) => (
            id.to_string(),
            origin,
            strip_progress(description.trim()).to_string(),
        ),
        None => (MALFORMED_ID.to_string(), None, rest.trim().to_string()),
    };
    Some(Task {
        id,
        origin,
        description,
        done,
        line: number,
        raw: line.to_string(),
    })
}

/// The five-position progress indicator the engine writes into the task line
///: five characters
/// from the stage letters, `.`, `-`, `+` and `!`, optionally followed by `!`,
/// in brackets, standing between the ID's colon and the description. The
/// parser strips it so the queue view and every ID match stay clean.
pub fn strip_progress(description: &str) -> &str {
    let Some(rest) = description.strip_prefix('[') else {
        return description;
    };
    let mut end = None;
    for (index, character) in rest.char_indices() {
        match character {
            ']' => {
                end = Some(index);
                break;
            }
            c if c.is_ascii_alphabetic() || matches!(c, '.' | '+' | '-' | '!') => {
                if index >= 6 {
                    return description;
                }
            }
            _ => return description,
        }
    }
    let Some(end) = end else {
        return description;
    };
    if end < 5 {
        // Not five positions: something else in brackets.
        return description;
    }
    let after = &rest[end + 1..];
    if after.is_empty() || after.starts_with(char::is_whitespace) {
        after.trim_start()
    } else {
        description
    }
}

/// Splits `T1.2: text` into `("T1.2", Some('T'), "text")`: an optional letter prefix (`T`, `H` or
/// `D`), digits, dot, digits. `T` and `D` are origins; an `H` prefix yields no origin, since
/// existing task files contain `H` tasks that must stay valid IDs.
/// Splits `T1.2: text` into the ID, its origin letter and the description: an optional letter prefix (`T`, `H` or `D`), digits,
/// dot, digits, colon. Public for the engine's task-file rewrites.
pub fn split_id(rest: &str) -> Option<(&str, Option<char>, &str)> {
    let (id, description) = rest.split_once(':')?;
    let letter = id.chars().next().filter(|c| matches!(c, 'T' | 'H' | 'D'));
    let digits = if letter.is_some() { &id[1..] } else { id };
    let (major, minor) = digits.split_once('.')?;
    (is_ascii_digits(major) && is_ascii_digits(minor)).then_some((
        id,
        letter.filter(|c| *c != 'H'),
        description,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# Tasks\n\nSome prose.\n\n## Phase 1\n- [x] T1.1: first thing done\n- [ ] T1.2: second thing pending\n  - [ ] 1.3: bare id, indented\n- [ ] no id here\n- [X] T9.9: uppercase is ignored\n";

    #[test]
    fn parses_task_lines_only() {
        let tasks = parse(FILE);
        let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["T1.1", "T1.2", "1.3", "TASK"]);
        assert!(tasks[0].done && !tasks[1].done);
        assert_eq!(tasks[1].description, "second thing pending");
        assert_eq!(tasks[1].line, 7);
    }

    #[test]
    fn split_checkbox_reports_state_rest_and_rejects_non_task_lines() {
        assert_eq!(split_checkbox("- [ ] rest"), Some((false, "rest")));
        assert_eq!(split_checkbox("- [x] rest"), Some((true, "rest")));
        assert_eq!(split_checkbox("  - [x] indented"), Some((true, "indented")));
        assert_eq!(split_checkbox("prose"), None);
        // Uppercase is not a checkbox, matching `parses_task_lines_only`.
        assert_eq!(split_checkbox("- [X] upper"), None);
    }

    #[test]
    fn keeps_origin_letter() {
        let tasks = parse("- [ ] T1.1: a\n- [ ] H3.1: b\n- [ ] D2.3: c\n- [ ] 1.1: d\n");
        let got: Vec<_> = tasks
            .iter()
            .map(|t| (t.id.as_str(), t.origin, t.is_malformed()))
            .collect();
        assert_eq!(
            got,
            [
                ("T1.1", Some('T'), false),
                ("H3.1", None, false),
                ("D2.3", Some('D'), false),
                ("1.1", None, false)
            ]
        );
    }

    #[test]
    fn the_progress_token_is_stripped_from_descriptions() {
        for (line, id, description) in [
            (
                "- [ ] T1.1: [RP.BA] add the greeting file",
                "T1.1",
                "add the greeting file",
            ),
            (
                "- [x] T1.2: [-.-B-!] fix the second task",
                "T1.2",
                "fix the second task",
            ),
            (
                "- [ ] T1.3: [RP.+BA] a self-reviewed task",
                "T1.3",
                "a self-reviewed task",
            ),
        ] {
            let tasks = parse(line);
            assert_eq!(tasks[0].id, id, "{line}");
            assert_eq!(tasks[0].description, description, "{line}");
        }
        // A token-only description is empty but valid.
        let tasks = parse("- [ ] T1.4: [RP.BA]");
        assert_eq!(tasks[0].description, "");
    }

    #[test]
    fn bracketed_text_that_is_not_a_progress_token_is_kept() {
        for line in [
            "- [ ] T1.1: [TODO] fix this",
            "- [ ] T1.1: [see the spec] do the thing",
            "- [ ] T1.1: [RP.BAXY] too long",
            "- [ ] T1.1: [no-close-brace still going and going here",
        ] {
            let tasks = parse(line);
            assert!(
                tasks[0].description.starts_with('['),
                "{line}: {}",
                tasks[0].description
            );
        }
    }

    #[test]
    fn next_pending_and_mark_done_survive_progress_tokens() {
        let file = "- [x] T1.1: [RP.BA] done thing\n- [ ] T1.2: [--.B-] pending thing\n";
        let tasks = parse(file);
        assert_eq!(next_pending(&tasks).unwrap().id, "T1.2");
        let out = mark_done(file, next_pending(&tasks).unwrap()).unwrap();
        assert!(out.contains("- [x] T1.2: [--.B-] pending thing"));
        // The ID match still works after the token was written.
        let tasks = parse(&out);
        assert_eq!(next_pending(&tasks), None);
    }

    #[test]
    fn malformed_ids_have_no_origin() {
        for line in [
            "- [ ] X1.1: a",
            "- [ ] t1.1: a",
            "- [ ] TT1.1: a",
            "- [ ] T1: a",
            "- [ ] T.1: a",
            "- [ ] T1.: a",
            "- [ ] T1.1.2: a",
            "- [ ] T-1.1: a",
        ] {
            let tasks = parse(line);
            assert!(tasks[0].is_malformed(), "{line}");
            assert_eq!(tasks[0].origin, None, "{line}");
        }
    }

    #[test]
    fn next_pending_skips_done_and_malformed() {
        let tasks = parse("- [x] T1.1: a\n- [ ] free text\n- [ ] T1.2: b\n");
        assert_eq!(next_pending(&tasks).unwrap().id, "T1.2");
        assert!(next_pending(&parse("- [x] T1.1: a\n")).is_none());
    }

    #[test]
    fn is_pending_rejects_done_and_malformed_tasks() {
        let pending = &parse("- [ ] T1.1: a\n")[0];
        let done = &parse("- [x] T1.1: a\n")[0];
        let malformed = &parse("- [ ] no id here\n")[0];
        assert!(pending.is_pending());
        assert!(!done.is_pending());
        assert!(!malformed.is_pending());
    }

    #[test]
    fn group_number_is_the_leading_id_number() {
        let tasks = parse("- [ ] T1.2: a\n- [ ] D12.3: b\n- [ ] 1.2: c\n");
        assert_eq!(tasks[0].group_number(), Some("1".into()));
        assert_eq!(tasks[1].group_number(), Some("12".into()));
        assert_eq!(tasks[2].group_number(), Some("1".into()));
        assert_eq!(group_number_of("T1.2"), Some("1".into()));
        assert_eq!(group_number_of("D12.3"), Some("12".into()));
        assert_eq!(group_number_of("1.2"), Some("1".into()));
        assert_eq!(group_number_of("T1"), None);
        assert_eq!(group_number_of("TASK"), None);
        // A malformed task line has the placeholder ID, so no group number.
        let malformed = &parse("- [ ] no id here\n")[0];
        assert_eq!(malformed.group_number(), None);
    }

    #[test]
    fn description_hash_is_deterministic_and_distinguishes() {
        let first = &parse("- [ ] T1.1: a task description\n")[0];
        let second = &parse("- [x] T9.9: a task description\n")[0];
        let other = &parse("- [ ] T1.2: another description\n")[0];
        let hash = first.description_hash();
        assert_eq!(hash, second.description_hash());
        assert_ne!(hash, other.description_hash());
        // A SHA-256 hex digest: 64 lowercase ASCII hex characters.
        assert_eq!(hash.len(), 64);
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn mark_done_by_line_preserves_everything_else() {
        let tasks = parse(FILE);
        let out = mark_done(FILE, &tasks[1]).unwrap();
        assert_eq!(out, FILE.replace("- [ ] T1.2", "- [x] T1.2"));
    }

    #[test]
    fn mark_done_finds_shifted_task_by_id() {
        let tasks = parse(FILE);
        let shifted = format!("## Discovery Round 1\n- [ ] D1.1: injected\n{FILE}");
        let out = mark_done(&shifted, &tasks[1]).unwrap();
        assert!(out.contains("- [x] T1.2: second thing pending"));
        assert!(out.contains("- [ ] D1.1: injected"));
    }

    #[test]
    fn mark_done_falls_back_to_full_text_for_malformed() {
        let tasks = parse(FILE);
        let malformed = &tasks[3];
        let shifted = format!("extra\n{FILE}");
        let out = mark_done(&shifted, malformed).unwrap();
        assert!(out.contains("- [x] no id here"));
    }

    #[test]
    fn mark_done_is_a_no_op_when_already_checked() {
        let tasks = parse(FILE);
        let checked = FILE.replace("- [ ] T1.2", "- [x] T1.2");
        assert!(mark_done(&checked, &tasks[1]).is_none());
    }

    #[test]
    fn mark_done_adds_trailing_newline() {
        let tasks = parse("- [ ] T1.1: a");
        assert_eq!(
            mark_done("- [ ] T1.1: a", &tasks[0]).unwrap(),
            "- [x] T1.1: a\n"
        );
    }
}
