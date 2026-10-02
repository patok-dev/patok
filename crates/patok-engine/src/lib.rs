//! The engine process: owns the task queue, git, history log and provider supervision, and
//! serves the gRPC protocol to the shell over a Unix socket.

mod engine;
mod git;
mod plan;
mod prompt;
mod review;
mod schedule;
mod server;
mod settings;
pub mod taskfile;

pub use engine::{Engine, EngineConfig, QueueCreation};
pub use server::{prepare_runtime_dir, serve};

/// Name of the task file in the project root.
pub const TASK_FILE: &str = "TASKS.md";
