//! The engine process: it serves the gRPC protocol to the shell over a
//! Unix socket and runs the build loop around seven abstractions. A
//! Task (`patok_core::task`) is one line of work; the TaskList
//! (`task_list/`) owns every task-file concern of the queue. A Session
//! (`session.rs`) is one engine-owning unit of work driven through its
//! SessionContext (`context.rs`): the build session loops the pending
//! tasks, the append kinds run their pipeline once. A Pipeline
//! (`pipeline/`) orders its PipelineSteps (`step/`), and each step
//! pairs an Agent (`agents/`) with a Provider (`patok-providers`) or
//! performs one engine action.

mod agents;
mod artifact;
mod context;
mod engine;
mod git;
mod markdown;
mod pipeline;
mod schedule;
mod server;
mod session;
mod step;
mod task_list;

pub use engine::{Engine, EngineConfig};
/// The settings-change wire mapping, generated with the shell direction in
/// `patok-wire` from one key-to-case list (finding 10).
pub use patok_wire::from_proto;
pub use pipeline::queue_creation::QueueCreation;
pub use server::{prepare_runtime_dir, serve};
/// The pure task-file functions, re-exported under their historical name
/// (section 5 of docs/private/engine-restructure.md).
pub use task_list::file as taskfile;

/// Name of the task file in the project root.
pub use patok_core::scenario::TASK_FILE;
