//! The discovery step.

use patok_core::event::NoticeLevel;

use crate::agents::{Agent, DiscoveryAgent};
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};

/// The discovery step (T180.4, section 9 of docs/private/engine-restructure.md):
/// one discovery session that appends follow-up tasks. The shell is told the
/// discovery agent is the active agent for the whole round (T11.1). The exit
/// is returned: the caller runs the post-run append validation
/// ([`ValidateAppendStep`](crate::step::validate_append::ValidateAppendStep))
/// only when the session completed.
pub(crate) struct DiscoveryStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> DiscoveryStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One discovery session: the round announces itself, so there is no
    /// earlier announcement to pass. Returns the agent exit.
    pub(crate) async fn run(&self) -> StageExit {
        let agent = DiscoveryAgent::new();
        run_stage(
            self.ctx,
            self.ctx.provider(),
            self.ctx.model(),
            StageStart::Announce,
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: false,
                could_not_start: "discovery could not start".into(),
                default_failure: "discovery failed".into(),
                cancelled_notice: Some("Discovery cancelled.".into()),
                failure_level: NoticeLevel::Error,
                failure_prefix: "discovery: ".into(),
                failure_suffix: String::new(),
            },
        )
        .await
    }
}
