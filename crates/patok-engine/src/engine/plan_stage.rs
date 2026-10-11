use patok_core::event::NoticeLevel;
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::Task;

use super::Engine;
use crate::agents::{Agent, PlanAgent};
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};

/// The outcome of one task's plan stage (T10.1).
#[derive(Debug)]
pub(crate) enum PlanStage {
    /// The plan gate accepted this plan text.
    Accepted(String),
    /// The plan session was cancelled.
    Cancelled,
    /// The plan could not be produced, or was rejected twice; the reason was reported.
    Failed,
}

impl Engine {
    /// The per-task plan stage (T10.1): one plan session with the read-only allowlist,
    /// whose captured plan must pass the plan gate. On rejection the plan is retried once
    /// with a prompt carrying the rejected plan and the reason; a plan rejected twice
    /// fails the task. The shell is told the planner is the active agent for the whole
    /// stage (T11.1). `research` is the prior research report, when one exists (T68.1).
    pub(crate) async fn run_plan_stage(
        &self,
        task: &Task,
        ctx: &SessionContext,
        research: Option<&str>,
    ) -> PlanStage {
        // The plan session is the running stage: its tile is active with the
        // build tile still pending.
        ctx.update_tiles(|tiles| {
            tiles.set(Stage::Research, TileStatus::Done);
            tiles.set(Stage::Plan, TileStatus::Active);
        });
        let mut agent = PlanAgent::new(task, research);
        for attempt in 1..=2 {
            // Every attempt is one planner session with its own timing, so a retried
            // plan leaves its own finished line (T42.1) and starting line (T78.1).
            let exit = run_stage(
                ctx,
                ctx.provider(),
                ctx.model(),
                StageStart::Announce,
                agent.role(),
                AgentPrompt::of(&agent),
                StageWords {
                    capture: true,
                    could_not_start: format!("plan agent could not start for {}", task.id),
                    default_failure: "plan failed".into(),
                    cancelled_notice: Some("Plan cancelled; nothing committed.".into()),
                    failure_level: NoticeLevel::Error,
                    failure_prefix: format!("plan for {}: ", task.id),
                    failure_suffix: String::new(),
                },
            )
            .await;
            let events = match exit {
                StageExit::Cancelled => return PlanStage::Cancelled,
                StageExit::Failed => return PlanStage::Failed,
                StageExit::Completed(events) => events,
            };
            match agent.validate(&events) {
                Ok(text) => return PlanStage::Accepted(text),
                Err(reason) if attempt == 2 => {
                    ctx.notice(
                        NoticeLevel::Error,
                        format!(
                            "{}: the plan was rejected twice ({reason}); the task was not built",
                            task.id
                        ),
                    );
                    return PlanStage::Failed;
                }
                Err(reason) => {
                    agent = agent.retry(&events, &reason);
                }
            }
        }
        unreachable!("the plan stage returns within its two attempts")
    }
}
