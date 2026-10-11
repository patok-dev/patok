use std::time::Instant;

use patok_core::event::AgentRole;

use crate::context::SessionContext;
use crate::engine::QueueCreation;
use crate::step::queue_research::QueueResearchStep;
use crate::step::validate_append::ValidateAppendStep;
use crate::task_list::AppendMode;

/// The queue-creation pipeline (section 10 of
/// docs/private/engine-restructure.md): it orders the queue-creation
/// research step, the shared post-run append validation and the
/// report save of one queue-creation run. Created per run over the
/// session's context.
pub(crate) struct QueueCreationPipeline<'a> {
    ctx: &'a SessionContext,
}

impl<'a> QueueCreationPipeline<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One research session of `kind`, then the shared post-run check.
    /// `before` is the task file as it was before the run; `start` is
    /// the instant the run was announced. The appended IDs are recorded
    /// as this session's UI-added task IDs.
    pub(crate) async fn run(&self, kind: QueueCreation, before: &str, start: Instant) {
        let step = QueueResearchStep::new(self.ctx);
        if let Some(report) = step.run(kind, start).await {
            // Only a pass saves the research report, so a rejected run
            // re-runs rather than being covered by the reuse window.
            if ValidateAppendStep::new(self.ctx)
                .run(before, AppendMode::Tasks, AgentRole::Research, true)
                .await
                .is_ok()
            {
                step.save(kind, &report);
            }
        }
    }
}
