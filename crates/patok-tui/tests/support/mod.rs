//! The TUI integration-test harness shared across the test files (finding 27
//! in docs/private/structure-review.md): the draw and press helpers, the Setup
//! fixture and the Snapshot fixture, one copy of each, mirroring the engine's
//! tests/support module.

#![allow(dead_code)]
// Each test file consumes a different subset of this module, and an item
// unused by one test binary would fail the `-D warnings` clippy gate.

use std::collections::BTreeMap;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use patok_config::SettingValue;
use patok_core::event::{EngineEvent, Phase, Snapshot};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_engine::EngineConfig;
use patok_tui::{Action, App, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// The Snapshot fixture with the defaults every rendering test agrees on
/// (the demo project dir, the startup phase, the empty provider/model and
/// settings readout, today's pipeline, no recent events), with `tasks` as
/// the one input.
pub fn snapshot(tasks: &str) -> SnapshotBuilder {
    SnapshotBuilder {
        snapshot: Snapshot {
            project_dir: "/home/user/demo".into(),
            phase: Phase::Startup,
            tasks: task::parse(tasks),
            current_task: None,
            planning: false,
            discovering: false,
            provider: String::new(),
            model: String::new(),
            settings: BTreeMap::new(),
            pipeline: PipelineState::today(),
            model_catalog: Vec::new(),
            recent: vec![],
        },
    }
}

pub struct SnapshotBuilder {
    snapshot: Snapshot,
}

impl SnapshotBuilder {
    /// The project dir the header shows.
    pub fn project_dir(mut self, dir: &str) -> Self {
        self.snapshot.project_dir = dir.into();
        self
    }

    /// The engine phase the app opens in.
    pub fn phase(mut self, phase: Phase) -> Self {
        self.snapshot.phase = phase;
        self
    }

    /// The task the run state marks as current.
    pub fn current_task(mut self, id: &str) -> Self {
        self.snapshot.current_task = Some(id.into());
        self
    }

    /// The provider name the header shows.
    pub fn provider(mut self, provider: &str) -> Self {
        self.snapshot.provider = provider.into();
        self
    }

    /// The settings readout the header shows.
    pub fn settings(mut self, settings: BTreeMap<String, SettingValue>) -> Self {
        self.snapshot.settings = settings;
        self
    }

    /// The events a reattaching shell replays.
    pub fn recent(mut self, recent: Vec<EngineEvent>) -> Self {
        self.snapshot.recent = recent;
        self
    }

    pub fn build(self) -> Snapshot {
        self.snapshot
    }
}

/// The Snapshot fixture every rendering test builds its App from. The
/// project dir, phase, pipeline and version are identical across files;
/// the task list, provider name, settings readout and truecolor override
/// are the per-file parameters.
pub fn app(
    tasks: &str,
    provider: &str,
    settings: BTreeMap<String, SettingValue>,
    truecolor: Option<bool>,
) -> App {
    let mut app = App::new(
        snapshot(tasks)
            .provider(provider)
            .settings(settings)
            .build(),
        "0.1.0".into(),
    );
    if let Some(truecolor) = truecolor {
        app.tui.truecolor = Some(truecolor);
    }
    app
}

/// Renders into a width x height TestBackend and returns the drawn text,
/// one trimmed line per row.
pub fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders into a width x height TestBackend and returns the buffer, for
/// cell-level symbol and style assertions.
pub fn draw_buffer_of(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// Renders into an 80x24 TestBackend and returns the buffer, for cell-level
/// symbol and style assertions.
pub fn draw_buffer(app: &App) -> ratatui::buffer::Buffer {
    draw_buffer_of(app, 80, 24)
}

pub fn press(app: &mut App, code: KeyCode) -> Action {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub fn ctrl(app: &mut App, c: char) -> Action {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

pub fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Three temp dirs (project, runtime, data) and the engine config pointing
/// into them. No TASKS.md: each test writes the task file its scenario needs.
pub struct Setup {
    _dirs: Vec<tempfile::TempDir>,
    pub config: EngineConfig,
}

pub fn setup() -> Setup {
    let project = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        project_dir: project.path().to_path_buf(),
        runtime_dir: runtime.path().join("e"),
        data_dir: data.path().to_path_buf(),
        agent_timeout: Duration::from_secs(30),
        idle_shutdown: Duration::from_secs(3600),
        discovery_cooldown: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN,
        discovery_cooldown_cap: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN_CAP,
        pause_between_tasks: Duration::ZERO,
        pause_between_agents: Duration::ZERO,
        pause_between_cycles: Duration::ZERO,
        plan_enabled: false,
        run_mode: patok_config::RunMode::Sprint,
        skip_research_for_simple: true,
        review_in_loop: false,
        review_confidence: data.path().join("review-history.json"),
        open_group: data.path().join("open-group.json"),
        config_files: patok_config::ConfigFiles::default(),
    };
    Setup {
        _dirs: vec![project, runtime, data],
        config,
    }
}

/// Connects to the engine's socket, retrying until the server is listening.
pub async fn connect_retrying(
    socket: &std::path::Path,
) -> patok_proto::engine_client::EngineClient<tonic::transport::Channel> {
    for _ in 0..100 {
        if let Ok(client) = patok_proto::client::connect(socket).await {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("engine never became reachable");
}
