//! External task management on the shared task file: the `patok tasks`
//! entry point for task changes made outside the shell. The append goes
//! straight to the file instead of through the engine's RPC, which cannot
//! report the generated id back, and is safe while an engine runs: the
//! engine's task-file lock is in-process, but it tolerates external appends
//! by mtime-polling the file and reconciling, so new unchecked lines merge
//! into the queue while the running and completed tasks are protected.
//! The remove instead routes through the engine's `RemoveTask` command when
//! an engine runs: in-progress exists only in the engine's memory, and the
//! removal then runs under the engine's task-file lock with an immediate
//! reconcile. With no engine running it falls back to a direct rewrite of
//! the file -- in-progress cannot exist then, so the file is the whole
//! truth. Known races, deliberately left alone: with no cross-process lock,
//! an add concurrent with a TUI inject through a running engine can pick the
//! same number; and an engine can start between the failed connect and the
//! fallback write, whose mtime poll then reconciles the already-removed
//! line. The list reads the file directly too, so it works while an
//! engine runs: the file is the complete record -- pending, in-progress
//! and completed tasks all have a line -- and the engine's rewrites are
//! atomic, so a read concurrent with a rewrite sees either the old or
//! the new content, never a torn one.

use std::path::Path;

use anyhow::{Context, bail};
use patok_proto::command_request;
use patok_proto::{CommandRequest, RemoveTask};

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

/// Removes the task with `id` from the project's task file (T98.1),
/// printing a confirmation on success. When an engine runs for the
/// project, the removal goes through its `RemoveTask` command -- the engine
/// knows the current (in-progress) task, refuses it and completed tasks,
/// and rewrites the file under its own lock. With no engine, the file
/// itself is rewritten directly; in-progress cannot exist without an
/// engine. If the engine dies mid-call the transport error propagates
/// rather than falling back, so a removal is never applied twice.
pub async fn remove(project: &Path, id: &str) -> anyhow::Result<()> {
    let id = id.trim();
    if id.is_empty() {
        bail!("the task id is empty");
    }
    if let Some(mut client) = crate::daemon::try_connect(project).await {
        let response = client
            .submit_command(CommandRequest {
                action: Some(command_request::Action::RemoveTask(RemoveTask {
                    id: id.to_string(),
                })),
            })
            .await
            .context("the engine rejected the remove request")?
            .into_inner();
        if !response.accepted {
            bail!("{}", response.error);
        }
        println!("Removed task {id}.");
        return Ok(());
    }
    // No engine is running: in-progress cannot exist, so the file itself is
    // the whole truth.
    let path = project.join(patok_engine::TASK_FILE);
    match patok_engine::taskfile::remove_task(&path, id)
        .with_context(|| format!("cannot update {}", path.display()))?
    {
        patok_engine::taskfile::RemoveOutcome::Removed => println!("Removed task {id}."),
        patok_engine::taskfile::RemoveOutcome::Completed => {
            bail!("task {id} is completed and cannot be removed");
        }
        patok_engine::taskfile::RemoveOutcome::NotFound => {
            bail!("no task with id {id} in {}", patok_engine::TASK_FILE);
        }
    }
    Ok(())
}

/// Lists every task in the project's task file, one per line with the id
/// first, reading the file directly instead of contacting the engine: the
/// file is the complete record -- pending, in-progress, and completed tasks
/// all have a line -- and the engine's rewrites are atomic, so a read while
/// an engine runs sees either the old or the new content. A missing file
/// reads as empty and lists nothing, exactly like `add` reads it.
pub fn list(project: &Path) -> anyhow::Result<()> {
    let path = project.join(patok_engine::TASK_FILE);
    let file = match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        other => other.with_context(|| format!("cannot read {}", path.display()))?,
    };
    for task in patok_core::task::parse(&file) {
        println!("{} {}", task.id, task.description);
    }
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

    // No engine runs for a tempdir project, so these exercise the
    // stopped-engine fallback path directly against the file.
    #[tokio::test]
    async fn remove_deletes_a_pending_task_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("TASKS.md"),
            "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n",
        )
        .unwrap();
        remove(dir.path(), "T1.1").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            "# T\n- [ ] T1.2: b\n"
        );
    }

    #[tokio::test]
    async fn remove_refuses_a_completed_task() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [x] T1.1: done\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = remove(dir.path(), "T1.1").await.unwrap_err();
        assert!(error.to_string().contains("completed"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            text
        );
    }

    #[tokio::test]
    async fn remove_reports_an_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = remove(dir.path(), "T9.9").await.unwrap_err();
        assert!(error.to_string().contains("no task with id"), "{error:#}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            text
        );
    }

    #[tokio::test]
    async fn remove_rejects_an_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("TASKS.md"), "# T\n- [ ] T1.1: a\n").unwrap();
        assert!(remove(dir.path(), "  ").await.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            "# T\n- [ ] T1.1: a\n"
        );
    }

    #[tokio::test]
    async fn remove_reports_a_missing_file_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let error = remove(dir.path(), "T1.1").await.unwrap_err();
        assert!(error.to_string().contains("no task with id"), "{error:#}");
        assert!(!dir.path().join("TASKS.md").exists());
    }

    #[test]
    fn list_succeeds_on_a_missing_store() {
        let dir = tempfile::tempdir().unwrap();
        list(dir.path()).unwrap();
        assert!(!dir.path().join("TASKS.md").exists());
    }

    #[test]
    fn list_fails_when_the_store_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("TASKS.md")).unwrap();
        let error = list(dir.path()).unwrap_err();
        assert!(error.to_string().contains("cannot read"), "{error:#}");
    }
}
