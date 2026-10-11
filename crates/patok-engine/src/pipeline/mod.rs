//! The pipelines (section 10 of docs/private/engine-restructure.md): the
//! per-task build pipeline, the append pipelines (add-tasks, queue
//! creation, discovery) and the pure batch-review group helpers its
//! group wiring calls into.

pub(crate) mod add_task;
pub(crate) mod build;
pub(crate) mod discovery;
pub(crate) mod group;
pub(crate) mod queue_creation;
