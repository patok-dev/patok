//! The pipelines (section 10 of docs/private/engine-restructure.md):
//! each pipeline defines its steps, their order, the settings-level
//! enable/disable and every tile update, handing each step's output to
//! the steps that need it. `build` runs one task through the build
//! steps and owns the batch-review group wiring, whose pure helpers
//! live in `group`; the append-style runs are `add_task`,
//! `queue_creation` and `discovery`.

pub(crate) mod add_task;
pub(crate) mod build;
pub(crate) mod discovery;
pub(crate) mod group;
pub(crate) mod queue_creation;
