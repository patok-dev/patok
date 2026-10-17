//! The per-task plan step: the plan gate, its one retry and the plan file.

use patok_core::event::NoticeLevel;
use patok_core::task::Task;

use crate::agents::{Agent, PlanAgent};
use crate::artifact;
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};
use serde::Serialize;

/// The outcome of one task's plan step (T10.1).
#[derive(Debug, Serialize)]
pub(crate) enum PlanOutcome {
    /// The plan gate accepted this plan text.
    Accepted(String),
    /// The plan could not be produced, or was rejected twice; the reason was reported.
    Failed,
    /// The plan session was cancelled.
    Cancelled,
}

/// The per-task plan step (T10.1): one plan session with the read-only allowlist,
/// whose captured plan must pass the plan gate. On rejection the plan is retried once
/// with a prompt carrying the rejected plan and the reason; a plan rejected twice
/// fails the task. The shell is told the planner is the active agent for the whole
/// step (T11.1). `research` is the prior research report, when one exists (T68.1).
pub(crate) struct PlanStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> PlanStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    pub(crate) async fn run(&self, task: &Task, research: Option<&str>) -> PlanOutcome {
        let mut agent = PlanAgent::new(task, research);
        let (provider, model) = self.ctx.for_role(agent.role());

        for attempt in 1..=2 {
            // Every attempt is one planner session with its own timing, so a retried
            // plan leaves its own finished line (T42.1) and starting line (T78.1).
            let (exit, record) = run_stage(
                self.ctx,
                &provider,
                model.as_deref(),
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
                StageExit::Cancelled => {
                    let outcome = PlanOutcome::Cancelled;
                    record.write(&outcome);
                    return outcome;
                }
                StageExit::Failed => {
                    let outcome = PlanOutcome::Failed;
                    record.write(&outcome);
                    return outcome;
                }
                StageExit::Completed(events) => events,
            };
            match agent.validate(&events) {
                Ok(text) => {
                    let outcome = PlanOutcome::Accepted(text.clone());
                    write_plan_file(self.ctx, &task.id, &text);
                    record.write(&outcome);
                    return outcome;
                }
                Err(reason) if attempt == 2 => {
                    self.ctx.notice(
                        NoticeLevel::Error,
                        format!(
                            "{}: the plan was rejected twice ({reason}); the task was not built",
                            task.id
                        ),
                    );
                    let outcome = PlanOutcome::Failed;
                    record.failure(reason.clone()).write(&outcome);
                    return outcome;
                }
                Err(reason) => {
                    // First rejection: write record with Rejected outcome and retry
                    record
                        .failure(reason.clone())
                        .write(&serde_json::json!({ "Rejected": reason }));
                    agent = agent.retry(&events, &reason);
                }
            }
        }
        unreachable!("the plan step returns within its two attempts")
    }
}

/// Writes the accepted plan to the task directory as
/// `<task id>-<timestamp>.md` (T10.1).
fn write_plan_file(ctx: &SessionContext, task_id: &str, plan: &str) {
    let dir = ctx.current_task_dir();
    match artifact::write_artifact(&dir, &format!("{task_id}-plan"), plan) {
        Ok(path) => ctx.notice(
            NoticeLevel::Info,
            format!("Plan written to {}", path.display()),
        ),
        Err(e) => ctx.notice(
            NoticeLevel::Warning,
            format!("cannot write {}: {}", e.path.display(), e.error),
        ),
    }
}
