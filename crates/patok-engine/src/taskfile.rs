//! Reading and atomically rewriting the task file.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use patok_core::task::{self, Task};

use patok_core::task::split_id;

pub fn load(path: &Path) -> io::Result<Vec<Task>> {
    Ok(task::parse(&std::fs::read_to_string(path)?))
}

/// Merges the task file's current tasks into the in-memory queue. New tasks
/// are added in file order, pending tasks missing from the file are dropped, and pending ones
/// take the file's description and line number. Completed tasks and the `running` task are
/// never touched (the running one only follows the file when it was ticked there).
pub fn reconcile(memory: &[Task], file: Vec<Task>, running: Option<&str>) -> Vec<Task> {
    let find = |id: &str| memory.iter().position(|t| !t.is_malformed() && t.id == id);
    let mut merged: Vec<Task> = Vec::with_capacity(file.len());
    let mut in_file = HashSet::new();
    for ft in file {
        let kept = find(&ft.id)
            .map(|i| &memory[i])
            .filter(|m| m.done || (running == Some(m.id.as_str()) && !ft.done));
        if let Some(m) = kept {
            in_file.insert(m.id.clone());
            merged.push(m.clone());
        } else {
            if !ft.is_malformed() {
                in_file.insert(ft.id.clone());
            }
            merged.push(ft);
        }
    }
    // Completed and running tasks that vanished from the file stay, after their predecessor.
    for (i, m) in memory.iter().enumerate() {
        let protected = m.done || running == Some(m.id.as_str());
        if m.is_malformed() || !protected || in_file.contains(&m.id) {
            continue;
        }
        let at = memory[..i]
            .iter()
            .rev()
            .find_map(|p| {
                merged
                    .iter()
                    .position(|t| !p.is_malformed() && t.id == p.id)
            })
            .map_or(0, |pos| pos + 1);
        merged.insert(at, m.clone());
    }
    merged
}

/// Header of a task file the engine creates itself.
const MINIMAL_HEADER: &str = "# Tasks\n";

/// Returns the task file's text, first creating the file with a minimal header if it does
/// not exist.
pub fn read_or_create(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            write_atomic(path, MINIMAL_HEADER)?;
            Ok(MINIMAL_HEADER.to_string())
        }
        other => other,
    }
}

/// Ticks `task` in the file at `path`. Returns whether a matching unchecked line was found;
/// when none is, nothing is written (the task was already checked or the file moved on).
pub fn mark_done(path: &Path, task: &Task) -> io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let Some(updated) = task::mark_done(&text, task) else {
        return Ok(false);
    };
    write_atomic(path, &updated)?;
    Ok(true)
}

/// Writes the task's five-position progress token into its line, and ticks the line when `done`. The
/// token sits between the ID's colon and the description, replacing a token
/// already there; this is the last mutation before the commit, so it runs
/// under the task-file lock. Returns whether the task's line was found; when
/// none is, nothing is written. Every line except the target survives
/// byte-identical (T77.1): line endings -- `\n`, `\r\n` or none on an
/// unterminated last line -- are preserved, and nothing is normalized.
pub fn write_progress(path: &Path, task: &Task, token: &str, done: bool) -> io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let Some(index) = task_line(&text, task) else {
        return Ok(false);
    };
    let mut updated = String::with_capacity(text.len() + token.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i == index {
            // The line's own terminator is kept, whatever it is.
            let (content, terminator) = line_line_ending(line);
            updated.push_str(&with_progress(content, token, done));
            updated.push_str(terminator);
        } else {
            updated.push_str(line);
        }
    }
    write_atomic(path, &updated)?;
    Ok(true)
}

/// Splits one `split_inclusive('\n')` piece into its content and line ending:
/// `\n`, `\r\n`, or nothing on an unterminated final line.
fn line_line_ending(line: &str) -> (&str, &str) {
    match line.strip_suffix('\n') {
        Some(content) => match content.strip_suffix('\r') {
            Some(stripped) => (stripped, "\r\n"),
            None => (content, "\n"),
        },
        None => (line, ""),
    }
}

/// Appends `line` as the next line of the task file (T76.1's append, moved
/// here for T77.1 so the engine's inject path and the shell share one append
/// primitive): a single O_APPEND write, prefixed with a separating newline
/// only when the existing file is non-empty without a trailing one, so the
/// text lands exactly as given as one fresh line and no existing byte
/// changes. Returns the exact chunk written, which the engine records in its
/// injected-chunk ledger.
pub fn append_line(path: &Path, line: &str) -> io::Result<String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    // A separating newline is needed only after an existing last line that
    // does not end with one; a fresh or empty file starts the line itself.
    let mut prefix = "";
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > 0 {
        let mut file = std::fs::File::open(path)?;
        let mut last = [0u8];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            prefix = "\n";
        }
    }
    let mut chunk = String::with_capacity(prefix.len() + line.len() + 1);
    chunk.push_str(prefix);
    chunk.push_str(line);
    chunk.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    file.write_all(chunk.as_bytes())?;
    Ok(chunk)
}

/// The next unused `T` number for an injected line (T77.1): one past the
/// highest major number of a well-formed `T`-prefixed task line in `file`,
/// or 1 when the file has none. Completed and pending lines count alike;
/// `H`-, `D`- and bare-numbered and malformed lines are ignored.
pub fn next_task_number(file: &str) -> usize {
    task::parse(file)
        .iter()
        .filter(|t| !t.is_malformed() && t.origin == Some('T'))
        .filter_map(|t| {
            t.id[1..]
                .split_once('.')
                .and_then(|(major, _)| major.parse::<usize>().ok())
        })
        .max()
        .map_or(1, |highest| highest + 1)
}

/// Normalizes the inject modal's confirmed input (T77.1) into a
/// well-formed unchecked task line: the trimmed text gains the
/// `- [ ] ` checkbox when it does not already start with it, and a fresh
/// `T<N>.1: ` id -- with `N` from [`next_task_number`] over `file` -- when
/// the checkbox-stripped remainder does not already start with a
/// well-formed `T`-prefixed id. A text that already carries both is
/// returned with only the whitespace trimming, and a text with other text
/// before them is treated as a plain description and gains a fresh id.
/// Never reads or writes the file; the caller reads it under the
/// task-file lock first.
pub fn normalize_injected(text: &str, file: &str) -> String {
    const CHECKBOX: &str = "- [ ] ";
    const CHECKED: &str = "- [x] ";
    let trimmed = text.trim();
    let rest = if let Some(rest) = trimmed.strip_prefix(CHECKBOX) {
        rest
    } else if let Some(rest) = trimmed.strip_prefix(CHECKED) {
        // A checked input still lands as an unchecked line.
        rest
    } else {
        trimmed
    };
    let has_id = split_id(rest).is_some_and(|(_, origin, _)| origin == Some('T'));
    if has_id {
        return format!("{CHECKBOX}{rest}");
    }
    format!("{CHECKBOX}T{}.1: {rest}", next_task_number(file))
}

/// The task's unchecked line, by the same tiers as `mark_done`: the recorded
/// line number, the first unchecked line with the same `ID:` prefix, then a
/// line whose full text equals the original. An already-checked line is not
/// touched again.
fn task_line(text: &str, task: &Task) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
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

/// Whether the line is an unchecked task line.
fn is_unchecked(line: &str) -> bool {
    line.trim_start().starts_with("- [ ] ")
}

/// Whether the line's text after the checkbox starts with `<task id>:`.
fn same_id(line: &str, task: &Task) -> bool {
    if task.is_malformed() {
        return line == task.raw;
    }
    let rest = line.trim_start().strip_prefix("- [ ] ").unwrap_or("");
    rest.strip_prefix(task.id.as_str())
        .is_some_and(|r| r.starts_with(':'))
}

/// One task line with the progress token inserted after the ID's colon (a
/// token already there is replaced), optionally ticked. A line already checked
/// is returned unchanged.
fn with_progress(line: &str, token: &str, done: bool) -> String {
    let trimmed = line.trim_start();
    let (checkbox, rest) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
        ("- [ ] ", rest)
    } else {
        return line.to_string();
    };
    let Some((id, _, description)) = split_id(rest) else {
        return line.to_string();
    };
    let description = strip_progress_token(description.trim());
    let mut updated = String::with_capacity(line.len() + token.len());
    updated.push_str(if done { "- [x] " } else { checkbox });
    updated.push_str(id);
    updated.push_str(": ");
    updated.push_str(token);
    if !description.is_empty() {
        updated.push(' ');
        updated.push_str(description);
    }
    updated
}

/// Strips a progress token standing between the ID's colon and the
/// description, so a replaced token does not pile up.
fn strip_progress_token(description: &str) -> &str {
    patok_core::task::strip_progress(description)
}

/// Checks the planner's result: `after` must be `before` plus appended content, and every task
/// line in the appended part must be unchecked with a well-formed `T`-prefixed ID that is
/// unique in the whole file. Existing `H` and `D` lines in `before` are untouched and stay
/// valid. Returns the number of tasks added.
pub fn validate_append(before: &str, after: &str) -> Result<usize, String> {
    validate_append_ids(before, after).map(|ids| ids.len())
}

/// Like [`validate_append`], but returns the IDs of the appended tasks in file order instead
/// of their count. The engine remembers these as the session's UI-added task IDs.
pub fn validate_append_ids(before: &str, after: &str) -> Result<Vec<String>, String> {
    validate_appended_ignoring(before, after, &[], 'T', "planner")
}

/// The discovery round's variant of [`validate_append_ids`]: appended tasks must carry
/// `D`-prefixed IDs; an appended `T`, `H` or bare-number ID is a violation.
pub fn validate_discovery_append_ids(before: &str, after: &str) -> Result<Vec<String>, String> {
    validate_appended_ignoring(before, after, &[], 'D', "discovery")
}

/// [`validate_append_ids`] with the injected-chunk accounting of T77.1: the
/// exact chunks the engine itself appended through `inject_task` during the
/// session are filtered out of the appended region first, so an injected
/// line -- whatever its shape -- neither fails the session's append
/// validation nor counts as one of the session's tasks.
pub fn validate_append_ignoring(
    before: &str,
    after: &str,
    injected: &[String],
) -> Result<Vec<String>, String> {
    validate_appended_ignoring(before, after, injected, 'T', "planner")
}

/// The discovery round's variant of [`validate_append_ignoring`].
pub fn validate_discovery_append_ignoring(
    before: &str,
    after: &str,
    injected: &[String],
) -> Result<Vec<String>, String> {
    validate_appended_ignoring(before, after, injected, 'D', "discovery")
}

/// The shared append check: `after` must be `before` plus appended content, and every task
/// line in the appended part must be unchecked with a well-formed `prefix`-prefixed ID that
/// is unique in the whole file. Lines with other prefixes in `before` are untouched and stay
/// valid. Returns the IDs of the appended tasks in file order.
///
/// `injected` holds the exact chunks the engine appended through
/// `inject_task` since the run started (T77.1): each is removed from the
/// appended region -- one occurrence per record, unmatched records ignored
/// -- before the remainder is validated, so a line injected mid-session
/// (even a malformed or duplicate one) is not this run's doing and does not
/// reject it.
fn validate_appended_ignoring(
    before: &str,
    after: &str,
    injected: &[String],
    prefix: char,
    role: &str,
) -> Result<Vec<String>, String> {
    let Some(appended) = after.strip_prefix(before) else {
        return Err(format!(
            "the {role} changed existing content of the task file"
        ));
    };
    if !before.is_empty()
        && !before.ends_with('\n')
        && !appended.is_empty()
        && !appended.starts_with('\n')
    {
        return Err(format!(
            "the {role} changed the last existing line of the task file"
        ));
    }
    let mut remaining = appended.to_string();
    for chunk in injected {
        if let Some(at) = remaining.find(chunk.as_str()) {
            remaining.replace_range(at..at + chunk.len(), "");
        }
    }
    let mut seen: HashSet<String> = task::parse(before)
        .into_iter()
        .filter(|t| !t.is_malformed())
        .map(|t| t.id)
        .collect();
    let added = task::parse(&remaining);
    for t in &added {
        if t.is_malformed() {
            return Err(format!(
                "the {role} added a task without a valid ID: {}",
                t.raw.trim()
            ));
        }
        if t.origin != Some(prefix) {
            return Err(format!(
                "the {role} added a task without a {prefix}-prefixed ID (only {prefix} tasks \
                 may be appended, not other prefixes or bare numbers): {}",
                t.raw.trim()
            ));
        }
        if t.done {
            return Err(format!(
                "the {role} added an already checked task: {}",
                t.id
            ));
        }
        if !seen.insert(t.id.clone()) {
            return Err(format!("the {role} added a duplicate task ID: {}", t.id));
        }
    }
    Ok(added.into_iter().map(|t| t.id).collect())
}

/// Puts `contents` back as the task file after a rejected planner run.
pub fn restore(path: &Path, contents: &str) -> io::Result<()> {
    write_atomic(path, contents)
}

/// Puts `before` back as the task file after a rejected append-style run,
/// keeping the recorded injected chunks (T77.1): each chunk already contained
/// in `before` -- the line was injected before the run started -- is skipped;
/// the rest are re-appended verbatim in recorded order, so a line injected
/// during the session survives the restore at the end of the file.
pub fn restore_keeping(path: &Path, before: &str, injected: &[String]) -> io::Result<()> {
    use std::io::Write;
    write_atomic(path, before)?;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    for chunk in injected {
        if before.contains(chunk.as_str()) {
            continue;
        }
        file.write_all(chunk.as_bytes())?;
    }
    Ok(())
}

/// How many of the most recent completed task lines the startup cleanup keeps.
pub const KEEP_COMPLETED: usize = 5;

/// The startup cleanup (T72.1), as a pure function of the file text: removes
/// checked task lines of finished groups (every task of the group checked),
/// except the last [`KEEP_COMPLETED`] checked lines in file order and every
/// line of the last group. Unchecked lines, unfinished groups and non-task
/// lines are never touched. A `## ` section header whose section (from the
/// heading line to the next heading line or end of file) is left with no task
/// lines is removed too (T79.1), whether the pruning emptied the section or
/// it was already empty, so no dangling heading is left over an empty
/// section; a heading whose section keeps at least one task line (pending,
/// unchecked or kept completed) always stays. Blank lines that a heading
/// removal would double up are dropped. Every kept line survives
/// byte-identical. Returns `None` when no line is removed, so nothing needs
/// to be written.
pub fn cleanup_text(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // Per line index: the group and done state of a task line with a
    // well-formed ID (`None` for everything else, which is never pruned).
    let mut info: Vec<Option<(u32, bool)>> = vec![None; lines.len()];
    let mut groups: HashMap<u32, bool> = HashMap::new();
    let mut checked: Vec<usize> = Vec::new();
    let mut last_group: Option<u32> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let (done, rest) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            (false, rest)
        } else if let Some(rest) = trimmed.strip_prefix("- [x] ") {
            (true, rest)
        } else {
            continue;
        };
        let Some((id, _, _)) = split_id(rest) else {
            continue;
        };
        // The group is the number before the dot; the origin letter is ignored.
        let Some(group) = id
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .split_once('.')
            .and_then(|(major, _)| major.parse::<u32>().ok())
        else {
            continue;
        };
        groups
            .entry(group)
            .and_modify(|all_done| *all_done &= done)
            .or_insert(done);
        if done {
            checked.push(i);
        }
        info[i] = Some((group, done));
        last_group = Some(group);
    }
    // The last KEEP_COMPLETED checked lines by file position are kept.
    let kept_recent: HashSet<usize> = checked[checked.len().saturating_sub(KEEP_COMPLETED)..]
        .iter()
        .copied()
        .collect();
    // The completed-task pruning (T72.1) as a per-line deletion mask.
    let mut deleted: Vec<bool> = vec![false; lines.len()];
    for i in 0..lines.len() {
        deleted[i] = matches!(info[i], Some((group, true))
            if groups[&group] && Some(group) != last_group && !kept_recent.contains(&i));
    }
    // Heading removal (T79.1): a section header whose section ends up with
    // no remaining task lines goes too. A heading is literally a line
    // starting with `## ` (no leading-whitespace trim, unlike task lines);
    // its section spans to the next heading line or end of file, always in
    // the original file, so one removal never extends another section.
    // Task-line detection is independent of ID well-formedness: a malformed
    // task line is never pruned, so it keeps its heading.
    let is_heading = |i: usize| lines[i].starts_with("## ");
    let is_task_line = |i: usize| {
        let trimmed = lines[i].trim_start();
        trimmed.starts_with("- [ ] ") || trimmed.starts_with("- [x] ")
    };
    let headings: Vec<usize> = (0..lines.len()).filter(|&i| is_heading(i)).collect();
    for (pos, &h) in headings.iter().enumerate() {
        let end = headings.get(pos + 1).copied().unwrap_or(lines.len());
        let has_remaining_task = (h + 1..end).any(|i| is_task_line(i) && !deleted[i]);
        if !has_remaining_task {
            deleted[h] = true;
        }
    }
    // Blank collapse: a deletion run that removed a heading must not leave
    // a doubled blank line where the section was. When the line right before
    // such a run is blank, every blank line right after the run goes too.
    let mut i = 0;
    while i < lines.len() {
        if !deleted[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && deleted[i] {
            i += 1;
        }
        let cut_heading = (start..i).any(&is_heading);
        if cut_heading && start > 0 && lines[start - 1].trim().is_empty() {
            while i < lines.len() && lines[i].trim().is_empty() {
                deleted[i] = true;
                i += 1;
            }
        }
    }
    let mut cleaned = String::with_capacity(text.len());
    let mut removed = false;
    for (i, line) in lines.iter().enumerate() {
        if deleted[i] {
            removed = true;
        } else {
            cleaned.push_str(line);
        }
    }
    removed.then_some(cleaned)
}

/// Reads the task file at `path`, runs the startup cleanup and atomically
/// rewrites the file when it pruned something. A missing file is a no-op
/// (the file is not created). Returns whether the file was rewritten.
pub fn cleanup_completed(path: &Path) -> io::Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        other => other?,
    };
    match cleanup_text(&text) {
        Some(cleaned) => {
            write_atomic(path, &cleaned)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The result of [`remove_task`].
#[derive(Debug, PartialEq, Eq)]
pub enum RemoveOutcome {
    /// No well-formed task line carries the id; nothing was written.
    NotFound,
    /// A matching line is checked: the removal is refused, nothing was written.
    Completed,
    /// The matching unchecked line(s) were removed and the file rewritten.
    Removed,
}

/// Removes every unchecked task line whose well-formed ID equals `id`
/// (T98.1), atomically rewriting the file. A matching checked line refuses
/// the removal ([`RemoveOutcome::Completed`]); no matching line at all is
/// [`RemoveOutcome::NotFound`] -- both without writing anything, and a
/// missing file reads as `NotFound` and is not created. Malformed lines
/// (no well-formed ID) never match, and every other line survives
/// byte-identical, CRLF and unterminated last lines included, the same
/// convention as `write_progress`; headings and blank lines are left alone
/// (dangling-heading cleanup belongs to the startup cleanup).
pub fn remove_task(path: &Path, id: &str) -> io::Result<RemoveOutcome> {
    let text = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(RemoveOutcome::NotFound),
        other => other?,
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // Per-line deletion mask. A line matches when, after leading whitespace
    // and the checkbox, its ID is well-formed and equals `id`; a checked
    // match refuses the whole removal.
    let mut matched = vec![false; lines.len()];
    let mut completed = false;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let (checked, rest) = if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            (false, rest)
        } else if let Some(rest) = trimmed.strip_prefix("- [x] ") {
            (true, rest)
        } else {
            continue;
        };
        if split_id(rest).is_some_and(|(found, _, _)| found == id) {
            if checked {
                completed = true;
            } else {
                matched[i] = true;
            }
        }
    }
    if completed {
        return Ok(RemoveOutcome::Completed);
    }
    if !matched.iter().any(|&m| m) {
        return Ok(RemoveOutcome::NotFound);
    }
    let mut updated = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        if !matched[i] {
            updated.push_str(line);
        }
    }
    write_atomic(path, &updated)?;
    Ok(RemoveOutcome::Removed)
}

/// Writes through a temporary file in the same directory, then renames over the target.
fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".patok-tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "# T\n- [x] T1.1: a\n- [ ] T1.2: b\n";

    #[test]
    fn validate_append_counts_new_tasks() {
        let after = format!("{BASE}\n## More\n- [ ] T2.1: c\n- [ ] T2.2: d\n");
        assert_eq!(validate_append(BASE, &after), Ok(2));
        assert_eq!(validate_append(BASE, BASE), Ok(0));
        assert_eq!(validate_append("", "- [ ] T1.1: a\n"), Ok(1));
    }

    #[test]
    fn validate_append_ids_returns_the_appended_ids_in_file_order() {
        let after = format!("{BASE}\n## More\n- [ ] T2.1: c\n- [ ] T2.2: d\n");
        assert_eq!(
            validate_append_ids(BASE, &after),
            Ok(vec!["T2.1".to_string(), "T2.2".to_string()])
        );
        assert_eq!(validate_append_ids(BASE, BASE), Ok(vec![]));
        assert!(validate_append_ids(BASE, &format!("{BASE}- [ ] H2.1: h\n")).is_err());
    }

    #[test]
    fn validate_accepts_existing_h_and_d_lines_but_only_t_appends() {
        let with_origins = "# T\n- [x] H1.1: a\n- [ ] D1.2: b\n";
        assert_eq!(validate_append(with_origins, with_origins), Ok(0));
        let after = format!("{with_origins}- [ ] T2.1: c\n");
        assert_eq!(validate_append(with_origins, &after), Ok(1));
    }

    #[test]
    fn validate_append_rejects_violations() {
        let edited = BASE.replace("[ ] T1.2", "[x] T1.2");
        assert!(validate_append(BASE, &edited).is_err());
        assert!(validate_append(BASE, &format!("{BASE}- [x] T2.1: c\n")).is_err());
        assert!(validate_append(BASE, &format!("{BASE}- [ ] T1.2: dup\n")).is_err());
        assert!(validate_append(BASE, &format!("{BASE}- [ ] T2.1: a\n- [ ] T2.1: b\n")).is_err());
        assert!(validate_append(BASE, &format!("{BASE}- [ ] no id\n")).is_err());
        assert!(validate_append("# T\n- [ ] T1.1: a", "# T\n- [ ] T1.1: a more\n").is_err());
        // Appended lines must be T tasks.
        for line in [
            "- [ ] H2.1: human\n",
            "- [ ] D2.1: discovery\n",
            "- [ ] 2.1: bare number\n",
        ] {
            let err = validate_append(BASE, &format!("{BASE}{line}")).unwrap_err();
            assert!(err.contains("T-prefixed"), "{line}: {err}");
        }
    }

    #[test]
    fn validate_discovery_append_accepts_only_d_tasks() {
        let base = "# T\n- [x] T1.1: a\n- [ ] D1.2: b\n";
        let after = format!("{base}- [ ] D2.1: c\n- [ ] D3.1: d\n");
        assert_eq!(
            validate_discovery_append_ids(base, &after),
            Ok(vec!["D2.1".to_string(), "D3.1".to_string()])
        );
        assert_eq!(validate_discovery_append_ids(base, base), Ok(vec![]));
        // Appended T, H and bare-number IDs are violations.
        for line in [
            "- [ ] T2.1: planner\n",
            "- [ ] H2.1: human\n",
            "- [ ] 2.1: bare number\n",
        ] {
            let err = validate_discovery_append_ids(base, &format!("{base}{line}")).unwrap_err();
            assert!(err.contains("D-prefixed"), "{line}: {err}");
        }
        let dup = format!("{base}- [ ] D2.1: c\n- [ ] D2.1: d\n");
        assert!(validate_discovery_append_ids(base, &dup).is_err());
        let checked = format!("{base}- [x] D2.1: c\n");
        assert!(validate_discovery_append_ids(base, &checked).is_err());
        let edited = base.replace("[ ] D1.2", "[x] D1.2");
        assert!(validate_discovery_append_ids(base, &edited).is_err());
    }

    #[test]
    fn reconcile_follows_the_file_but_protects_done_and_running_tasks() {
        let memory = task::parse("- [x] T1.1: a\n- [ ] T1.2: b\n- [ ] T1.3: c\n- [ ] T1.4: d\n");
        let file =
            task::parse("- [ ] T1.1: edited\n- [ ] T1.2: b2\n\n- [ ] T1.4: d\n- [ ] T1.5: e\n");
        let out = reconcile(&memory, file, Some("T1.4"));
        let view: Vec<_> = out
            .iter()
            .map(|t| (t.id.as_str(), t.done, t.description.as_str(), t.line))
            .collect();
        assert_eq!(
            view,
            [
                ("T1.1", true, "a", 1),
                ("T1.2", false, "b2", 2),
                ("T1.4", false, "d", 4),
                ("T1.5", false, "e", 5),
            ]
        );
        // The running task survives removal from the file.
        let out = reconcile(&memory, task::parse("- [ ] T1.2: b\n"), Some("T1.3"));
        let ids: Vec<_> = out.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["T1.1", "T1.2", "T1.3"]);
    }

    #[test]
    fn read_or_create_adds_a_header_only_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        assert_eq!(read_or_create(&path).unwrap(), "# Tasks\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Tasks\n");
        std::fs::write(&path, "# Mine\n").unwrap();
        assert_eq!(read_or_create(&path).unwrap(), "# Mine\n");
    }

    #[test]
    fn write_progress_inserts_the_token_and_optionally_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# T\n- [ ] T1.1: do it\n- [ ] T1.2: later\n").unwrap();
        let tasks = load(&path).unwrap();
        // Without ticking: the token lands between the ID's colon and the text.
        assert!(write_progress(&path, &tasks[0], "[RP.BA]", false).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [ ] T1.1: [RP.BA] do it\n- [ ] T1.2: later\n"
        );
        // With ticking, on the freshly reloaded task.
        let tasks = load(&path).unwrap();
        assert!(write_progress(&path, &tasks[0], "[RP.BA!]", true).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [x] T1.1: [RP.BA!] do it\n- [ ] T1.2: later\n"
        );
        // An existing token is replaced, not piled up.
        let tasks = load(&path).unwrap();
        assert!(write_progress(&path, &tasks[1], "[-.-B-]", true).unwrap());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("- [x] T1.2: [-.-B-] later")
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_progress_is_a_no_op_without_a_matching_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# T\n- [x] T1.1: done\n").unwrap();
        let tasks = load(&path).unwrap();
        // The only line is already checked: nothing is written.
        assert!(!write_progress(&path, &tasks[0], "[RP.BA]", true).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [x] T1.1: done\n"
        );
        // A line shifted but still findable by ID is updated where it is now.
        std::fs::write(&path, "# T\nextra\n- [ ] T1.1: done\n").unwrap();
        let tasks = load(&path).unwrap();
        assert!(write_progress(&path, &tasks[0], "[--.B-]", true).unwrap());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("- [x] T1.1: [--.B-] done")
        );
    }

    #[test]
    fn mark_done_rewrites_file_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# T\n- [ ] T1.1: do it\n").unwrap();
        let tasks = load(&path).unwrap();
        assert!(mark_done(&path, &tasks[0]).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [x] T1.1: do it\n"
        );
        assert!(!mark_done(&path, &tasks[0]).unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn cleanup_keeps_only_the_newest_five_completed_lines() {
        let text = "# T\n- [x] T1.1: a\n- [x] T1.2: b\n- [x] T2.1: c\n- [x] T2.2: d\n\
                    - [x] T3.1: e\n- [x] T3.2: f\n- [x] T3.3: g\n- [ ] T4.1: pending\n";
        assert_eq!(
            cleanup_text(text),
            Some(
                "# T\n- [x] T2.1: c\n- [x] T2.2: d\n- [x] T3.1: e\n- [x] T3.2: f\n\
                  - [x] T3.3: g\n- [ ] T4.1: pending\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn cleanup_keeps_completed_siblings_of_an_unchecked_task() {
        // Group 1 is unfinished (T1.2 unchecked), so its old completed line
        // T1.1 stays; the finished group 2, outside the newest five, goes.
        let text = "- [x] T1.1: a\n- [ ] T1.2: b\n- [x] T2.1: c\n- [x] T2.2: d\n\
                    - [x] T3.1: e\n- [x] T3.2: f\n- [x] T3.3: g\n- [x] T3.4: h\n\
                    - [x] T3.5: i\n- [ ] T4.1: pending\n";
        assert_eq!(
            cleanup_text(text),
            Some(
                "- [x] T1.1: a\n- [ ] T1.2: b\n- [x] T3.1: e\n- [x] T3.2: f\n\
                  - [x] T3.3: g\n- [x] T3.4: h\n- [x] T3.5: i\n- [ ] T4.1: pending\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn cleanup_keeps_the_last_group_even_when_fully_completed() {
        // Eight checked lines: the oldest three of group 1 go, group 2 is
        // last and stays however finished it is.
        let text = "- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n- [x] T1.4: d\n\
                    - [x] T1.5: e\n- [x] T1.6: f\n- [x] T1.7: g\n- [x] T2.1: h\n";
        assert_eq!(
            cleanup_text(text),
            Some(
                "- [x] T1.4: d\n- [x] T1.5: e\n- [x] T1.6: f\n- [x] T1.7: g\n\
                  - [x] T2.1: h\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn cleanup_leaves_an_all_pending_list_unchanged() {
        assert_eq!(cleanup_text("- [ ] T1.1: a\n- [ ] T1.2: b\n"), None);
        // A finished single group is the last group: nothing to prune.
        assert_eq!(cleanup_text("- [x] T1.1: a\n"), None);
    }

    #[test]
    fn cleanup_handles_empty_and_missing_files() {
        assert_eq!(cleanup_text(""), None);
        assert_eq!(cleanup_text("# Tasks\n"), None);
        // A missing file is a no-op and is not created.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        assert!(!cleanup_completed(&path).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn cleanup_keeps_every_kept_line_byte_identical() {
        // Mixed CRLF and LF, an indented H line, prose with trailing spaces,
        // a progress token and no trailing newline on the final line.
        let text = "# T\r\n- [x] T1.1: a\r\n- [x] T1.2: b\r\n- [x] T1.3: c\n\
                    - [x] T1.4: d\n- [x] T1.5: e\n- [x] T1.6: [--.B-] f\n  - [x] H1.7: g\r\n\
                    prose  \n- [x] T2.1: last";
        let expected = "# T\r\n- [x] T1.4: d\n- [x] T1.5: e\n- [x] T1.6: [--.B-] f\n  \
                        - [x] H1.7: g\r\nprose  \n- [x] T2.1: last";
        let cleaned = cleanup_text(text).unwrap();
        assert_eq!(cleaned, expected);
        // Every kept line, its \r included, survives verbatim.
        for kept in [
            "# T\r\n",
            "- [x] T1.6: [--.B-] f\n",
            "  - [x] H1.7: g\r\n",
            "prose  \n",
            "- [x] T2.1: last",
        ] {
            assert!(cleaned.contains(kept), "missing verbatim {kept:?}");
        }
    }

    #[test]
    fn cleanup_removes_a_heading_whose_section_had_only_pruned_tasks() {
        // Group 1 is finished, outside the newest five and not last: all its
        // lines go, so `## Old` loses its section and goes with them. `## Mid`
        // keeps its five checked lines and stays.
        let text = "## Old\n- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n\
                    ## Mid\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                    - [x] T2.4: g\n- [x] T2.5: h\n- [ ] T3.1: last\n";
        assert_eq!(
            cleanup_text(text),
            Some(
                "## Mid\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                  - [x] T2.4: g\n- [x] T2.5: h\n- [ ] T3.1: last\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn cleanup_removes_a_heading_already_over_an_empty_section() {
        // A heading with no task lines at all, empty even before the cleanup.
        assert_eq!(
            cleanup_text("## Empty\n## Next\n- [ ] T1.1: a\n"),
            Some("## Next\n- [ ] T1.1: a\n".to_string())
        );
        // Prose does not count as a task line: the heading goes, prose stays.
        assert_eq!(
            cleanup_text("## Notes\nprose only\n## Real\n- [ ] T1.1: a\n"),
            Some("prose only\n## Real\n- [ ] T1.1: a\n".to_string())
        );
        // A file that is only a heading becomes empty; a single-hash title
        // is not a heading and keeps the file as it was.
        assert_eq!(cleanup_text("## Tasks\n"), Some("".to_string()));
        assert_eq!(cleanup_text("# Tasks\n"), None);
    }

    #[test]
    fn cleanup_keeps_last_group_and_unfinished_group_headings() {
        // The last group is fully completed but never pruned: heading stays.
        assert_eq!(cleanup_text("## Last\n- [x] T1.1: only\n"), None);
        // An unfinished group keeps its checked line: heading stays.
        assert_eq!(cleanup_text("## Wip\n- [x] T1.1: a\n- [ ] T1.2: b\n"), None);
    }

    #[test]
    fn cleanup_leaves_a_file_with_headings_and_nothing_removable_unchanged() {
        // Every section holds a pending task: no heading is removable and
        // nothing is prunable, so no rewrite happens at all.
        assert_eq!(
            cleanup_text("# T\n## A\n- [ ] T1.1: a\n## B\n- [ ] T1.2: b\n"),
            None
        );
    }

    #[test]
    fn cleanup_collapses_blank_lines_around_a_removed_heading() {
        // The blank line before the removed `## Gone` section is kept, the
        // one that would double it after the cut goes; the blank run around
        // plain task pruning elsewhere is untouched by this rule.
        let text = "A\n\n## Gone\n- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n\n\
                    ## Kept\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                    - [x] T2.4: g\n- [x] T2.5: h\n- [ ] T3.1: last\n";
        assert_eq!(
            cleanup_text(text),
            Some(
                "A\n\n## Kept\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                  - [x] T2.4: g\n- [x] T2.5: h\n- [ ] T3.1: last\n"
                    .to_string()
            )
        );
    }

    #[test]
    fn cleanup_keeps_every_kept_line_byte_identical_with_headings() {
        // A removed heading between blanks, a surviving CRLF heading,
        // trailing-space prose and no trailing newline on the final line.
        let text = "A\n\n## Gone\n- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n\n\
                    ## X\r\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                    - [x] T2.4: g\n- [x] T2.5: h\nprose  \n- [ ] T4.1: last";
        let expected = "A\n\n## X\r\n- [x] T2.1: d\n- [x] T2.2: e\n- [x] T2.3: f\n\
                        - [x] T2.4: g\n- [x] T2.5: h\nprose  \n- [ ] T4.1: last";
        let cleaned = cleanup_text(text).unwrap();
        assert_eq!(cleaned, expected);
        // Every kept line, its `\r` included, survives verbatim.
        for kept in [
            "A\n",
            "\n",
            "## X\r\n",
            "- [x] T2.5: h\n",
            "prose  \n",
            "- [ ] T4.1: last",
        ] {
            assert!(cleaned.contains(kept), "missing verbatim {kept:?}");
        }
    }

    #[test]
    fn cleanup_completed_rewrites_once_and_then_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(
            &path,
            "- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n- [x] T1.4: d\n- [x] T1.5: e\n\
              - [x] T1.6: f\n- [x] T1.7: g\n- [ ] T2.1: pending\n",
        )
        .unwrap();
        assert!(cleanup_completed(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [x] T1.3: c\n- [x] T1.4: d\n- [x] T1.5: e\n- [x] T1.6: f\n- [x] T1.7: g\n\
              - [ ] T2.1: pending\n"
        );
        // No temporary file left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // The cleaned file has nothing prunable left.
        assert!(!cleanup_completed(&path).unwrap());
    }

    /// T76.1 (moved from patok-tui for T77.1): the append lands verbatim after
    /// a file that ends with a newline, leaving every existing byte identical.
    #[test]
    fn append_line_appends_verbatim_after_a_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# Tasks\n- [ ] T1.1: first\n").unwrap();
        let chunk = append_line(&path, "- [ ] T2.1: second").unwrap();
        assert_eq!(chunk, "- [ ] T2.1: second\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Tasks\n- [ ] T1.1: first\n- [ ] T2.1: second\n"
        );
    }

    /// A file without a trailing newline gets the separating newline, so the
    /// appended text never joins the last existing line; the returned chunk
    /// carries that separator so the engine's accounting matches the file.
    #[test]
    fn append_line_separates_from_a_file_without_a_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [ ] T1.1: first").unwrap();
        let chunk = append_line(&path, "- [ ] T2.1: second").unwrap();
        assert_eq!(chunk, "\n- [ ] T2.1: second\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: first\n- [ ] T2.1: second\n"
        );
    }

    /// A missing or empty file is created, with the line as its whole content
    /// and no leading blank line.
    #[test]
    fn append_line_creates_a_missing_or_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("TASKS.md");
        append_line(&missing, "- [ ] T1.1: first").unwrap();
        assert_eq!(
            std::fs::read_to_string(&missing).unwrap(),
            "- [ ] T1.1: first\n"
        );

        std::fs::write(&missing, "").unwrap();
        append_line(&missing, "- [ ] T2.1: second").unwrap();
        assert_eq!(
            std::fs::read_to_string(&missing).unwrap(),
            "- [ ] T2.1: second\n"
        );
    }

    /// The text lands exactly as given -- no `- [ ]` prefix added, no
    /// trimming, multi-line text kept as it is.
    #[test]
    fn append_line_writes_the_text_exactly_as_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let text = "  - [ ] T9.1: keep my spaces  ";
        append_line(&path, text).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "  - [ ] T9.1: keep my spaces  \n"
        );
    }

    /// T77.1: plain text gains the checkbox and a fresh id, one past the
    /// highest `T` number in the file; an empty or prose-only file starts
    /// at 1.
    #[test]
    fn normalize_injected_adds_checkbox_and_fresh_id_to_plain_text() {
        assert_eq!(
            normalize_injected("fix the login flow", "# T\n- [x] T1.1: a\n"),
            "- [ ] T2.1: fix the login flow"
        );
        assert_eq!(
            normalize_injected("first thing ever", ""),
            "- [ ] T1.1: first thing ever"
        );
        assert_eq!(
            normalize_injected("only prose around", "# Tasks\n\nSome prose.\n"),
            "- [ ] T1.1: only prose around"
        );
    }

    /// T77.1: a text that already has the checkbox gains only the id.
    #[test]
    fn normalize_injected_adds_only_the_id_to_a_checkbox_text() {
        assert_eq!(
            normalize_injected("- [ ] fix the login flow", "# T\n- [x] T1.1: a\n"),
            "- [ ] T2.1: fix the login flow"
        );
    }

    /// T77.1: a text that already carries both the checkbox and a
    /// `T`-prefixed id is left as it is -- even when its number equals or
    /// exceeds the file's highest, and whatever the file holds; nothing is
    /// reused or rewritten.
    #[test]
    fn normalize_injected_keeps_a_fully_formatted_line_as_is() {
        let file = "# T\n- [x] T3.1: a\n- [ ] T3.2: b\n";
        assert_eq!(
            normalize_injected("- [ ] T5.1: fix", file),
            "- [ ] T5.1: fix"
        );
        assert_eq!(
            normalize_injected("- [ ] T3.1: same as the highest", file),
            "- [ ] T3.1: same as the highest"
        );
        assert_eq!(
            normalize_injected("- [ ] T9.1: beyond the highest", file),
            "- [ ] T9.1: beyond the highest"
        );
        assert_eq!(file, "# T\n- [x] T3.1: a\n- [ ] T3.2: b\n");
    }

    /// T77.1: leading and trailing whitespace is trimmed before the checks,
    /// so an already-formatted line is not double-prefixed.
    #[test]
    fn normalize_injected_trims_whitespace_before_the_checks() {
        assert_eq!(
            normalize_injected("  fix it  ", "# T\n- [ ] T1.1: a\n"),
            "- [ ] T2.1: fix it"
        );
        assert_eq!(
            normalize_injected("  - [ ] T5.1: x  ", "# T\n- [ ] T1.1: a\n"),
            "- [ ] T5.1: x"
        );
        assert_eq!(
            normalize_injected(" - [ ] keep my spaces inside ", ""),
            "- [ ] T1.1: keep my spaces inside"
        );
    }

    /// T77.1: other text before the checkbox and the id makes the whole
    /// input description text, gaining a fresh id; a checked input still
    /// lands as an unchecked line.
    #[test]
    fn normalize_injected_treats_prefaced_text_as_description_and_unchecks() {
        assert_eq!(
            normalize_injected("note - [ ] T5.1: x", "# T\n- [x] T1.1: a\n"),
            "- [ ] T2.1: note - [ ] T5.1: x"
        );
        assert_eq!(
            normalize_injected(" - [x] T5.1: done ", "# T\n- [x] T1.1: a\n"),
            "- [ ] T5.1: done"
        );
        // A non-T id (D, H or bare) after the checkbox gains a fresh T id.
        assert_eq!(
            normalize_injected("- [ ] D2.1: x", "# T\n- [x] T1.1: a\n"),
            "- [ ] T2.1: D2.1: x"
        );
    }

    /// T77.1: the scan counts the major numbers of well-formed `T` lines
    /// only -- completed and pending alike -- ignoring prose, `H`, `D`,
    /// bare-numbered and malformed lines.
    #[test]
    fn next_task_number_scans_the_highest_t_line_completed_or_pending() {
        let mixed = "# T\n\
                     Some prose.\n\
                     - [x] T1.1: a\n\
                     - [ ] T1.2: b\n\
                     - [x] T4.1: c\n\
                     - [ ] T2.1: d\n\
                     - [x] H3.1: e\n\
                     - [ ] D7.1: f\n\
                     - [ ] 9.1: g\n\
                     - [ ] no id\n";
        assert_eq!(next_task_number(mixed), 5);
        assert_eq!(next_task_number(""), 1);
        assert_eq!(next_task_number("# Tasks\n\nSome prose.\n"), 1);
        assert_eq!(next_task_number("- [ ] D9.1: only discovery\n"), 1);
    }

    /// T77.1: `write_progress` is byte-exact -- a CRLF file keeps every `\r`
    /// on the untouched lines (and the target line keeps its own CRLF), and
    /// an appended non-target line survives verbatim.
    #[test]
    fn write_progress_preserves_line_endings_and_appended_lines_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(
            &path,
            "# T\r\n- [ ] T1.1: do it\r\n- [ ] T9.1: injected  \r\nlast line no newline",
        )
        .unwrap();
        let tasks = load(&path).unwrap();
        assert!(write_progress(&path, &tasks[0], "[RP.BA]", true).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\r\n- [x] T1.1: [RP.BA] do it\r\n- [ ] T9.1: injected  \r\nlast line no newline"
        );
    }

    /// T77.1: an unterminated last line as the target keeps its lack of a
    /// trailing newline.
    #[test]
    fn write_progress_keeps_an_unterminated_last_line_terminated_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [ ] T1.1: do it").unwrap();
        let tasks = load(&path).unwrap();
        assert!(write_progress(&path, &tasks[0], "[--.B!]", true).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [x] T1.1: [--.B!] do it"
        );
    }

    /// T77.1: a line injected during a session no longer fails the session's
    /// append validation, whatever its shape, and does not count as one of
    /// the session's tasks.
    #[test]
    fn validate_append_ignores_injected_chunks() {
        let before = "# T\n- [ ] T1.1: a\n";
        let injected = "- [ ] no valid id\n";
        let after = format!("{before}{injected}- [ ] T9.1: planned\n");
        // Without the accounting the malformed injected line rejects the run.
        assert!(validate_append_ids(before, &after).is_err());
        assert_eq!(
            validate_append_ignoring(before, &after, &[injected.into()]),
            Ok(vec!["T9.1".to_string()])
        );
        // An unmatched record (the line is part of `before`) is ignored.
        assert_eq!(
            validate_append_ignoring(before, before, &[injected.into()]),
            Ok(vec![])
        );
    }

    /// An injected chunk whose text equals an appended line filters one
    /// occurrence only -- an accepted, documented imprecision of the
    /// chunk-filter accounting.
    #[test]
    fn validate_append_filters_one_occurrence_per_injected_chunk() {
        let before = "# T\n- [ ] T1.1: a\n";
        let chunk = "- [ ] T2.1: same text\n";
        let after = format!("{before}{chunk}{chunk}");
        assert_eq!(
            validate_append_ignoring(before, &after, &[chunk.into()]),
            Ok(vec!["T2.1".to_string()])
        );
    }

    /// T77.1: a rejected run's restore keeps the lines injected during the
    /// session, re-appended verbatim at the end, and skips a chunk that is
    /// already part of `before` (injected before the run started).
    #[test]
    fn restore_keeping_re_appends_injected_chunks_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let before = "# T\n- [ ] T1.1: a\n";
        let pre_session = "- [ ] T8.1: earlier\n";
        let mid_session = "- [ ] T9.1: injected during the session  \n";
        std::fs::write(&path, format!("{before}{pre_session}{mid_session}garbage")).unwrap();
        restore_keeping(&path, before, &[pre_session.into(), mid_session.into()]).unwrap();
        // The pre-session chunk is in `before`, so it is not duplicated.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{before}{pre_session}{mid_session}")
        );
    }

    #[test]
    fn restore_keeping_without_injected_chunks_matches_restore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let before = "# T\n- [ ] T1.1: a\n";
        std::fs::write(&path, format!("{before}garbage")).unwrap();
        restore_keeping(&path, before, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn remove_task_removes_a_pending_line_and_keeps_every_other_byte() {
        // Mixed CRLF and LF, trailing spaces, an indented H line, a heading
        // and no trailing newline on the final line.
        let text = "# T\r\n- [ ] T1.1: gone\n  - [ ] H1.2: stays\nprose  \r\n\
                    - [ ] T1.3: [--.B-] last";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, text).unwrap();
        assert_eq!(remove_task(&path, "T1.1").unwrap(), RemoveOutcome::Removed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\r\n  - [ ] H1.2: stays\nprose  \r\n- [ ] T1.3: [--.B-] last"
        );
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn remove_task_refuses_a_completed_line_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let text = "# T\n- [x] T1.1: done\n- [ ] T1.2: b\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            remove_task(&path, "T1.1").unwrap(),
            RemoveOutcome::Completed
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // A checked duplicate refuses the removal even alongside an
        // unchecked line with the same id.
        let dup = "# T\n- [ ] T1.1: pending\n- [x] T1.1: done\n";
        std::fs::write(&path, dup).unwrap();
        assert_eq!(
            remove_task(&path, "T1.1").unwrap(),
            RemoveOutcome::Completed
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), dup);
    }

    #[test]
    fn remove_task_reports_unknown_ids_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(remove_task(&path, "T9.9").unwrap(), RemoveOutcome::NotFound);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        // A missing file reads as NotFound and is not created.
        let missing = dir.path().join("none.md");
        assert_eq!(
            remove_task(&missing, "T1.1").unwrap(),
            RemoveOutcome::NotFound
        );
        assert!(!missing.exists());
    }

    #[test]
    fn remove_task_removes_duplicate_unchecked_lines_together() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let text = "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n- [ ] T1.1: dup\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(remove_task(&path, "T1.1").unwrap(), RemoveOutcome::Removed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [ ] T1.2: b\n"
        );
    }

    #[test]
    fn remove_task_matches_by_well_formed_id_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        // An unchecked H line is removable by its id; a malformed line
        // (`- [ ] no id`) is never touched.
        let text = "# T\n  - [ ] H2.1: human\n- [ ] no id\n- [ ] T2.1: t\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(remove_task(&path, "H2.1").unwrap(), RemoveOutcome::Removed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [ ] no id\n- [ ] T2.1: t\n"
        );
        // A progress token in the line does not block the id match.
        std::fs::write(&path, "- [ ] T3.1: [RP.BA] mid\n").unwrap();
        assert_eq!(remove_task(&path, "T3.1").unwrap(), RemoveOutcome::Removed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }
}
