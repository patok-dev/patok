//! The discovery pipeline.

use patok_core::event::AgentRole;

use crate::context::SessionContext;
use crate::step::agent_run::StageExit;
use crate::step::discovery::DiscoveryStep;
use crate::step::validate_append::ValidateAppendStep;
use crate::task_list::AppendMode;

/// The discovery pipeline (section 10 of
/// docs/private/engine-restructure.md): it orders the discovery step
/// and the shared post-run append validation of one discovery round.
/// Created per round over the session's context.
pub(crate) struct DiscoveryPipeline<'a> {
    ctx: &'a SessionContext,
}

impl<'a> DiscoveryPipeline<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One discovery session, then the shared post-run check when the
    /// session completed. `before` is the task file as it was before the
    /// round. Returns how many tasks the round appended once it
    /// completes, or `None` when it did not run to completion (error,
    /// cancelled or failed); a round whose session completed but was
    /// rejected still counts as completed, with zero tasks appended.
    /// The appended IDs are not recorded as UI-added.
    pub(crate) async fn run(&self, before: &str) -> Option<usize> {
        match DiscoveryStep::new(self.ctx).run().await {
            StageExit::Completed(_) => Some(
                ValidateAppendStep::new(self.ctx)
                    .run(before, AppendMode::Discovery, AgentRole::Discovery, false)
                    .await
                    .unwrap_or_default()
                    .len(),
            ),
            _ => None,
        }
    }
}
