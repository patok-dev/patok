use std::time::Duration;

use patok_core::event::EngineEvent;

use super::Engine;

/// How often the task file's modification time is checked.
const TASK_FILE_POLL: Duration = Duration::from_secs(2);

impl Engine {
    /// The startup cleanup (T72.1): prunes old completed task lines from the
    /// task file and removes its orphaned temp siblings (both in
    /// `TaskList::cleanup_completed`, under the task-file lock), then adopts
    /// the cleaned file as the queue and broadcasts the change under one
    /// state lock. Called once from `serve`, before the task-file watcher
    /// starts, never during the session.
    pub async fn cleanup_completed_tasks(&self) {
        match self.inner.task_list.cleanup_completed().await {
            Ok(true) => {
                let mut state = self.state();
                if let Some(merged) = self.inner.task_list.reset() {
                    self.emit_locked(&mut state, EngineEvent::TasksChanged { tasks: merged });
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    "startup cleanup of {}: {error}",
                    self.inner.task_list.path().display()
                );
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
