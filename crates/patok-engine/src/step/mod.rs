//! The pipeline steps (section 9 of docs/private/engine-restructure.md):
//! each step encapsulates one agent -- or one engine action for the
//! non-agent steps -- its retries, its post-processing and its own
//! artifact only; what an earlier step produced is handed in by the
//! pipeline. `agent_run` holds the shared announce-run-classify
//! skeleton the agent steps run on.

pub(crate) mod add_task;
pub(crate) mod agent_run;
pub(crate) mod build;
pub(crate) mod commit;
pub(crate) mod discovery;
pub(crate) mod plan;
pub(crate) mod queue_research;
pub(crate) mod research;
pub(crate) mod review;
pub(crate) mod validate_append;
