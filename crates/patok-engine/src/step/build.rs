//! The per-task build step: the builder session and the claims file.

use patok_core::event::NoticeLevel;
use patok_core::task::Task;

use crate::agents::{self, Agent, BuilderAgent};
use crate::artifact;
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};
use serde::Serialize;

/// The outcome of one task's build step.
#[derive(Debug, Serialize)]
pub(crate) enum BuildOutcome {
    /// The builder session completed: the trimmed claims text when the
    /// final message carried a build claims section.
    Completed { claims: Option<String> },
    /// The builder session failed.
    Failed,
    /// The builder session was cancelled.
    Cancelled,
}

/// The per-task build step: one builder session whose final message may
/// carry the build claims section the review step reviews. The builder's
/// failure is an error (the pipeline still finalizes with a WIP commit);
/// a cancelled one ends the task. `plan` and `research` are the prior
/// plan and research artifacts, when they exist.
pub(crate) struct BuildStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> BuildStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    pub(crate) async fn run(
        &self,
        task: &Task,
        plan: Option<&str>,
        research: Option<&str>,
    ) -> BuildOutcome {
        // The builder's final message carries the build claims section the review
        // step reviews, so the events are captured like the plan step's.
        let agent = BuilderAgent::new(task, plan, research);
        let (provider, model) = self.ctx.for_role(agent.role());
        let (exit, record) = run_stage(
            self.ctx,
            &provider,
            model.as_deref(),
            StageStart::Announce,
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: true,
                could_not_start: format!("agent could not start for {}", task.id),
                default_failure: "agent failed".into(),
                cancelled_notice: Some("Agent cancelled; nothing committed.".into()),
                failure_level: NoticeLevel::Error,
                failure_prefix: format!("{}: ", task.id),
                failure_suffix: String::new(),
            },
        )
        .await;
        let outcome = match exit {
            StageExit::Cancelled => BuildOutcome::Cancelled,
            StageExit::Failed => BuildOutcome::Failed,
            StageExit::Completed(builder_events) => {
                // The builder succeeded: its final message's build claims section
                // becomes the claims artifact, with the
                // verification-results trimming applied. The builder agent's
                // validation is trivial acceptance, so this arm cannot fire.
                let final_text = agent.validate(&builder_events).unwrap_or_default();
                let claims = agents::claims_section(&final_text).map(|claims| {
                    let trimmed = agents::trim_claims(claims);
                    write_claims_file(self.ctx, &task.id, &trimmed);
                    trimmed
                });
                BuildOutcome::Completed { claims }
            }
        };
        record.write(&outcome);
        outcome
    }
}

/// Writes the builder's build claims to the task directory as
/// `<task id>-claims-<timestamp>.md`.
fn write_claims_file(ctx: &SessionContext, task_id: &str, claims: &str) {
    let dir = ctx.current_task_dir();
    match artifact::write_artifact(&dir, &format!("{task_id}-claims"), claims) {
        Ok(path) => ctx.notice(
            NoticeLevel::Info,
            format!("Build claims written to {}", path.display()),
        ),
        Err(e) => ctx.notice(
            NoticeLevel::Warning,
            format!("cannot write {}: {}", e.path.display(), e.error),
        ),
    }
}
