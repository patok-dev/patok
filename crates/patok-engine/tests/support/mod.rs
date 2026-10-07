#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use patok_core::config::RunMode;
use patok_core::event::EngineEvent;
use patok_engine::{Engine, EngineConfig};
use patok_providers::Provider;
use tempfile::TempDir;

pub struct Fixture {
    pub project: TempDir,
    pub runtime: TempDir,
    pub data: TempDir,
}

impl Fixture {
    /// A git repository with the given task file committed.
    pub fn new(tasks: &str) -> Self {
        let fixture = Self {
            project: tempfile::tempdir().unwrap(),
            runtime: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        };
        std::fs::write(fixture.project.path().join("TASKS.md"), tasks).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Test"],
            &["config", "user.email", "test@example.com"],
            &["config", "commit.gpgsign", "false"],
            &["add", "-A"],
            &["commit", "-q", "-m", "initial"],
        ] {
            fixture.git(args);
        }
        fixture
    }

    pub fn path(&self) -> &Path {
        self.project.path()
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    pub fn config(&self) -> EngineConfig {
        self.config_for(false)
    }

    /// The config with the plan stage on or off (the `plan_enabled` daemon key, T10.1).
    pub fn config_for(&self, plan_enabled: bool) -> EngineConfig {
        self.config_full(plan_enabled, true)
    }

    /// The config with the plan stage and the research-skip key set (T68.1).
    /// The review stage is off, so the existing tests keep their exact session
    /// counts and task lines; the review tests turn it on (T70.1).
    /// The run mode is continuous, the mode the scheduled discovery round
    /// belongs to (T103.1); the sprint tests opt out via [`Fixture::config_sprint`].
    pub fn config_full(&self, plan_enabled: bool, skip_research_for_simple: bool) -> EngineConfig {
        EngineConfig {
            project_dir: self.path().to_path_buf(),
            runtime_dir: self.runtime.path().join("engine"),
            data_dir: self.data.path().to_path_buf(),
            agent_timeout: Duration::from_secs(30),
            idle_shutdown: Duration::from_secs(3600),
            discovery_cooldown: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN,
            discovery_cooldown_cap: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN_CAP,
            plan_enabled,
            run_mode: RunMode::Continuous,
            skip_research_for_simple,
            review_in_loop: false,
            review_history: self.data.path().join("review-history.json"),
            open_group: self.data.path().join("open-group.json"),
            config_files: patok_core::config::ConfigFiles::default(),
        }
    }

    /// The config with sprint as the run mode (T103.1): no scheduled discovery
    /// round on the emptied queue.
    pub fn config_sprint(&self) -> EngineConfig {
        let mut config = self.config_full(false, true);
        config.run_mode = RunMode::Sprint;
        config
    }

    /// An engine in sprint mode (T103.1).
    pub fn engine_sprint(&self, provider: impl Provider + 'static) -> Engine {
        Engine::new(self.config_sprint(), Arc::new(provider))
    }

    /// The config with the review stage on (T70.1): the plan stage off
    /// and the research-skip key on, like [`Fixture::config`].
    pub fn config_review(&self) -> EngineConfig {
        let mut config = self.config_full(false, true);
        config.review_in_loop = true;
        config
    }

    /// An engine with the review stage on (T70.1).
    pub fn engine_with_review(&self, provider: impl Provider + 'static) -> Engine {
        Engine::new(self.config_review(), Arc::new(provider))
    }

    /// An engine whose daemon settings come from a written project-local
    /// `patok.config.toml` (the layered configuration of T13.1, in the project's slot
    /// of the projects dir), with the
    /// given `[daemon]` table; the provider resolves to the mock whatever the
    /// kind. The review stage is on.
    pub fn engine_configured(
        &self,
        provider: impl Provider + 'static,
        daemon_toml: &str,
    ) -> Engine {
        std::fs::write(
            self.project_config(),
            format!("[daemon]\nplan_enabled = false\nrun_mode = \"sprint\"\nreview_in_loop = true\n{daemon_toml}"),
        )
        .unwrap();
        let config = self.config_review();
        let provider = Arc::new(provider);
        Engine::configured_with_env(
            config,
            &patok_core::config::DaemonEnv::default(),
            move |_kind| Ok(provider.clone()),
        )
    }

    /// The project-local config file, in the project's slot of the projects dir.
    pub fn project_config(&self) -> PathBuf {
        self.data.path().join("patok.config.toml")
    }

    /// The project data directory's artifact folders (claims, reviews).
    pub fn data_path(&self, folder: &str) -> PathBuf {
        self.data.path().join(folder)
    }

    pub fn engine(&self, provider: impl Provider + 'static) -> Engine {
        Engine::new(self.config(), Arc::new(provider))
    }

    /// An engine with the plan stage enabled (T10.1).
    pub fn engine_with_plan(&self, provider: impl Provider + 'static) -> Engine {
        Engine::new(self.config_for(true), Arc::new(provider))
    }

    /// An engine with the plan stage off and the research-skip key off, so
    /// every task runs the research stage before its builder session (T68.1).
    pub fn engine_with_research(&self, provider: impl Provider + 'static) -> Engine {
        Engine::new(self.config_full(false, false), Arc::new(provider))
    }

    pub fn socket(&self) -> PathBuf {
        self.config().socket_path()
    }

    pub fn tasks_file(&self) -> String {
        std::fs::read_to_string(self.path().join("TASKS.md")).unwrap()
    }
}

/// Receives events until `done` matches, failing the test after a timeout.
pub async fn collect_until(
    rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    mut done: impl FnMut(&EngineEvent) -> bool,
) -> Vec<EngineEvent> {
    let mut seen = vec![];
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = rx.recv().await.expect("event stream open");
            let finished = done(&event);
            seen.push(event);
            if finished {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; saw {seen:#?}"));
    seen
}

pub fn is_phase_startup(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::PhaseChanged {
            phase: patok_core::event::Phase::Startup
        }
    )
}
