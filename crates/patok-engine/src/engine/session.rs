use std::time::Instant;

use patok_core::config::RunMode;
use patok_core::event::{AgentRole, EngineEvent, NoticeLevel, TaskOutcome};
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::Task;

use super::Engine;
use super::commands::QueueCreation;
use super::group;
use super::plan_stage::PlanStage;
use super::research_stage::ResearchStage;
use super::review_stage::ReviewStage;
use crate::agents::{
    self, Agent, AppendPlannerAgent, BuilderAgent, DiscoveryAgent, ResearchQueueAgent,
};
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};
use crate::task_list::AppendMode;

impl Engine {
    /// The session-level build loop: pending tasks are
    /// built one after another until none remain, then in continuous mode one discovery
    /// round runs if the cooldown has elapsed; a round that appends tasks continues the
    /// session with them. In sprint mode the empty queue ends the session (the mode comes
    /// from the session's snapshot, so a change takes effect at the next start).
    /// A task that does not complete ends the session (it stays pending, so re-picking it
    /// immediately would loop), as does a stop request. `first` is the task `start_build`
    /// already announced: it runs even when a stop request arrives before the loop does,
    /// so the stop cancels the running agent as before. `ctx` is the settings snapshot
    /// and cancel token the whole session runs on.
    pub(crate) async fn run_session(&self, first: Task, ctx: SessionContext) {
        if self.run_task(first, &ctx).await != TaskOutcome::Done {
            self.end_session();
            return;
        }
        loop {
            while !self.state().stop.is_pending() {
                let Some(task) = self.begin_next_task() else {
                    break;
                };
                if self.run_task(task, &ctx).await != TaskOutcome::Done {
                    self.end_session();
                    return;
                }
            }
            // The queue is empty: sprint mode stops here; in continuous mode one
            // discovery round runs, but only past the cooldown.
            if self.state().stop.is_pending()
                || ctx.settings().run_mode != RunMode::Continuous
                || !self.discovery_due()
            {
                break;
            }
            match self.run_scheduled_discovery(&ctx).await {
                Some(added) if added > 0 => {} // the round filled the queue: keep going
                _ => break,
            }
        }
        self.end_session();
    }

    /// Picks the first pending task, marks it current and announces it; `None` when none
    /// remain (a task added externally mid-session is picked up here like any other).
    fn begin_next_task(&self) -> Option<Task> {
        let task = self.inner.task_list.next_pending().ok().flatten()?;
        let mut state = self.state();
        state.current_task = Some(task.id.clone());
        self.emit_locked(
            &mut state,
            EngineEvent::TaskStarted {
                id: task.id.clone(),
                description: task.description.clone(),
            },
        );
        self.task_tiles_started_locked(&mut state);
        Some(task)
    }

    /// Ends the session: nothing is current and the engine is idle again.
    fn end_session(&self) {
        let mut state = self.state();
        state.current_task = None;
        let run = state.run.build_ended();
        self.apply_run_state(&mut state, run);
        // A safety refresh: the task end already muted its tiles, so this
        // normally emits nothing, but keeps the rail correct even if a path
        // above ever skips `finish_task`.
        self.refresh_pipeline_locked(&mut state);
    }

    /// Whether the discovery cooldown has elapsed, so a round may run.
    fn discovery_due(&self) -> bool {
        let state = self.state();
        state.schedule.is_eligible(Instant::now())
    }

    /// One scheduled discovery round at the empty end of a session: like the manual round,
    /// the file is recorded first -- under the task-file lock only for that read, released
    /// before the session runs (T77.1) and re-acquired for the post-session check. It runs
    /// on the session's settings snapshot. Returns how many tasks it appended, or `None`
    /// when no round ran to completion.
    async fn run_scheduled_discovery(&self, ctx: &SessionContext) -> Option<usize> {
        let before = self.inner.task_list.read_before().await?;
        {
            let mut state = self.state();
            state.run = state.run.gap_discovery_started();
            self.emit_locked(
                &mut state,
                EngineEvent::DiscoveryChanged { discovering: true },
            );
            self.refresh_pipeline_locked(&mut state);
        }
        let added = self.run_discovery(before, ctx).await;
        self.record_round(added);
        added
    }

    /// Records a finished discovery round in the cooldown schedule; a round that did not
    /// complete leaves the schedule alone.
    pub(crate) fn record_round(&self, added: Option<usize>) {
        if let Some(added) = added {
            let mut state = self.state();
            state.schedule.round_finished(Instant::now(), added);
        }
    }

    /// One planner session. `before` is the task file as it was before the run;
    /// `start` is the moment the run was announced in `start_add_tasks`. The run
    /// uses the settings snapshot taken when it was started. The task-file lock
    /// is free for the whole session and re-acquired only for the post-session
    /// check, so an inject during the session is neither blocked nor lost
    /// (T77.1).
    pub(crate) async fn run_planner(
        &self,
        request: String,
        before: String,
        ctx: SessionContext,
        start: Instant,
    ) {
        // The append-tasks planner is the active agent for the whole run (T11.1), so the
        // shell's output frame shows the planner title while it streams.
        let agent = AppendPlannerAgent::new(&request);
        let exit = run_stage(
            &ctx,
            ctx.provider(),
            ctx.model(),
            StageStart::Announced(start),
            agent.role(),
            AgentPrompt::of(&agent),
            StageWords {
                capture: false,
                could_not_start: "planner could not start".into(),
                default_failure: "planner failed".into(),
                cancelled_notice: Some("Planner cancelled.".into()),
                failure_level: NoticeLevel::Error,
                failure_prefix: "planner: ".into(),
                failure_suffix: String::new(),
            },
        )
        .await;
        if let StageExit::Completed(_) = exit {
            self.check_planner_result(&before).await;
        }

        {
            let mut state = self.state();
            state.run = state.run.planning_ended();
            self.reconcile_locked(&mut state);
            self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: false });
        }
        self.inner.planning_done.notify_waiters();
    }

    /// One research queue-creation run (T69.1): the Research agent investigates
    /// the project and appends the resulting tasks. `before` is the task file
    /// as it was before the run; `start` is the moment the run was announced in
    /// [`Engine::start_queue_creation`]. The appended tasks pass the same
    /// append-tasks validation as the planner's and are recorded as UI-added;
    /// the report lands under the research report stem of the run's kind.
    pub(crate) async fn run_queue_creation(
        &self,
        kind: QueueCreation,
        before: String,
        ctx: SessionContext,
        start: Instant,
    ) {
        // The research agent is the active agent for the whole run (T11.1), so
        // the shell's output frame shows the research title while it streams.
        let agent = match kind {
            QueueCreation::Bootstrap => ResearchQueueAgent::bootstrap(),
            QueueCreation::Scan => ResearchQueueAgent::scan(),
        };
        let exit = run_stage(
            &ctx,
            ctx.provider(),
            ctx.model(),
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
        if let StageExit::Completed(events) = exit {
            // The appended tasks pass the existing append-tasks validation,
            // count as UI-added like the planner's, and reconcile the queue;
            // only a pass saves the research report, so a rejected run
            // re-runs rather than being covered by the reuse window.
            if self
                .check_appended(&before, AgentRole::Research, AppendMode::Tasks, true)
                .await
                .is_ok()
            {
                let report = agent.validate(&events).unwrap_or_default();
                if report.trim().is_empty() {
                    self.notice(
                        NoticeLevel::Warning,
                        "the research run returned no report; nothing was saved",
                    );
                } else {
                    self.write_research_report(kind.stem(), &report);
                }
            }
        }

        {
            let mut state = self.state();
            state.run = state.run.planning_ended();
            self.reconcile_locked(&mut state);
            self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: false });
        }
        self.inner.planning_done.notify_waiters();
    }

    /// One discovery session. `before` is the task file as it was before the run; the run
    /// uses the settings snapshot taken when it was started, with the task-file lock free
    /// for its whole duration (T77.1). Returns how many tasks the round appended once it
    /// completes, or `None` when it did not run to completion (error, cancelled or
    /// failed); only completed rounds move the schedule.
    pub(crate) async fn run_discovery(
        &self,
        before: String,
        ctx: &SessionContext,
    ) -> Option<usize> {
        // The discovery agent is the active agent for the whole round (T11.1), so the
        // shell's output frame shows its title while it streams.
        let agent = DiscoveryAgent::new();
        let exit = run_stage(
            ctx,
            ctx.provider(),
            ctx.model(),
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
        .await;
        let added = match exit {
            StageExit::Completed(_) => Some(self.check_discovery_result(&before).await.len()),
            _ => None,
        };

        {
            let mut state = self.state();
            state.run = state.run.discovery_ended();
            // Only a round that ran to completion marks the DISCOVER tile done
            //; a cancelled or failed one does not.
            state.discovery_ran |= added.is_some();
            self.reconcile_locked(&mut state);
            self.refresh_pipeline_locked(&mut state);
            self.emit_locked(
                &mut state,
                EngineEvent::DiscoveryChanged { discovering: false },
            );
        }
        self.inner.discovery_done.notify_waiters();
        added
    }

    /// Validates the task file after a completed planner session; restores `before` and
    /// reports an error on violation, otherwise reports how many tasks were added and
    /// remembers the appended IDs as this session's UI-added task IDs.
    async fn check_planner_result(&self, before: &str) {
        let _ = self
            .check_appended(before, AgentRole::Planner, AppendMode::Tasks, true)
            .await;
    }

    /// Validates the task file after a completed discovery session: like the planner check,
    /// but the appended tasks must carry `D`-prefixed IDs and are not recorded as UI-added
    /// task IDs. Returns the IDs the round appended.
    async fn check_discovery_result(&self, before: &str) -> Vec<String> {
        self.check_appended(before, AgentRole::Discovery, AppendMode::Discovery, false)
            .await
            .unwrap_or_default()
    }

    /// The shared post-session check of an append-style run: the whole check --
    /// read, validate and, on a violation, restore, with the lines injected
    /// through `inject_task` during the session kept out of the accounting
    /// (T77.1) -- runs in the task list under its file lock. On violation an
    /// error is reported; otherwise the number of tasks added is. Returns
    /// the IDs that were appended, or the rejection reason on a violation
    /// (the queue-creation run saves its research report only on a pass, so
    /// a rejected run re-runs).
    async fn check_appended(
        &self,
        before: &str,
        role: AgentRole,
        mode: AppendMode,
        record_ui_added: bool,
    ) -> Result<Vec<String>, String> {
        match self
            .inner
            .task_list
            .check_appended(before, mode, role)
            .await
        {
            Ok(ids) => {
                if record_ui_added {
                    self.record_ui_added(&ids);
                }
                match ids.len() {
                    0 => self.notice(NoticeLevel::Info, "No tasks added."),
                    1 => self.notice(NoticeLevel::Info, "1 task added."),
                    n => self.notice(NoticeLevel::Info, format!("{n} tasks added.")),
                }
                Ok(ids)
            }
            Err(rejection) => {
                let restored = if rejection.restored {
                    "the previous task file was restored"
                } else {
                    "restoring the previous task file failed"
                };
                self.notice(
                    NoticeLevel::Error,
                    format!("{role} result rejected: {}; {restored}.", rejection.reason),
                );
                Err(rejection.reason)
            }
        }
    }

    /// Runs one task's build session, preceded by the research stage (T68.1) and
    /// the plan stage when it is enabled (`plan_enabled`, T10.1). Emits `TaskFinished`
    /// with the outcome and returns it; the session-level loop decides what happens
    /// next. All sessions run on the session's settings snapshot in `ctx`.
    async fn run_task(&self, task: Task, ctx: &SessionContext) -> TaskOutcome {
        // The batch-review group of this task is noted first: a new leading
        // number starts or resumes a group through the open-group record,
        // capturing the base its last review diffs across.
        self.note_group(&task, ctx).await;

        // The research stage (T68.1): one research session runs before the task is
        // planned (or built, with the plan stage off), unless a fresh report is on
        // disk to reuse or the task is Simple with the skip key on. A failed session
        // is non-blocking: the pipeline continues without a report.
        let mut ran_research = true;
        let research = match self.research_stage(&task, ctx).await {
            ResearchStage::Cancelled => {
                return self.finish_task(&task, TaskOutcome::Cancelled, None);
            }
            ResearchStage::Report(text) => Some(text),
            ResearchStage::Skipped => {
                ran_research = false;
                None
            }
            ResearchStage::Failed => None,
        };

        // The plan stage: behind the `plan_enabled` key, one plan session with the
        // read-only allowlist runs before the builder session. A plan rejected twice
        // fails the task without a builder session, a commit or a task-file tick.
        // The research report is the plan prompt's prior artifact (T68.1).
        let plan = if ctx.settings().plan_enabled {
            match self.run_plan_stage(&task, ctx, research.as_deref()).await {
                PlanStage::Accepted(text) => {
                    self.write_plan_file(&task.id, &text);
                    Some(text)
                }
                PlanStage::Cancelled => {
                    return self.finish_task(&task, TaskOutcome::Cancelled, None);
                }
                PlanStage::Failed => {
                    return self.finish_task(&task, TaskOutcome::Failed, None);
                }
            }
        } else {
            None
        };
        let progress = |ran_review: bool, validated: bool| {
            group::progress(ran_research, plan.is_some(), true, ran_review, validated)
        };

        // The builder takes over from the planner (T11.1); also announced when the plan
        // stage is disabled, so the shell never shows a stale agent. The research and
        // plan tiles are done and the build tile active from here on -- also when a
        // stage was skipped, so the stages before the running one are always done.
        ctx.update_tiles(|tiles| {
            tiles.set(Stage::Research, TileStatus::Done);
            tiles.set(Stage::Plan, TileStatus::Done);
            tiles.set(Stage::Build, TileStatus::Active);
        });

        // The builder's final message carries the build claims section the review
        // stage reviews, so the events are captured like the plan stage's.
        let agent = BuilderAgent::new(&task, plan.as_deref(), research.as_deref());
        let exit = run_stage(
            ctx,
            ctx.provider(),
            ctx.model(),
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

        let (outcome, commit) = match exit {
            StageExit::Failed => {
                let commit = self
                    .finalize(&task, ctx, &progress(false, false), false)
                    .await;
                (TaskOutcome::Failed, commit)
            }
            StageExit::Completed(builder_events) => {
                // The builder succeeded: its final message's build claims section
                // becomes the claims artifact, with the
                // verification-results trimming applied. The builder agent's
                // validation is trivial acceptance, so this arm cannot fire.
                let final_text = agent.validate(&builder_events).unwrap_or_default();
                if let Some(claims) = agents::claims_section(&final_text) {
                    self.write_claims_file(&task.id, &agents::trim_claims(claims));
                }
                // The build tile is done once the builder session completed: the
                // stages before the running one are always done, so review never renders with the build
                // still in-progress.
                ctx.update_tiles(|tiles| tiles.set(Stage::Build, TileStatus::Done));
                // The review stage: the review pipeline
                // slot between the builder and the commit finalization.
                let review = self.review_stage(&task, ctx, plan.as_deref()).await;
                match review {
                    ReviewStage::Cancelled => {
                        return self.finish_task(&task, TaskOutcome::Cancelled, None);
                    }
                    ReviewStage::Passed => {
                        let commit = self.finalize(&task, ctx, &progress(true, true), true).await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewStage::Skipped => {
                        let commit = self
                            .finalize(&task, ctx, &progress(false, true), true)
                            .await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewStage::Failed => {
                        let commit = self
                            .finalize(&task, ctx, &progress(true, false), false)
                            .await;
                        (TaskOutcome::Failed, commit)
                    }
                }
            }
            StageExit::Cancelled => (TaskOutcome::Cancelled, None),
        };

        self.finish_task(&task, outcome, commit)
    }

    /// Ends one task: records the outcome and clears the current task. A completed
    /// UI-added task postpones the next discovery round by the cooldown and resets a
    /// doubled one: the engine remembers the IDs it appended
    /// through the append-tasks flow this session.
    fn finish_task(
        &self,
        task: &Task,
        outcome: TaskOutcome,
        commit: Option<String>,
    ) -> TaskOutcome {
        let mut state = self.state();
        if outcome == TaskOutcome::Done && self.inner.task_list.is_ui_added(&task.id) {
            state.schedule.ui_added_task_completed(Instant::now());
        }
        self.emit_locked(
            &mut state,
            EngineEvent::TaskFinished {
                id: task.id.clone(),
                outcome,
                commit,
            },
        );
        state.current_task = None;
        self.task_tiles_finished_locked(&mut state);
        outcome
    }
}
