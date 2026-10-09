use patok_core::config::daemon_readout;
use patok_core::event::{AgentRole, EngineEvent, NoticeLevel, Snapshot};
use patok_core::task;
use tokio_util::sync::CancellationToken;

use super::{AttachError, Attachment, Engine};
use crate::TASK_FILE;
use crate::taskfile;

/// Which `start_*` command is asking: the four entry points share one guard
/// preamble and differ only in the wording of two of its rejections (finding
/// 17 in docs/private/structure-review.md).
pub(crate) enum StartCommand {
    Build,
    AddTasks,
    QueueCreation,
    Discovery,
}

/// The two research queue-creation runs (T69.1): the Research agent's second
/// duty besides investigating a task is creating the project's initial task
/// queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueCreation {
    /// The NeedsQueue scenario's empty submit: create the initial queue from
    /// the spec.
    Bootstrap,
    /// The QueueComplete scenario's empty submit: a gap scan for worthwhile
    /// follow-up work.
    Scan,
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

impl QueueCreation {
    /// The run's research report stem: its report is saved as
    /// `research/<stem>-<timestamp>.md` and reused within the research
    /// reuse window by stem.
    pub(crate) fn stem(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Scan => "scan",
        }
    }
}

impl Engine {
    /// Takes the single controlling-shell slot and returns the current state plus a live feed.
    pub fn attach(&self) -> Result<Attachment, AttachError> {
        let mut state = self.state();
        if state.attached {
            return Err(AttachError::AlreadyAttached(
                self.inner.config.project_dir.display().to_string(),
            ));
        }
        state.attached = true;
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
            tasks: state.tasks.clone(),
            current_task: state.current_task.clone(),
            planning: state.run.is_planning(),
            discovering: state.run.is_discovering(),
            provider,
            model: model.unwrap_or_default(),
            settings,
            pipeline: state.pipeline.clone(),
            recent: state.recent.iter().cloned().collect(),
        };
        let events = self.inner.events.subscribe();
        drop(state);
        Ok(Attachment {
            snapshot,
            events,
            engine: self.clone(),
        })
    }

    /// Starts the build on the first pending task. The session-level loop then continues with the next pending task after each finish until
    /// none remain, and in continuous mode runs one discovery round if the cooldown has
    /// elapsed. The central
    /// settings are snapshotted here and the whole session runs on that unit. Errors are user-facing rejection reasons.
    pub fn start_build(&self) -> Result<(), String> {
        let unit = self.unit()?;
        let path = self.inner.config.task_file();
        let tasks =
            taskfile::load(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::Build)?;
        let task = task::next_pending(&tasks)
            .ok_or_else(|| format!("no pending tasks in {TASK_FILE}"))?
            .clone();
        let cancel = CancellationToken::new();
        state.cancel = cancel.clone();
        // A new session starts a new batch-review group: the same leading number recurring after a different task ran
        // starts a fresh group with a fresh base, and an interrupted group
        // resumes through the open-group record (see `note_group`).
        state.group_number = None;
        state.group_base = None;
        state.current_task = Some(task.id.clone());
        let run = state.run.build_started();
        self.apply_run_state(&mut state, run);
        self.emit_locked(
            &mut state,
            EngineEvent::TaskStarted {
                id: task.id.clone(),
                description: task.description.clone(),
            },
        );
        self.task_tiles_started_locked(&mut state);
        drop(state);

        let engine = self.clone();
        tokio::spawn(async move { engine.run_session(task, cancel, unit).await });
        Ok(())
    }

    /// Starts one planner session that appends tasks for `request` to the task file.
    /// Errors are user-facing rejection reasons.
    pub fn start_add_tasks(&self, request: &str) -> Result<(), String> {
        let request = request.trim();
        if request.is_empty() {
            return Err("the request is empty".into());
        }
        let unit = self.unit()?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::AddTasks)?;
        // Record the task list as it is before the planner touches it (validated in T3.6).
        // A missing file is created with a minimal header first. The lock is held only for
        // that read and released before the session runs, so an inject is never blocked by
        // the planner (T77.1); the post-session check re-acquires it for the validation.
        let path = self.inner.config.task_file();
        let Ok(guard) = self.inner.task_file_lock.clone().try_lock_owned() else {
            return Err("the task file is being updated; try again".into());
        };
        let before = taskfile::read_or_create(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        drop(guard);
        let cancel = CancellationToken::new();
        state.cancel = cancel.clone();
        // The planner announces itself before the planning flag flips (T24.1),
        // so the shell's header and output frame title show the Planner from
        // the moment the run starts.
        let start = self.announce_session_locked(
            &mut state,
            AgentRole::Planner,
            unit.provider.slug(),
            unit.model.as_deref(),
        );
        state.run = state.run.planning_started();
        self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: true });
        drop(state);

        let engine = self.clone();
        let request = request.to_string();
        tokio::spawn(async move {
            engine
                .run_planner(request, before, cancel, unit, start)
                .await
        });
        Ok(())
    }

    /// Starts one research queue-creation run (T69.1): the research agent
    /// investigates the project and appends the resulting tasks to the task
    /// file. A fresh same-kind research report on disk is reused without a
    /// session (the T68.1 reuse window). Errors are user-facing rejection
    /// reasons.
    pub fn start_queue_creation(&self, kind: QueueCreation) -> Result<(), String> {
        let unit = self.unit()?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::QueueCreation)?;
        // A fresh same-kind research report is reused: no session (the T68.1
        // reuse window covers the queue-creation reports).
        if self.reusable_research_report(kind.stem()).is_some() {
            self.emit_locked(
                &mut state,
                EngineEvent::Notice {
                    level: NoticeLevel::Info,
                    text: format!("Reusing the research report for {}.", kind.stem()),
                },
            );
            return Ok(());
        }
        // Like the planner run: the file is recorded first under a briefly held
        // lock, released before the session runs (T77.1).
        let path = self.inner.config.task_file();
        let Ok(guard) = self.inner.task_file_lock.clone().try_lock_owned() else {
            return Err("the task file is being updated; try again".into());
        };
        let before = taskfile::read_or_create(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        drop(guard);
        let cancel = CancellationToken::new();
        state.cancel = cancel.clone();
        // The research agent announces itself before the planning flag flips,
        // so the shell's header and output frame title show Research from the
        // moment the run starts (T24.1).
        let start = self.announce_session_locked(
            &mut state,
            AgentRole::Research,
            unit.provider.slug(),
            unit.model.as_deref(),
        );
        state.run = state.run.planning_started();
        self.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: true });
        drop(state);

        let engine = self.clone();
        tokio::spawn(async move {
            engine
                .run_queue_creation(kind, before, cancel, unit, start)
                .await
        });
        Ok(())
    }

    /// Starts one discovery session that scans the project for follow-up work and appends
    /// `D`-prefixed tasks to the task file. Errors are user-facing
    /// rejection reasons.
    pub fn start_discovery(&self) -> Result<(), String> {
        let unit = self.unit()?;
        let mut state = self.state();
        Self::check_start(&state, StartCommand::Discovery)?;
        // Like the planner run: the file is recorded first under a briefly held
        // lock, released before the session runs (T77.1).
        let path = self.inner.config.task_file();
        let Ok(guard) = self.inner.task_file_lock.clone().try_lock_owned() else {
            return Err("the task file is being updated; try again".into());
        };
        let before = taskfile::read_or_create(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        drop(guard);
        let cancel = CancellationToken::new();
        state.cancel = cancel.clone();
        state.run = state.run.discovery_started();
        self.emit_locked(
            &mut state,
            EngineEvent::DiscoveryChanged { discovering: true },
        );
        self.refresh_pipeline_locked(&mut state);
        drop(state);

        let engine = self.clone();
        tokio::spawn(async move {
            let added = engine.run_discovery(before, cancel, &unit).await;
            engine.record_round(added);
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
        let path = self.inner.config.task_file();
        let _guard = self.inner.task_file_lock.clone().lock_owned().await;
        // The file is read under the same lock the append follows, so two
        // rapid injects get distinct numbers; a missing file reads as empty
        // and the append creates it.
        let file = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            other => other.map_err(|e| format!("cannot read {}: {e}", path.display()))?,
        };
        let line = taskfile::normalize_injected(text, &file);
        let chunk = taskfile::append_line(&path, &line)
            .map_err(|e| format!("cannot append to {}: {e}", path.display()))?;
        {
            let mut state = self.state();
            state.injected.push(chunk);
        }
        drop(_guard);
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
    /// Known race, same class as the add/inject race: `begin_next_task`
    /// loads the file without this lock, so a start concurrent with a
    /// removal can still pick the removed task; the consequence is the
    /// session's finalize `write_progress` reporting a missing line, a
    /// documented no-op warning. Errors are user-facing rejection reasons.
    pub async fn remove_task(&self, id: &str) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("the task id is empty".into());
        }
        let path = self.inner.config.task_file();
        let guard = self.inner.task_file_lock.clone().lock_owned().await;
        // Reading `current_task` under the file lock shrinks the race
        // window against `begin_next_task` (see the doc comment above).
        {
            let state = self.state();
            if state.current_task.as_deref() == Some(id) {
                return Err(format!(
                    "task {id} is in progress; wait for it to finish before removing it"
                ));
            }
        }
        match taskfile::remove_task(&path, id) {
            Ok(taskfile::RemoveOutcome::Removed) => {}
            Ok(taskfile::RemoveOutcome::Completed) => {
                return Err(format!("task {id} is completed and cannot be removed"));
            }
            Ok(taskfile::RemoveOutcome::NotFound) => {
                return Err(format!("no task with id {id} in {TASK_FILE}"));
            }
            Err(e) => {
                return Err(format!("cannot update {}: {e}", path.display()));
            }
        }
        drop(guard);
        self.reconcile();
        Ok(())
    }
}
