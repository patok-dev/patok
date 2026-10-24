//! The task list (section 5 of docs/private/engine-restructure.md): one
//! shared owner of every task-file concern of the task queue -- the
//! task-file path, the in-memory queue, the async file lock, the
//! injected chunks and the UI-added task IDs. [`TaskList`] emits no
//! events and writes no notices -- it returns what happened and the
//! caller emits -- so the lock order stays engine state first and task
//! list second. The pure text and file functions of `file` stay public
//! as `patok_engine::taskfile`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use patok_core::event::AgentRole;
use patok_core::task::{self, Task};
use patok_core::utils::atomic_write;

pub mod file;

use file::{MoveDirection, RemoveOutcome, TaskEditOutcome};

/// Which validator an append-style run uses: the planner and queue-creation
/// runs check `T`-prefixed appends, the discovery round `D`-prefixed ones.
#[derive(Clone, Copy)]
pub(crate) enum AppendMode {
    Tasks,
    Discovery,
}

/// A rejected append-style run: the rejection reason, and whether the
/// previous file was restored (keeping the injected lines). The caller
/// reports it; [`TaskList`] emits nothing.
#[derive(Debug)]
pub(crate) struct AppendRejection {
    pub(crate) reason: String,
    pub(crate) restored: bool,
}

/// Why an engine-side pending-task edit (T2.3) wrote nothing. The caller words it.
#[derive(Debug)]
pub(crate) enum EditFailure {
    /// The guard refused before the file was read (a planner or discovery run is active);
    /// the reason is user-facing.
    Refused(String),
    /// The text operation refused.
    Outcome(TaskEditOutcome),
    /// Reading or writing the task file failed.
    Io(io::Error),
}

/// The queue, the injected chunks and the UI-added IDs under one guard, held
/// only for short non-awaiting critical sections.
struct Shared {
    tasks: Vec<Task>,
    injected: Vec<String>,
    ui_added_ids: Vec<String>,
}

pub(crate) struct TaskList {
    path: PathBuf,
    /// The async file lock (T77.1): serialises short atomic file operations,
    /// never held across an agent session.
    lock: tokio::sync::Mutex<()>,
    shared: Mutex<Shared>,
}

impl TaskList {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: tokio::sync::Mutex::new(()),
            shared: Mutex::new(Shared {
                tasks: Vec::new(),
                injected: Vec::new(),
                ui_added_ids: Vec::new(),
            }),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().expect("task list lock")
    }

    /// The in-memory queue.
    pub(crate) fn tasks(&self) -> Vec<Task> {
        self.shared().tasks.clone()
    }

    /// Re-reads the task file and merges it into the queue; `Some(merged)`
    /// when the queue changed, `None` when it did not or the file could not
    /// be read. Synchronous, so a caller may run it while holding the engine
    /// state lock (state first, task list second).
    pub(crate) fn reconcile(&self, running: Option<&str>) -> Option<Vec<Task>> {
        let Ok(file) = file::load(&self.path) else {
            return None;
        };
        let mut shared = self.shared();
        let merged = file::reconcile(&shared.tasks, file, running);
        if merged == shared.tasks {
            return None;
        }
        shared.tasks = merged.clone();
        Some(merged)
    }

    /// The first pending task as the file has it now, read without the file
    /// lock (the documented race against inject and remove stands).
    pub(crate) fn next_pending(&self) -> io::Result<Option<Task>> {
        let tasks = file::load(&self.path)?;
        Ok(task::next_pending(&tasks).cloned())
    }

    /// IDs of the pending tasks in the file (the review deferral's input); a
    /// file that cannot be read has none.
    pub(crate) fn pending_ids(&self) -> Vec<String> {
        file::load(&self.path)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.is_pending())
            .map(|t| t.id)
            .collect()
    }

    /// The file text before an append-style run, created with a header when
    /// missing, read under a try-lock of the file lock so a concurrent update
    /// rejects the run's start instead of waiting. Errors are user-facing
    /// rejection reasons.
    pub(crate) fn snapshot_before(&self) -> Result<String, String> {
        let Ok(guard) = self.lock.try_lock() else {
            return Err("the task file is being updated; try again".into());
        };
        let before = file::read_or_create(&self.path)
            .map_err(|e| format!("cannot read {}: {e}", self.path.display()))?;
        drop(guard);
        Ok(before)
    }

    /// The gap round's read of the file before its session: like
    /// [`TaskList::snapshot_before`] but waiting for the lock instead of
    /// rejecting, and `None` on a read error.
    pub(crate) async fn read_before(&self) -> Option<String> {
        let _guard = self.lock.lock().await;
        file::read_or_create(&self.path).ok()
    }

    /// The whole post-session check of an append-style run, under the file
    /// lock: the chunks injected since the run started are drained inside the
    /// same lock hold that reads and validates the file, so an inject cannot
    /// slip between the drain and the check (T77.1); a rejection restores the
    /// previous file, keeping the injected lines. Returns the appended IDs;
    /// recording them as UI-added is the caller's decision.
    pub(crate) async fn check_appended(
        &self,
        before: &str,
        mode: AppendMode,
        role: AgentRole,
    ) -> Result<Vec<String>, AppendRejection> {
        let _guard = self.lock.lock().await;
        let injected: Vec<String> = std::mem::take(&mut self.shared().injected);
        let verdict = match std::fs::read_to_string(&self.path) {
            Ok(after) => match mode {
                AppendMode::Tasks => file::validate_append_ignoring(before, &after, &injected),
                AppendMode::Discovery => {
                    file::validate_discovery_append_ignoring(before, &after, &injected)
                }
            },
            Err(e) => Err(format!(
                "cannot read {} after the {role} session: {e}",
                self.path.display()
            )),
        };
        match verdict {
            Ok(ids) => Ok(ids),
            Err(reason) => {
                let restored = match file::restore_keeping(&self.path, before, &injected) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::error!("cannot restore {}: {e}", self.path.display());
                        false
                    }
                };
                Err(AppendRejection { reason, restored })
            }
        }
    }

    /// Injects `text` as the next task line of the file (T77.1): the file is
    /// read, the text normalized and one atomic append written under the
    /// file lock, and the exact chunk recorded in the injected-chunk ledger.
    /// Errors are user-facing rejection reasons.
    pub(crate) async fn inject(&self, text: &str) -> Result<(), String> {
        let _guard = self.lock.lock().await;
        // The file is read under the same lock the append follows, so two
        // rapid injects get distinct numbers; a missing file reads as empty
        // and the append creates it.
        let file = match std::fs::read_to_string(&self.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            other => other.map_err(|e| format!("cannot read {}: {e}", self.path.display()))?,
        };
        let line = file::normalize_injected(text, &file);
        let chunk = file::append_line(&self.path, &line)
            .map_err(|e| format!("cannot append to {}: {e}", self.path.display()))?;
        self.shared().injected.push(chunk);
        Ok(())
    }

    /// Removes the task line with the given id from the file (T98.1), under
    /// the file lock. `running` runs inside the same lock hold, before
    /// the file is touched, which shrinks the race window against the
    /// session's next-task pick; it returns the running task id. A match
    /// with `id` refuses the removal and `None` is returned; otherwise the
    /// id is passed down so renumbering leaves the running task alone. It may take the
    /// engine state lock: the only path that takes the file lock under the
    /// state lock, [`TaskList::snapshot_before`], only tries it.
    pub(crate) async fn remove(
        &self,
        id: &str,
        running: impl FnOnce() -> Option<String>,
    ) -> io::Result<Option<RemoveOutcome>> {
        let _guard = self.lock.lock().await;
        let running = running();
        if running.as_deref() == Some(id) {
            return Ok(None);
        }
        file::remove_task(&self.path, id, running.as_deref()).map(Some)
    }

    /// One locked read-modify-write of the task file for a pending-task edit: `guard` runs inside
    /// the file lock (it may take the engine state lock, see [`TaskList::remove`]) and returns the
    /// running id or a refusal; `op` rewrites the text; the result is written atomically. A
    /// missing file is `NotFound` and is not created; nothing is written on any refusal.
    async fn rewrite<T>(
        &self,
        guard: impl FnOnce() -> Result<Option<String>, String>,
        op: impl FnOnce(&str, Option<&str>) -> Result<(String, T), TaskEditOutcome>,
    ) -> Result<T, EditFailure> {
        let _lock = self.lock.lock().await;
        let running = guard().map_err(EditFailure::Refused)?;
        let text = match std::fs::read_to_string(&self.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(EditFailure::Outcome(TaskEditOutcome::NotFound));
            }
            other => other.map_err(EditFailure::Io)?,
        };
        let (updated, value) = op(&text, running.as_deref()).map_err(EditFailure::Outcome)?;
        atomic_write::write_atomic(&self.path, &updated).map_err(EditFailure::Io)?;
        Ok(value)
    }

    /// Replaces the description of the pending task `id` (T2.3), under the file lock. `guard`
    /// runs inside the lock before the file is read and returns the running task id or a refusal.
    pub(crate) async fn edit_task(
        &self,
        id: &str,
        description: &str,
        guard: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<(), EditFailure> {
        self.rewrite(guard, |text, running| {
            file::edit_task(text, id, description, running).map(|t| (t, ()))
        })
        .await
    }

    /// Moves the pending task `id` one place up or down (T2.3), under the file lock, and returns
    /// its new id. `guard` is as for [`TaskList::edit_task`].
    pub(crate) async fn move_task(
        &self,
        id: &str,
        direction: MoveDirection,
        guard: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<String, EditFailure> {
        self.rewrite(guard, |text, running| {
            file::move_task(text, id, direction, running).map(|m| (m.text, m.id))
        })
        .await
    }

    /// Indents the pending task `id` into the previous group (T2.4), under the file lock, and
    /// returns its new id. `guard` is as for [`TaskList::edit_task`].
    pub(crate) async fn indent_task(
        &self,
        id: &str,
        guard: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<String, EditFailure> {
        self.rewrite(guard, |text, running| {
            file::indent_task(text, id, running).map(|m| (m.text, m.id))
        })
        .await
    }

    /// Outdents the pending task `id` and the tasks after it in its group into a new group
    /// (T2.4), under the file lock, and returns its new id. `guard` is as for
    /// [`TaskList::edit_task`].
    pub(crate) async fn outdent_task(
        &self,
        id: &str,
        guard: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<String, EditFailure> {
        self.rewrite(guard, |text, running| {
            file::outdent_task(text, id, running).map(|m| (m.text, m.id))
        })
        .await
    }

    /// Writes the task's five-position progress token into its line, ticking
    /// it when `done` (T77.1), under the file lock. Returns whether the
    /// task's line was found; when none is, nothing is written.
    pub(crate) async fn write_progress(
        &self,
        task: &Task,
        token: &str,
        done: bool,
    ) -> io::Result<bool> {
        let _guard = self.lock.lock().await;
        file::write_progress(&self.path, task, token, done)
    }

    /// The startup cleanup (T72.1) plus the task file's orphaned temp-file
    /// sweep, both under the file lock. Returns whether the file was
    /// rewritten; the caller then starts the queue over with
    /// [`TaskList::reset`] under the engine state lock.
    pub(crate) async fn cleanup_completed(&self) -> io::Result<bool> {
        let _guard = self.lock.lock().await;
        match atomic_write::sweep_temp_files(&self.path) {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(
                    "removed {count} orphaned temp file(s) of {}",
                    self.path.display()
                );
            }
            Err(error) => tracing::warn!("startup temp sweep of {}: {error}", self.path.display()),
        }
        file::cleanup_completed(&self.path)
    }

    /// Starts the queue over from the file -- after the startup cleanup a
    /// plain reconcile would keep the pruned completed tasks in memory
    /// (reconcile protects done tasks that vanish from the file), undoing
    /// the cleanup in the queue. `Some(merged)` when the queue changed,
    /// `None` when it did not or the file could not be read. Synchronous,
    /// so a caller may run it while holding the engine state lock.
    pub(crate) fn reset(&self) -> Option<Vec<Task>> {
        let file = file::load(&self.path).ok()?;
        let merged = file::reconcile(&[], file, None);
        let mut shared = self.shared();
        if merged == shared.tasks {
            return None;
        }
        shared.tasks = merged.clone();
        Some(merged)
    }

    /// Adds `ids` to the run's UI-added task IDs (the append-tasks flow).
    pub(crate) fn record_ui_added(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        self.shared().ui_added_ids.extend(ids.iter().cloned());
    }

    /// The IDs appended through the append-tasks flow this run, in the
    /// order they were added.
    pub(crate) fn ui_added_ids(&self) -> Vec<String> {
        self.shared().ui_added_ids.clone()
    }

    /// Whether `id` was appended through the append-tasks flow this run.
    pub(crate) fn is_ui_added(&self, id: &str) -> bool {
        self.shared().ui_added_ids.iter().any(|i| i == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_reports_only_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [ ] T1.1: a\n").unwrap();
        let list = TaskList::new(path.clone());
        // The first reconcile adopts the file's tasks; a second over the
        // unchanged file reports nothing, as does a file that vanished.
        assert_eq!(list.reconcile(None).unwrap().len(), 1);
        assert_eq!(list.reconcile(None), None);
        std::fs::write(&path, "- [ ] T1.1: a\n- [ ] T1.2: b\n").unwrap();
        assert_eq!(list.reconcile(None).unwrap().len(), 2);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(list.reconcile(None), None);
        // The queue keeps its last merged state through the failed read.
        assert_eq!(list.tasks().len(), 2);
    }

    #[tokio::test]
    async fn check_appended_accepts_keeping_injected_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# Tasks\n- [ ] T1.1: a\n").unwrap();
        let list = TaskList::new(path.clone());
        let before = list.snapshot_before().unwrap();
        // The inject lands a T2.1 line and records its chunk.
        list.inject("do a thing").await.unwrap();
        // The session appends a valid T3.1 line to the file, after the
        // injected one.
        std::fs::write(
            &path,
            format!("{}- [ ] T3.1: b\n", std::fs::read_to_string(&path).unwrap()),
        )
        .unwrap();
        let ids = list
            .check_appended(&before, AppendMode::Tasks, AgentRole::Planner)
            .await
            .unwrap();
        assert_eq!(ids, vec!["T3.1".to_string()]);
        // The injected line neither rejected the run nor was removed.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("- [ ] T2.1: do a thing"));
        assert!(text.contains("- [ ] T3.1: b"));
    }

    #[tokio::test]
    async fn check_appended_rejects_and_restores_keeping_injected_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# Tasks\n- [ ] T1.1: a\n").unwrap();
        let list = TaskList::new(path.clone());
        let before = list.snapshot_before().unwrap();
        list.inject("do a thing").await.unwrap();
        // The session's append is invalid: an already-checked task, landed
        // after the injected line.
        std::fs::write(
            &path,
            format!("{}- [x] T3.1: b\n", std::fs::read_to_string(&path).unwrap()),
        )
        .unwrap();
        let rejection = list
            .check_appended(&before, AppendMode::Tasks, AgentRole::Planner)
            .await
            .unwrap_err();
        assert!(rejection.restored);
        assert_eq!(
            rejection.reason,
            "the planner added an already checked task: T3.1"
        );
        // The file is `before` with the injected chunk re-appended.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{before}- [ ] T2.1: do a thing\n")
        );
    }

    /// T2.1: a planner append that skips a group number is rejected and the
    /// file is restored, keeping a line injected during the session.
    #[tokio::test]
    async fn check_appended_rejects_a_skipped_group_and_restores() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# Tasks\n- [ ] T1.1: a\n").unwrap();
        let list = TaskList::new(path.clone());
        let before = list.snapshot_before().unwrap();
        std::fs::write(&path, format!("{before}- [ ] T3.1: b\n")).unwrap();
        let rejection = list
            .check_appended(&before, AppendMode::Tasks, AgentRole::Planner)
            .await
            .unwrap_err();
        assert!(rejection.restored);
        assert_eq!(
            rejection.reason,
            "the planner skipped a task group number: T3.1 (join group 1 or open group 2)"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // With an injected T2.1, T5.1 still skips; the restore keeps the
        // injected line.
        list.inject("do a thing").await.unwrap();
        std::fs::write(
            &path,
            format!("{}- [ ] T5.1: b\n", std::fs::read_to_string(&path).unwrap()),
        )
        .unwrap();
        let rejection = list
            .check_appended(&before, AppendMode::Tasks, AgentRole::Planner)
            .await
            .unwrap_err();
        assert!(rejection.restored);
        assert!(rejection.reason.contains("skipped"), "{}", rejection.reason);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{before}- [ ] T2.1: do a thing\n")
        );
    }

    #[test]
    fn ui_added_ids_are_recorded_and_queried() {
        let dir = tempfile::tempdir().unwrap();
        let list = TaskList::new(dir.path().join("TASKS.md"));
        assert!(list.ui_added_ids().is_empty());
        list.record_ui_added(&["T5.1".to_string()]);
        assert_eq!(list.ui_added_ids(), vec!["T5.1".to_string()]);
        assert!(list.is_ui_added("T5.1"));
        assert!(!list.is_ui_added("T1.1"));
    }

    #[tokio::test]
    async fn write_progress_inserts_the_token_and_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# T\n- [ ] T1.1: do it\n").unwrap();
        let list = TaskList::new(path.clone());
        let task = file::load(&path).unwrap().remove(0);
        assert!(list.write_progress(&task, "[RP.BA]", true).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [x] T1.1: [RP.BA] do it\n"
        );
        // The only line is already checked: nothing is written.
        assert!(!list.write_progress(&task, "[RP.BA]", true).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# T\n- [x] T1.1: [RP.BA] do it\n"
        );
    }

    #[tokio::test]
    async fn remove_refuses_the_task_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [ ] T1.1: a\n- [ ] T1.2: b\n").unwrap();
        let list = TaskList::new(path.clone());
        // The in-progress check refuses: the file is left alone.
        assert!(
            list.remove("T1.1", || Some("T1.1".to_string()))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: a\n- [ ] T1.2: b\n"
        );
        assert!(matches!(
            list.remove("T1.1", || None).await.unwrap(),
            Some(RemoveOutcome::Removed)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "- [ ] T1.1: b\n");
        // A different running task is passed down and never renumbered.
        std::fs::write(&path, "- [ ] T1.1: a\n- [ ] T1.2: b\n- [ ] T1.3: c\n").unwrap();
        assert!(matches!(
            list.remove("T1.3", || Some("T1.2".to_string()))
                .await
                .unwrap(),
            Some(RemoveOutcome::Removed)
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: a\n- [ ] T1.2: b\n"
        );
    }

    #[tokio::test]
    async fn edit_task_rewrites_the_file_and_refuses_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let original = "- [ ] T1.1: a\n- [ ] T1.2: b\n";
        std::fs::write(&path, original).unwrap();
        let list = TaskList::new(path.clone());
        let refused = list.edit_task("T1.2", "x", || Err("busy".into())).await;
        assert!(matches!(refused, Err(EditFailure::Refused(ref r)) if r == "busy"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let running = list
            .edit_task("T1.2", "x", || Ok(Some("T1.2".to_string())))
            .await;
        assert!(matches!(
            running,
            Err(EditFailure::Outcome(TaskEditOutcome::Running))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        list.edit_task("T1.2", "x", || Ok(None)).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: a\n- [ ] T1.2: x\n"
        );
        let missing = TaskList::new(dir.path().join("NOPE.md"));
        assert!(matches!(
            missing.edit_task("T1.1", "x", || Ok(None)).await,
            Err(EditFailure::Outcome(TaskEditOutcome::NotFound))
        ));
        assert!(!dir.path().join("NOPE.md").exists());
    }

    #[tokio::test]
    async fn move_task_returns_the_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [ ] T1.1: a\n- [ ] T1.2: b\n").unwrap();
        let list = TaskList::new(path.clone());
        let id = list
            .move_task("T1.2", MoveDirection::Up, || Ok(None))
            .await
            .unwrap();
        assert_eq!(id, "T1.1");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: b\n- [ ] T1.2: a\n"
        );
    }

    #[tokio::test]
    async fn indent_and_outdent_task_return_the_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        let original = "- [ ] T1.1: a\n- [ ] T2.1: b\n";
        std::fs::write(&path, original).unwrap();
        let list = TaskList::new(path.clone());
        let id = list.indent_task("T2.1", || Ok(None)).await.unwrap();
        assert_eq!(id, "T1.2");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "- [ ] T1.1: a\n- [ ] T1.2: b\n"
        );
        let id = list.outdent_task("T1.2", || Ok(None)).await.unwrap();
        assert_eq!(id, "T2.1");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let refused = list
            .indent_task("T2.1", || Err("busy".into()))
            .await
            .unwrap_err();
        assert!(matches!(refused, EditFailure::Refused(r) if r == "busy"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn reset_drops_done_tasks_reconcile_would_keep() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "- [x] T1.1: a\n- [ ] T1.2: b\n").unwrap();
        let list = TaskList::new(path.clone());
        list.reconcile(None).unwrap();
        // The done task vanishes from the file: reconcile protects it, the
        // reset starts the queue over from the file.
        std::fs::write(&path, "- [ ] T1.2: b\n").unwrap();
        list.reconcile(None);
        assert_eq!(list.tasks().len(), 2);
        assert_eq!(list.reset().unwrap().len(), 1);
        assert_eq!(list.reset(), None);
    }
}
