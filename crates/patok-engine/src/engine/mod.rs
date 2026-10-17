//! The engine facade: a cheaply cloneable [`Engine`] handle on the shared
//! `Inner`, plus the attachment counter it manages for every attached shell
//! ([`Attachment`], [`AttachError`]). The directory holds the facade files
//! only, one responsibility each: construction and configuration
//! (`config`), the shared state vocabulary and the status queries
//! (`state`), the stop state (`stop`), the event bus (`bus`), the
//! command entry points (`commands`), the central settings and reload
//! (`settings`), the task-file watcher (`watch`) and the start guards,
//! run-state application, queue reconcile, rail refresh and tile updates
//! (`tiles`). In the crate's model the engine owns the task list
//! (`crate::task_list`), a session (`crate::session`) owns one unit of
//! work, a pipeline (`crate::pipeline`) orders pipeline steps
//! (`crate::step`), and each step pairs an agent (`crate::agents`) with
//! a provider.

mod bus;
mod commands;
mod config;
mod settings;
mod state;
mod stop;
mod tiles;
mod watch;

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

/// Held by each attached shell; dropping it decrements the attachment counter.
/// Commands like stop, shutdown, and restart remain global across all attached shells.
pub struct Attachment {
    pub snapshot: Snapshot,
    pub events: broadcast::Receiver<EngineEvent>,
    pub(crate) engine: Engine,
}

#[derive(Debug, thiserror::Error)]
pub enum AttachError {}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.engine.state().attach_count -= 1;
    }
}
