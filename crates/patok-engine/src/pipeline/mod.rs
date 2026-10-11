//! The pipelines (section 10 of docs/private/engine-restructure.md): the
//! per-task build pipeline and the pure batch-review group helpers its
//! group wiring calls into.

pub(crate) mod build;
pub(crate) mod group;
