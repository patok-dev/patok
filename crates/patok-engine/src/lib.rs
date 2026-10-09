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
mod settings;
pub mod taskfile;

pub use engine::{Engine, EngineConfig, QueueCreation};
pub use server::{prepare_runtime_dir, serve};
pub use settings::from_proto;

/// Name of the task file in the project root.
pub use patok_core::scenario::TASK_FILE;
