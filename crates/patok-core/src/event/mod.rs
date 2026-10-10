//! The normalised agent event vocabulary and the events the engine pushes to the
//! shell. The wire format carries these as JSON in the opaque `bytes` fields of the proto.

mod agent;
mod engine;

pub use agent::{AgentEvent, AgentRole, ErrorKind, Usage, classify_error, preview};
pub use engine::{EngineEvent, NoticeLevel, Outcome, Phase, SessionOutcome, Snapshot, TaskOutcome};
