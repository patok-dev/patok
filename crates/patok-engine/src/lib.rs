//! The engine process: owns the task queue, git, history log and provider supervision, and
//! serves the gRPC protocol to the shell over a Unix socket.

mod artifact;
mod engine;
mod git;
mod markdown;
mod plan;
mod prompt;
mod review;
mod schedule;
mod server;
pub mod taskfile;

pub use engine::{Engine, EngineConfig, QueueCreation};
/// The settings-change wire mapping, generated with the shell direction in
/// `patok-wire` from one key-to-case list (finding 10).
pub use patok_wire::from_proto;
pub use server::{prepare_runtime_dir, serve};

/// Name of the task file in the project root.
pub use patok_core::scenario::TASK_FILE;
