//! The session context (section 8 of docs/private/engine-restructure.md):
//! the session's unit-of-work snapshot -- the resolved provider, the
//! model override, the merged settings and the cancel token -- plus a
//! private handle on the engine internals. It is the only bridge from
//! the sessions, pipelines and steps to the engine's bus, tile,
//! task-list and state paths, exposed through narrow methods.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use patok_core::config::DaemonSettings;
use patok_core::event::{AgentRole, EngineEvent, NoticeLevel, SessionOutcome, TaskOutcome};
use patok_core::pipeline::Stage;
use patok_core::task::Task;
use patok_providers::Provider;
use tokio_util::sync::CancellationToken;

use crate::Engine;
use crate::engine::{Inner, Tiles, resolve_provider};
use crate::git::Git;
use crate::task_list::{AppendMode, AppendRejection};

/// Which configured pause the loop waits before the next unit of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    Agent,
    Task,
    Cycle,
}

/// The adaptive pause: what a pause shrinks to when the last agent was not rate limited.
pub(crate) const ADAPTIVE_PAUSE: Duration = Duration::from_millis(500);

/// The pause of `kind` (`pause_between_*_secs`); with `adaptive_pauses` on and the last
/// agent not rate limited, it shrinks to at most [`ADAPTIVE_PAUSE`].
pub(crate) fn pause_duration(
    settings: &DaemonSettings,
    kind: Pause,
    rate_limited: bool,
) -> Duration {
    let full = Duration::from_secs(match kind {
        Pause::Agent => settings.pause_between_agents_secs,
        Pause::Task => settings.pause_between_tasks_secs,
        Pause::Cycle => settings.pause_between_cycles_secs,
    });
    if settings.adaptive_pauses && !rate_limited {
        full.min(ADAPTIVE_PAUSE)
    } else {
        full
    }
}

/// One session's context (section 8 of docs/private/engine-restructure.md):
/// the unit-of-work snapshot taken at the session's start -- the provider,
/// the model and the merged settings it relies on -- plus the session's
/// cancel token and a private handle on the engine internals. The steps and
/// pipelines borrow `&SessionContext`; it is the only bridge to the engine's
/// bus, tile and reconcile paths, exposed through these narrow methods.
pub(crate) struct SessionContext {
    inner: Arc<Inner>,
    provider: Arc<dyn Provider>,
    settings: DaemonSettings,
    cancel: CancellationToken,
    /// The session directory path for history logging.
    session_path: Option<std::path::PathBuf>,
    /// Whether the last agent session was rate limited.
    last_rate_limited: AtomicBool,
    /// Whether an agent has run since the session started or since the last task or cycle pause.
    agent_since_pause: AtomicBool,
}

impl SessionContext {
    pub(crate) fn new(
        inner: Arc<Inner>,
        provider: Arc<dyn Provider>,
        settings: DaemonSettings,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            inner,
            provider,
            settings,
            cancel,
            session_path: None,
            last_rate_limited: AtomicBool::new(false),
            agent_since_pause: AtomicBool::new(false),
        }
    }

    /// Records one finished agent session's rate-limit status for the next pause.
    pub(crate) fn record_agent(&self, rate_limited: bool) {
        self.last_rate_limited
            .store(rate_limited, Ordering::Relaxed);
        self.agent_since_pause.store(true, Ordering::Relaxed);
    }

    /// Waits the `kind` pause before the next agent, task or discovery cycle; the session's
    /// cancel token cuts it short. An agent pause only follows an agent of the same task:
    /// the first agent of a task or session starts at once.
    pub(crate) async fn pause(&self, kind: Pause) {
        if kind == Pause::Agent && !self.agent_since_pause.load(Ordering::Relaxed) {
            return;
        }
        if kind != Pause::Agent {
            self.agent_since_pause.store(false, Ordering::Relaxed);
        }
        let duration = pause_duration(
            &self.settings,
            kind,
            self.last_rate_limited.load(Ordering::Relaxed),
        );
        if duration.is_zero() {
            return;
        }
        // A soft stop does not fire the cancel token, so poll for it in short slices.
        let deadline = tokio::time::Instant::now() + duration;
        while !self.stop_pending() {
            let slice = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .min(Duration::from_millis(100));
            if slice.is_zero() {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(slice) => {}
                () = self.cancel.cancelled() => break,
            }
        }
    }

    /// Sets the session directory path for history logging.
    pub(crate) fn with_session_path(mut self, path: std::path::PathBuf) -> Self {
        self.session_path = Some(path);
        self
    }

    /// Returns the session directory path if available.
    pub(crate) fn session_path(&self) -> Option<&std::path::Path> {
        self.session_path.as_deref()
    }

    /// Returns a clone of the inner Arc for use by SessionCloseGuard.
    pub(crate) fn inner_clone(&self) -> Arc<Inner> {
        self.inner.clone()
    }

    /// The facade over this context's internals handle, for delegation to the
    /// engine's existing bus, tile and reconcile paths.
    fn engine(&self) -> Engine {
        Engine {
            inner: self.inner.clone(),
        }
    }

    /// The session's settings snapshot, taken when it started.
    pub(crate) fn settings(&self) -> &DaemonSettings {
        &self.settings
    }

    /// The `auto_push_remote` value, read live from the central settings (an
    /// `immediate` field), trimmed; empty means no push.
    pub(crate) fn auto_push_remote(&self) -> String {
        self.engine().settings().auto_push_remote.trim().to_string()
    }

    /// The idle timeout of one agent session (`agent_timeout_secs`).
    pub(crate) fn agent_timeout(&self) -> Duration {
        Duration::from_secs(self.settings.agent_timeout_secs)
    }

    /// Whether the review stage is enabled in the pipeline stage list (an
    /// absent entry is disabled).
    pub(crate) fn review_stage_enabled(&self) -> bool {
        self.settings
            .stages
            .iter()
            .find(|entry| entry.stage == Stage::Review)
            .is_some_and(|entry| entry.enabled)
    }

    /// The token that cancels the session's running agent.
    pub(crate) fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// A `'static` emitter for the agent-event forwarder, which runs on its
    /// own task beside the provider session: each call records the event for
    /// replay and pushes it to all attached shells.
    pub(crate) fn event_sink(&self) -> impl Fn(EngineEvent) + Send + 'static {
        let engine = self.engine();
        move |event| engine.emit(event)
    }

    /// Emits one notice to all attached shells.
    pub(crate) fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.engine().notice(level, text);
    }

    /// Announces `role`'s agent as the engine's active agent and returns the
    /// instant the session's duration is measured from.
    pub(crate) fn announce_agent(
        &self,
        role: AgentRole,
        provider: &str,
        model: Option<&str>,
    ) -> Instant {
        self.engine().announce_session(role, provider, model)
    }

    /// Pushes one agent session's end to the attached shells and the replayed
    /// recent events.
    pub(crate) fn finish_agent(&self, role: AgentRole, start: Instant, outcome: SessionOutcome) {
        self.engine().finish_session(role, start, outcome);
    }

    /// One stage-tile update followed by the rail refresh -- the single
    /// update path every tile change goes through.
    pub(crate) fn update_tiles(&self, update: impl FnOnce(&mut Tiles)) {
        self.engine().update_tiles(update);
    }

    /// The task file's path.
    pub(crate) fn task_file(&self) -> &Path {
        self.inner.task_list.path()
    }

    /// The first pending task as the task file has it now; `None` when none
    /// remain or the file cannot be read.
    pub(crate) fn next_pending(&self) -> Option<Task> {
        self.inner.task_list.next_pending().ok().flatten()
    }

    /// IDs of the pending tasks in the task file (the review deferral's
    /// input).
    pub(crate) fn pending_ids(&self) -> Vec<String> {
        self.inner.task_list.pending_ids()
    }

    /// The task file before a gap discovery round, read under the file lock
    /// (T77.1); `None` on a read error.
    pub(crate) async fn read_before(&self) -> Option<String> {
        self.inner.task_list.read_before().await
    }

    /// The post-session check of an append-style run, under the file lock.
    pub(crate) async fn check_appended(
        &self,
        before: &str,
        mode: AppendMode,
        role: AgentRole,
    ) -> Result<Vec<String>, AppendRejection> {
        self.inner
            .task_list
            .check_appended(before, mode, role)
            .await
    }

    /// Adds `ids` to the engine run's UI-added task IDs.
    pub(crate) fn record_ui_added(&self, ids: &[String]) {
        self.inner.task_list.record_ui_added(ids);
    }

    /// Writes the task's progress token into its line, ticking it when
    /// `done`, under the file lock; whether the line was found.
    pub(crate) async fn write_progress(
        &self,
        task: &Task,
        token: &str,
        done: bool,
    ) -> std::io::Result<bool> {
        self.inner.task_list.write_progress(task, token, done).await
    }

    /// Re-reads the task file, merges it into the queue and broadcasts the
    /// new list when it changed; returns whether it did.
    pub(crate) fn reconcile(&self) -> bool {
        self.engine().reconcile()
    }

    /// A git handle on the session's project directory.
    pub(crate) fn git(&self) -> Git {
        Git::new(&self.inner.config.project_dir)
    }

    /// The project directory the session builds in.
    pub(crate) fn project_dir(&self) -> &Path {
        &self.inner.config.project_dir
    }

    /// The session's persistent per-project data directory.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.inner.config.data_dir
    }

    /// Returns the stored task directory if set, without any fallback.
    pub(crate) fn task_dir(&self) -> Option<PathBuf> {
        let state = self.inner.state.lock().expect("engine state lock");
        state.current_task_dir.clone()
    }

    /// Returns the current task directory if a task is active, otherwise the session directory.
    pub(crate) fn current_task_dir(&self) -> PathBuf {
        let state = self.inner.state.lock().expect("engine state lock");
        // If we have a stored task directory, use it
        if let Some(ref task_dir) = state.current_task_dir {
            return task_dir.clone();
        }
        // Otherwise, fall back to session directory
        if let Some(session_path) = self.session_path() {
            return session_path.to_path_buf();
        }
        // Fallback to data dir if no session path
        self.data_dir().to_path_buf()
    }

    /// Returns the next zero-padded 3-digit sequence number for the current task directory.
    pub(crate) fn next_sequence_number(&self) -> u32 {
        let task_dir = self.current_task_dir();
        crate::history::sequence_counter(&task_dir).unwrap_or(1)
    }

    /// The persistent learned-confidence store file.
    pub(crate) fn review_confidence_path(&self) -> &Path {
        &self.inner.config.review_confidence
    }

    /// The open-group record (`open-group.json`): the batch-review group open
    /// across restarts.
    pub(crate) fn open_group_path(&self) -> &Path {
        &self.inner.config.open_group
    }

    /// The leading task-ID number of the current batch-review group.
    pub(crate) fn group_number(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .group_number
            .clone()
    }

    /// The full SHA the current batch-review group diffs across.
    pub(crate) fn group_base(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .group_base
            .clone()
    }

    /// Writes the batch-review group's number and base in one state update.
    pub(crate) fn set_group(&self, number: Option<String>, base: Option<String>) {
        let mut state = self.inner.state.lock().expect("engine state lock");
        state.group_number = number;
        state.group_base = base;
    }

    /// Whether a stop is pending: no new task starts.
    pub(crate) fn stop_pending(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .stop
            .is_pending()
    }

    /// Marks `task` current, announces it (`TaskStarted`) and starts its
    /// tiles over -- under one state lock, atomically with the
    /// announcement.
    pub(crate) fn begin_task(&self, task: &Task) {
        let engine = self.engine();
        let mut state = self.inner.state.lock().expect("engine state lock");
        state.current_task = Some(task.id.clone());

        // Emit task open marker before TaskStarted if session path is available
        if let Some(session_path) = self.session_path() {
            let task_timestamp = crate::artifact::unix_now().as_millis() as u64;
            let task_dir_name = crate::history::task_dir_name(&task.id, task_timestamp);
            let task_path = crate::history::ensure_task_dir(session_path, &task_dir_name)
                .unwrap_or_else(|_| session_path.join(&task_dir_name));
            state.current_task_dir = Some(task_path.clone());
            engine.emit_history_msg_locked(
                &mut state,
                crate::history::HistoryMsg::TaskOpen { path: task_path },
            );
        }

        engine.emit_locked(
            &mut state,
            EngineEvent::TaskStarted {
                id: task.id.clone(),
                description: task.description.clone(),
            },
        );
        engine.task_tiles_started_locked(&mut state);
    }

    /// Ends one task under one state lock: a completed UI-added task
    /// postpones the next discovery round (the cooldown rule),
    /// `TaskFinished` carries the outcome and commit, the current task is
    /// cleared and the tiles muted. The UI-added check reads the task list
    /// while the state lock is held (state first, task list second -- the
    /// engine's lock order).
    pub(crate) fn finish_task(&self, task: &Task, outcome: TaskOutcome, commit: Option<String>) {
        let engine = self.engine();
        let mut state = self.inner.state.lock().expect("engine state lock");
        if outcome == TaskOutcome::Done && self.inner.task_list.is_ui_added(&task.id) {
            state.schedule.ui_added_task_completed(Instant::now());
        }
        engine.emit_locked(
            &mut state,
            EngineEvent::TaskFinished {
                id: task.id.clone(),
                outcome,
                commit,
            },
        );
        state.current_task = None;
        state.current_task_dir = None;
        engine.task_tiles_finished_locked(&mut state);

        // Emit task close marker after TaskFinished
        engine.emit_history_msg_locked(&mut state, crate::history::HistoryMsg::TaskClose);
    }

    /// Ends the build session: nothing is current, the run state returns to
    /// idle through the phase-aware apply, and the rail is refreshed (a
    /// safety refresh -- the task end already muted its tiles).
    pub(crate) fn build_ended(&self) {
        let engine = self.engine();
        let mut state = self.inner.state.lock().expect("engine state lock");
        state.current_task = None;
        let run = state.run.build_ended();
        engine.apply_run_state(&mut state, run);
        engine.refresh_pipeline_locked(&mut state);
    }

    /// Whether the discovery cooldown has elapsed, so a round may run.
    pub(crate) fn discovery_due(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .schedule
            .is_eligible(Instant::now())
    }

    /// The session's gap round starts: the run-state transition (deliberately
    /// not through the phase-aware apply -- the phase stays running), the
    /// `DiscoveryChanged { discovering: true }` event and the rail refresh,
    /// under one state lock.
    pub(crate) fn gap_discovery_started(&self) {
        let engine = self.engine();
        let mut state = self.inner.state.lock().expect("engine state lock");
        state.run = state.run.gap_discovery_started();
        engine.emit_locked(
            &mut state,
            EngineEvent::DiscoveryChanged { discovering: true },
        );
        engine.refresh_pipeline_locked(&mut state);
    }

    /// A discovery round ended: the run-state transition, the
    /// `discovery_ran` flag (only a round that ran to completion marks the
    /// DISCOVER tile done), the reconcile, the rail refresh and the
    /// `DiscoveryChanged { discovering: false }` event under one state
    /// lock; the `discovery_done` notification fires after the lock is
    /// dropped.
    pub(crate) fn discovery_ended(&self, added: Option<usize>) {
        let engine = self.engine();
        {
            let mut state = self.inner.state.lock().expect("engine state lock");
            state.run = state.run.discovery_ended();
            // Only a round that ran to completion marks the DISCOVER tile
            // done; a cancelled or failed one does not.
            state.discovery_ran |= added.is_some();
            engine.reconcile_locked(&mut state);
            engine.refresh_pipeline_locked(&mut state);
            engine.emit_locked(
                &mut state,
                EngineEvent::DiscoveryChanged { discovering: false },
            );
        }
        self.inner.discovery_done.notify_waiters();
    }

    /// Records a finished discovery round in the cooldown schedule; a round
    /// that did not complete leaves the schedule alone.
    pub(crate) fn record_round(&self, added: Option<usize>) {
        if let Some(added) = added {
            let mut state = self.inner.state.lock().expect("engine state lock");
            state.schedule.round_finished(Instant::now(), added);
        }
    }

    /// A planner or queue-creation run ended: the run-state transition, the
    /// reconcile and the `PlanningChanged { planning: false }` event under
    /// one state lock; the `planning_done` notification fires after the
    /// lock is dropped.
    pub(crate) fn planning_ended(&self) {
        let engine = self.engine();
        {
            let mut state = self.inner.state.lock().expect("engine state lock");
            state.run = state.run.planning_ended();
            engine.reconcile_locked(&mut state);
            engine.emit_locked(&mut state, EngineEvent::PlanningChanged { planning: false });
        }
        self.inner.planning_done.notify_waiters();
    }

    /// The provider and model a role's session runs on: the role's provider
    /// (`<role>_provider`), resolved like any other unit of work, with the role's model
    /// (`<role>_model`, empty meaning the provider's default). A role model
    /// only reaches a provider that supports one
    /// ([`Provider::supports_reviewer_model`]).
    /// A name this build does not implement keeps the session's (builder) provider
    /// silently; an unavailable provider warns and falls back to the
    /// session's provider, as does a role provider on a fixed-provider engine
    /// without a resolver. The fallback keeps the role's own model when the
    /// session's provider supports one.
    pub(crate) fn for_role(&self, role: AgentRole) -> (Arc<dyn Provider>, Option<String>) {
        let (name, role_model) = match role {
            AgentRole::Research => (
                self.settings.research_provider,
                &self.settings.research_model,
            ),
            AgentRole::Planner => (self.settings.planner_provider, &self.settings.planner_model),
            AgentRole::Builder => (self.settings.builder_provider, &self.settings.builder_model),
            AgentRole::Reviewer => (
                self.settings.reviewer_provider,
                &self.settings.reviewer_model,
            ),
            AgentRole::Discovery => (
                self.settings.discovery_provider,
                &self.settings.discovery_model,
            ),
            AgentRole::Orchestrator => (
                self.settings.orchestrator_provider,
                &self.settings.orchestrator_model,
            ),
        };
        let role_model = Some(role_model.clone()).filter(|m| !m.is_empty());
        let fallback = || {
            let model = role_model
                .clone()
                .filter(|_| self.provider.supports_reviewer_model());
            (self.provider.clone(), model)
        };
        let Some(resolver) = self.inner.resolver.as_ref() else {
            if name.kind().is_some() && name != self.settings.builder_provider {
                tracing::warn!(
                    "the {name} {role} provider has no resolver; \
                     the {role} runs on the fixed provider"
                );
            }
            return fallback();
        };
        match resolve_provider(resolver, &name) {
            Ok(Some(provider)) => {
                let model = role_model.filter(|_| provider.supports_reviewer_model());
                (provider, model)
            }
            Ok(None) => fallback(),
            Err(error) => {
                tracing::warn!(
                    "the {name} {role} provider is unavailable ({error}); \
                     the {role} runs on the configured provider"
                );
                fallback()
            }
        }
    }

    /// The reviewer session's provider and model ([`Self::for_role`]).
    pub(crate) fn reviewer(&self) -> (Arc<dyn Provider>, Option<String>) {
        self.for_role(AgentRole::Reviewer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(adaptive_pauses: bool) -> DaemonSettings {
        DaemonSettings {
            pause_between_agents_secs: 3,
            pause_between_tasks_secs: 10,
            pause_between_cycles_secs: 30,
            adaptive_pauses,
            ..DaemonSettings::default()
        }
    }

    #[test]
    fn adaptive_not_rate_limited_shrinks_every_pause() {
        let s = settings(true);
        for kind in [Pause::Agent, Pause::Task, Pause::Cycle] {
            assert_eq!(pause_duration(&s, kind, false), ADAPTIVE_PAUSE);
        }
    }

    #[test]
    fn adaptive_rate_limited_keeps_the_full_pause() {
        let s = settings(true);
        assert_eq!(
            pause_duration(&s, Pause::Agent, true),
            Duration::from_secs(3)
        );
        assert_eq!(
            pause_duration(&s, Pause::Task, true),
            Duration::from_secs(10)
        );
        assert_eq!(
            pause_duration(&s, Pause::Cycle, true),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn non_adaptive_keeps_the_full_pause_either_way() {
        let s = settings(false);
        for rate_limited in [false, true] {
            assert_eq!(
                pause_duration(&s, Pause::Agent, rate_limited),
                Duration::from_secs(3)
            );
        }
    }

    #[test]
    fn a_zero_pause_stays_zero_when_adaptive() {
        let s = DaemonSettings {
            pause_between_agents_secs: 0,
            adaptive_pauses: true,
            ..DaemonSettings::default()
        };
        assert_eq!(pause_duration(&s, Pause::Agent, false), Duration::ZERO);
    }

    #[test]
    fn the_defaults_adapt_to_500_ms() {
        let s = DaemonSettings::default();
        for kind in [Pause::Agent, Pause::Task, Pause::Cycle] {
            assert_eq!(pause_duration(&s, kind, false), ADAPTIVE_PAUSE);
        }
    }
}
