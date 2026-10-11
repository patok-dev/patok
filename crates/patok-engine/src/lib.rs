//! The engine process: owns the task queue, git, history log and provider supervision, and
//! serves the gRPC protocol to the shell over a Unix socket.

mod agents;
mod artifact;
mod context;
mod engine;
mod git;
mod markdown;
mod schedule;
mod server;
mod step;
mod task_list;

pub use engine::{Engine, EngineConfig, QueueCreation};
/// The settings-change wire mapping, generated with the shell direction in
/// `patok-wire` from one key-to-case list (finding 10).
pub use patok_wire::from_proto;
pub use server::{prepare_runtime_dir, serve};
/// The pure task-file functions, re-exported under their historical name
/// (section 5 of docs/private/engine-restructure.md).
pub use task_list::file as taskfile;

/// Name of the task file in the project root.
pub use patok_core::scenario::TASK_FILE;
