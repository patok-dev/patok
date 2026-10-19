//! Per-daemon status: probe an engine socket with an observer attach and follow its run state
//! and task progress until the engine goes away. Used by the fleet monitor and `patok ps`.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use patok_core::event::{EngineEvent, Snapshot};
use patok_core::paths::{self, DaemonMeta};
use patok_core::task::Task;
use patok_proto::{AttachRequest, EngineUpdate, engine_update};
use tonic::Streaming;

pub use patok_proto::RunState;

/// Done and total task counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskProgress {
    pub done: usize,
    pub total: usize,
}

/// What one daemon is doing, as of the latest update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    pub socket_path: PathBuf,
    pub project_dir: PathBuf,
    pub run_state: RunState,
    pub tasks: TaskProgress,
    /// From `meta.json`; `None` when the file is missing or unreadable.
    pub pid: Option<u32>,
    /// Unix seconds, from `meta.json`.
    pub started_at: Option<u64>,
    pub engine_version: String,
}

/// One result of following a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusUpdate {
    Changed(DaemonStatus),
    Gone,
}

/// A live observer attach that keeps a [`DaemonStatus`] current.
pub struct StatusStream {
    status: DaemonStatus,
    updates: Streaming<EngineUpdate>,
    gone: bool,
}

/// Connects to `socket`, attaches as an observer and decodes the first snapshot. An error means
/// no live engine answered there.
pub async fn probe(socket: &Path) -> anyhow::Result<StatusStream> {
    let mut client = patok_proto::client::connect(socket)
        .await
        .with_context(|| format!("no engine listening on {}", socket.display()))?;
    let mut updates = client
        .attach(AttachRequest {
            shell_version: env!("CARGO_PKG_VERSION").to_string(),
            observer: true,
        })
        .await
        .map_err(|status| anyhow!("{}", status.message()))?
        .into_inner();

    let first = updates
        .message()
        .await?
        .context("the engine closed the connection during attach")?;
    let Some(engine_update::Payload::Info(info)) = first.payload else {
        anyhow::bail!("the engine did not start with its info message");
    };
    let snapshot = info.snapshot.context("attach carried no snapshot")?;
    let state = Snapshot::from_state(&snapshot.state).context("unreadable engine snapshot")?;
    let meta = socket
        .parent()
        .map(paths::meta_path)
        .and_then(|path| DaemonMeta::read(&path).ok());

    Ok(StatusStream {
        status: DaemonStatus {
            socket_path: socket.to_path_buf(),
            project_dir: state.project_dir.clone().into(),
            run_state: run_state_of(snapshot.run_state),
            tasks: progress(&state.tasks),
            pid: meta.as_ref().map(|m| m.pid),
            started_at: meta.as_ref().map(|m| m.started_at),
            engine_version: info.engine_version,
        },
        updates,
        gone: false,
    })
}

impl StatusStream {
    /// The status as of the last update returned by [`next`](Self::next).
    pub fn status(&self) -> &DaemonStatus {
        &self.status
    }

    /// Waits for the next change in run state or task progress, or for the engine to go away.
    /// Once `Gone` is returned, every later call returns `Gone`.
    pub async fn next(&mut self) -> StatusUpdate {
        loop {
            if self.gone {
                return StatusUpdate::Gone;
            }
            match self.updates.message().await {
                Ok(Some(update)) => match update.payload {
                    Some(engine_update::Payload::RunState(update)) => {
                        let run_state = run_state_of(update.state);
                        if run_state != self.status.run_state {
                            self.status.run_state = run_state;
                            return StatusUpdate::Changed(self.status.clone());
                        }
                    }
                    // The engine carries task-list changes as this JSON event; the typed
                    // `TaskListChanged` is never produced and holds no task list.
                    Some(engine_update::Payload::Event(event)) if event.kind == "tasks_changed" => {
                        if let Ok(EngineEvent::TasksChanged { tasks }) =
                            EngineEvent::from_payload(&event.payload)
                        {
                            let tasks = progress(&tasks);
                            if tasks != self.status.tasks {
                                self.status.tasks = tasks;
                                return StatusUpdate::Changed(self.status.clone());
                            }
                        }
                    }
                    Some(engine_update::Payload::ShutdownNotice(_)) => self.gone = true,
                    _ => {}
                },
                Ok(None) | Err(_) => self.gone = true,
            }
        }
    }
}

fn run_state_of(raw: i32) -> RunState {
    RunState::try_from(raw).unwrap_or(RunState::Idle)
}

fn progress(tasks: &[Task]) -> TaskProgress {
    TaskProgress {
        done: tasks.iter().filter(|task| task.done).count(),
        total: tasks.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts_done_and_total() {
        let tasks = patok_core::task::parse("- [x] T1.1: a\n- [ ] T1.2: b\n- [x] T1.3: c\n");
        assert_eq!(progress(&tasks), TaskProgress { done: 2, total: 3 });
        assert_eq!(progress(&[]), TaskProgress { done: 0, total: 0 });
    }
}
