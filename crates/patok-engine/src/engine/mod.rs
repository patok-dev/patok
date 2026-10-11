//! Engine state and the single-task build flow.
//!
//! The responsibilities are split one file each: configuration and construction
//! (`config`), the shared state vocabulary and the status queries (`state`), the
//! stop state (`stop`), the event bus (`bus`), the run-state application, queue
//! reconcile and stage tiles (`pipeline`), the task-file watcher (`watch`), the
//! central settings and reload (`settings`), the command entry points
//! (`commands`), the session loop (`session`), the open-group group
//! machinery (`group`), and the group tracking and commit recording
//! (`commit`). The research, plan, build, review and commit steps of the
//! build flow live in `crate::step` (reached through the session context
//! `crate::context`), which also holds the shared agent-run skeleton they
//! run on.

mod bus;
mod commands;
mod commit;
mod config;
pub(crate) mod group;
mod pipeline;
mod session;
mod settings;
mod state;
mod stop;
mod watch;

pub use self::commands::QueueCreation;
pub use self::config::EngineConfig;
pub(crate) use self::state::{Inner, RunState, Tiles, resolve_provider};

use std::sync::Arc;

use patok_core::event::{EngineEvent, Snapshot};
use tokio::sync::broadcast;

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
