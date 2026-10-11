//! Engine state and the single-task build flow.
//!
//! The responsibilities are split one file each: configuration and construction
//! (`config`), the shared state vocabulary and the status queries (`state`), the
//! stop state (`stop`), the event bus (`bus`), the run-state application, queue
//! reconcile and stage tiles (`pipeline`), the task-file watcher (`watch`), the
//! central settings and reload (`settings`), the command entry points
//! (`commands`), the session loop and the shared stage skeleton (`session`),
//! the research, plan and review stages (`research_stage`, `plan_stage`,
//! `review_stage`), driving their agents from the pure per-agent definitions in
//! `crate::agents`, the artifact files (`artifacts`), the open-group group
//! machinery (`group`), and the group tracking and commit
//! finalization (`commit`).

mod artifacts;
mod bus;
mod commands;
mod commit;
mod config;
mod group;
mod pipeline;
mod plan_stage;
mod research_stage;
mod review_stage;
mod session;
mod settings;
mod state;
mod stop;
mod watch;

pub use self::commands::QueueCreation;
pub use self::config::EngineConfig;
pub(crate) use self::state::RunState;

use std::sync::Arc;

use patok_core::event::{EngineEvent, Snapshot};
use tokio::sync::broadcast;

use self::state::Inner;

/// Cheaply cloneable handle on the engine's state.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

/// Held by the one controlling shell; dropping it frees the attach slot.
pub struct Attachment {
    pub snapshot: Snapshot,
    pub events: broadcast::Receiver<EngineEvent>,
    pub(crate) engine: Engine,
}

#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("another shell is already attached to the engine for {0}")]
    AlreadyAttached(String),
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.engine.state().attached = false;
    }
}
