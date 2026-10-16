//! The append-tasks planner step.

use std::time::Instant;

use patok_core::event::NoticeLevel;
use patok_providers::ExitKind;

use crate::agents::{Agent, AppendPlannerAgent};
use crate::context::SessionContext;
use crate::step::agent_run::{
    AgentPrompt, AgentResultRecord, StageExit, StageStart, StageWords, run_stage, write_result_json,
};

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
        let label = agent.label().to_string();
        let provider_slug = self.ctx.provider().slug().to_string();
        let model = self.ctx.model().map(|s| s.to_string());
        let (exit, session_result, seq) = run_stage(
            self.ctx,
            self.ctx.provider(),
            self.ctx.model(),
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
        // Write result JSON for add_task step
        let exit_kind = session_result
            .as_ref()
            .map(|r| r.exit)
            .unwrap_or(ExitKind::Failed);
        let success = session_result.as_ref().map(|r| r.success).unwrap_or(false);
        let failure = session_result.as_ref().and_then(|r| r.failure.clone());
        let record = AgentResultRecord {
            agent: label.clone(),
            provider: provider_slug,
            model,
            exit_kind,
            success,
            failure,
            outcome: serde_json::Value::Null,
        };
        write_result_json(self.ctx.current_task_dir().as_path(), seq, &label, record);
        exit
    }
}
