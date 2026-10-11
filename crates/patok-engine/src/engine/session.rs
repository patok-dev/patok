use std::time::Instant;

use patok_core::config::RunMode;
use patok_core::event::{AgentRole, EngineEvent, TaskOutcome};
use patok_core::task::Task;

use super::Engine;
use super::commands::QueueCreation;
use crate::context::SessionContext;
use crate::pipeline::build::BuildPipeline;
use crate::step::add_task::AddTaskStep;
use crate::step::agent_run::StageExit;
use crate::step::discovery::DiscoveryStep;
use crate::step::queue_research::QueueResearchStep;
use crate::step::validate_append::ValidateAppendStep;
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

    /// One planner session: [`AddTaskStep`] runs the append-tasks planner and,
    /// when the session completed, [`ValidateAppendStep`] validates the file
    /// afterwards. `before` is the task file as it was before the run;
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
        let exit = AddTaskStep::new(&ctx).run(&request, start).await;
        if let StageExit::Completed(_) = exit {
            let _ = ValidateAppendStep::new(&ctx)
                .run(&before, AppendMode::Tasks, AgentRole::Planner, true)
                .await;
        }

        {
            let mut state = self.state();
            state.run = state.run.planning_ended();
            self.reconcile_locked(&mut state);
            self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: false });
        }
        self.inner.planning_done.notify_waiters();
    }

    /// One research queue-creation run (T69.1): [`QueueResearchStep`] runs the
    /// Research agent, which investigates the project and appends the
    /// resulting tasks, and [`ValidateAppendStep`] validates the file
    /// afterwards. `before` is the task file as it was before the run;
    /// `start` is the moment the run was announced in
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
        let step = QueueResearchStep::new(&ctx);
        if let Some(report) = step.run(kind, start).await {
            // Only a pass saves the research report, so a rejected run
            // re-runs rather than being covered by the reuse window.
            if ValidateAppendStep::new(&ctx)
                .run(&before, AppendMode::Tasks, AgentRole::Research, true)
                .await
                .is_ok()
            {
                step.save(kind, &report);
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

    /// One discovery session: [`DiscoveryStep`] runs the discovery agent and,
    /// when the session completed, [`ValidateAppendStep`] validates the file
    /// afterwards. `before` is the task file as it was before the run; the run
    /// uses the settings snapshot taken when it was started, with the task-file lock free
    /// for its whole duration (T77.1). Returns how many tasks the round appended once it
    /// completes, or `None` when it did not run to completion (error, cancelled or
    /// failed); only completed rounds move the schedule. A round whose session
    /// completed but was rejected still counts as completed, with zero tasks
    /// appended, so the schedule records it.
    pub(crate) async fn run_discovery(
        &self,
        before: String,
        ctx: &SessionContext,
    ) -> Option<usize> {
        let added = match DiscoveryStep::new(ctx).run().await {
            StageExit::Completed(_) => Some(
                ValidateAppendStep::new(ctx)
                    .run(&before, AppendMode::Discovery, AgentRole::Discovery, false)
                    .await
                    .unwrap_or_default()
                    .len(),
            ),
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

    /// Runs one task's build session: [`BuildPipeline`] owns the task's
    /// per-task build flow -- the research, plan, build, review and commit
    /// steps, their order, every tile update, the progress token and the
    /// batch-review group wiring -- on the session's settings snapshot in
    /// `ctx`. This ends the task through `finish_task`, which emits
    /// `TaskFinished` with the outcome; the session-level loop decides what
    /// happens next.
    async fn run_task(&self, task: Task, ctx: &SessionContext) -> TaskOutcome {
        let (outcome, commit) = BuildPipeline::new(ctx).run(&task).await;
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
