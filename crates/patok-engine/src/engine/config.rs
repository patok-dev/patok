use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use patok_core::config::{DaemonEnv, DaemonSettings, ProviderKind, RunMode, load_daemon};
use patok_core::event::Phase;
use patok_core::pipeline::PipelineState;
use patok_providers::{Provider, ProviderError};
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::state::{Central, Inner, Resolver, RunState, State, Tiles, resolve_or_reject};
use super::stop::Stop;
use crate::TASK_FILE;
use crate::schedule::DiscoverySchedule;
use crate::task_list::TaskList;

const BROADCAST_CAPACITY: usize = 4096;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub project_dir: PathBuf,
    /// Holds the socket and `engine.log`.
    pub runtime_dir: PathBuf,
    /// Persistent per-project directory: the default history root, `open-group.json` and the project-local config.
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
    /// The persistent learned-confidence store file (`review-history.json`), stored globally per user.
    pub review_confidence: PathBuf,
    /// The open-group record (`open-group.json`), stored in the project data
    /// directory: the batch-review group open across restarts.
    pub open_group: PathBuf,
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
        let review_confidence = Self::default_review_confidence(&data_dir);
        let open_group = data_dir.join("open-group.json");
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
            review_confidence,
            open_group,
            config_files: patok_core::config::ConfigFiles::from_env(),
        })
    }

    /// The learned-confidence store file: stored globally per user under the config directory,
    /// next to the config files; a missing config directory falls back to
    /// the project data directory.
    fn default_review_confidence(data_dir: &std::path::Path) -> PathBuf {
        patok_core::paths::config_dir_from_env()
            .map(|dir| dir.join("review-history.json"))
            .unwrap_or_else(|| data_dir.join("review-history.json"))
    }

    pub fn task_file(&self) -> PathBuf {
        self.project_dir.join(TASK_FILE)
    }

    pub fn socket_path(&self) -> PathBuf {
        patok_core::paths::socket_path(&self.runtime_dir)
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
        let name = settings.builder_provider.to_string();
        let model = Some(settings.builder_model.clone());
        let central = Central {
            settings,
            provider_name: name.clone(),
            model,
        };
        // An unrecognized provider value refuses runs.
        // The startup resolution is kept for error reporting; every unit of work
        // re-resolves from the central settings instead.
        let resolver: Resolver = Arc::new(resolve);
        let provider = match loaded.errors.first() {
            Some(error) => Err(error.clone()),
            None => resolve_or_reject(&resolver, &central.settings.builder_provider),
        };
        Self::build(config, provider, central, Some(resolver), env.clone())
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
        let task_list = Arc::new(TaskList::new(config.task_file()));
        // Create the unbounded channel for history messages
        let (history_sender, history_receiver) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                config,
                provider,
                env,
                resolver,
                central: Mutex::new(central),
                state: Mutex::new(State {
                    run: RunState::default(),
                    current_task: None,
                    current_task_dir: None,
                    recent: VecDeque::new(),
                    attach_count: 0,
                    stop: Stop::default(),
                    schedule,
                    cancel: CancellationToken::new(),
                    pipeline: PipelineState::today(),
                    tiles: Tiles::default(),
                    group_number: None,
                    group_base: None,
                    discovery_ran: false,
                }),
                events,
                phase,
                planning_done: Notify::new(),
                discovery_done: Notify::new(),
                soft_stop_cancelled: Notify::new(),
                task_list,
                shutdown: CancellationToken::new(),
                history_sender: Some(history_sender),
                history_receiver: Arc::new(Mutex::new(Some(history_receiver))),
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

    /// Spawns the history writer tokio task.
    /// This should be called once when the engine starts up in an async context.
    pub fn spawn_history_writer(&self) {
        // Try to take the receiver from the Mutex
        let mut guard = self.inner.history_receiver.lock().unwrap();
        if let Some(receiver) = guard.take() {
            drop(guard); // Release the lock before spawning
            let writer = crate::history::HistoryWriter::new(receiver);
            tokio::spawn(writer.run());
        }
    }
}
