//! External task management on the shared task file: the `patok tasks`
//! entry point for task changes made outside the shell. The append goes
//! straight to the file instead of through the engine's RPC, which cannot
//! report the generated id back, and is safe while an engine runs: the
//! engine's task-file lock is in-process, but it tolerates external appends
//! by mtime-polling the file and reconciling, so new unchecked lines merge
//! into the queue while the running and completed tasks are protected.
//! The remove, edit and move instead route through the engine's `RemoveTask` /
//! `EditTask` / `MoveTask` commands when an engine runs: in-progress exists only in the
//! engine's memory, and the change then runs under the engine's task-file
//! lock with an immediate reconcile. With no engine running they fall back to
//! a direct rewrite of the file -- in-progress cannot exist then, so the file
//! is the whole truth. The move then prints the renumbered ids read back from
//! the file. Known races, deliberately left alone: with no
//! cross-process lock, an add concurrent with a TUI inject through a running
//! engine can pick the same number; and an engine can start between the
//! failed connect and the fallback write, whose mtime poll then reconciles
//! the already-removed (or edited) line. The list reads the file directly too, so it works while an
//! engine runs: the file is the complete record -- pending, in-progress
//! and completed tasks all have a line -- and the engine's rewrites are
//! atomic, so a read concurrent with a rewrite sees either the old or
//! the new content, never a torn one.

use std::path::Path;

use anyhow::{Context, bail};
use patok_proto::command_request;
use patok_proto::{CommandRequest, EditTask, MoveTask, RemoveTask};

/// The task file read whole: a missing file reads as empty (the append then
/// creates it, the list then lists nothing), while any other read failure
/// fails with the path in the message.
fn read_task_file(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other.with_context(|| format!("cannot read {}", path.display())),
    }
}

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
    let file = read_task_file(&path)?;
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
    // the whole truth. With no running task (`None`), renumbering can't
    // touch one.
    let path = project.join(patok_engine::TASK_FILE);
    match patok_engine::taskfile::remove_task(&path, id, None)
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

/// Replaces the description of the pending task `id` in the project's task
/// file (T3.1), keeping its id, and prints the resulting id. When an engine
/// runs for the project, the edit goes through its `EditTask` command -- the
/// engine knows the current (in-progress) task and refuses it and completed
/// tasks. With no engine, the file itself is rewritten directly. If the
/// engine dies mid-call the transport error propagates rather than falling
/// back, so an edit is never applied twice.
pub async fn edit(project: &Path, id: &str, text: &str) -> anyhow::Result<()> {
    let id = id.trim();
    if id.is_empty() {
        bail!("the task id is empty");
    }
    let description = text.trim();
    if description.is_empty() {
        bail!("the task description is empty");
    }
    if description.contains('\n') {
        bail!("the task description must be a single line");
    }
    if let Some(mut client) = crate::daemon::try_connect(project).await {
        let response = client
            .submit_command(CommandRequest {
                action: Some(command_request::Action::EditTask(EditTask {
                    id: id.to_string(),
                    description: description.to_string(),
                })),
            })
            .await
            .context("the engine rejected the edit request")?
            .into_inner();
        if !response.accepted {
            bail!("{}", response.error);
        }
        println!("Edited task {id}: {description}.");
        return Ok(());
    }
    // No engine is running: in-progress cannot exist, so there is no running
    // task (`None`) to refuse.
    let path = project.join(patok_engine::TASK_FILE);
    match patok_engine::taskfile::rewrite_pending(&path, |file| {
        patok_engine::taskfile::edit_task(file, id, description, None).map(|updated| (updated, ()))
    })
    .with_context(|| format!("cannot update {}", path.display()))?
    {
        Ok(()) => println!("Edited task {id}: {description}."),
        Err(patok_engine::taskfile::TaskEditOutcome::Completed) => {
            bail!("task {id} is completed and cannot be edited");
        }
        Err(patok_engine::taskfile::TaskEditOutcome::NotFound) => {
            bail!("no task with id {id} in {}", patok_engine::TASK_FILE);
        }
        // Unreachable with no running task; kept with the engine's wording.
        Err(patok_engine::taskfile::TaskEditOutcome::Running) => {
            bail!("task {id} is in progress; wait for it to finish before editing it");
        }
        Err(
            patok_engine::taskfile::TaskEditOutcome::Invalid
            | patok_engine::taskfile::TaskEditOutcome::Ok,
        ) => bail!(
            "the task description must be a single line and must not start with a progress token"
        ),
    }
    Ok(())
}

/// Prints every unchecked task of the file at `path` as `{id} {description}`,
/// one per line: after a move these are exactly the ids that renumbering
/// produced. Completed tasks never move or renumber, so they are left out.
fn print_unchecked(path: &Path) -> anyhow::Result<()> {
    let file = read_task_file(path)?;
    for task in patok_core::task::parse(&file) {
        if !task.done {
            println!("{} {}", task.id, task.description);
        }
    }
    Ok(())
}

/// Moves the pending task `id` one place up or down in the project's task
/// file (T3.2), then prints the ids that result after renumbering, read back
/// from the file. When an engine runs for the project, the move goes through
/// its `MoveTask` command -- the engine knows the current (in-progress) task
/// and refuses moves that touch it or a completed task. With no engine, the
/// file itself is rewritten directly. If the engine dies mid-call the
/// transport error propagates rather than falling back, so a move is never
/// applied twice. The engine's rewrite is atomic and finishes before it
/// answers, so the read-back sees the renumbered file.
pub async fn move_task(
    project: &Path,
    id: &str,
    direction: patok_engine::taskfile::MoveDirection,
) -> anyhow::Result<()> {
    use patok_engine::taskfile::MoveDirection;
    let id = id.trim();
    if id.is_empty() {
        bail!("the task id is empty");
    }
    let (word, side) = match direction {
        MoveDirection::Up => ("up", "above"),
        MoveDirection::Down => ("down", "below"),
    };
    let path = project.join(patok_engine::TASK_FILE);
    if let Some(mut client) = crate::daemon::try_connect(project).await {
        let wire = match direction {
            MoveDirection::Up => patok_proto::move_task::Direction::Up,
            MoveDirection::Down => patok_proto::move_task::Direction::Down,
        };
        let response = client
            .submit_command(CommandRequest {
                action: Some(command_request::Action::MoveTask(MoveTask {
                    id: id.to_string(),
                    direction: wire as i32,
                })),
            })
            .await
            .context("the engine rejected the move request")?
            .into_inner();
        if !response.accepted {
            bail!("{}", response.error);
        }
    } else {
        // No engine is running: in-progress cannot exist, so there is no
        // running task (`None`) to refuse.
        match patok_engine::taskfile::rewrite_pending(&path, |file| {
            patok_engine::taskfile::move_task(file, id, direction, None).map(|m| (m.text, ()))
        })
        .with_context(|| format!("cannot update {}", path.display()))?
        {
            Ok(()) => {}
            Err(patok_engine::taskfile::TaskEditOutcome::Completed) => {
                bail!("task {id} cannot be moved: it or the task it would pass is completed");
            }
            Err(patok_engine::taskfile::TaskEditOutcome::NotFound) => {
                bail!("no task with id {id} in {}", patok_engine::TASK_FILE);
            }
            // Unreachable with no running task; kept with the engine's wording.
            Err(patok_engine::taskfile::TaskEditOutcome::Running) => {
                bail!(
                    "task {id} cannot be moved: the move would touch the in-progress task or its group"
                );
            }
            Err(
                patok_engine::taskfile::TaskEditOutcome::Invalid
                | patok_engine::taskfile::TaskEditOutcome::Ok,
            ) => bail!("task {id} has no task {side} it to swap with"),
        }
    }
    println!("Moved task {id} {word}.");
    print_unchecked(&path)
}

/// Lists every task in the project's task file, one per line with the id
/// first, reading the file directly instead of contacting the engine: the
/// file is the complete record -- pending, in-progress, and completed tasks
/// all have a line -- and the engine's rewrites are atomic, so a read while
/// an engine runs sees either the old or the new content. A missing file
/// reads as empty and lists nothing, exactly like `add` reads it.
pub fn list(project: &Path) -> anyhow::Result<()> {
    let path = project.join(patok_engine::TASK_FILE);
    let file = read_task_file(&path)?;
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
    async fn remove_deletes_a_pending_task_and_renumbers_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("TASKS.md"),
            "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n",
        )
        .unwrap();
        remove(dir.path(), "T1.1").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("TASKS.md")).unwrap(),
            "# T\n- [ ] T1.1: b\n"
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

    fn read_tasks(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("TASKS.md")).unwrap()
    }

    #[tokio::test]
    async fn edit_replaces_a_pending_description_keeping_its_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("TASKS.md"),
            "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n",
        )
        .unwrap();
        edit(dir.path(), "T1.2", "  new text  ").await.unwrap();
        assert_eq!(
            read_tasks(dir.path()),
            "# T\n- [ ] T1.1: a\n- [ ] T1.2: new text\n"
        );
    }

    #[tokio::test]
    async fn edit_keeps_the_progress_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("TASKS.md"), "- [ ] T1.1: [RP.B-] old\n").unwrap();
        edit(dir.path(), "T1.1", "new").await.unwrap();
        assert_eq!(read_tasks(dir.path()), "- [ ] T1.1: [RP.B-] new\n");
    }

    #[tokio::test]
    async fn edit_refuses_a_completed_task() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [x] T1.1: done\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = edit(dir.path(), "T1.1", "x").await.unwrap_err();
        assert!(error.to_string().contains("completed"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn edit_reports_an_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = edit(dir.path(), "T9.9", "x").await.unwrap_err();
        assert!(error.to_string().contains("no task with id"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn edit_rejects_an_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        assert!(edit(dir.path(), "  ", "x").await.is_err());
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn edit_rejects_empty_multi_line_and_token_text() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        assert!(edit(dir.path(), "T1.1", "  ").await.is_err());
        assert!(edit(dir.path(), "T1.1", "a\nb").await.is_err());
        let error = edit(dir.path(), "T1.1", "[RP.B-] x").await.unwrap_err();
        assert!(error.to_string().contains("progress token"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn edit_reports_a_missing_file_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let error = edit(dir.path(), "T1.1", "x").await.unwrap_err();
        assert!(error.to_string().contains("no task with id"), "{error:#}");
        assert!(!dir.path().join("TASKS.md").exists());
    }

    const TWO_GROUPS: &str =
        "## P1\n- [ ] T1.1: a\n- [ ] T1.2: b\n## P2\n- [ ] T2.1: c\n- [ ] T2.2: d\n";

    #[tokio::test]
    async fn move_task_swaps_with_its_neighbour() {
        use patok_engine::taskfile::MoveDirection::{Down, Up};
        let swapped = "# T\n- [ ] T1.1: b\n- [ ] T1.2: a\n";
        for (id, direction) in [("T1.2", Up), ("T1.1", Down)] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("TASKS.md"),
                "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n",
            )
            .unwrap();
            move_task(dir.path(), id, direction).await.unwrap();
            assert_eq!(read_tasks(dir.path()), swapped);
        }
    }

    #[tokio::test]
    async fn move_task_across_groups_renumbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("TASKS.md"), TWO_GROUPS).unwrap();
        move_task(
            dir.path(),
            "T2.1",
            patok_engine::taskfile::MoveDirection::Up,
        )
        .await
        .unwrap();
        assert_eq!(
            read_tasks(dir.path()),
            "## P1\n- [ ] T1.1: a\n- [ ] T1.2: c\n- [ ] T1.3: b\n## P2\n- [ ] T2.1: d\n"
        );
    }

    #[tokio::test]
    async fn move_task_refuses_without_a_neighbour() {
        use patok_engine::taskfile::MoveDirection::{Down, Up};
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = move_task(dir.path(), "T1.1", Up).await.unwrap_err();
        assert!(error.to_string().contains("no task above"), "{error:#}");
        let error = move_task(dir.path(), "T1.2", Down).await.unwrap_err();
        assert!(error.to_string().contains("below"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn move_task_refuses_passing_a_completed_task() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [x] T1.1: done\n- [ ] T1.2: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = move_task(
            dir.path(),
            "T1.2",
            patok_engine::taskfile::MoveDirection::Up,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("completed"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn move_task_reports_an_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        let error = move_task(
            dir.path(),
            "T9.9",
            patok_engine::taskfile::MoveDirection::Up,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no task with id"), "{error:#}");
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn move_task_rejects_an_empty_id() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# T\n- [ ] T1.1: a\n- [ ] T1.2: b\n";
        std::fs::write(dir.path().join("TASKS.md"), text).unwrap();
        assert!(
            move_task(dir.path(), "  ", patok_engine::taskfile::MoveDirection::Up)
                .await
                .is_err()
        );
        assert_eq!(read_tasks(dir.path()), text);
    }

    #[tokio::test]
    async fn move_task_reports_a_missing_file_as_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let error = move_task(
            dir.path(),
            "T1.1",
            patok_engine::taskfile::MoveDirection::Up,
        )
        .await
        .unwrap_err();
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
