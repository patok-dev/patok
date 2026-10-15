//! The append-tasks pipeline.

use std::time::Instant;

use patok_core::event::AgentRole;

use crate::context::SessionContext;
use crate::step::add_task::AddTaskStep;
use crate::step::agent_run::StageExit;
use crate::step::validate_append::ValidateAppendStep;
use crate::task_list::AppendMode;

/// The append-tasks pipeline (section 10 of
/// docs/private/engine-restructure.md): it orders the append-tasks
/// planner step and the post-session append validation of one
/// add-tasks run. Created per run over the session's context.
pub(crate) struct AddTaskPipeline<'a> {
    ctx: &'a SessionContext,
}

impl<'a> AddTaskPipeline<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One planner session for `request`, then the shared post-run check
    /// when the session completed. `before` is the task file as it was
    /// before the run; `start` is the instant the run was announced.
    /// The appended IDs are recorded as this session's UI-added task IDs.
    /// The validation result is deliberately ignored: the check reports
    /// its outcome itself, and a rejected run restores the previous file.
    pub(crate) async fn run(&self, request: &str, before: &str, start: Instant) {
        let exit = AddTaskStep::new(self.ctx).run(request, start).await;
        if let StageExit::Completed(_) = exit {
            let _ = ValidateAppendStep::new(self.ctx)
                .run(before, AppendMode::Tasks, AgentRole::Planner, true)
                .await;
        }
    }
}
