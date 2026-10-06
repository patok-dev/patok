//! Engine state and the single-task build flow.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use patok_core::complexity::{self, Complexity};
use patok_core::config::{
    ApplyTiming, DaemonEnv, DaemonSettings, ProviderKind, ProviderName, RunMode, SettingValue,
    daemon_apply_timing, daemon_changed_fields, daemon_field_value, daemon_readout, load_daemon,
    set_daemon_field,
};
use patok_core::event::{
    AgentEvent, EngineEvent, NoticeLevel, Phase, SessionOutcome, Snapshot, TaskOutcome,
};
use patok_core::pipeline::{PipelineState, Stage, Tile, TileStatus};
use patok_core::task::{self, Task};
use patok_providers::{
    BUILDER_TOOLS, DISCOVERY_TOOLS, ExitKind, PLAN_TOOLS, PLANNER_TOOLS, Provider, ProviderError,
    RESEARCH_TOOLS, REVIEWER_TOOLS, SessionRequest, SessionResult,
};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::git::{CommitKind, Git};
use crate::schedule::DiscoverySchedule;
use crate::{TASK_FILE, plan, prompt, review, taskfile};

/// Events kept for replay to a reattaching shell.
const RECENT_LIMIT: usize = 2000;
/// How often the task file's modification time is checked.
const TASK_FILE_POLL: Duration = Duration::from_secs(2);
/// How often the config files' modification times are checked.
const CONFIG_FILE_POLL: Duration = Duration::from_secs(2);
/// How long a research report on disk stays reusable across restarts.
const RESEARCH_REUSE_SECS: u64 = 600;
const BROADCAST_CAPACITY: usize = 4096;
/// The largest git diff the reviewer prompt carries:
/// 50 KB; a larger or empty diff falls back to the changed-file list.
const DIFF_LIMIT_BYTES: usize = 50 * 1024;
/// The build claims prerequisite gate: a file under
/// this many bytes warns and the reviewer falls back to the plan and files.
const CLAIMS_MIN_BYTES: usize = 10;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub project_dir: PathBuf,
    /// Holds the socket and `engine.log`.
    pub runtime_dir: PathBuf,
    /// Persistent per-project directory (history log).
    pub data_dir: PathBuf,
    /// Idle timeout of one agent session.
    pub agent_timeout: Duration,
    /// How long an idle engine without a shell stays alive.
    pub idle_shutdown: Duration,
    /// The discovery cooldown: completing a UI-added task postpones
    /// the next round by it, and a round that adds nothing doubles it up to the cap.
    pub discovery_cooldown: Duration,
    /// The cap the discovery cooldown doubles up to.
    pub discovery_cooldown_cap: Duration,
    /// Whether a plan session runs before each builder session (`plan_enabled`, T10.1).
    pub plan_enabled: bool,
    /// Seeds the central settings on `Engine::new` exactly like `plan_enabled`;
    /// a configured engine reads the mode from the config files instead.
    /// The scheduled discovery round runs only in continuous mode (T103.1).
    pub run_mode: patok_core::config::RunMode,
    /// Whether a task the complexity classifier marks Simple skips the research
    /// stage (`skip_research_for_simple`, T68.1; default on).
    pub skip_research_for_simple: bool,
    /// Whether the review stage runs at all (`review_in_loop`; default on).
    pub review_in_loop: bool,
    /// The persistent learned-confidence history file, stored globally per user.
    pub review_history: PathBuf,
    /// The user-layer config files (user-global and user-local); the project-local
    /// `patok.config.toml` is always derived from `data_dir`.
    pub config_files: patok_core::config::ConfigFiles,
}

impl EngineConfig {
    pub const DEFAULT_AGENT_TIMEOUT: Duration = Duration::from_secs(600);
    pub const DEFAULT_IDLE_SHUTDOWN: Duration = Duration::from_secs(30 * 60);
    pub const DEFAULT_DISCOVERY_COOLDOWN: Duration =
        Duration::from_secs(patok_core::config::DEFAULT_DISCOVERY_COOLDOWN_SECS);
    pub const DEFAULT_DISCOVERY_COOLDOWN_CAP: Duration =
        Duration::from_secs(patok_core::config::DEFAULT_DISCOVERY_COOLDOWN_CAP_SECS);

    /// Resolves directories from the environment. `project_dir` must already be canonical.
    pub fn resolve(project_dir: PathBuf) -> anyhow::Result<Self> {
        let data_dir =
            patok_core::paths::project_data_dir_from_env(&project_dir).ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot determine the data directory: neither XDG_DATA_HOME nor HOME is set"
                )
            })?;
        let review_history = Self::default_review_history(&data_dir);
        Ok(Self {
            runtime_dir: patok_core::paths::runtime_dir_from_env(&project_dir),
            project_dir,
            data_dir,
            agent_timeout: Self::DEFAULT_AGENT_TIMEOUT,
            idle_shutdown: Self::DEFAULT_IDLE_SHUTDOWN,
            discovery_cooldown: Self::DEFAULT_DISCOVERY_COOLDOWN,
            discovery_cooldown_cap: Self::DEFAULT_DISCOVERY_COOLDOWN_CAP,
            plan_enabled: true,
            run_mode: RunMode::Sprint,
            skip_research_for_simple: true,
            review_in_loop: true,
            review_history,
            config_files: patok_core::config::ConfigFiles::from_env(),
        })
    }

    /// The learned-confidence history file: stored globally per user under the config directory,
    /// next to the config files; a missing config directory falls back to
    /// the project data directory.
    fn default_review_history(data_dir: &std::path::Path) -> PathBuf {
        let config = [
            std::env::var("XDG_CONFIG_HOME").ok(),
            std::env::var("HOME")
                .ok()
                .map(|home| format!("{home}/.config")),
        ]
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty());
        match config {
            Some(dir) => std::path::Path::new(&dir)
                .join("patok")
                .join("review-history.json"),
            None => data_dir.join("review-history.json"),
        }
    }

    pub fn task_file(&self) -> PathBuf {
        self.project_dir.join(TASK_FILE)
    }

    pub fn socket_path(&self) -> PathBuf {
        patok_core::paths::socket_path(&self.runtime_dir)
    }
}

struct State {
    phase: Phase,
    current_task: Option<String>,
    recent: VecDeque<EngineEvent>,
    attached: bool,
    /// A stop was requested: no new unit of work starts. Both scopes are
    /// released by [`Engine::end_stop`] once the loop has wound down; the
    /// process exits only for a NOW stop requested while idle (the plain
    /// quit), a signal, or the idle timeout.
    stopping: bool,
    /// A soft stop is pending: `stopping` was requested with `now` false and
    /// not yet released or cancelled. Distinguishes a cancellable soft stop
    /// from a NOW stop or the restart flow, which share only `stopping`.
    soft_stop: bool,
    /// An append-tasks planner session is active.
    planning: bool,
    /// A discovery session is active.
    discovering: bool,
    /// The in-memory task queue, kept in step with the task file by `reconcile_locked`.
    tasks: Vec<Task>,
    /// The IDs this engine appended through the append-tasks flow during the session.
    ui_added_ids: Vec<String>,
    /// The exact chunks appended through `inject_task` since the last
    /// append-style check drained them (T77.1): the append-style runs filter
    /// these out of their appended region before validating it, so a line
    /// injected mid-session neither rejects the run nor counts as its task,
    /// and a rejected run's restore re-appends them.
    injected: Vec<String>,
    /// The discovery cooldown schedule.
    schedule: DiscoverySchedule,
    /// Cancels the running agent ("stop now").
    cancel: CancellationToken,
    /// The pipeline rail as last broadcast, with the
    /// stage list the engine runs today; recomputed by `refresh_pipeline_locked`.
    pipeline: PipelineState,
    /// The plan stage tile's status within the running task.
    plan: TileStatus,
    /// The research stage tile's status within the running task (T68.1).
    research: TileStatus,
    /// The build stage tile's status within the running task.
    build: TileStatus,
    /// The review stage tile's status within the running task (T70.1).
    review: TileStatus,
    /// The ship tile's status: active while the engine commits a task.
    ship: TileStatus,
    /// The leading task-ID number of the current batch-review group: a contiguous run of same-numbered tasks.
    group_number: Option<String>,
    /// The HEAD SHA captured when the current batch-review group started, so
    /// the group's last review diffs across every commit of the group.
    group_base: Option<String>,
    /// A discovery round ran to completion this session.
    discovery_ran: bool,
    /// A learning was learned this session; recorded through
    /// [`Engine::record_learning_learned`], the post-review extractor's future
    /// hook -- learning extraction is not implemented yet.
    learning_learned: bool,
}

/// The central in-memory configuration: the merged, normalized
/// daemon settings plus the display fields derived from them, guarded together so every
/// reader sees a consistent pair. Every consumer reads through it; a unit of work
/// snapshots what it relies on at its start.
struct Central {
    settings: DaemonSettings,
    /// Name of the active provider, for display: the fixed provider's slug on engines
    /// built with [`Engine::new`], the configured provider's name otherwise.
    provider_name: String,
    /// The configured model override, for display.
    model: Option<String>,
}

/// Resolves a provider kind at the start of each unit of work.
type Resolver = Arc<dyn Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

/// One append-style run's validator for [`Engine::check_appended`]: the
/// `before` text, the `after` text and the chunks injected during the run,
/// returning the appended IDs or the rejection reason.
type AppendValidator = fn(&str, &str, &[String]) -> Result<Vec<String>, String>;

struct Inner {
    config: EngineConfig,
    /// The selected provider, or why none could be set up (reported by the commands).
    /// On engines with a resolver this is the startup resolution, kept for error
    /// reporting; each unit of work re-resolves from the central settings instead.
    provider: Result<Arc<dyn Provider>, String>,
    /// The environment overrides the central settings are reloaded with.
    env: DaemonEnv,
    /// Re-resolves the provider at each unit-of-work start; `None` on engines with a
    /// fixed provider.
    resolver: Option<Resolver>,
    central: Mutex<Central>,
    state: Mutex<State>,
    events: broadcast::Sender<EngineEvent>,
    phase: watch::Sender<Phase>,
    /// Notified when a planning run ends.
    planning_done: Notify,
    /// Notified when a discovery round ends.
    discovery_done: Notify,
    /// Notified when a pending soft stop is cancelled, so the shutdown
    /// handshake's stream can end instead of lingering.
    soft_stop_cancelled: Notify,
    /// Serialises the engine's short atomic task-file operations (the startup
    /// cleanup, the progress write, the inject append and the append-style
    /// runs' read/validate/restore checks); it is never held across an agent
    /// session, so an inject is never blocked by one (T77.1).
    task_file_lock: Arc<tokio::sync::Mutex<()>>,
    /// Cancelled when the engine process should exit.
    shutdown: CancellationToken,
}

/// Cheaply cloneable handle on the engine's state.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

/// Held by the one controlling shell; dropping it frees the attach slot.
pub struct Attachment {
    pub snapshot: Snapshot,
    pub events: broadcast::Receiver<EngineEvent>,
    engine: Engine,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.engine.state().attached = false;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("another shell is already attached to the engine for {0}")]
    AlreadyAttached(String),
}

/// The outcome of one task's plan stage (T10.1).
#[derive(Debug)]
enum PlanStage {
    /// The plan gate accepted this plan text.
    Accepted(String),
    /// The plan session was cancelled.
    Cancelled,
    /// The plan could not be produced, or was rejected twice; the reason was reported.
    Failed,
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

impl QueueCreation {
    /// The run's research report stem: its report is saved as
    /// `research/<stem>-<timestamp>.md` and reused within the research
    /// reuse window by stem.
    fn stem(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Scan => "scan",
        }
    }
}

/// The outcome of one task's research stage (T68.1).
#[derive(Debug)]
enum ResearchStage {
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

/// One unit of work's snapshot of the central settings: taken at the unit's start -- a build-loop start (covering its whole
/// session), an add-tasks planner run, a discovery round -- and relied on for the unit's
/// duration, so a settings change mid-run takes effect at the next such start.
#[derive(Clone)]
struct Unit {
    provider: Arc<dyn Provider>,
    model: Option<String>,
    agent_timeout: Duration,
    plan_enabled: bool,
    /// The run mode (`run_mode`), snapshotted at the unit's start exactly like
    /// the other fields: the scheduled discovery round runs only when it says
    /// continuous (T103.1), and a mode change takes effect at the next unit.
    run_mode: RunMode,
    skip_research_for_simple: bool,
    /// The review stage's knobs, snapshot at the
    /// unit's start exactly like the other fields.
    review_in_loop: bool,
    skip_review_for_simple: bool,
    batch_review: bool,
    review_confidence_threshold: u64,
    review_multipass_threshold: u64,
    confidence_threshold: f64,
    reviewer_provider: ProviderName,
    /// The reviewer model the review stage's reviewer session runs with
    /// (the `reviewer_model` daemon key), snapshot at the unit's start exactly
    /// like the other fields.
    reviewer_model: String,
    /// Whether the review stage is enabled in the pipeline stage list (an
    /// absent entry is disabled).
    review_stage_enabled: bool,
}

/// One agent session's prompt bundle: the user prompt, the system prompt, the transcript
/// label and the tool allowlist.
struct AgentPrompt {
    prompt: String,
    system_prompt: &'static str,
    label: &'static str,
    tools: &'static [&'static str],
}

/// The outcome of one task's review stage.
#[derive(Debug)]
enum ReviewStage {
    /// The review passed: the task is validated.
    Passed,
    /// The review failed: a stage failure with suggestions, leading to WIP.
    Failed,
    /// A reviewer session was cancelled.
    Cancelled,
    /// The stage was skipped by one of the spec's skip rules; the reason was
    /// recorded with the skip notice, and any skip treats the task as
    /// validated.
    Skipped,
}

/// The outcome of the reviewer sessions of one review.
enum ReviewRun {
    /// The final review report.
    Report(String),
    /// The reviewer session failed: a missing report counts as one HIGH finding.
    Failed,
    /// A reviewer session was cancelled.
    Cancelled,
}

/// Merges `findings` into `merged`, de-duplicated by file and issue.
fn merge_findings(merged: &mut review::Findings, findings: review::Findings) {
    for severity in ["high", "medium", "low"] {
        let bucket = match severity {
            "high" => &mut merged.high,
            "medium" => &mut merged.medium,
            _ => &mut merged.low,
        };
        for finding in findings.bucket(severity) {
            if !bucket
                .iter()
                .any(|existing| existing.file == finding.file && existing.issue == finding.issue)
            {
                bucket.push(finding.clone());
            }
        }
    }
}

impl Engine {
    /// An engine on a fixed provider. The config files are ignored for routing, but daemon
    /// settings are still persisted and reloaded from them, so the settings flow behaves
    /// exactly as on a configured engine.
    pub fn new(config: EngineConfig, provider: Arc<dyn Provider>) -> Self {
        let name = provider.slug().to_string();
        // Seed the central settings from the given config so existing behaviour is kept
        // bit-identical; a config reload replaces them with the merged file values.
        let settings = DaemonSettings {
            agent_timeout_secs: config.agent_timeout.as_secs(),
            engine_idle_timeout_secs: config.idle_shutdown.as_secs(),
            discovery_cooldown_secs: config.discovery_cooldown.as_secs(),
            discovery_cooldown_cap_secs: config.discovery_cooldown_cap.as_secs(),
            plan_enabled: config.plan_enabled,
            run_mode: config.run_mode,
            skip_research_for_simple: config.skip_research_for_simple,
            review_in_loop: config.review_in_loop,
            ..DaemonSettings::default()
        };
        let central = Central {
            settings,
            provider_name: name,
            model: None,
        };
        Self::build(config, Ok(provider), central, None, DaemonEnv::default())
    }

    /// An engine whose provider and model come from the config layers, with the daemon
    /// schema's environment overrides read from the process environment. A failure there
    /// does not stop the engine: it is reported as the error of every build or planner
    /// command.
    pub fn configured<F>(config: EngineConfig, resolve: F) -> Self
    where
        F: Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync + 'static,
    {
        Self::configured_with_env(config, &DaemonEnv::from_process_env(), resolve)
    }

    /// [`Engine::configured`] with explicit environment overrides (testable).
    pub fn configured_with_env<F>(config: EngineConfig, env: &DaemonEnv, resolve: F) -> Self
    where
        F: Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync + 'static,
    {
        // Load the daemon schema only (the tui table belongs to the shell); it becomes
        // the engine's central in-memory config, which every consumer reads through and
        // each unit of work snapshots from.
        let loaded = load_daemon(
            &config.config_files,
            &config.project_dir,
            &config.data_dir,
            env,
        );
        for warning in &loaded.warnings {
            tracing::warn!("{warning}");
        }
        for error in &loaded.errors {
            tracing::error!("{error}");
        }
        let settings = loaded.settings;
        let name = settings.provider.to_string();
        let model = settings.model.clone();
        let central = Central {
            settings,
            provider_name: name.clone(),
            model,
        };
        // An unrecognized provider value refuses runs.
        // The startup resolution is kept for error reporting; every unit of work
        // re-resolves from the central settings instead.
        let provider = match loaded.errors.first() {
            Some(error) => Err(error.clone()),
            None => match central.settings.provider.kind() {
                Some(kind) => resolve(kind).map_err(|error| {
                    tracing::error!("provider {name} unavailable: {error}");
                    error.to_string()
                }),
                None => Err(format!(
                    "the `{name}` provider is not available in this build; use claude, codex or mistral"
                )),
            },
        };
        Self::build(
            config,
            provider,
            central,
            Some(Arc::new(resolve)),
            env.clone(),
        )
    }

    fn build(
        config: EngineConfig,
        provider: Result<Arc<dyn Provider>, String>,
        central: Central,
        resolver: Option<Resolver>,
        env: DaemonEnv,
    ) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let (phase, _) = watch::channel(Phase::Startup);
        let schedule = DiscoverySchedule::new(
            Duration::from_secs(central.settings.discovery_cooldown_secs),
            Duration::from_secs(central.settings.discovery_cooldown_cap_secs),
            Instant::now(),
        );
        Self {
            inner: Arc::new(Inner {
                config,
                provider,
                env,
                resolver,
                central: Mutex::new(central),
                state: Mutex::new(State {
                    phase: Phase::Startup,
                    current_task: None,
                    recent: VecDeque::new(),
                    attached: false,
                    stopping: false,
                    soft_stop: false,
                    planning: false,
                    discovering: false,
                    tasks: Vec::new(),
                    ui_added_ids: Vec::new(),
                    injected: Vec::new(),
                    schedule,
                    cancel: CancellationToken::new(),
                    pipeline: PipelineState::today(),
                    research: TileStatus::Muted,
                    plan: TileStatus::Muted,
                    build: TileStatus::Muted,
                    review: TileStatus::Muted,
                    ship: TileStatus::Muted,
                    group_number: None,
                    group_base: None,
                    discovery_ran: false,
                    learning_learned: false,
                }),
                events,
                phase,
                planning_done: Notify::new(),
                discovery_done: Notify::new(),
                soft_stop_cancelled: Notify::new(),
                task_file_lock: Arc::new(tokio::sync::Mutex::new(())),
                shutdown: CancellationToken::new(),
            }),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.inner.config
    }

    /// Cancelled when the engine process should exit.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.inner.shutdown.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().expect("engine state lock")
    }

    /// Records the event for replay and pushes it to the attached shell, atomically with
    /// respect to [`Engine::attach`] so a snapshot never misses or duplicates an event.
    fn emit_locked(&self, state: &mut State, event: EngineEvent) {
        match (&event, state.recent.back_mut()) {
            (
                EngineEvent::Agent {
                    event: AgentEvent::TextDelta { text },
                },
                Some(EngineEvent::Agent {
                    event: AgentEvent::TextDelta { text: last },
                }),
            ) => last.push_str(text),
            _ => {
                if state.recent.len() == RECENT_LIMIT {
                    state.recent.pop_front();
                }
                state.recent.push_back(event.clone());
            }
        }
        // No receiver is normal: nobody is attached.
        let _ = self.inner.events.send(event);
    }

    fn emit(&self, event: EngineEvent) {
        let mut state = self.state();
        self.emit_locked(&mut state, event);
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(EngineEvent::Notice {
            level,
            text: text.into(),
        });
    }

    /// Announces `agent` as the engine's active agent, carrying the session's
    /// engine-recorded start (T42.1), and returns the instant the session's
    /// duration is measured from.
    fn announce_session(&self, agent: &str, provider: &str, model: Option<&str>) -> Instant {
        let mut state = self.state();
        self.announce_session_locked(&mut state, agent, provider, model)
    }

    /// [`Engine::announce_session`] under an already-held state lock: announces
    /// `agent` as the active agent before a run's state flag flips, so the
    /// shell can label the run from the moment it starts. The starting line
    /// (T78.1) names the provider and model the run itself uses, so a settings
    /// change mid-run cannot mislabel a later run.
    fn announce_session_locked(
        &self,
        state: &mut State,
        agent: &str,
        provider: &str,
        model: Option<&str>,
    ) -> Instant {
        self.emit_locked(
            state,
            EngineEvent::AgentStarted {
                agent: agent.to_string(),
                provider: provider.to_string(),
                model: model.map(str::to_string),
            },
        );
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.emit_locked(
            state,
            EngineEvent::AgentChanged {
                agent: agent.to_string(),
                started_ms,
            },
        );
        Instant::now()
    }

    /// Pushes one agent session's end to the attached shells and the replayed
    /// recent events: the agent type name, how it ended and the total duration
    /// measured from `start` (T42.1).
    fn finish_session(&self, agent: &str, start: Instant, outcome: SessionOutcome) {
        self.emit(EngineEvent::AgentFinished {
            agent: agent.to_string(),
            outcome,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    /// The finished line's outcome of one agent session.
    fn session_outcome(session: &Result<SessionResult, ProviderError>) -> SessionOutcome {
        match session {
            Err(_) => SessionOutcome::Failed,
            Ok(result) => match result.exit {
                ExitKind::Completed => SessionOutcome::Finished,
                ExitKind::Cancelled => SessionOutcome::Cancelled,
                ExitKind::Failed | ExitKind::TimedOut => SessionOutcome::Failed,
            },
        }
    }

    fn set_phase(&self, state: &mut State, phase: Phase) {
        state.phase = phase;
        self.inner.phase.send_replace(phase);
        self.emit_locked(state, EngineEvent::PhaseChanged { phase });
    }

    /// Re-reads the task file and merges it into the queue; broadcasts the new list when it
    /// changed. Returns whether it did.
    fn reconcile_locked(&self, state: &mut State) -> bool {
        let Ok(file) = taskfile::load(&self.inner.config.task_file()) else {
            return false;
        };
        let merged = taskfile::reconcile(&state.tasks, file, state.current_task.as_deref());
        if merged == state.tasks {
            return false;
        }
        state.tasks = merged.clone();
        self.emit_locked(state, EngineEvent::TasksChanged { tasks: merged });
        true
    }

    fn reconcile(&self) -> bool {
        let mut state = self.state();
        self.reconcile_locked(&mut state)
    }

    /// Recomputes the pipeline rail from the
    /// per-task and session inputs and, when it changed, swaps it in and
    /// broadcasts it. The run mode is read from the central settings at the
    /// point of use. Every transition calls this one helper, so the broadcast,
    /// the snapshot and the replayed recent events can never disagree.
    fn refresh_pipeline_locked(&self, state: &mut State) {
        let sprint = self.central().settings.run_mode == RunMode::Sprint;
        let discover = if sprint {
            TileStatus::Muted
        } else if state.discovering {
            TileStatus::Active
        } else if state.discovery_ran {
            TileStatus::Done
        } else {
            TileStatus::Muted
        };
        let pipeline = PipelineState {
            stages: state
                .pipeline
                .stages
                .iter()
                .map(|tile| Tile {
                    stage: tile.stage,
                    status: match tile.stage {
                        Stage::Research => state.research,
                        Stage::Plan => state.plan,
                        Stage::Build => state.build,
                        Stage::Review => state.review,
                    },
                })
                .collect(),
            ship: state.ship,
            discover,
            learnings: if state.learning_learned {
                TileStatus::Done
            } else {
                TileStatus::Muted
            },
        };
        if pipeline != state.pipeline {
            state.pipeline = pipeline.clone();
            self.emit_locked(state, EngineEvent::PipelineChanged { state: pipeline });
        }
    }

    /// A task's tiles start over: both stages pending, ship muted (the statuses
    /// reset per task).
    fn task_tiles_started_locked(&self, state: &mut State) {
        state.research = TileStatus::Pending;
        state.plan = TileStatus::Pending;
        state.build = TileStatus::Pending;
        state.review = TileStatus::Pending;
        state.ship = TileStatus::Muted;
        self.refresh_pipeline_locked(state);
    }

    /// A task ended: its stage and ship tiles are muted again, whatever the
    /// outcome.
    fn task_tiles_finished_locked(&self, state: &mut State) {
        state.research = TileStatus::Muted;
        state.plan = TileStatus::Muted;
        state.build = TileStatus::Muted;
        state.review = TileStatus::Muted;
        state.ship = TileStatus::Muted;
        self.refresh_pipeline_locked(state);
    }

    /// The startup cleanup (T72.1): prunes old completed task lines from the
    /// task file, adopts the cleaned file as the queue and broadcasts the
    /// change. Called once from `serve`, before the task-file watcher starts,
    /// never during the session.
    pub async fn cleanup_completed_tasks(&self) {
        let path = self.inner.config.task_file();
        let guard = self.inner.task_file_lock.clone().lock_owned().await;
        let cleaned = match taskfile::cleanup_completed(&path) {
            Ok(cleaned) => cleaned,
            Err(error) => {
                tracing::warn!("startup cleanup of {}: {error}", path.display());
                return;
            }
        };
        drop(guard);
        if !cleaned {
            return;
        }
        // The queue starts over from the cleaned file. A plain reconcile would
        // keep the pruned completed tasks in memory (reconcile protects done
        // tasks that vanish from the file), undoing the cleanup in the queue.
        let mut state = self.state();
        if let Ok(file) = taskfile::load(&path) {
            let merged = taskfile::reconcile(&[], file, None);
            if merged != state.tasks {
                state.tasks = merged.clone();
                self.emit_locked(&mut state, EngineEvent::TasksChanged { tasks: merged });
            }
        }
    }

    /// Polls the task file's modification time and reconciles the queue when it changes.
    /// Runs until the engine shuts down.
    pub fn spawn_task_file_watch(&self) {
        let engine = self.clone();
        tokio::spawn(async move {
            let path = engine.inner.config.task_file();
            let mtime = || std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let mut last = mtime();
            engine.reconcile();
            let mut tick = tokio::time::interval(TASK_FILE_POLL);
            loop {
                tokio::select! {
                    () = engine.inner.shutdown.cancelled() => return,
                    _ = tick.tick() => {}
                }
                let now = mtime();
                if now != last {
                    last = now;
                    engine.reconcile();
                }
            }
        });
    }

    /// The central in-memory daemon settings: every consumer
    /// reads through this rather than holding an independent copy.
    pub fn settings(&self) -> DaemonSettings {
        self.central().settings.clone()
    }

    fn central(&self) -> MutexGuard<'_, Central> {
        self.inner.central.lock().expect("engine settings lock")
    }

    /// The unit-of-work snapshot the starting commands validate and run with: the fixed
    /// provider on `Engine::new` engines, else the central settings re-resolved through
    /// the resolver. Errors are user-facing rejection reasons, exactly like a startup
    /// resolution failure.
    fn unit(&self) -> Result<Unit, String> {
        let central = self.central();
        let provider = match self.inner.resolver.as_ref() {
            Some(resolver) => {
                // The startup load's hard errors (an unrecognized provider value) refuse
                // runs; a healthy config re-resolves from the current settings.
                if let Err(error) = self.inner.provider.as_ref() {
                    return Err(error.clone());
                }
                let name = central.settings.provider.to_string();
                match central.settings.provider.kind() {
                    Some(kind) => resolver(kind).map_err(|error| {
                        tracing::error!("provider {name} unavailable: {error}");
                        error.to_string()
                    })?,
                    None => {
                        return Err(format!(
                            "the `{name}` provider is not available in this build; use claude, codex or mistral"
                        ));
                    }
                }
            }
            None => self.inner.provider.clone()?,
        };
        Ok(Unit {
            provider,
            model: central.model.clone(),
            agent_timeout: Duration::from_secs(central.settings.agent_timeout_secs),
            plan_enabled: central.settings.plan_enabled,
            run_mode: central.settings.run_mode,
            skip_research_for_simple: central.settings.skip_research_for_simple,
            review_in_loop: central.settings.review_in_loop,
            skip_review_for_simple: central.settings.skip_review_for_simple,
            batch_review: central.settings.batch_review,
            review_confidence_threshold: central.settings.review_confidence_threshold,
            review_multipass_threshold: central.settings.review_multipass_threshold,
            confidence_threshold: central.settings.confidence_threshold,
            reviewer_provider: central.settings.reviewer_provider,
            reviewer_model: central.settings.reviewer_model.clone(),
            review_stage_enabled: central
                .settings
                .stages
                .iter()
                .find(|stage| stage.id == "review")
                .is_some_and(|stage| stage.enabled),
        })
    }

    /// Applies one settings-overlay change to a daemon-schema field: validated exactly as an on-disk field, persisted to the resolved layer
    /// (`patok.config.toml` in the project's slot of the projects dir), re-merged into
    /// the central config and broadcast back field
    /// by field. Returns when the change takes effect; errors are user-facing rejection
    /// reasons.
    pub fn apply_settings_change(
        &self,
        field: &str,
        value: SettingValue,
    ) -> Result<ApplyTiming, String> {
        let mut warnings = Vec::new();
        set_daemon_field(&self.inner.config.data_dir, field, value, &mut warnings)
            .map_err(|error| error.message)?;
        for warning in &warnings {
            tracing::warn!("{warning}");
        }
        let timing = daemon_apply_timing(field).unwrap_or(ApplyTiming::NextUnitOfWork);
        self.reload_config();
        Ok(timing)
    }

    /// Re-merges the config files and the environment into the central settings and
    /// swaps the result in: the same merge and normalization as at startup, field by
    /// field, with the same fallback warnings. Used after the engine persists a change
    /// and when a file changes on disk.
    fn reload_config(&self) {
        let loaded = load_daemon(
            &self.inner.config.config_files,
            &self.inner.config.project_dir,
            &self.inner.config.data_dir,
            &self.inner.env,
        );
        self.swap_settings(loaded.settings, &loaded.warnings);
    }

    /// Swaps `new` in as the central settings: logs the reload `warnings`, refreshes the
    /// display fields, reconfigures the discovery schedule and broadcasts one
    /// `ConfigChanged` per changed field, carrying the field's effective (post-merge,
    /// post-normalization) value and apply timing. An empty
    /// diff emits nothing.
    fn swap_settings(&self, new: DaemonSettings, warnings: &[String]) {
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        let changed = {
            let mut central = self.central();
            let old = std::mem::replace(&mut central.settings, new.clone());
            if self.inner.resolver.is_some() {
                central.provider_name = new.provider.to_string();
                central.model = new.model.clone();
            }
            daemon_changed_fields(&old, &new)
        };
        let mut state = self.state();
        state.schedule.reconfigure(
            Duration::from_secs(new.discovery_cooldown_secs),
            Duration::from_secs(new.discovery_cooldown_cap_secs),
        );
        // A sprint <-> continuous change re-mutes or unmutes the DISCOVER tile
        // live, so the rail follows the central mode; the running loop still
        // honors the mode from its session snapshot and picks the change up at
        // the next build-loop start.
        let run_mode_changed = changed.iter().any(|field| field == "run_mode");
        for field in changed {
            let Some(value) = daemon_field_value(&new, &field) else {
                continue;
            };
            let timing = daemon_apply_timing(&field).unwrap_or(ApplyTiming::NextUnitOfWork);
            self.emit_locked(
                &mut state,
                EngineEvent::ConfigChanged {
                    field,
                    value,
                    timing,
                },
            );
        }
        if run_mode_changed {
            self.refresh_pipeline_locked(&mut state);
        }
    }

    /// Polls the config files' modification times and reloads the central settings when
    /// one changes. Runs until the engine
    /// shuts down.
    pub fn spawn_config_watch(&self) {
        let engine = self.clone();
        tokio::spawn(async move {
            let paths: Vec<PathBuf> = [
                engine.inner.config.config_files.user_global.clone(),
                engine.inner.config.config_files.user_local.clone(),
                Some(patok_core::config::project_file(
                    &engine.inner.config.data_dir,
                )),
            ]
            .into_iter()
            .flatten()
            .collect();
            let mtimes = || -> Vec<_> {
                paths
                    .iter()
                    .map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
                    .collect()
            };
            let mut last = mtimes();
            let mut tick = tokio::time::interval(CONFIG_FILE_POLL);
            loop {
                tokio::select! {
                    () = engine.inner.shutdown.cancelled() => return,
                    _ = tick.tick() => {}
                }
                let now = mtimes();
                if now != last {
                    last = now;
                    engine.reload_config();
                }
            }
        });
    }

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
            phase: state.phase,
            tasks: state.tasks.clone(),
            current_task: state.current_task.clone(),
            planning: state.planning,
            discovering: state.discovering,
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
        if state.stopping {
            return Err("the engine is shutting down".into());
        }
        if state.phase == Phase::Running {
            return Err("a build is already running".into());
        }
        if state.planning {
            return Err("tasks are being planned; wait for the planner to finish".into());
        }
        if state.discovering {
            return Err("a discovery round is running; wait for it to finish".into());
        }
        let task = task::next_pending(&tasks)
            .ok_or_else(|| format!("no pending tasks in {TASK_FILE}"))?
            .clone();
        let cancel = CancellationToken::new();
        state.cancel = cancel.clone();
        // A new session starts a new batch-review group: the same leading number recurring after a different task ran
        // starts a fresh group with a fresh base.
        state.group_number = None;
        state.group_base = None;
        state.current_task = Some(task.id.clone());
        self.set_phase(&mut state, Phase::Running);
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
    async fn run_session(&self, first: Task, cancel: CancellationToken, unit: Unit) {
        if self.run_task(first, &unit, cancel.clone()).await != TaskOutcome::Done {
            self.end_session();
            return;
        }
        loop {
            while !self.state().stopping {
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
            if self.state().stopping
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
        self.set_phase(&mut state, Phase::Startup);
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
            state.discovering = true;
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
    fn record_round(&self, added: Option<usize>) {
        if let Some(added) = added {
            let mut state = self.state();
            state.schedule.round_finished(Instant::now(), added);
        }
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
        if state.stopping {
            return Err("the engine is shutting down".into());
        }
        if state.phase == Phase::Running {
            return Err("a build is running; wait for it to finish".into());
        }
        if state.planning {
            return Err("the planner is already running".into());
        }
        if state.discovering {
            return Err("a discovery round is running; wait for it to finish".into());
        }
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
            "planner",
            unit.provider.slug(),
            unit.model.as_deref(),
        );
        state.planning = true;
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
        if state.stopping {
            return Err("the engine is shutting down".into());
        }
        if state.phase == Phase::Running {
            return Err("a build is running; wait for it to finish".into());
        }
        if state.planning {
            return Err("tasks are being planned; wait for the planner to finish".into());
        }
        if state.discovering {
            return Err("a discovery round is running; wait for it to finish".into());
        }
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
            "research",
            unit.provider.slug(),
            unit.model.as_deref(),
        );
        state.planning = true;
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
        if state.stopping {
            return Err("the engine is shutting down".into());
        }
        if state.phase == Phase::Running {
            return Err("a build is running; wait for it to finish".into());
        }
        if state.planning {
            return Err("tasks are being planned; wait for the planner to finish".into());
        }
        if state.discovering {
            return Err("a discovery round is running; wait for it to finish".into());
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
        state.discovering = true;
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

    /// Requests a stop: no new unit of work starts. A running task finishes first
    /// unless `now`, which kills the agent. Neither scope quits the process: a
    /// soft stop (`now` false) and a NOW stop on a busy engine are both
    /// interrupt-stops -- the engine returns to its idle state and keeps serving
    /// once [`Engine::end_stop`] releases the stop. The process exits only for a
    /// NOW stop requested while idle (the plain quit), a signal, or the idle
    /// timeout, which cancel the shutdown token directly.
    pub fn request_stop(&self, now: bool) {
        let mut state = self.state();
        state.stopping = true;
        state.soft_stop = !now;
        if now {
            state.cancel.cancel();
        }
    }

    /// Releases a stop once the loop has wound down (the running task finished
    /// or was cancelled, and nothing new started): new units of work --
    /// StartBuild, AddTasks, RunDiscovery -- are accepted again. Both scopes
    /// end here; only the shutdown handshake calls this. `stopping` stays set
    /// until the engine has actually gone idle, otherwise a command could
    /// sneak a new run in mid-wind-down.
    pub fn end_stop(&self) {
        let mut state = self.state();
        state.stopping = false;
        state.soft_stop = false;
    }

    /// Notified when a pending soft stop is cancelled; the shutdown handshake
    /// selects on this so its stream ends with a cancelled update instead of
    /// lingering until the whole build finishes.
    pub fn soft_stop_cancelled(&self) -> &Notify {
        &self.inner.soft_stop_cancelled
    }

    /// Cancels a pending soft stop: the build loop continues with the next
    /// pending task once the running task finishes, and the engine returns to
    /// normal RUNNING operation. The state change is broadcast to attached
    /// shells as an info notice. Errors are user-facing command errors, for
    /// the `CancelSoftStop` command with no soft stop pending.
    pub fn cancel_soft_stop(&self) -> Result<(), String> {
        let mut state = self.state();
        if !state.stopping || !state.soft_stop {
            return Err("no soft stop is pending".into());
        }
        state.stopping = false;
        state.soft_stop = false;
        self.emit_locked(
            &mut state,
            EngineEvent::Notice {
                level: NoticeLevel::Info,
                text: "Soft stop cancelled -- the build loop continues with the next task.".into(),
            },
        );
        drop(state);
        self.inner.soft_stop_cancelled.notify_waiters();
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.state().phase == Phase::Running
    }

    /// Whether a planner session is currently active.
    pub fn is_planning(&self) -> bool {
        self.state().planning
    }

    /// Whether a discovery round is currently active.
    pub fn is_discovering(&self) -> bool {
        self.state().discovering
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
                (state.planning, state.discovering)
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
        !state.attached
            && state.phase == Phase::Startup
            && !state.planning
            && !state.discovering
            && !pending
    }

    /// Adds `ids` to the session's UI-added task IDs (append-tasks flow).
    fn record_ui_added(&self, ids: &[String]) {
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

    /// Records that a learning was learned this session, so the pipeline rail's
    /// LEARNINGS tile turns done. This is the hook
    /// the post-review learning extractor will call once it exists; the engine
    /// owns the flag because the session is its lifetime.
    pub fn record_learning_learned(&self) {
        let mut state = self.state();
        state.learning_learned = true;
        self.refresh_pipeline_locked(&mut state);
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
                label: agent.label.into(),
                idle_timeout: unit.agent_timeout,
                cancel,
                allowed_tools: agent.tools.iter().map(ToString::to_string).collect(),
            })
            .await;
        // The request (and its sender) is gone: let the forwarder flush the tail of the stream.
        let _ = forwarder.await;
        let captured = recorded.lock().expect("capture lock").drain(..).collect();
        (session, captured)
    }

    /// One planner session. `before` is the task file as it was before the run;
    /// `start` is the moment the run was announced in `start_add_tasks`. The run
    /// uses the settings snapshot taken when it was started. The task-file lock
    /// is free for the whole session and re-acquired only for the post-session
    /// check, so an inject during the session is neither blocked nor lost
    /// (T77.1).
    async fn run_planner(
        &self,
        request: String,
        before: String,
        cancel: CancellationToken,
        unit: Unit,
        start: Instant,
    ) {
        // The append-tasks planner is the active agent for the whole run (T11.1), so the
        // shell's output frame shows the planner title while it streams.
        let (session, _) = self
            .run_agent_session(
                AgentPrompt {
                    prompt: prompt::planner(&request),
                    system_prompt: prompt::PLANNER_SYSTEM,
                    label: "planner",
                    tools: PLANNER_TOOLS,
                },
                cancel,
                false,
                &unit,
            )
            .await;
        self.finish_session("planner", start, Self::session_outcome(&session));

        match session {
            Err(error) => {
                tracing::error!("planner could not start: {error}");
                self.notice(NoticeLevel::Error, error.to_string());
            }
            Ok(result) if result.exit == ExitKind::Completed => {
                self.check_planner_result(&before).await;
            }
            Ok(result) if result.exit == ExitKind::Cancelled => {
                self.notice(NoticeLevel::Warning, "Planner cancelled.");
            }
            Ok(result) => {
                let reason = result.failure.unwrap_or_else(|| "planner failed".into());
                self.notice(NoticeLevel::Error, format!("planner: {reason}"));
            }
        }

        {
            let mut state = self.state();
            state.planning = false;
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
    async fn run_queue_creation(
        &self,
        kind: QueueCreation,
        before: String,
        cancel: CancellationToken,
        unit: Unit,
        start: Instant,
    ) {
        // The research agent is the active agent for the whole run (T11.1), so
        // the shell's output frame shows the research title while it streams.
        let (session, events) = self
            .run_agent_session(
                AgentPrompt {
                    prompt: match kind {
                        QueueCreation::Bootstrap => prompt::research_queue_bootstrap(),
                        QueueCreation::Scan => prompt::research_queue_scan(),
                    },
                    system_prompt: prompt::RESEARCH_QUEUE_SYSTEM,
                    label: "research",
                    tools: RESEARCH_TOOLS,
                },
                cancel,
                true,
                &unit,
            )
            .await;
        self.finish_session("research", start, Self::session_outcome(&session));

        match session {
            Err(error) => {
                tracing::error!("research agent could not start: {error}");
                self.notice(NoticeLevel::Error, error.to_string());
            }
            Ok(result) if result.exit == ExitKind::Completed => {
                // The appended tasks pass the existing append-tasks validation,
                // count as UI-added like the planner's, and reconcile the queue;
                // only a pass saves the research report, so a rejected run
                // re-runs rather than being covered by the reuse window.
                if self
                    .check_appended(
                        &before,
                        "research",
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
            Ok(result) if result.exit == ExitKind::Cancelled => {
                self.notice(NoticeLevel::Warning, "Research cancelled.");
            }
            Ok(result) => {
                let reason = result.failure.unwrap_or_else(|| "research failed".into());
                self.notice(NoticeLevel::Error, format!("research: {reason}"));
            }
        }

        {
            let mut state = self.state();
            state.planning = false;
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
    async fn run_discovery(
        &self,
        before: String,
        cancel: CancellationToken,
        unit: &Unit,
    ) -> Option<usize> {
        // The discovery agent is the active agent for the whole round (T11.1), so the
        // shell's output frame shows its title while it streams.
        let start = self.announce_session("discovery", unit.provider.slug(), unit.model.as_deref());
        let (session, _) = self
            .run_agent_session(
                AgentPrompt {
                    prompt: prompt::discovery(),
                    system_prompt: prompt::DISCOVERY_SYSTEM,
                    label: "discovery",
                    tools: DISCOVERY_TOOLS,
                },
                cancel,
                false,
                unit,
            )
            .await;
        self.finish_session("discovery", start, Self::session_outcome(&session));

        let added = match session {
            Err(error) => {
                tracing::error!("discovery could not start: {error}");
                self.notice(NoticeLevel::Error, error.to_string());
                None
            }
            Ok(result) if result.exit == ExitKind::Completed => {
                Some(self.check_discovery_result(&before).await.len())
            }
            Ok(result) if result.exit == ExitKind::Cancelled => {
                self.notice(NoticeLevel::Warning, "Discovery cancelled.");
                None
            }
            Ok(result) => {
                let reason = result.failure.unwrap_or_else(|| "discovery failed".into());
                self.notice(NoticeLevel::Error, format!("discovery: {reason}"));
                None
            }
        };

        {
            let mut state = self.state();
            state.discovering = false;
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
            .check_appended(before, "planner", taskfile::validate_append_ignoring, true)
            .await;
    }

    /// Validates the task file after a completed discovery session: like the planner check,
    /// but the appended tasks must carry `D`-prefixed IDs and are not recorded as UI-added
    /// task IDs. Returns the IDs the round appended.
    async fn check_discovery_result(&self, before: &str) -> Vec<String> {
        self.check_appended(
            before,
            "discovery",
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
        role: &str,
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
        // The batch-review group of this task is noted first: a new leading number starts a group and
        // captures the current HEAD as the base its last review diffs across.
        self.note_group(&task.id).await;

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
        {
            let mut state = self.state();
            state.research = TileStatus::Done;
            state.plan = TileStatus::Done;
            state.build = TileStatus::Active;
            self.refresh_pipeline_locked(&mut state);
        }
        let session_start =
            self.announce_session("builder", unit.provider.slug(), unit.model.as_deref());

        let inner = &self.inner;
        let (events, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        // The builder's final message carries the build claims section the review
        // stage reviews, so the events are
        // recorded like the plan stage's.
        let recorded = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
        let recorder = recorded.clone();
        let forwarder = {
            let engine = self.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    recorder.lock().expect("capture lock").push(event.clone());
                    engine.emit(EngineEvent::Agent { event });
                }
            })
        };

        let provider = unit.provider.clone();
        let request = SessionRequest {
            model: unit.model.clone(),
            prompt: prompt::builder(&task, plan.as_deref(), research.as_deref()),
            system_prompt: Some(prompt::SYSTEM.to_string()),
            project_dir: inner.config.project_dir.clone(),
            events,
            log_dir: inner.config.data_dir.join("history"),
            label: task.id.clone(),
            idle_timeout: unit.agent_timeout,
            cancel: cancel.clone(),
            allowed_tools: BUILDER_TOOLS.iter().map(ToString::to_string).collect(),
        };
        let session = provider.run_session(request).await;
        // The request (and its sender) is gone: let the forwarder flush the tail of the stream.
        let _ = forwarder.await;
        let builder_events: Vec<AgentEvent> =
            recorded.lock().expect("capture lock").drain(..).collect();
        // One finished line lands after the agent output and before the task's own
        // outcome line (T42.1).
        self.finish_session("builder", session_start, Self::session_outcome(&session));

        let (outcome, commit) = match session {
            Err(error) => {
                tracing::error!("agent could not start for {}: {error}", task.id);
                self.notice(NoticeLevel::Error, error.to_string());
                let commit = self.finalize(&task, &progress(false, false), false).await;
                (TaskOutcome::Failed, commit)
            }
            Ok(result) if result.exit == ExitKind::Completed => {
                // The builder succeeded: its final message's build claims section
                // becomes the claims artifact, with the
                // verification-results trimming applied.
                let final_text = plan::captured_text(&builder_events);
                if let Some(claims) = review::claims_section(&final_text) {
                    self.write_claims_file(&task.id, &review::trim_claims(claims));
                }
                // The build tile is done once the builder session completed: the
                // stages before the running one are always done, so review and ship never render with the build
                // still in-progress.
                {
                    let mut state = self.state();
                    state.build = TileStatus::Done;
                    self.refresh_pipeline_locked(&mut state);
                }
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
            Ok(result) if result.exit == ExitKind::Cancelled => {
                self.notice(NoticeLevel::Warning, "Agent cancelled; nothing committed.");
                (TaskOutcome::Cancelled, None)
            }
            Ok(result) => {
                let reason = result.failure.unwrap_or_else(|| "agent failed".into());
                self.notice(NoticeLevel::Error, format!("{}: {reason}", task.id));
                let commit = self.finalize(&task, &progress(false, false), false).await;
                (TaskOutcome::Failed, commit)
            }
        };

        self.finish_task(&task, outcome, commit)
    }

    /// Records the task's batch-review group: a contiguous run of same-leading-number task IDs in execution
    /// order. A changed number starts a new group and captures the current
    /// HEAD as its base, so the group's last review diffs across every commit
    /// made for the group. The group state is reset at each session start.
    async fn note_group(&self, task_id: &str) {
        let number = review::leading_number(task_id);
        {
            let state = self.state();
            if state.group_number.is_some() && state.group_number == number {
                return;
            }
        }
        let base = self.git().head_sha().await;
        let mut state = self.state();
        state.group_number = number;
        state.group_base = base;
    }

    /// Writes the builder's build claims to the project data directory as
    /// `claims/<task id>-<timestamp>.md`, next to
    /// the plans and research folders.
    fn write_claims_file(&self, task_id: &str, claims: &str) {
        let dir = self.inner.config.data_dir.join("claims");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(format!("{task_id}-{timestamp}.md"));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, claims));
        match written {
            Ok(()) => self.notice(
                NoticeLevel::Info,
                format!("Build claims written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {e}", path.display()),
            ),
        }
    }

    /// The newest build claims of `task_id` on disk, when it exists and passes
    /// the prerequisite gate of at least 10 bytes;
    /// the gate is a warning only, and the reviewer prompt falls back to the
    /// plan and the changed-file list.
    fn claims_file(&self, task_id: &str) -> Option<String> {
        let dir = self.inner.config.data_dir.join("claims");
        let newest = std::fs::read_dir(&dir).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| {
                    name.strip_prefix(&format!("{task_id}-"))
                        .is_some_and(|rest| rest.strip_suffix(".md").is_some())
                })
                .max()
        });
        let found = newest.map(|name| dir.join(name)).and_then(|path| {
            std::fs::read_to_string(&path)
                .ok()
                .map(|claims| (path, claims))
        });
        let Some((_path, claims)) = found else {
            // The gate is a warning only: the reviewer proceeds with the plan
            // and the changed files as the fallback.
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "no build claims found for {task_id}; the reviewer falls back to the plan and the changed files"
                ),
            );
            return None;
        };
        if claims.len() < CLAIMS_MIN_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "the build claims for {task_id} are under {CLAIMS_MIN_BYTES} bytes; the reviewer falls back to the plan and the changed files"
                ),
            );
            return None;
        }
        Some(claims)
    }

    /// The review stage: decides by the skip
    /// rules in precedence order -- each recorded with its reason and every
    /// skip treating the task as validated -- and, when it runs, drives the
    /// fresh-context reviewer sessions that review and fix in one pass, then
    /// computes the verdict. No crash-recovery checkpoint is written for the
    /// stage.
    async fn review_stage(
        &self,
        task: &Task,
        unit: &Unit,
        cancel: &CancellationToken,
        plan: Option<&str>,
    ) -> ReviewStage {
        let complexity = complexity::classify(&task.description);
        // The skip decision, recorded with its reason.
        let pending_after: Vec<String> = taskfile::load(&self.inner.config.task_file())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.done && !t.is_malformed())
            .map(|t| t.id)
            .collect();
        let history = review::ReviewHistory::load(&self.inner.config.review_history);
        let inputs = review::SkipInputs {
            review_in_loop: unit.review_in_loop,
            batch_deferred: unit.batch_review && review::batch_deferred(&task.id, &pending_after),
            skip_review_for_simple: unit.skip_review_for_simple,
            complexity: Some(complexity),
            builder_clean: true,
            review_confidence_threshold: unit.review_confidence_threshold,
            history_consecutive_passes: history.match_shape(&task.description, complexity),
            stage_enabled: unit.review_stage_enabled,
        };
        if let Some(reason) = review::skip_review(&inputs) {
            tracing::info!("review skipped for {}: {}", task.id, reason.reason());
            self.notice(
                NoticeLevel::Info,
                format!("Review skipped for {}: {}.", task.id, reason.reason()),
            );
            // A skipped stage renders done on the rail like the research and
            // plan stages do; the dash lives in the task-line indicator.
            let mut state = self.state();
            state.review = TileStatus::Done;
            self.refresh_pipeline_locked(&mut state);
            return ReviewStage::Skipped;
        }

        // The claims prerequisite gate: a warning only.
        let claims = self.claims_file(&task.id);

        // The changed files: from the working tree,
        // and against the batch group's base so the group's last review spans
        // every commit of the group. No files changed fails the review.
        let base = self.state().group_base.clone();
        let git = self.git();
        let changed = git.changed_files(base.as_deref()).await;
        if changed.is_empty() {
            self.notice(
                NoticeLevel::Error,
                format!("{}: no changed files to review", task.id),
            );
            {
                let mut state = self.state();
                state.review = TileStatus::Done;
                self.refresh_pipeline_locked(&mut state);
            }
            return ReviewStage::Failed;
        }
        // The reviewer receives the diff when it is non-empty and at most
        // 50 KB, otherwise the changed-file list.
        let diff = git.diff(base.as_deref()).await;
        let diff = (!diff.trim().is_empty() && diff.len() <= DIFF_LIMIT_BYTES).then_some(diff);

        // The review tile is active while the reviewer sessions run (T49.1).
        {
            let mut state = self.state();
            state.review = TileStatus::Active;
            self.refresh_pipeline_locked(&mut state);
        }

        // Applied fixes are inferred from per-file content hashes taken before
        // and after the reviewer, over the same changed-file set.
        let before = self.file_hashes(&changed);
        // Generated files (snapshots, lockfiles) are no unit for a per-file
        // pass: they neither trigger multipass nor get a reviewer of their own,
        // so a snapshot-heavy change does not run dozens of per-file sessions.
        let reviewable: Vec<String> = changed
            .iter()
            .filter(|file| !review::is_generated(file))
            .cloned()
            .collect();
        let run = if unit.review_multipass_threshold > 0
            && reviewable.len() as u64 > unit.review_multipass_threshold
        {
            self.multipass_review(task, unit, cancel, &reviewable, &changed)
                .await
        } else {
            let prompt =
                prompt::reviewer(task, claims.as_deref(), plan, &changed, diff.as_deref(), 1);
            self.one_review(task, unit, cancel, &prompt, "reviewer")
                .await
        };
        let after = self.file_hashes(&changed);
        let applied: Vec<String> = changed
            .iter()
            .zip(before.iter().zip(after.iter()))
            .filter(|(_, (was, now))| was != now)
            .map(|(file, _)| file.clone())
            .collect();
        if applied.is_empty() {
            tracing::info!("the reviewer applied no fixes for {}", task.id);
        } else {
            tracing::info!(
                "the reviewer applied fixes in {} for {}: {}",
                applied.len(),
                task.id,
                applied.join(", ")
            );
        }

        let result = match run {
            ReviewRun::Cancelled => {
                self.notice(NoticeLevel::Warning, "Review cancelled; nothing committed.");
                {
                    let mut state = self.state();
                    state.review = TileStatus::Done;
                    self.refresh_pipeline_locked(&mut state);
                }
                return ReviewStage::Cancelled;
            }
            ReviewRun::Report(report) => (report, true),
            ReviewRun::Failed => {
                // The reviewer agent must succeed: a failed session counts as
                // a missing report (one HIGH finding).
                tracing::error!("the reviewer session failed for {}", task.id);
                (String::new(), false)
            }
        };

        // The verdict.
        let report = result.0;
        let verdict = review::compute_verdict(
            result.1.then_some(report.as_str()),
            unit.confidence_threshold,
        );
        for below in &verdict.below_threshold {
            tracing::warn!(
                "below-confidence {} finding for {} (confidence {:.2}): {}",
                below.severity,
                task.id,
                below.finding.confidence,
                below.finding.issue
            );
        }
        let review_path = self.write_review_file(&task.id, &report);
        // The pass/fail outcome is recorded in the persistent learned-confidence
        // history in the background.
        self.record_review_outcome(task, complexity, verdict.passed);

        {
            let mut state = self.state();
            state.review = TileStatus::Done;
            self.refresh_pipeline_locked(&mut state);
        }
        if verdict.passed {
            self.notice(NoticeLevel::Info, format!("Review passed for {}.", task.id));
            ReviewStage::Passed
        } else {
            let report = review_path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "the review report".into());
            self.notice(
                NoticeLevel::Error,
                format!(
                    "review failed for {}: HIGH/MEDIUM issues remain, check report at {}",
                    task.id, report
                ),
            );
            ReviewStage::Failed
        }
    }

    /// The reviewer session the review runs on when the reviewer provider names
    /// a provider this build implements (`reviewer_provider`): that provider,
    /// resolved like any other unit of work. The claude provider also runs
    /// with the configured reviewer model (`reviewer_model`); the other
    /// providers keep the provider's default model.
    fn reviewer_unit(&self, unit: &Unit) -> Unit {
        let mut reviewer = unit.clone();
        if let Some(kind) = reviewer.reviewer_provider.kind() {
            let name = reviewer.reviewer_provider;
            match self.inner.resolver.as_ref() {
                Some(resolver) => match resolver(kind) {
                    Ok(provider) => {
                        reviewer.provider = provider;
                        reviewer.model = if kind == ProviderKind::Claude {
                            Some(reviewer.reviewer_model.clone())
                        } else {
                            None
                        };
                    }
                    Err(error) => {
                        tracing::warn!(
                            "the {name} reviewer provider is unavailable ({error}); \
                             the reviewer runs on the configured provider"
                        );
                    }
                },
                None => {
                    tracing::warn!(
                        "the {name} reviewer provider has no resolver; \
                         the reviewer runs on the fixed provider"
                    );
                }
            }
        }
        reviewer
    }

    /// One reviewer session: announced as the active agent (T11.1), recorded
    /// with the reviewer allowlist, its raw stream written to the history log,
    /// and its captured final message taken as the review report.
    async fn one_review(
        &self,
        task: &Task,
        unit: &Unit,
        cancel: &CancellationToken,
        prompt: &str,
        agent: &str,
    ) -> ReviewRun {
        // The starting line names the provider the review actually runs on
        // (mistral when the reviewer provider is mistral), so the unit resolves first
        // (T78.1).
        let reviewer_unit = self.reviewer_unit(unit);
        let start = self.announce_session(
            agent,
            reviewer_unit.provider.slug(),
            reviewer_unit.model.as_deref(),
        );
        let (session, events) = self
            .run_agent_session(
                AgentPrompt {
                    prompt: prompt.to_string(),
                    system_prompt: prompt::REVIEWER_SYSTEM,
                    label: "review",
                    tools: REVIEWER_TOOLS,
                },
                cancel.clone(),
                true,
                &reviewer_unit,
            )
            .await;
        self.finish_session(agent, start, Self::session_outcome(&session));
        match session {
            Err(error) => {
                tracing::error!("reviewer could not start for {}: {error}", task.id);
                self.notice(NoticeLevel::Error, error.to_string());
                ReviewRun::Failed
            }
            Ok(result) if result.exit == ExitKind::Completed => {
                ReviewRun::Report(plan::captured_text(&events))
            }
            Ok(result) if result.exit == ExitKind::Cancelled => ReviewRun::Cancelled,
            Ok(result) => {
                let reason = result.failure.unwrap_or_else(|| "review failed".into());
                self.notice(
                    NoticeLevel::Error,
                    format!("review for {}: {reason}", task.id),
                );
                ReviewRun::Failed
            }
        }
    }

    /// The multipass review: one report-only reviewer
    /// session per changed file (a failed file review is skipped, the loop
    /// continues), then one integration reviewer session over the merged,
    /// de-duplicated findings that produces the final report.
    async fn multipass_review(
        &self,
        task: &Task,
        unit: &Unit,
        cancel: &CancellationToken,
        reviewable: &[String],
        changed: &[String],
    ) -> ReviewRun {
        let mut merged = review::Findings::default();
        for file in reviewable {
            let prompt = prompt::reviewer_file(task, file, 1);
            let run = self
                .one_review(task, unit, cancel, &prompt, "reviewer")
                .await;
            match run {
                ReviewRun::Cancelled => return ReviewRun::Cancelled,
                ReviewRun::Failed => {
                    // A failed file review is skipped and the loop continues.
                    continue;
                }
                ReviewRun::Report(report) => {
                    if let Some(findings) = review::parse_report(&report).findings {
                        merge_findings(&mut merged, findings);
                    }
                }
            }
        }
        let findings_json = serde_json::to_string_pretty(&merged).unwrap_or_default();
        let prompt = prompt::reviewer_integration(task, &findings_json, changed, 2);
        self.one_review(task, unit, cancel, &prompt, "reviewer")
            .await
    }

    /// The SHA-256 content hashes of `files` (missing files hash to none):
    /// taken before and after the reviewer, they infer the applied fixes.
    fn file_hashes(&self, files: &[String]) -> Vec<Option<String>> {
        use sha2::{Digest, Sha256};
        files
            .iter()
            .map(|file| {
                std::fs::read(self.inner.config.project_dir.join(file))
                    .ok()
                    .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
            })
            .collect()
    }

    /// Writes the review report to the project data directory as
    /// `reviews/<task id>-<timestamp>.md`, returning
    /// its path for the failure notice.
    fn write_review_file(&self, task_id: &str, report: &str) -> Option<std::path::PathBuf> {
        let dir = self.inner.config.data_dir.join("reviews");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(format!("{task_id}-{timestamp}.md"));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, report));
        match written {
            Ok(()) => {
                self.notice(
                    NoticeLevel::Info,
                    format!("Review report written to {}", path.display()),
                );
                Some(path)
            }
            Err(e) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("cannot write {}: {e}", path.display()),
                );
                None
            }
        }
    }

    /// Records one review outcome in the persistent learned-confidence history
    /// in the background, for the
    /// simple and medium shapes the feature matches on.
    fn record_review_outcome(&self, task: &Task, complexity: Complexity, passed: bool) {
        if complexity == Complexity::Complex {
            return;
        }
        let description = task.description.clone();
        let path = self.inner.config.review_history.clone();
        let date = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            .to_string();
        tokio::spawn(async move {
            let mut history = review::ReviewHistory::load(&path);
            history.record(&description, complexity, passed, &date);
            if let Err(error) = history.save(&path) {
                tracing::warn!(
                    "cannot write the review history {}: {error}",
                    path.display()
                );
            }
        });
    }

    /// Commit finalization: the five-position
    /// progress token is written into the task line under the task-file lock
    /// as the last mutation before the commit, the task is marked done only
    /// when validated, and the commit is `feat` or `WIP` accordingly. The
    /// builder-failure and review-failure paths finalize the same way with
    /// their own token and `validated` false. No crash-recovery checkpoint is
    /// written for the review stage.
    async fn finalize(&self, task: &Task, token: &str, validated: bool) -> Option<String> {
        let path = self.inner.config.task_file();
        {
            let _guard = self.inner.task_file_lock.lock().await;
            match taskfile::write_progress(&path, task, token, validated) {
                Ok(true) => {}
                Ok(false) => self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "{} was not found as an unchecked task; not ticked.",
                        task.id
                    ),
                ),
                Err(e) => self.notice(
                    NoticeLevel::Error,
                    format!("cannot update {}: {e}", path.display()),
                ),
            }
        }
        self.reconcile();
        self.ship_committing();
        let kind = if validated {
            CommitKind::Feat
        } else {
            CommitKind::Wip
        };
        let commit = self
            .git()
            .commit_all(kind, &task.id, &task.description)
            .await;
        self.report_commit(commit.as_deref());
        commit
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

    /// The per-task research stage (T68.1): one research
    /// session with the research allowlist runs before the task is planned. A
    /// report on disk younger than ten minutes is reused without a session
    /// (across restarts); a Simple-classified task skips the stage when the
    /// `skip_research_for_simple` key is on. A failed session is non-blocking
    /// (the pipeline continues without a report); a cancelled one ends the
    /// task. The shell is told the research agent is active for the whole
    /// stage (T11.1).
    async fn research_stage(
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
        {
            let mut state = self.state();
            state.research = TileStatus::Active;
            self.refresh_pipeline_locked(&mut state);
        }
        let start = self.announce_session("research", unit.provider.slug(), unit.model.as_deref());
        let (session, events) = self
            .run_agent_session(
                AgentPrompt {
                    prompt: prompt::research(task),
                    system_prompt: prompt::RESEARCH_SYSTEM,
                    label: "research",
                    tools: RESEARCH_TOOLS,
                },
                cancel.clone(),
                true,
                unit,
            )
            .await;
        self.finish_session("research", start, Self::session_outcome(&session));
        let result = match session {
            Err(error) => {
                tracing::error!("research agent could not start for {}: {error}", task.id);
                self.notice(NoticeLevel::Error, error.to_string());
                return ResearchStage::Failed;
            }
            Ok(result) => result,
        };
        match result.exit {
            ExitKind::Completed => {}
            ExitKind::Cancelled => {
                self.notice(
                    NoticeLevel::Warning,
                    "Research cancelled; nothing committed.",
                );
                return ResearchStage::Cancelled;
            }
            _ => {
                let reason = result.failure.unwrap_or_else(|| "research failed".into());
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "research for {}: {reason}; continuing without a report",
                        task.id
                    ),
                );
                return ResearchStage::Failed;
            }
        }
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

    /// The newest research report of `stem` on disk whose filename timestamp
    /// is younger than the reuse window, or `None`.
    /// The stem is a task ID for the per-task research stage and `bootstrap` /
    /// `scan` for the queue-creation runs (T69.1).
    fn reusable_research_report(&self, stem: &str) -> Option<String> {
        let dir = self.inner.config.data_dir.join("research");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let newest = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let timestamp = name
                    .strip_prefix(&format!("{stem}-"))
                    .and_then(|rest| rest.strip_suffix(".md"))
                    .and_then(|secs| secs.parse::<u64>().ok())?;
                Some((timestamp, name))
            })
            .filter(|(timestamp, _)| now.saturating_sub(*timestamp) < RESEARCH_REUSE_SECS)
            .max_by_key(|(timestamp, _)| *timestamp)?;
        std::fs::read_to_string(dir.join(newest.1)).ok()
    }

    /// Writes the research report to the project data directory as
    /// `research/<stem>-<timestamp>.md` (T68.1; the queue-creation runs use
    /// their kind's stem, T69.1).
    fn write_research_report(&self, stem: &str, report: &str) {
        let dir = self.inner.config.data_dir.join("research");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(format!("{stem}-{timestamp}.md"));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, report));
        match written {
            Ok(()) => self.notice(
                NoticeLevel::Info,
                format!("Research report written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {e}", path.display()),
            ),
        }
    }

    /// The per-task plan stage (T10.1): one plan session with the read-only allowlist,
    /// whose captured plan must pass the plan gate. On rejection the plan is retried once
    /// with a prompt carrying the rejected plan and the reason; a plan rejected twice
    /// fails the task. The shell is told the planner is the active agent for the whole
    /// stage (T11.1). `research` is the prior research report, when one exists (T68.1).
    async fn run_plan_stage(
        &self,
        task: &Task,
        cancel: &CancellationToken,
        unit: &Unit,
        research: Option<&str>,
    ) -> PlanStage {
        // The plan session is the running stage: its tile is active with the
        // build tile still pending.
        {
            let mut state = self.state();
            state.research = TileStatus::Done;
            state.plan = TileStatus::Active;
            self.refresh_pipeline_locked(&mut state);
        }
        let mut prompt_text = prompt::plan(task, research);
        for attempt in 1..=2 {
            // Every attempt is one planner session with its own timing, so a retried
            // plan leaves its own finished line (T42.1) and starting line (T78.1).
            let start =
                self.announce_session("planner", unit.provider.slug(), unit.model.as_deref());
            let (session, events) = self
                .run_agent_session(
                    AgentPrompt {
                        prompt: prompt_text,
                        system_prompt: prompt::PLAN_SYSTEM,
                        label: "plan",
                        tools: PLAN_TOOLS,
                    },
                    cancel.clone(),
                    true,
                    unit,
                )
                .await;
            self.finish_session("planner", start, Self::session_outcome(&session));
            let result = match session {
                Err(error) => {
                    tracing::error!("plan agent could not start for {}: {error}", task.id);
                    self.notice(NoticeLevel::Error, error.to_string());
                    return PlanStage::Failed;
                }
                Ok(result) => result,
            };
            match result.exit {
                ExitKind::Completed => {}
                ExitKind::Cancelled => {
                    self.notice(NoticeLevel::Warning, "Plan cancelled; nothing committed.");
                    return PlanStage::Cancelled;
                }
                _ => {
                    let reason = result.failure.unwrap_or_else(|| "plan failed".into());
                    self.notice(
                        NoticeLevel::Error,
                        format!("plan for {}: {reason}", task.id),
                    );
                    return PlanStage::Failed;
                }
            }
            let text = plan::captured_text(&events);
            match plan::gate_plan(&text) {
                Ok(()) => return PlanStage::Accepted(text),
                Err(reason) if attempt == 2 => {
                    self.notice(
                        NoticeLevel::Error,
                        format!(
                            "{}: the plan was rejected twice ({reason}); the task was not built",
                            task.id
                        ),
                    );
                    return PlanStage::Failed;
                }
                Err(reason) => {
                    prompt_text = prompt::plan_retry(task, &text, &reason, research);
                }
            }
        }
        unreachable!("the plan stage returns within its two attempts")
    }

    /// Writes the accepted plan to the project data directory as
    /// `plans/<task id>-<timestamp>.md` (T10.1).
    fn write_plan_file(&self, task_id: &str, plan: &str) {
        let dir = self.inner.config.data_dir.join("plans");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(format!("{task_id}-{timestamp}.md"));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, plan));
        match written {
            Ok(()) => self.notice(
                NoticeLevel::Info,
                format!("Plan written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {e}", path.display()),
            ),
        }
    }

    /// The ship tile turns active while the engine commits a task, around both commit sites -- the `feat` commit of a completed task
    /// and the `WIP` commit on the failure path. `finish_task` mutes it again.
    fn ship_committing(&self) {
        let mut state = self.state();
        state.ship = TileStatus::Active;
        self.refresh_pipeline_locked(&mut state);
    }

    fn report_commit(&self, commit: Option<&str>) {
        match commit {
            Some(sha) => self.notice(NoticeLevel::Info, format!("Committed {sha}")),
            None => self.notice(
                NoticeLevel::Info,
                "Not committed (not a git repository, nothing to commit, or the commit failed).",
            ),
        }
    }

    fn git(&self) -> Git {
        Git::new(&self.inner.config.project_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipass_findings_merge_and_de_duplicate_by_file_and_issue() {
        let mut merged = review::Findings::default();
        let finding = |file: &str, issue: &str| review::Finding {
            file: file.into(),
            issue: issue.into(),
            confidence: 0.9,
            ..review::Finding::default()
        };
        for (file, issue) in [
            ("a.rs", "a leak"),
            ("a.rs", "a leak"),
            ("a.rs", "another"),
            ("b.rs", "a leak"),
        ] {
            let mut findings = review::Findings::default();
            findings.high.push(finding(file, issue));
            merge_findings(&mut merged, findings);
        }
        assert_eq!(merged.high.len(), 3, "{merged:?}");
        assert_eq!(merged.medium.len(), 0);
        assert!(
            merged
                .high
                .iter()
                .any(|f| f.file == "a.rs" && f.issue == "another")
        );
        // The same finding in a different severity bucket is its own entry.
        let mut findings = review::Findings::default();
        findings.medium.push(finding("a.rs", "a leak"));
        merge_findings(&mut merged, findings);
        assert_eq!(merged.medium.len(), 1);
    }
}
