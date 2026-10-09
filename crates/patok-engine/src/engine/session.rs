use std::sync::{Arc, Mutex};
use std::time::Instant;

use patok_core::config::RunMode;
use patok_core::event::{AgentEvent, AgentRole, EngineEvent, NoticeLevel, TaskOutcome};
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::{self, Task};
use patok_providers::{
    BUILDER_TOOLS, DISCOVERY_TOOLS, ExitKind, PLANNER_TOOLS, ProviderError, RESEARCH_TOOLS,
    SessionRequest, SessionResult, Tool,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::commands::QueueCreation;
use super::plan_stage::PlanStage;
use super::research_stage::ResearchStage;
use super::review_stage::ReviewStage;
use super::state::Unit;
use crate::{plan, prompt, review, taskfile};

/// One append-style run's validator for [`Engine::check_appended`]: the
/// `before` text, the `after` text and the chunks injected during the run,
/// returning the appended IDs or the rejection reason.
type AppendValidator = fn(&str, &str, &[String]) -> Result<Vec<String>, String>;

/// One agent session's prompt bundle: the user prompt, the system prompt, the transcript
/// label and the tool allowlist.
pub(crate) struct AgentPrompt {
    pub(crate) prompt: String,
    pub(crate) system_prompt: &'static str,
    pub(crate) label: String,
    /// The session's tool allowlist, provider-neutral; the provider maps each
    /// entry to its own CLI's tool name(s).
    pub(crate) tools: &'static [Tool],
}

/// How the shared stage session's starting line is produced (finding 17 in
/// docs/private/structure-review.md): five stages announce inside
/// [`Engine::run_stage`]; the planner and queue-creation runs were announced
/// by their `start_*` under the state guard, before the planning flag
/// flipped (T24.1), and pass that instant.
pub(crate) enum StageStart {
    /// Announce now, as the active agent.
    Announce,
    /// Already announced; the recorded instant the duration is measured from.
    Announced(Instant),
}

/// The stage-specific words of the shared announce-run-match-exit skeleton
/// (finding 17 in docs/private/structure-review.md): every stage reports a
/// session that could not start with the provider's error, a cancelled one
/// with the stage's warning (the reviewer session stays silent), and a failed
/// one with the stage's level and wording (the research stage's failure is a
/// non-blocking warning; every other stage's is an error).
pub(crate) struct StageWords {
    /// Whether the stage reads the session's normalised events (the plan
    /// stage reads the plan out of them, T10.1); the exit's `Completed`
    /// carries them only when this is on.
    pub(crate) capture: bool,
    /// The tracing subject, e.g. "planner could not start" or
    /// "agent could not start for T42.1".
    pub(crate) could_not_start: String,
    /// The failure reason when the provider reported none.
    pub(crate) default_failure: String,
    /// The cancelled-session warning, or `None` to stay silent.
    pub(crate) cancelled_notice: Option<String>,
    /// The failed-session notice level.
    pub(crate) failure_level: NoticeLevel,
    /// The failed-session notice around the reason.
    pub(crate) failure_prefix: String,
    pub(crate) failure_suffix: String,
}

/// One stage session's classified exit (finding 17 in
/// docs/private/structure-review.md): the caller continues with the
/// stage-specific post-processing.
pub(crate) enum StageExit {
    /// Completed; the captured events when the stage asked for capture.
    Completed(Vec<AgentEvent>),
    /// Cancelled; the stage's warning, when it has one, was reported.
    Cancelled,
    /// Failed or could not start; the failure notice was reported.
    Failed,
}

impl Engine {
    /// The session-level build loop: pending tasks are
    /// built one after another until none remain, then in continuous mode one discovery
    /// round runs if the cooldown has elapsed; a round that appends tasks continues the
    /// session with them. In sprint mode the empty queue ends the session (the mode comes
    /// from the session's snapshot, so a change takes effect at the next start).
    /// A task that does not complete ends the session (it stays pending, so re-picking it
    /// immediately would loop), as does a stop request. `first` is the task `start_build`
    /// already announced: it runs even when a stop request arrives before the loop does,
    /// so the stop cancels the running agent as before. `unit` is the settings snapshot
    /// the whole session runs on.
    pub(crate) async fn run_session(&self, first: Task, cancel: CancellationToken, unit: Unit) {
        if self.run_task(first, &unit, cancel.clone()).await != TaskOutcome::Done {
            self.end_session();
            return;
        }
        loop {
            while !self.state().stop.is_pending() {
                let Some(task) = self.begin_next_task() else {
                    break;
                };
                if self.run_task(task, &unit, cancel.clone()).await != TaskOutcome::Done {
                    self.end_session();
                    return;
                }
            }
            // The queue is empty: sprint mode stops here; in continuous mode one
            // discovery round runs, but only past the cooldown.
            if self.state().stop.is_pending()
                || unit.run_mode != RunMode::Continuous
                || !self.discovery_due()
            {
                break;
            }
            match self.run_scheduled_discovery(cancel.clone(), &unit).await {
                Some(added) if added > 0 => {} // the round filled the queue: keep going
                _ => break,
            }
        }
        self.end_session();
    }

    /// Picks the first pending task, marks it current and announces it; `None` when none
    /// remain (a task added externally mid-session is picked up here like any other).
    fn begin_next_task(&self) -> Option<Task> {
        let path = self.inner.config.task_file();
        let tasks = taskfile::load(&path).ok()?;
        let task = task::next_pending(&tasks)?.clone();
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
    async fn run_scheduled_discovery(
        &self,
        cancel: CancellationToken,
        unit: &Unit,
    ) -> Option<usize> {
        let path = self.inner.config.task_file();
        let before = {
            let _guard = self.inner.task_file_lock.lock().await;
            taskfile::read_or_create(&path).ok()?
        };
        {
            let mut state = self.state();
            state.run = state.run.gap_discovery_started();
            self.emit_locked(
                &mut state,
                EngineEvent::DiscoveryChanged { discovering: true },
            );
            self.refresh_pipeline_locked(&mut state);
        }
        let added = self.run_discovery(before, cancel, unit).await;
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

    /// Runs one agent session on the configured provider: its normalised events are streamed
    /// to attached shells and its raw stream is written to the history log. With `capture`,
    /// the normalised events are also collected and returned (the plan stage reads the plan
    /// out of them, T10.1). Returns the session's outcome, or the error when it could not
    /// start.
    async fn run_agent_session(
        &self,
        agent: AgentPrompt,
        cancel: CancellationToken,
        capture: bool,
        unit: &Unit,
    ) -> (Result<SessionResult, ProviderError>, Vec<AgentEvent>) {
        let inner = &self.inner;
        let (events, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let recorded = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
        let recorder = recorded.clone();
        let forwarder = {
            let engine = self.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if capture {
                        recorder.lock().expect("capture lock").push(event.clone());
                    }
                    engine.emit(EngineEvent::Agent { event });
                }
            })
        };

        let provider = unit.provider.clone();
        let session = provider
            .run_session(SessionRequest {
                model: unit.model.clone(),
                prompt: agent.prompt,
                system_prompt: Some(agent.system_prompt.to_string()),
                project_dir: inner.config.project_dir.clone(),
                events,
                log_dir: inner.config.data_dir.join("history"),
                label: agent.label,
                idle_timeout: unit.agent_timeout,
                cancel,
                allowed_tools: agent.tools.to_vec(),
            })
            .await;
        // The request (and its sender) is gone: let the forwarder flush the tail of the stream.
        let _ = forwarder.await;
        let captured = recorded.lock().expect("capture lock").drain(..).collect();
        (session, captured)
    }

    /// Runs one stage session through the shared announce-run-match-exit
    /// skeleton (finding 17 in docs/private/structure-review.md): the agent
    /// is announced as the active agent (or its already-recorded start
    /// instant is used when the `start_*` command announced it under the
    /// state guard, T24.1), the session runs, and one finished line lands
    /// after the agent output and before the stage's own outcome notices
    /// (T42.1). A session that could not start reports the provider's
    /// error, a cancelled one the stage's warning (the reviewer session
    /// stays silent), and a failed one the stage's level and wording; the
    /// caller continues with the stage-specific post-processing.
    pub(crate) async fn run_stage(
        &self,
        start: StageStart,
        role: AgentRole,
        agent: AgentPrompt,
        cancel: CancellationToken,
        unit: &Unit,
        words: StageWords,
    ) -> StageExit {
        let start = match start {
            StageStart::Announce => {
                self.announce_session(role, unit.provider.slug(), unit.model.as_deref())
            }
            StageStart::Announced(start) => start,
        };
        let (session, events) = self
            .run_agent_session(agent, cancel, words.capture, unit)
            .await;
        self.finish_session(role, start, Self::session_outcome(&session));
        match session {
            Err(error) => {
                tracing::error!("{}: {error}", words.could_not_start);
                self.notice(NoticeLevel::Error, error.to_string());
                StageExit::Failed
            }
            Ok(result) if result.exit == ExitKind::Completed => StageExit::Completed(events),
            Ok(result) if result.exit == ExitKind::Cancelled => {
                if let Some(text) = &words.cancelled_notice {
                    self.notice(NoticeLevel::Warning, text.clone());
                }
                StageExit::Cancelled
            }
            Ok(result) => {
                let reason = result
                    .failure
                    .unwrap_or_else(|| words.default_failure.clone());
                self.notice(
                    words.failure_level,
                    format!("{}{reason}{}", words.failure_prefix, words.failure_suffix),
                );
                StageExit::Failed
            }
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
        cancel: CancellationToken,
        unit: Unit,
        start: Instant,
    ) {
        // The append-tasks planner is the active agent for the whole run (T11.1), so the
        // shell's output frame shows the planner title while it streams.
        let exit = self
            .run_stage(
                StageStart::Announced(start),
                AgentRole::Planner,
                AgentPrompt {
                    prompt: prompt::planner(&request),
                    system_prompt: prompt::PLANNER_SYSTEM,
                    label: "planner".into(),
                    tools: PLANNER_TOOLS,
                },
                cancel,
                &unit,
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
        cancel: CancellationToken,
        unit: Unit,
        start: Instant,
    ) {
        // The research agent is the active agent for the whole run (T11.1), so
        // the shell's output frame shows the research title while it streams.
        let exit = self
            .run_stage(
                StageStart::Announced(start),
                AgentRole::Research,
                AgentPrompt {
                    prompt: match kind {
                        QueueCreation::Bootstrap => prompt::research_queue_bootstrap(),
                        QueueCreation::Scan => prompt::research_queue_scan(),
                    },
                    system_prompt: prompt::RESEARCH_QUEUE_SYSTEM,
                    label: "research".into(),
                    tools: RESEARCH_TOOLS,
                },
                cancel,
                &unit,
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
                .check_appended(
                    &before,
                    AgentRole::Research,
                    taskfile::validate_append_ignoring,
                    true,
                )
                .await
                .is_ok()
            {
                let report = plan::captured_text(&events);
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
        cancel: CancellationToken,
        unit: &Unit,
    ) -> Option<usize> {
        // The discovery agent is the active agent for the whole round (T11.1), so the
        // shell's output frame shows its title while it streams.
        let exit = self
            .run_stage(
                StageStart::Announce,
                AgentRole::Discovery,
                AgentPrompt {
                    prompt: prompt::discovery(),
                    system_prompt: prompt::DISCOVERY_SYSTEM,
                    label: "discovery".into(),
                    tools: DISCOVERY_TOOLS,
                },
                cancel,
                unit,
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
            .check_appended(
                before,
                AgentRole::Planner,
                taskfile::validate_append_ignoring,
                true,
            )
            .await;
    }

    /// Validates the task file after a completed discovery session: like the planner check,
    /// but the appended tasks must carry `D`-prefixed IDs and are not recorded as UI-added
    /// task IDs. Returns the IDs the round appended.
    async fn check_discovery_result(&self, before: &str) -> Vec<String> {
        self.check_appended(
            before,
            AgentRole::Discovery,
            taskfile::validate_discovery_append_ignoring,
            false,
        )
        .await
        .unwrap_or_default()
    }

    /// The shared post-session check of an append-style run: the original text must be
    /// unchanged except for appended unchecked tasks with unique, well-formed IDs of the
    /// session's prefix (`validate`) and the lines injected through `inject_task` during
    /// the session, which the validator filters out of the appended region (T77.1). The
    /// whole check runs under the task-file lock -- read, validate and, on a violation,
    /// restore -- which is the only time the run holds it. On violation the previous file
    /// is restored (keeping the injected lines) and an error reported; otherwise the number
    /// of tasks added is reported. Returns the IDs that were appended, or the rejection
    /// reason on a violation (the queue-creation run saves its research report only on a
    /// pass, so a rejected run re-runs).
    async fn check_appended(
        &self,
        before: &str,
        role: AgentRole,
        validate: AppendValidator,
        record_ui_added: bool,
    ) -> Result<Vec<String>, String> {
        let path = self.inner.config.task_file();
        let _guard = self.inner.task_file_lock.lock().await;
        // The chunks injected since the run started are taken out of the
        // accounting here, inside the same lock hold that reads and validates
        // the file, so an inject cannot slip between the drain and the check.
        let injected: Vec<String> = {
            let mut state = self.state();
            std::mem::take(&mut state.injected)
        };
        let verdict = match std::fs::read_to_string(&path) {
            Ok(after) => validate(before, &after, &injected),
            Err(e) => Err(format!(
                "cannot read {} after the {role} session: {e}",
                path.display()
            )),
        };
        match verdict {
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
            Err(reason) => {
                let restored = match taskfile::restore_keeping(&path, before, &injected) {
                    Ok(()) => "the previous task file was restored",
                    Err(e) => {
                        tracing::error!("cannot restore {}: {e}", path.display());
                        "restoring the previous task file failed"
                    }
                };
                self.notice(
                    NoticeLevel::Error,
                    format!("{role} result rejected: {reason}; {restored}."),
                );
                Err(reason)
            }
        }
    }

    /// Runs one task's build session, preceded by the research stage (T68.1) and
    /// the plan stage when it is enabled (`plan_enabled`, T10.1). Emits `TaskFinished`
    /// with the outcome and returns it; the session-level loop decides what happens
    /// next. All sessions run on the session's settings snapshot `unit`.
    async fn run_task(&self, task: Task, unit: &Unit, cancel: CancellationToken) -> TaskOutcome {
        // The batch-review group of this task is noted first: a new leading
        // number starts or resumes a group through the open-group record,
        // capturing the base its last review diffs across.
        self.note_group(&task).await;

        // The research stage (T68.1): one research session runs before the task is
        // planned (or built, with the plan stage off), unless a fresh report is on
        // disk to reuse or the task is Simple with the skip key on. A failed session
        // is non-blocking: the pipeline continues without a report.
        let mut ran_research = true;
        let research = match self.research_stage(&task, &cancel, unit).await {
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
        let plan = if unit.plan_enabled {
            match self
                .run_plan_stage(&task, &cancel, unit, research.as_deref())
                .await
            {
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
            review::progress(ran_research, plan.is_some(), true, ran_review, validated)
        };

        // The builder takes over from the planner (T11.1); also announced when the plan
        // stage is disabled, so the shell never shows a stale agent. The research and
        // plan tiles are done and the build tile active from here on -- also when a
        // stage was skipped, so the stages before the running one are always done.
        self.update_tiles(|tiles| {
            tiles.set(Stage::Research, TileStatus::Done);
            tiles.set(Stage::Plan, TileStatus::Done);
            tiles.set(Stage::Build, TileStatus::Active);
        });

        // The builder's final message carries the build claims section the review
        // stage reviews, so the events are captured like the plan stage's.
        let exit = self
            .run_stage(
                StageStart::Announce,
                AgentRole::Builder,
                AgentPrompt {
                    prompt: prompt::builder(&task, plan.as_deref(), research.as_deref()),
                    system_prompt: prompt::SYSTEM,
                    label: task.id.clone(),
                    tools: BUILDER_TOOLS,
                },
                cancel.clone(),
                unit,
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
                let commit = self.finalize(&task, &progress(false, false), false).await;
                (TaskOutcome::Failed, commit)
            }
            StageExit::Completed(builder_events) => {
                // The builder succeeded: its final message's build claims section
                // becomes the claims artifact, with the
                // verification-results trimming applied.
                let final_text = plan::captured_text(&builder_events);
                if let Some(claims) = review::claims_section(&final_text) {
                    self.write_claims_file(&task.id, &review::trim_claims(claims));
                }
                // The build tile is done once the builder session completed: the
                // stages before the running one are always done, so review never renders with the build
                // still in-progress.
                self.update_tiles(|tiles| tiles.set(Stage::Build, TileStatus::Done));
                // The review stage: the review pipeline
                // slot between the builder and the commit finalization.
                let review = self
                    .review_stage(&task, unit, &cancel, plan.as_deref())
                    .await;
                match review {
                    ReviewStage::Cancelled => {
                        return self.finish_task(&task, TaskOutcome::Cancelled, None);
                    }
                    ReviewStage::Passed => {
                        let commit = self.finalize(&task, &progress(true, true), true).await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewStage::Skipped => {
                        let commit = self.finalize(&task, &progress(false, true), true).await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewStage::Failed => {
                        let commit = self.finalize(&task, &progress(true, false), false).await;
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
        if outcome == TaskOutcome::Done && state.ui_added_ids.contains(&task.id) {
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
