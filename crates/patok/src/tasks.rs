//! External task management on the shared task file: the `patok tasks`
//! entry point for task changes made outside the shell. The append goes
//! straight to the file instead of through the engine's RPC, which cannot
//! report the generated id back, and is safe while an engine runs: the
//! engine's task-file lock is in-process, but it tolerates external appends
//! by mtime-polling the file and reconciling, so new unchecked lines merge
//! into the queue while the running and completed tasks are protected.
//! Known race, deliberately left alone: with no cross-process lock, an add
//! concurrent with a TUI inject through a running engine can pick the same
//! number; the window is one read-to-append and the duplicate id is
//! surfaced by the engine's session validation.

use std::path::Path;

use anyhow::{Context, bail};

/// Appends `text` as a new unchecked task line to the project's task file,
/// with a fresh patok-generated `T<N>.1:` id, then prints a confirmation
/// with that id. One task is one line. A missing file reads as empty and
/// the append creates it, exactly like the engine's inject path.
pub fn add(project: &Path, text: &str) -> anyhow::Result<()> {
    if text.trim().is_empty() {
        bail!("the task text is empty");
    }
    if text.trim().contains('\n') {
        bail!("the task text must be a single line");
    }
    let path = project.join(patok_engine::TASK_FILE);
    // The file is read only to number the new task; a missing file reads as
    // empty and the append creates it.
    let file = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        other => other.with_context(|| format!("cannot read {}", path.display()))?,
    };
    let line = patok_engine::taskfile::normalize_injected(text, &file);
    // The generated id is parsed back out of the line before appending, so
    // the confirmation reports exactly what landed in the file.
    let parsed = patok_core::task::parse(&line);
    let Some(task) = parsed.first() else {
        bail!("the added task line could not be parsed: {line}");
    };
    patok_engine::taskfile::append_line(&path, &line)
        .with_context(|| format!("cannot append to {}", path.display()))?;
    println!("Added task {}: {}.", task.id, task.description);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_appends_a_task_with_a_fresh_id() {
        let dir = tempfile::tempdir().unwrap();
        add(dir.path(), "sample task text").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            "- [ ] T1.1: sample task text\n"
        );
    }

    #[test]
    fn add_numbers_past_the_highest_t_task() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("TASKS.md"), "# T\n- [ ] T3.1: existing\n").unwrap();
        add(dir.path(), "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            "# T\n- [ ] T3.1: existing\n- [ ] T4.1: new\n"
        );
    }

    #[test]
    fn add_rejects_empty_and_multi_line_text() {
        let dir = tempfile::tempdir().unwrap();
        assert!(add(dir.path(), "  ").is_err());
        assert!(add(dir.path(), "a\nb").is_err());
        assert!(!dir.path().join("TASKS.md").exists());
    }

    #[test]
    fn add_fails_when_the_file_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        // The project directory does not exist: the append cannot create the
        // task file inside it.
        assert!(add(&dir.path().join("missing"), "x").is_err());
        assert!(!dir.path().join("missing").join("TASKS.md").exists());
    }
}
