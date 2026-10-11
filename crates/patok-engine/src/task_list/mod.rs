//! The task-file abstraction (section 5 of docs/private/engine-restructure.md):
//! one shared owner of the task-file path, the in-memory queue, the async file
//! lock, the injected chunks and the UI-added task IDs. [`TaskList`] emits no
//! events and writes no notices -- it returns what happened and the caller
//! emits -- so the lock order stays engine state first and task list second.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use patok_core::event::AgentRole;
use patok_core::task::{self, Task};
use patok_core::utils::atomic_write;

pub mod file;

use file::RemoveOutcome;

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
    /// the file lock.
    pub(crate) async fn remove(&self, id: &str) -> io::Result<RemoveOutcome> {
        let _guard = self.lock.lock().await;
        file::remove_task(&self.path, id)
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
    /// sweep, both under the file lock. When the file was rewritten, the
    /// queue starts over from the cleaned file -- a plain reconcile would
    /// keep the pruned completed tasks in memory (reconcile protects done
    /// tasks that vanish from the file), undoing the cleanup in the queue --
    /// and `Some(merged)` is returned when the queue changed, so the caller
    /// can broadcast it.
    pub(crate) async fn cleanup_completed(&self) -> io::Result<Option<Vec<Task>>> {
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
        if !file::cleanup_completed(&self.path)? {
            return Ok(None);
        }
        // The queue starts over from the cleaned file.
        let file = file::load(&self.path)?;
        let merged = file::reconcile(&[], file, None);
        let mut shared = self.shared();
        if merged == shared.tasks {
            return Ok(None);
        }
        shared.tasks = merged.clone();
        Ok(Some(merged))
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
}
