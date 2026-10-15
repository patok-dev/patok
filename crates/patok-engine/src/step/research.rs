//! The per-task research step and the research-report helpers it shares
//! with the queue-creation research step.

use patok_core::complexity::{self, Complexity};
use patok_core::event::NoticeLevel;
use patok_core::task::Task;

use crate::agents::{Agent, ResearchAgent};
use crate::artifact;
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};

/// How long a research report on disk stays reusable across restarts.
pub(crate) const RESEARCH_REUSE_SECS: u64 = 600;

/// The outcome of one task's research step (T68.1).
#[derive(Debug)]
pub(crate) enum ResearchOutcome {
    /// A report exists: fresh from a session or reused from disk.
    Report(String),
    /// The step was skipped (a Simple task with the key on).
    Skipped,
    /// The research session failed; the pipeline continues without a report
    ///.
    Failed,
    /// The research session was cancelled.
    Cancelled,
}

/// The per-task research step (T68.1): one research
/// session with the research allowlist runs before the task is planned. A
/// report on disk younger than ten minutes is reused without a session
/// (across restarts); a Simple-classified task skips the step when the
/// `skip_research_for_simple` key is on. A failed session is non-blocking
/// (the pipeline continues without a report); a cancelled one ends the
/// task. The shell is told the research agent is active for the whole
/// step (T11.1).
pub(crate) struct ResearchStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> ResearchStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// The outcome when no session runs: a reusable report or the Simple
    /// skip. Pure: no notices, no writes, so `run` can redo it as its first
    /// check.
    fn without_session(&self, task: &Task) -> Option<ResearchOutcome> {
        // A fresh report on disk is reused: no session.
        if let Some(report) = reusable_research_report(self.ctx, &task.id) {
            return Some(ResearchOutcome::Report(report));
        }
        // A Simple task skips the step when the key says so.
        if self.ctx.settings().skip_research_for_simple
            && complexity::classify(&task.description) == Complexity::Simple
        {
            return Some(ResearchOutcome::Skipped);
        }
        None
    }

    /// Whether `run` would start a research session for `task` (the build
    /// pipeline asks before its research-tile update; the check is
    /// deterministic next to `run`'s own).
    pub(crate) fn runs_session(&self, task: &Task) -> bool {
        self.without_session(task).is_none()
    }

    pub(crate) async fn run(&self, task: &Task) -> ResearchOutcome {
        if let Some(outcome) = self.without_session(task) {
            if let ResearchOutcome::Report(_) = outcome {
                self.ctx.notice(
                    NoticeLevel::Info,
                    format!("Reusing the research report for {}.", task.id),
                );
            }
            return outcome;
        }
        let agent = ResearchAgent::new(task);
        let exit = run_stage(
            self.ctx,
            self.ctx.provider(),
            self.ctx.model(),
            StageStart::Announce,
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: true,
                could_not_start: format!("research agent could not start for {}", task.id),
                default_failure: "research failed".into(),
                cancelled_notice: Some("Research cancelled; nothing committed.".into()),
                failure_level: NoticeLevel::Warning,
                failure_prefix: format!("research for {}: ", task.id),
                failure_suffix: "; continuing without a report".into(),
            },
        )
        .await;
        let events = match exit {
            StageExit::Cancelled => return ResearchOutcome::Cancelled,
            StageExit::Failed => return ResearchOutcome::Failed,
            StageExit::Completed(events) => events,
        };
        // The research agent's validation is trivial acceptance, so this arm
        // cannot fire; it stays dead-safe.
        let Ok(report) = agent.validate(&events) else {
            return ResearchOutcome::Failed;
        };
        if report.trim().is_empty() {
            self.ctx.notice(
                NoticeLevel::Warning,
                format!(
                    "research for {} returned no report; continuing without one",
                    task.id
                ),
            );
            return ResearchOutcome::Failed;
        }
        write_research_report(self.ctx, &task.id, &report);
        ResearchOutcome::Report(report)
    }
}

/// The newest research report of `stem` on disk whose filename timestamp
/// is younger than the reuse window, or `None`.
/// The stem is a task ID for the per-task research step and `bootstrap` /
/// `scan` for the queue-creation runs (T69.1).
pub(crate) fn reusable_research_report(ctx: &SessionContext, stem: &str) -> Option<String> {
    let dir = ctx.data_dir().join("research");
    let path = artifact::newest_artifact(&dir, stem, Some(RESEARCH_REUSE_SECS))?;
    std::fs::read_to_string(path).ok()
}

/// Writes the research report to the project data directory as
/// `research/<stem>-<timestamp>.md` (T68.1; the queue-creation runs use
/// their kind's stem, T69.1).
pub(crate) fn write_research_report(ctx: &SessionContext, stem: &str, report: &str) {
    let dir = ctx.data_dir().join("research");
    match artifact::write_artifact(&dir, stem, report) {
        Ok(path) => ctx.notice(
            NoticeLevel::Info,
            format!("Research report written to {}", path.display()),
        ),
        Err(e) => ctx.notice(
            NoticeLevel::Warning,
            format!("cannot write {}: {}", e.path.display(), e.error),
        ),
    }
}
