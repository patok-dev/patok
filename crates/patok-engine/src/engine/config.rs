use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use patok_core::config::{DaemonEnv, DaemonSettings, ProviderKind, RunMode, load_daemon};
use patok_core::event::Phase;
use patok_core::pipeline::PipelineState;
use patok_providers::{Provider, ProviderError};
use tokio::sync::{Notify, broadcast, watch};
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::state::{Central, Inner, Resolver, RunState, State, Tiles};
use super::stop::Stop;
use crate::TASK_FILE;
use crate::schedule::DiscoverySchedule;

const BROADCAST_CAPACITY: usize = 4096;

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
        let review_history = Self::default_review_history(&data_dir);
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
            review_history,
            open_group,
            config_files: patok_core::config::ConfigFiles::from_env(),
        })
    }

    /// The learned-confidence history file: stored globally per user under the config directory,
    /// next to the config files; a missing config directory falls back to
    /// the project data directory.
    fn default_review_history(data_dir: &std::path::Path) -> PathBuf {
        let get = |k: &str| std::env::var(k).ok();
        match patok_core::paths::base_dir(
            get("XDG_CONFIG_HOME").as_deref(),
            get("HOME").as_deref(),
            ".config",
        ) {
            Some(dir) => dir.join("patok").join("review-history.json"),
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
                    run: RunState::default(),
                    current_task: None,
                    recent: VecDeque::new(),
                    attached: false,
                    stop: Stop::default(),
                    tasks: Vec::new(),
                    ui_added_ids: Vec::new(),
                    injected: Vec::new(),
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
}
