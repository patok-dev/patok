use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use patok_core::config::{DaemonEnv, DaemonSettings, ProviderKind, ProviderName};
use patok_core::event::{EngineEvent, Phase};
use patok_core::pipeline::{PipelineState, Stage, TileStatus};
use patok_core::task::{self, Task};
use patok_providers::{Provider, ProviderError};
use tokio::sync::{Notify, broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::config::EngineConfig;
use super::stop::Stop;
use crate::schedule::DiscoverySchedule;
use crate::taskfile;

/// The engine's run state: which unit of work owns the engine. At most one
/// unit runs, and the scheduled round inside a build session's empty-queue
/// gap is its own variant, so the invalid combinations of five loose flags
/// are unrepresentable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RunState {
    #[default]
    Idle,
    /// The build session loop runs.
    Running,
    /// A planner or queue-creation session runs.
    Planning,
    /// A discovery round runs on an otherwise idle engine.
    Discovering,
    /// A scheduled discovery round runs in a build session's empty-queue
    /// gap; the session resumes with the appended tasks when it ends.
    SessionDiscovery,
}

/// The per-task stage tiles within the running task, grouped so every update
/// goes through [`Tiles::set`] or [`Tiles::set_all`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tiles {
    /// The research stage tile (T68.1).
    research: TileStatus,
    /// The plan stage tile.
    plan: TileStatus,
    /// The build stage tile.
    build: TileStatus,
    /// The review stage tile (T70.1).
    review: TileStatus,
}

pub(crate) struct State {
    /// The engine's run state, the single owner of which unit of work runs.
    pub(crate) run: RunState,
    pub(crate) current_task: Option<String>,
    pub(crate) recent: VecDeque<EngineEvent>,
    pub(crate) attached: bool,
    /// The pending stop, if any.
    pub(crate) stop: Stop,
    /// The in-memory task queue, kept in step with the task file by `reconcile_locked`.
    pub(crate) tasks: Vec<Task>,
    /// The IDs this engine appended through the append-tasks flow during the session.
    pub(crate) ui_added_ids: Vec<String>,
    /// The exact chunks appended through `inject_task` since the last
    /// append-style check drained them (T77.1): the append-style runs filter
    /// these out of their appended region before validating it, so a line
    /// injected mid-session neither rejects the run nor counts as its task,
    /// and a rejected run's restore re-appends them.
    pub(crate) injected: Vec<String>,
    /// The discovery cooldown schedule.
    pub(crate) schedule: DiscoverySchedule,
    /// Cancels the running agent ("stop now").
    pub(crate) cancel: CancellationToken,
    /// The pipeline rail as last broadcast, with the
    /// stage list the engine runs today; recomputed by `refresh_pipeline_locked`.
    pub(crate) pipeline: PipelineState,
    /// The per-task stage tiles within the running task.
    pub(crate) tiles: Tiles,
    /// The leading task-ID number of the current batch-review group: a contiguous run of same-numbered tasks.
    pub(crate) group_number: Option<String>,
    /// The full HEAD SHA the current batch-review group started at (or the
    /// open-group record's base, on a resumed group), so the group's last
    /// review diffs across every commit of the group.
    pub(crate) group_base: Option<String>,
    /// A discovery round ran to completion this session.
    pub(crate) discovery_ran: bool,
}

/// The central in-memory configuration: the merged, normalized
/// daemon settings plus the display fields derived from them, guarded together so every
/// reader sees a consistent pair. Every consumer reads through it; a unit of work
/// snapshots what it relies on at its start.
pub(crate) struct Central {
    pub(crate) settings: DaemonSettings,
    /// Name of the active provider, for display: the fixed provider's slug on engines
    /// built with [`Engine::new`], the configured provider's name otherwise.
    pub(crate) provider_name: String,
    /// The configured model override, for display.
    pub(crate) model: Option<String>,
}

/// Resolves a provider kind at the start of each unit of work.
pub(crate) type Resolver =
    Arc<dyn Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

/// Resolves `provider` through `resolver`: the shared resolution shape of the
/// startup load, every unit of work and the reviewer routing. `Ok(None)`
/// means the name is not a provider this build implements.
pub(crate) fn resolve_provider(
    resolver: &Resolver,
    provider: &ProviderName,
) -> Result<Option<Arc<dyn Provider>>, ProviderError> {
    match provider.kind() {
        Some(kind) => resolver(kind).map(Some),
        None => Ok(None),
    }
}

/// [`resolve_provider`] as a user-facing rejection: a resolver failure is
/// logged and reported with the provider's error, a name this build does not
/// implement with the fixed message.
pub(crate) fn resolve_or_reject(
    resolver: &Resolver,
    provider: &ProviderName,
) -> Result<Arc<dyn Provider>, String> {
    let name = provider.to_string();
    match resolve_provider(resolver, provider) {
        Ok(Some(provider)) => Ok(provider),
        Ok(None) => Err(format!(
            "the `{name}` provider is not available in this build; use claude, codex or mistral"
        )),
        Err(error) => {
            tracing::error!("provider {name} unavailable: {error}");
            Err(error.to_string())
        }
    }
}

pub(crate) struct Inner {
    pub(crate) config: EngineConfig,
    /// The selected provider, or why none could be set up (reported by the commands).
    /// On engines with a resolver this is the startup resolution, kept for error
    /// reporting; each unit of work re-resolves from the central settings instead.
    pub(crate) provider: Result<Arc<dyn Provider>, String>,
    /// The environment overrides the central settings are reloaded with.
    pub(crate) env: DaemonEnv,
    /// Re-resolves the provider at each unit-of-work start; `None` on engines with a
    /// fixed provider.
    pub(crate) resolver: Option<Resolver>,
    pub(crate) central: Mutex<Central>,
    pub(crate) state: Mutex<State>,
    pub(crate) events: broadcast::Sender<EngineEvent>,
    pub(crate) phase: watch::Sender<Phase>,
    /// Notified when a planning run ends.
    pub(crate) planning_done: Notify,
    /// Notified when a discovery round ends.
    pub(crate) discovery_done: Notify,
    /// Notified when a pending soft stop is cancelled, so the shutdown
    /// handshake's stream can end instead of lingering.
    pub(crate) soft_stop_cancelled: Notify,
    /// Serialises the engine's short atomic task-file operations (the startup
    /// cleanup, the progress write, the inject append and the append-style
    /// runs' read/validate/restore checks); it is never held across an agent
    /// session, so an inject is never blocked by one (T77.1).
    pub(crate) task_file_lock: Arc<tokio::sync::Mutex<()>>,
    /// Cancelled when the engine process should exit.
    pub(crate) shutdown: CancellationToken,
}

/// One unit of work's snapshot of the central configuration: taken at the unit's start -- a build-loop start (covering its whole
/// session), an add-tasks planner run, a discovery round -- and relied on for the unit's
/// duration, so a settings change mid-run takes effect at the next such start.
#[derive(Clone)]
pub(crate) struct Unit {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) model: Option<String>,
    /// The merged daemon settings held whole instead of copied field by field.
    pub(crate) settings: DaemonSettings,
}

impl Unit {
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
}

impl RunState {
    /// The wire phase this state reports: planning and discovery runs never
    /// touched the old phase flag, so they keep the startup phase.
    pub(crate) fn phase(self) -> Phase {
        match self {
            Self::Idle | Self::Planning | Self::Discovering => Phase::Startup,
            Self::Running | Self::SessionDiscovery => Phase::Running,
        }
    }

    /// Whether a planner session is active.
    pub(crate) fn is_planning(self) -> bool {
        self == Self::Planning
    }

    /// Whether a discovery round is active, standalone or inside a session's
    /// empty-queue gap.
    pub(crate) fn is_discovering(self) -> bool {
        matches!(self, Self::Discovering | Self::SessionDiscovery)
    }

    /// Whether a build session is active (the old `phase == Running` read,
    /// true also while the session's gap round runs).
    pub(crate) fn is_build_session(self) -> bool {
        matches!(self, Self::Running | Self::SessionDiscovery)
    }

    /// The build session starts. The guards of `start_build` guarantee the
    /// idle source.
    pub(crate) fn build_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        Self::Running
    }

    /// The build session ends. `end_session` guarantees the source.
    pub(crate) fn build_ended(self) -> Self {
        debug_assert_eq!(self, Self::Running);
        Self::Idle
    }

    /// A planner or queue-creation run starts. The `start_*` guards
    /// guarantee the idle source.
    pub(crate) fn planning_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        Self::Planning
    }

    /// A planner or queue-creation run ends; the engine is idle again.
    pub(crate) fn planning_ended(self) -> Self {
        debug_assert_eq!(self, Self::Planning);
        Self::Idle
    }

    /// A standalone discovery round starts. The guards of `start_discovery`
    /// guarantee the idle source.
    pub(crate) fn discovery_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        Self::Discovering
    }

    /// The session's scheduled round starts in the empty-queue gap; the
    /// phase stays running.
    pub(crate) fn gap_discovery_started(self) -> Self {
        debug_assert_eq!(self, Self::Running);
        Self::SessionDiscovery
    }

    /// A discovery round ends: a standalone one leaves the engine idle, and
    /// the session's gap round returns to the still-running session.
    pub(crate) fn discovery_ended(self) -> Self {
        match self {
            Self::Discovering => Self::Idle,
            Self::SessionDiscovery => Self::Running,
            other => other,
        }
    }
}

impl Tiles {
    /// One stage's status.
    pub(crate) fn get(self, stage: Stage) -> TileStatus {
        match stage {
            Stage::Research => self.research,
            Stage::Plan => self.plan,
            Stage::Build => self.build,
            Stage::Review => self.review,
        }
    }

    /// The single per-stage update primitive.
    pub(crate) fn set(&mut self, stage: Stage, status: TileStatus) {
        match stage {
            Stage::Research => self.research = status,
            Stage::Plan => self.plan = status,
            Stage::Build => self.build = status,
            Stage::Review => self.review = status,
        }
    }

    /// All four stages at once: a task's tiles start over as pending and end
    /// muted, whatever the outcome.
    pub(crate) fn set_all(&mut self, status: TileStatus) {
        self.research = status;
        self.plan = status;
        self.build = status;
        self.review = status;
    }
}

impl Engine {
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().expect("engine state lock")
    }

    pub(crate) fn central(&self) -> MutexGuard<'_, Central> {
        self.inner.central.lock().expect("engine settings lock")
    }

    /// The engine's run state: which unit of work, if any, owns it.
    pub(crate) fn run_state(&self) -> RunState {
        self.state().run
    }

    pub fn is_running(&self) -> bool {
        self.run_state().is_build_session()
    }

    /// Whether a planner session is currently active.
    pub fn is_planning(&self) -> bool {
        self.run_state().is_planning()
    }

    /// Whether a discovery round is currently active.
    pub fn is_discovering(&self) -> bool {
        self.run_state().is_discovering()
    }

    /// Resolves once no build, no planning run and no discovery round is active.
    pub async fn wait_until_idle(&self) {
        let mut phase = self.inner.phase.subscribe();
        // The sender lives as long as the engine, so this cannot fail.
        let _ = phase.wait_for(|p| *p == Phase::Startup).await;
        loop {
            // Register both before checking so a finish in between is not missed.
            let planning = self.inner.planning_done.notified();
            tokio::pin!(planning);
            planning.as_mut().enable();
            let discovery = self.inner.discovery_done.notified();
            tokio::pin!(discovery);
            discovery.as_mut().enable();
            let (planning_active, discovery_active) = {
                let state = self.state();
                (state.run.is_planning(), state.run.is_discovering())
            };
            if !planning_active && !discovery_active {
                return;
            }
            tokio::select! {
                () = &mut planning => {}
                () = &mut discovery => {}
            }
        }
    }

    /// Whether the engine counts as idle for self-shutdown: no shell
    /// attached, no build running, no planning or discovery run, nothing pending.
    pub fn is_idle(&self) -> bool {
        let pending = taskfile::load(&self.inner.config.task_file())
            .is_ok_and(|tasks| task::next_pending(&tasks).is_some());
        let state = self.state();
        !state.attached && state.run == RunState::Idle && !pending
    }

    /// Adds `ids` to the session's UI-added task IDs (append-tasks flow).
    pub(crate) fn record_ui_added(&self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }
        let mut state = self.state();
        state.ui_added_ids.extend(ids.iter().cloned());
    }

    /// The IDs this engine appended through the append-tasks flow during the session, in the
    /// order they were added. Completing one of them postpones the next discovery round by
    /// the cooldown, checked where the task finishes in `run_task`.
    pub fn ui_added_task_ids(&self) -> Vec<String> {
        self.state().ui_added_ids.clone()
    }

    /// Whether `id` was appended through the UI's append-tasks flow this session.
    pub fn is_ui_added(&self, id: &str) -> bool {
        self.state().ui_added_ids.iter().any(|i| i == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_state_derives_the_wire_views() {
        // Planning and standalone discovery never touched the old phase flag,
        // so they keep the startup phase; the session's gap round reports a
        // running session and active discovery at once.
        let views = |run: RunState| {
            (
                run.phase(),
                run.is_planning(),
                run.is_discovering(),
                run.is_build_session(),
            )
        };
        assert_eq!(views(RunState::Idle), (Phase::Startup, false, false, false));
        assert_eq!(
            views(RunState::Running),
            (Phase::Running, false, false, true)
        );
        assert_eq!(
            views(RunState::Planning),
            (Phase::Startup, true, false, false)
        );
        assert_eq!(
            views(RunState::Discovering),
            (Phase::Startup, false, true, false)
        );
        assert_eq!(
            views(RunState::SessionDiscovery),
            (Phase::Running, false, true, true)
        );
    }

    #[test]
    fn state_transitions_walk_idle_running_and_idle_planning() {
        assert_eq!(RunState::Idle.build_started(), RunState::Running);
        assert_eq!(RunState::Running.build_ended(), RunState::Idle);
        assert_eq!(RunState::Idle.planning_started(), RunState::Planning);
        assert_eq!(RunState::Planning.planning_ended(), RunState::Idle);
    }

    #[test]
    fn a_discovery_round_returns_to_its_hosting_state() {
        // A standalone round leaves the engine idle.
        assert_eq!(RunState::Idle.discovery_started(), RunState::Discovering);
        assert_eq!(RunState::Discovering.discovery_ended(), RunState::Idle);
        // The session's gap round returns to the still-running session.
        assert_eq!(
            RunState::Running.gap_discovery_started(),
            RunState::SessionDiscovery
        );
        assert_eq!(
            RunState::SessionDiscovery.discovery_ended(),
            RunState::Running
        );
    }

    #[test]
    fn tiles_update_through_the_single_method() {
        let mut tiles = Tiles::default();
        assert_eq!(
            tiles,
            Tiles {
                research: TileStatus::Muted,
                plan: TileStatus::Muted,
                build: TileStatus::Muted,
                review: TileStatus::Muted,
            }
        );
        // `set` changes only its stage.
        tiles.set(Stage::Research, TileStatus::Active);
        assert_eq!(tiles.get(Stage::Research), TileStatus::Active);
        assert_eq!(tiles.get(Stage::Plan), TileStatus::Muted);
        assert_eq!(tiles.get(Stage::Build), TileStatus::Muted);
        assert_eq!(tiles.get(Stage::Review), TileStatus::Muted);
        tiles.set(Stage::Plan, TileStatus::Done);
        assert_eq!(tiles.get(Stage::Plan), TileStatus::Done);
        tiles.set(Stage::Build, TileStatus::Pending);
        assert_eq!(tiles.get(Stage::Build), TileStatus::Pending);
        tiles.set(Stage::Review, TileStatus::Active);
        assert_eq!(tiles.get(Stage::Review), TileStatus::Active);
        assert_eq!(tiles.research, TileStatus::Active);
        // `set_all` changes all four at once.
        tiles.set_all(TileStatus::Pending);
        assert_eq!(tiles.get(Stage::Research), TileStatus::Pending);
        assert_eq!(tiles.get(Stage::Plan), TileStatus::Pending);
        assert_eq!(tiles.get(Stage::Build), TileStatus::Pending);
        assert_eq!(tiles.get(Stage::Review), TileStatus::Pending);
        tiles.set_all(TileStatus::Muted);
        assert_eq!(tiles, Tiles::default());
    }
}
