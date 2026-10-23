use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use patok_config::{DaemonEnv, DaemonSettings, EnumSpelling};
use patok_core::event::{EngineEvent, Phase, ProviderCatalog};
use patok_core::pipeline::{PipelineState, Stage, TileStatus};
use patok_providers::{ModelCache, Provider, ProviderError, ProviderKind, ProviderName};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::config::EngineConfig;
use super::stop::Stop;
use crate::schedule::DiscoverySchedule;
use crate::task_list::TaskList;

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
    /// The current task's directory path for history logging.
    pub(crate) current_task_dir: Option<PathBuf>,
    pub(crate) recent: VecDeque<EngineEvent>,
    pub(crate) attach_count: usize,
    /// The pending stop, if any.
    pub(crate) stop: Stop,
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
    /// The model discovery state per provider, keyed by the canonical slug. Only
    /// providers that were asked have an entry; no entry means not discovered.
    pub(crate) model_catalog: BTreeMap<String, ProviderCatalog>,
    /// The model cache the daemon started with, reused by discoveries that a settings
    /// change starts later.
    pub(crate) model_cache: Option<ModelCache>,
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

/// The implemented providers as the rejection message lists them: every
/// `ProviderKind` spelling, comma-joined with "or" before the last.
fn implemented_providers() -> String {
    match ProviderKind::SPELLINGS {
        [] => String::new(),
        [only] => (*only).to_string(),
        [init @ .., last] => format!("{} or {}", init.join(", "), last),
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
            "the `{name}` provider is not available in this build; use {}",
            implemented_providers()
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
    /// The engine's task list: the task-file path, the in-memory queue, the
    /// async file lock that serialises short atomic file operations (the
    /// startup cleanup, the progress write, the inject append and the
    /// append-style runs' read/validate/restore checks; it is never held
    /// across an agent session, so an inject is never blocked by one,
    /// T77.1), the injected chunks and the UI-added task IDs.
    pub(crate) task_list: Arc<TaskList>,
    /// Cancelled when the engine process should exit.
    pub(crate) shutdown: CancellationToken,
    /// Sender for history messages to the HistoryWriter.
    pub(crate) history_sender: Option<mpsc::UnboundedSender<crate::history::HistoryMsg>>,
    /// Receiver for history messages, wrapped in Arc<Mutex> so it can be taken once.
    pub(crate) history_receiver:
        Arc<Mutex<Option<mpsc::UnboundedReceiver<crate::history::HistoryMsg>>>>,
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

    /// One checked transition, the path every state change goes through: a
    /// source matching `expected` moves to `target`; any other source -- a
    /// transition called out of order -- is logged as a warning naming the
    /// expected and actual state and left unchanged, so the engine keeps the
    /// state it truly had instead of moving to the transition's target. The
    /// `debug_assert_eq!` in each transition method still panics first in
    /// debug builds; in release it is compiled out and the mismatched source
    /// reaches this helper.
    fn checked(self, expected: Self, target: Self) -> Self {
        if self == expected {
            return target;
        }
        tracing::warn!(
            "out-of-order run-state transition: expected {expected:?}, found {self:?}; the state is left unchanged"
        );
        self
    }

    /// The build session starts. The guards of `start_build` guarantee the
    /// idle source.
    pub(crate) fn build_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        self.checked(Self::Idle, Self::Running)
    }

    /// The build session ends. The session's end in `crate::session`
    /// guarantees the source.
    pub(crate) fn build_ended(self) -> Self {
        debug_assert_eq!(self, Self::Running);
        self.checked(Self::Running, Self::Idle)
    }

    /// A planner or queue-creation run starts. The `start_*` guards
    /// guarantee the idle source.
    pub(crate) fn planning_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        self.checked(Self::Idle, Self::Planning)
    }

    /// A planner or queue-creation run ends; the engine is idle again.
    pub(crate) fn planning_ended(self) -> Self {
        debug_assert_eq!(self, Self::Planning);
        self.checked(Self::Planning, Self::Idle)
    }

    /// A standalone discovery round starts. The guards of `start_discovery`
    /// guarantee the idle source.
    pub(crate) fn discovery_started(self) -> Self {
        debug_assert_eq!(self, Self::Idle);
        self.checked(Self::Idle, Self::Discovering)
    }

    /// The session's scheduled round starts in the empty-queue gap; the
    /// phase stays running.
    pub(crate) fn gap_discovery_started(self) -> Self {
        debug_assert_eq!(self, Self::Running);
        self.checked(Self::Running, Self::SessionDiscovery)
    }

    /// A discovery round ends: a standalone one leaves the engine idle, and
    /// the session's gap round returns to the still-running session.
    pub(crate) fn discovery_ended(self) -> Self {
        if self == Self::SessionDiscovery {
            return self.checked(Self::SessionDiscovery, Self::Running);
        }
        debug_assert_eq!(self, Self::Discovering);
        self.checked(Self::Discovering, Self::Idle)
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

    /// Whether the engine counts as idle for self-shutdown: no (non-observer) shell
    /// attached, no build running, no planning or discovery run, nothing pending.
    pub fn is_idle(&self) -> bool {
        let pending = self
            .inner
            .task_list
            .next_pending()
            .is_ok_and(|t| t.is_some());
        let state = self.state();
        state.attach_count == 0 && state.run == RunState::Idle && !pending
    }

    /// The IDs this engine appended through the append-tasks flow during the session, in the
    /// order they were added. Completing one of them postpones the next discovery round by
    /// the cooldown, checked where the task finishes in `crate::session`.
    pub fn ui_added_task_ids(&self) -> Vec<String> {
        self.inner.task_list.ui_added_ids()
    }

    /// Whether `id` was appended through the UI's append-tasks flow this session.
    pub fn is_ui_added(&self, id: &str) -> bool {
        self.inner.task_list.is_ui_added(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rejection_list_names_every_implemented_kind() {
        assert_eq!(implemented_providers(), "claude, codex or vibe");
    }

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
    fn an_out_of_order_transition_is_warned_about_and_kept() {
        // In release the methods' debug_assert_eq! is compiled out, so a
        // mismatched source reaches `checked`, the helper every transition
        // routes through: it leaves the state unchanged instead of moving
        // to the target -- a false Idle from build_ended would let a start
        // through and strand the session.
        assert_eq!(
            RunState::Discovering.checked(RunState::Running, RunState::Idle),
            RunState::Discovering
        );
        assert_eq!(
            RunState::Idle.checked(RunState::Running, RunState::SessionDiscovery),
            RunState::Idle
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "assertion")]
    fn an_out_of_order_transition_still_panics_in_debug() {
        let _ = RunState::Discovering.build_ended();
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
