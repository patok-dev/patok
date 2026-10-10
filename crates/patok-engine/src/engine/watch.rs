use std::time::Duration;

use patok_core::atomic_write;
use patok_core::event::EngineEvent;

use super::Engine;
use crate::taskfile;

/// How often the task file's modification time is checked.
const TASK_FILE_POLL: Duration = Duration::from_secs(2);

impl Engine {
    /// The startup cleanup (T72.1): prunes old completed task lines from the
    /// task file, adopts the cleaned file as the queue and broadcasts the
    /// change, and removes the task file's orphaned temp siblings -- files a
    /// hard kill of an atomic rewrite leaves behind and `git add -A` would
    /// otherwise commit (item 2 of docs/private/structure-review-followup.md).
    /// Both run under the task-file lock, before any write can create a fresh
    /// temp. Called once from `serve`, before the task-file watcher starts,
    /// never during the session.
    pub async fn cleanup_completed_tasks(&self) {
        let path = self.inner.config.task_file();
        let guard = self.inner.task_file_lock.clone().lock_owned().await;
        match atomic_write::sweep_temp_files(&path) {
            Ok(0) => {}
            Ok(count) => {
                tracing::info!(
                    "removed {count} orphaned temp file(s) of {}",
                    path.display()
                );
            }
            Err(error) => tracing::warn!("startup temp sweep of {}: {error}", path.display()),
        }
        let cleaned = match taskfile::cleanup_completed(&path) {
            Ok(cleaned) => cleaned,
            Err(error) => {
                tracing::warn!("startup cleanup of {}: {error}", path.display());
                return;
            }
        };
        drop(guard);
        if !cleaned {
            return;
        }
        // The queue starts over from the cleaned file. A plain reconcile would
        // keep the pruned completed tasks in memory (reconcile protects done
        // tasks that vanish from the file), undoing the cleanup in the queue.
        let mut state = self.state();
        if let Ok(file) = taskfile::load(&path) {
            let merged = taskfile::reconcile(&[], file, None);
            if merged != state.tasks {
                state.tasks = merged.clone();
                self.emit_locked(&mut state, EngineEvent::TasksChanged { tasks: merged });
            }
        }
    }

    /// Polls the task file's modification time and reconciles the queue when it changes.
    /// Runs until the engine shuts down.
    pub fn spawn_task_file_watch(&self) {
        let engine = self.clone();
        tokio::spawn(async move {
            let path = engine.inner.config.task_file();
            let mtime = || std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let mut last = mtime();
            engine.reconcile();
            let mut tick = tokio::time::interval(TASK_FILE_POLL);
            loop {
                tokio::select! {
                    () = engine.inner.shutdown.cancelled() => return,
                    _ = tick.tick() => {}
                }
                let now = mtime();
                if now != last {
                    last = now;
                    engine.reconcile();
                }
            }
        });
    }
}
