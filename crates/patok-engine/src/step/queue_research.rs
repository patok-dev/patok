//! The queue-creation research step and its report save.

use std::time::Instant;

use patok_core::event::NoticeLevel;

use crate::agents::{Agent, ResearchQueueAgent};
use crate::context::SessionContext;
use crate::pipeline::queue_creation::QueueCreation;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};
use crate::step::research::write_research_report;

/// The queue-creation research step (T180.4, section 9 of
/// docs/private/engine-restructure.md): one research session for the
/// Bootstrap or Scan kind (T69.1). The shell is told the research agent is
/// the active agent for the whole run (T11.1).
pub(crate) struct QueueResearchStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> QueueResearchStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// One research session of `kind`. `start` is the instant the run was
    /// announced in `start_queue_creation` under the state guard (T24.1).
    /// Returns the report text only when the session completed; `None`
    /// otherwise.
    pub(crate) async fn run(&self, kind: QueueCreation, start: Instant) -> Option<String> {
        let agent = match kind {
            QueueCreation::Bootstrap => ResearchQueueAgent::bootstrap(),
            QueueCreation::Scan => ResearchQueueAgent::scan(),
        };
        let exit = run_stage(
            self.ctx,
            self.ctx.provider(),
            self.ctx.model(),
            StageStart::Announced(start),
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: true,
                could_not_start: "research agent could not start".into(),
                default_failure: "research failed".into(),
                cancelled_notice: Some("Research cancelled.".into()),
                failure_level: NoticeLevel::Error,
                failure_prefix: "research: ".into(),
                failure_suffix: String::new(),
            },
        )
        .await;
        match exit {
            // The research agent's validation is trivial acceptance, so this
            // stays dead-safe.
            StageExit::Completed(events) => Some(agent.validate(&events).unwrap_or_default()),
            _ => None,
        }
    }

    /// Saves the run's research report under the kind's stem. Deliberately
    /// separate from `run`: the caller saves only after the append validation
    /// passed, so a rejected run re-runs rather than being covered by the
    /// reuse window. An empty report warns instead of writing.
    pub(crate) fn save(&self, kind: QueueCreation, report: &str) {
        if report.trim().is_empty() {
            self.ctx.notice(
                NoticeLevel::Warning,
                "the research run returned no report; nothing was saved",
            );
        } else {
            write_research_report(self.ctx, kind.stem(), report);
        }
    }
}
