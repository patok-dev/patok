//! The append-tasks planner step.

use std::time::Instant;

use patok_core::event::NoticeLevel;

use crate::agents::{Agent, AppendPlannerAgent};
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};

/// The append-tasks step (T180.4, section 9 of docs/private/engine-restructure.md):
/// one planner session for the append-tasks request. The shell is told the
/// planner is the active agent for the whole run (T11.1). The exit is
/// returned: the caller runs the post-session append validation
/// ([`ValidateAppendStep`](crate::step::validate_append::ValidateAppendStep))
/// only when the session completed.
pub(crate) struct AddTaskStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> AddTaskStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One planner session for `request`. `start` is the instant the run was
    /// announced in `start_add_tasks` under the state guard (T24.1). Returns
    /// the agent exit.
    pub(crate) async fn run(&self, request: &str, start: Instant) -> StageExit {
        let agent = AppendPlannerAgent::new(request);
        let (provider, model) = self.ctx.for_role(agent.role());
        let (exit, record) = run_stage(
            self.ctx,
            &provider,
            model.as_deref(),
            StageStart::Announced(start),
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: false,
                could_not_start: "planner could not start".into(),
                default_failure: "planner failed".into(),
                cancelled_notice: Some("Planner cancelled.".into()),
                failure_level: NoticeLevel::Error,
                failure_prefix: "planner: ".into(),
                failure_suffix: String::new(),
            },
        )
        .await;
        record.write(&serde_json::Value::Null);
        exit
    }
}
