//! The per-task plan step: the plan gate, its one retry and the plan file.

use patok_core::event::NoticeLevel;
use patok_core::task::Task;
use patok_providers::ExitKind;

use crate::agents::{Agent, PlanAgent};
use crate::artifact;
use crate::context::SessionContext;
use crate::step::agent_run::{
    AgentPrompt, AgentResultRecord, StageExit, StageStart, StageWords, run_stage, write_result_json,
};
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
        let provider_slug = self.ctx.provider().slug().to_string();
        let model = self.ctx.model().map(|s| s.to_string());

        for attempt in 1..=2 {
            // Every attempt is one planner session with its own timing, so a retried
            // plan leaves its own finished line (T42.1) and starting line (T78.1).
            let label = agent.label().to_string();
            let (exit, session_result, seq) = run_stage(
                self.ctx,
                self.ctx.provider(),
                self.ctx.model(),
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
                    let exit_kind = session_result
                        .as_ref()
                        .map(|r| r.exit)
                        .unwrap_or(ExitKind::Cancelled);
                    let success = session_result.as_ref().map(|r| r.success).unwrap_or(false);
                    let failure = session_result.as_ref().and_then(|r| r.failure.clone());
                    let record = AgentResultRecord {
                        agent: label.clone(),
                        provider: provider_slug.clone(),
                        model: model.clone(),
                        exit_kind,
                        success,
                        failure,
                        outcome: serde_json::to_value(&outcome).unwrap(),
                    };
                    write_result_json(self.ctx.current_task_dir().as_path(), seq, &label, record);
                    return outcome;
                }
                StageExit::Failed => {
                    let outcome = PlanOutcome::Failed;
                    let exit_kind = session_result
                        .as_ref()
                        .map(|r| r.exit)
                        .unwrap_or(ExitKind::Failed);
                    let success = session_result.as_ref().map(|r| r.success).unwrap_or(false);
                    let failure = session_result.as_ref().and_then(|r| r.failure.clone());
                    let record = AgentResultRecord {
                        agent: label.clone(),
                        provider: provider_slug.clone(),
                        model: model.clone(),
                        exit_kind,
                        success,
                        failure,
                        outcome: serde_json::to_value(&outcome).unwrap(),
                    };
                    write_result_json(self.ctx.current_task_dir().as_path(), seq, &label, record);
                    return outcome;
                }
                StageExit::Completed(events) => events,
            };
            match agent.validate(&events) {
                Ok(text) => {
                    let outcome = PlanOutcome::Accepted(text.clone());
                    write_plan_file(self.ctx, &task.id, &text);
                    let exit_kind = session_result
                        .as_ref()
                        .map(|r| r.exit)
                        .unwrap_or(ExitKind::Completed);
                    let success = session_result.as_ref().map(|r| r.success).unwrap_or(false);
                    let failure = session_result.as_ref().and_then(|r| r.failure.clone());
                    let record = AgentResultRecord {
                        agent: label.clone(),
                        provider: provider_slug.clone(),
                        model: model.clone(),
                        exit_kind,
                        success,
                        failure,
                        outcome: serde_json::to_value(&outcome).unwrap(),
                    };
                    write_result_json(self.ctx.current_task_dir().as_path(), seq, &label, record);
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
                    let exit_kind = session_result
                        .as_ref()
                        .map(|r| r.exit)
                        .unwrap_or(ExitKind::Failed);
                    let success = session_result.as_ref().map(|r| r.success).unwrap_or(false);
                    let failure = session_result
                        .as_ref()
                        .and_then(|r| r.failure.clone())
                        .or(Some(reason));
                    let record = AgentResultRecord {
                        agent: label.clone(),
                        provider: provider_slug.clone(),
                        model: model.clone(),
                        exit_kind,
                        success,
                        failure,
                        outcome: serde_json::to_value(&outcome).unwrap(),
                    };
                    write_result_json(self.ctx.current_task_dir().as_path(), seq, &label, record);
                    return outcome;
                }
                Err(reason) => {
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
