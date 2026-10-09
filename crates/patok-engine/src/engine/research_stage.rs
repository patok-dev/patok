use patok_core::complexity::{self, Complexity};
use patok_core::event::{AgentRole, NoticeLevel};
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::Task;
use patok_providers::RESEARCH_TOOLS;
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::session::{AgentPrompt, StageExit, StageStart, StageWords};
use super::state::Unit;
use crate::{plan, prompt};

/// The outcome of one task's research stage (T68.1).
#[derive(Debug)]
pub(crate) enum ResearchStage {
    /// A report exists: fresh from a session or reused from disk.
    Report(String),
    /// The stage was skipped (a Simple task with the key on).
    Skipped,
    /// The research session failed; the pipeline continues without a report
    ///.
    Failed,
    /// The research session was cancelled.
    Cancelled,
}

impl Engine {
    /// The per-task research stage (T68.1): one research
    /// session with the research allowlist runs before the task is planned. A
    /// report on disk younger than ten minutes is reused without a session
    /// (across restarts); a Simple-classified task skips the stage when the
    /// `skip_research_for_simple` key is on. A failed session is non-blocking
    /// (the pipeline continues without a report); a cancelled one ends the
    /// task. The shell is told the research agent is active for the whole
    /// stage (T11.1).
    pub(crate) async fn research_stage(
        &self,
        task: &Task,
        cancel: &CancellationToken,
        unit: &Unit,
    ) -> ResearchStage {
        // A fresh report on disk is reused: no session.
        if let Some(report) = self.reusable_research_report(&task.id) {
            self.notice(
                NoticeLevel::Info,
                format!("Reusing the research report for {}.", task.id),
            );
            return ResearchStage::Report(report);
        }
        // A Simple task skips the stage when the key says so.
        if unit.skip_research_for_simple
            && complexity::classify(&task.description) == Complexity::Simple
        {
            return ResearchStage::Skipped;
        }
        // The research session is the running stage: its tile is active with the
        // later stages still pending.
        self.update_tiles(|tiles| tiles.set(Stage::Research, TileStatus::Active));
        let exit = self
            .run_stage(
                StageStart::Announce,
                AgentRole::Research,
                AgentPrompt {
                    prompt: prompt::research(task),
                    system_prompt: prompt::RESEARCH_SYSTEM,
                    label: "research".into(),
                    tools: RESEARCH_TOOLS,
                },
                cancel.clone(),
                unit,
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
            StageExit::Cancelled => return ResearchStage::Cancelled,
            StageExit::Failed => return ResearchStage::Failed,
            StageExit::Completed(events) => events,
        };
        let report = plan::captured_text(&events);
        if report.trim().is_empty() {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "research for {} returned no report; continuing without one",
                    task.id
                ),
            );
            return ResearchStage::Failed;
        }
        self.write_research_report(&task.id, &report);
        ResearchStage::Report(report)
    }
}
