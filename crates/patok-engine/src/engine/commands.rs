use patok_config::daemon_readout;
use patok_core::event::{AgentRole, EngineEvent, Snapshot};
use patok_providers::ProviderKind;
use tokio_util::sync::CancellationToken;

use super::state::State;
use super::{AttachError, Attachment, Engine};
use crate::session::{Session, SessionKind};
use crate::taskfile::RemoveOutcome;
use crate::{QueueCreation, TASK_FILE};

/// Which `start_*` command is asking: the four entry points share one guard
/// preamble and differ only in the wording of two of its rejections (finding
/// 17 in docs/private/structure-review.md).
pub(crate) enum StartCommand {
    Build,
    AddTasks,
    QueueCreation,
    Discovery,
}

impl StartCommand {
    /// The rejection when a build session is active.
    pub(crate) fn running_rejection(self) -> &'static str {
        match self {
            Self::Build => "a build is already running",
            _ => "a build is running; wait for it to finish",
        }
    }

    /// The rejection when a planner session is active.
    pub(crate) fn planning_rejection(self) -> &'static str {
        match self {
            Self::AddTasks => "the planner is already running",
            _ => "tasks are being planned; wait for the planner to finish",
        }
    }
}

impl Engine {
    /// Records the task file as it is before an append-style run (a planner,
    /// queue-creation or discovery session) and installs the run's fresh
    /// cancel token on `state`: a missing file is created with a minimal
    /// header first (validated in T3.6), and the lock is held only for that
    /// read and released before the session runs, so an inject is never
    /// blocked by the run (T77.1). Errors are user-facing rejection
    /// reasons.
    fn prepare_task_file_run(
        &self,
        state: &mut State,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let before = self.inner.task_list.snapshot_before()?;
        state.cancel = cancel;
        Ok(before)
    }

    /// Returns the current state plus a live event feed for an attached shell.
    pub fn attach(&self) -> Result<Attachment, AttachError> {
        self.attach_as(false)
    }

    /// Like [`Engine::attach`], but the attachment does not count as an attached shell:
    /// it gets the snapshot and live feed without keeping the engine from idle-shutting-down.
    pub fn attach_observer(&self) -> Result<Attachment, AttachError> {
        self.attach_as(true)
    }

    fn attach_as(&self, observer: bool) -> Result<Attachment, AttachError> {
        let mut state = self.state();
        if !observer {
            state.attach_count += 1;
        }
        // The display fields and the settings readout come from the central config: a
        // settings swap holds this lock only briefly and never the state lock inside it,
        // so they are consistent.
        let (provider, model, settings) = {
            let central = self.central();
            (
                central.provider_name.clone(),
                central.model.clone(),
                daemon_readout(&central.settings),
            )
        };
        // A missing task file leaves the queue as it is; start_build reports it.
        self.reconcile_locked(&mut state);
        let snapshot = Snapshot {
            project_dir: self.inner.config.project_dir.display().to_string(),
            phase: state.run.phase(),
            tasks: self.inner.task_list.tasks(),
            current_task: state.current_task.clone(),
            planning: state.run.is_planning(),
            discovering: state.run.is_discovering(),
            provider,
            model: model.unwrap_or_default(),
            settings,
            pipeline: state.pipeline.clone(),
            model_catalog: state.model_catalog.values().cloned().collect(),
            recent: state.recent.iter().cloned().collect(),
        };
        let events = self.inner.events.subscribe();
        drop(state);
        Ok(Attachment {
            snapshot,
            events,
            engine: self.clone(),
            observer,
        })
    }

    /// Starts the build on the first pending task. The session-level loop then continues with the next pending task after each finish until
    /// none remain, and in continuous mode runs one discovery round if the cooldown has
    /// elapsed. The central
    /// settings are snapshotted here and the whole session runs on that session
    /// context. Errors are user-facing rejection reasons.
    pub fn start_build(&self) -> Result<(), String> {
        let cancel = CancellationToken::new();
        let mut ctx = self.session_context(cancel.clone())?;
        let path = self.inner.task_list.path();
        let pending = self
            .inner
            .task_list
            .next_pending()
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::Build)?;
        let task = pending.ok_or_else(|| format!("no pending tasks in {TASK_FILE}"))?;

        // Create both directories before any state changes, so a filesystem
        // failure rejects the start without leaving a half-started session.
        let session_name = crate::history::session_dir_name();
        let history_root = crate::history::resolve_root(ctx.settings(), ctx.data_dir());
        let session_path = crate::history::ensure_session_dir(&history_root, &session_name)
            .map_err(|e| e.to_string())?;
        let task_timestamp = crate::artifact::unix_now().as_millis() as u64;
        let task_dir_name = crate::history::task_dir_name(&task.id, task_timestamp);
        let task_path = crate::history::ensure_task_dir(&session_path, &task_dir_name)
            .map_err(|e| e.to_string())?;
        ctx = ctx.with_session_path(session_path.clone());

        state.cancel = cancel.clone();
        // A new session starts a new batch-review group: the same leading number recurring after a different task ran
        // starts a fresh group with a fresh base, and an interrupted group
        // resumes through the open-group record (see `BuildPipeline::note_group`).
        state.group_number = None;
        state.group_base = None;
        state.current_task = Some(task.id.clone());
        state.current_task_dir = Some(task_path.clone());

        // Emit session open marker and run retention pruning before first event
        let retention_tasks = ctx.settings().history_retention_tasks;
        let retention_days = ctx.settings().history_retention_days;
        self.emit_history_msg_locked(
            &mut state,
            crate::history::HistoryMsg::SessionOpen {
                path: session_path.clone(),
            },
        );
        // Run retention pruning at session open
        let _ = crate::history::prune(
            &history_root,
            retention_tasks,
            retention_days,
            Some(&session_name),
        );

        let run = state.run.build_started();
        self.apply_run_state(&mut state, run);

        // Emit task open marker before TaskStarted
        self.emit_history_msg_locked(
            &mut state,
            crate::history::HistoryMsg::TaskOpen { path: task_path },
        );

        self.emit_locked(
            &mut state,
            EngineEvent::TaskStarted {
                id: task.id.clone(),
                description: task.description.clone(),
            },
        );
        self.task_tiles_started_locked(&mut state);
        drop(state);

        tokio::spawn(async move {
            Session::new(SessionKind::Build { first: task }, ctx)
                .run()
                .await
        });
        Ok(())
    }

    /// Starts one planner session that appends tasks for `request` to the task file.
    /// Errors are user-facing rejection reasons.
    pub fn start_add_tasks(&self, request: &str) -> Result<(), String> {
        let request = request.trim();
        if request.is_empty() {
            return Err("the request is empty".into());
        }
        let cancel = CancellationToken::new();
        let mut ctx = self.session_context(cancel.clone())?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::AddTasks)?;

        let before = self.prepare_task_file_run(&mut state, cancel)?;

        // Generate session directory name and ensure it exists
        let session_name = crate::history::session_dir_name();
        let history_root = crate::history::resolve_root(ctx.settings(), ctx.data_dir());
        let session_path = crate::history::ensure_session_dir(&history_root, &session_name)
            .map_err(|e| e.to_string())?;

        // Emit session open marker and run retention pruning before first event
        let retention_tasks = ctx.settings().history_retention_tasks;
        let retention_days = ctx.settings().history_retention_days;
        self.emit_history_msg_locked(
            &mut state,
            crate::history::HistoryMsg::SessionOpen {
                path: session_path.clone(),
            },
        );
        let _ = crate::history::prune(
            &history_root,
            retention_tasks,
            retention_days,
            Some(&session_name),
        );

        // Set session path in context for task open/close markers
        ctx = ctx.with_session_path(session_path);

        // The planner announces itself before the planning flag flips (T24.1),
        // so the shell's header and output frame title show the Planner from
        // the moment the run starts.
        let (role_provider, role_model) = ctx.for_role(AgentRole::Planner);
        let start = self.announce_session_locked(
            &mut state,
            AgentRole::Planner,
            role_provider.slug(),
            role_model.as_deref(),
        );
        state.run = state.run.planning_started();
        self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: true });
        drop(state);

        let request = request.to_string();
        tokio::spawn(async move {
            Session::new(
                SessionKind::AddTasks {
                    request,
                    before,
                    start,
                },
                ctx,
            )
            .run()
            .await
        });
        Ok(())
    }

    /// Starts one research queue-creation run (T69.1): the research agent
    /// investigates the project and appends the resulting tasks to the task
    /// file. A fresh same-kind research report on disk is reused without a
    /// session (the T68.1 reuse window covers the queue-creation reports).
    /// Errors are user-facing rejection reasons.
    pub fn start_queue_creation(&self, kind: QueueCreation) -> Result<(), String> {
        let cancel = CancellationToken::new();
        let mut ctx = self.session_context(cancel.clone())?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::QueueCreation)?;
        let before = self.prepare_task_file_run(&mut state, cancel)?;

        // Generate session directory name and ensure it exists
        let session_name = crate::history::session_dir_name();
        let history_root = crate::history::resolve_root(ctx.settings(), ctx.data_dir());
        let session_path = crate::history::ensure_session_dir(&history_root, &session_name)
            .map_err(|e| e.to_string())?;

        // Emit session open marker and run retention pruning before first event
        let retention_tasks = ctx.settings().history_retention_tasks;
        let retention_days = ctx.settings().history_retention_days;
        self.emit_history_msg_locked(
            &mut state,
            crate::history::HistoryMsg::SessionOpen {
                path: session_path.clone(),
            },
        );
        let _ = crate::history::prune(
            &history_root,
            retention_tasks,
            retention_days,
            Some(&session_name),
        );

        // Set session path in context for task open/close markers
        ctx = ctx.with_session_path(session_path);

        // The research agent announces itself before the planning flag flips,
        // so the shell's header and output frame title show Research from the
        // moment the run starts (T24.1).
        let (role_provider, role_model) = ctx.for_role(AgentRole::Research);
        let start = self.announce_session_locked(
            &mut state,
            AgentRole::Research,
            role_provider.slug(),
            role_model.as_deref(),
        );
        state.run = state.run.planning_started();
        self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: true });
        drop(state);

        tokio::spawn(async move {
            Session::new(
                SessionKind::QueueCreation {
                    kind,
                    before,
                    start,
                },
                ctx,
            )
            .run()
            .await
        });
        Ok(())
    }

    /// Discovers one provider's models on demand (T8.1): the settings overlay's model
    /// picker asks when it opens for a provider whose catalog is missing, not discovered or
    /// failed. The claim follows the saved-settings rule (`discover_unclaimed`), so a Ready
    /// or Pending catalog starts nothing and a fresh ModelCache entry is served without the
    /// harness. Errors are user-facing rejection reasons.
    pub fn discover_models(&self, provider: &str) -> Result<(), String> {
        let kind = ProviderKind::parse(provider)
            .ok_or_else(|| format!("unknown provider {provider:?}"))?;
        if !kind.info().supports_model_selection {
            return Err(format!("{} has no model discovery", kind.info().slug));
        }
        self.discover_unclaimed(self.state(), [kind]);
        Ok(())
    }

    /// Starts one discovery session that scans the project for follow-up work and appends
    /// `D`-prefixed tasks to the task file. Errors are user-facing
    /// rejection reasons.
    pub fn start_discovery(&self) -> Result<(), String> {
        let cancel = CancellationToken::new();
        let mut ctx = self.session_context(cancel.clone())?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::Discovery)?;

        let before = self.prepare_task_file_run(&mut state, cancel)?;

        // Generate session directory name and ensure it exists
        let session_name = crate::history::session_dir_name();
        let history_root = crate::history::resolve_root(ctx.settings(), ctx.data_dir());
        let session_path = crate::history::ensure_session_dir(&history_root, &session_name)
            .map_err(|e| e.to_string())?;

        // Emit session open marker and run retention pruning before first event
        let retention_tasks = ctx.settings().history_retention_tasks;
        let retention_days = ctx.settings().history_retention_days;
        self.emit_history_msg_locked(
            &mut state,
            crate::history::HistoryMsg::SessionOpen {
                path: session_path.clone(),
            },
        );
        let _ = crate::history::prune(
            &history_root,
            retention_tasks,
            retention_days,
            Some(&session_name),
        );

        // Set session path in context for task open/close markers
        ctx = ctx.with_session_path(session_path);

        state.run = state.run.discovery_started();
        self.emit_locked(
            &mut state,
            EngineEvent::DiscoveryChanged { discovering: true },
        );
        self.refresh_pipeline_locked(&mut state);
        drop(state);

        tokio::spawn(async move {
            Session::new(SessionKind::Discovery { before }, ctx)
                .run()
                .await
        });
        Ok(())
    }

    /// Injects `text` as the next task line of the task file (T77.1): the
    /// confirmed text is normalized into a well-formed unchecked task line
    /// -- the `- [ ] ` checkbox and a fresh `T<N>.1:` id added when missing,
    /// `N` one past the file's highest `T` number, an already well-formed
    /// line kept as it is -- then written as one atomic append under the
    /// task-file lock, the same lock the engine's own rewrites take, and
    /// free for the whole duration of every agent session, followed by an
    /// immediate reconcile, so the new line reaches the in-memory queue and
    /// the attached shells right away, without waiting for the mtime poll.
    /// The exact chunk written is recorded in the task list's
    /// injected-chunk ledger, which the next append-style check drains.
    /// Accepted in every engine state (nothing about the running task
    /// changes: `reconcile` protects it, and the queue only picks the line
    /// up when it starts its next task). Errors are user-facing rejection
    /// reasons.
    pub async fn inject_task(&self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("the task line is empty".into());
        }
        // The modal's shared input accepts multi-line text (Shift-Enter, a
        // paste), but one inject is always one task line: a line break in
        // the text would land the lines after the first as raw text in
        // TASKS.md. The trimmed check lets a trailing Shift-Enter through.
        if text.trim().contains('\n') {
            return Err("the task line must be a single line".into());
        }
        self.inner.task_list.inject(text).await?;
        self.reconcile();
        Ok(())
    }

    /// Removes the task line with the given id from the task file (T98.1).
    /// The engine is the only place where "in progress" exists -- it lives in
    /// `current_task`, not in the file -- so the CLI routes removal through
    /// this method while an engine runs: the current (in-progress) task and
    /// completed tasks are refused, and the removal itself runs under the
    /// task-file lock, the same lock the engine's own rewrites take, followed
    /// by an immediate reconcile, so the line leaves the in-memory queue and
    /// reaches the attached shells right away. Accepted in every engine
    /// state: removing a *pending* task while a build runs is required
    /// behavior, and the running and completed tasks are protected anyway.
    /// Known race, same class as the add/inject race: the Session's
    /// next-task pick loads the file without the lock, so a start concurrent with a
    /// removal can still pick the removed task; the consequence is the
    /// session's commit step `write_progress` reporting a missing line, a
    /// documented no-op warning. Errors are user-facing rejection reasons.
    pub async fn remove_task(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("the task id is empty".into());
        }
        let path = self.inner.task_list.path();
        // `current_task` is read under the task-file lock, which shrinks the
        // race window against the Session's next-task pick (see above).
        let in_progress = || self.state().current_task.as_deref() == Some(id);
        match self.inner.task_list.remove(id, in_progress).await {
            Ok(None) => {
                return Err(format!(
                    "task {id} is in progress; wait for it to finish before removing it"
                ));
            }
            Ok(Some(RemoveOutcome::Removed)) => {}
            Ok(Some(RemoveOutcome::Completed)) => {
                return Err(format!("task {id} is completed and cannot be removed"));
            }
            Ok(Some(RemoveOutcome::NotFound)) => {
                return Err(format!("no task with id {id} in {TASK_FILE}"));
            }
            Err(e) => {
                return Err(format!("cannot update {}: {e}", path.display()));
            }
        }
        self.reconcile();
        Ok(())
    }
}
