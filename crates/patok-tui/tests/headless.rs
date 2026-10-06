//! The headless driver over the real gRPC protocol: engine with a mock provider,
//! `run_until` streaming into a buffer.

use std::sync::Arc;
use std::time::Duration;

use patok_core::event::AgentEvent;
use patok_engine::{Engine, EngineConfig};
use patok_providers::mock::{MockProvider, Step};
use patok_providers::{ExitKind, Provider, ProviderError};
use tokio_util::sync::CancellationToken;

struct Setup {
    _dirs: Vec<tempfile::TempDir>,
    config: EngineConfig,
}

/// Strips ANSI escape sequences (`\x1b[...m` colour wraps) from a buffer, so
/// substring and ordering assertions can run against the plain text the
/// escapes wrap (the colored run wraps pieces of a line, not whole lines).
fn strip_escapes(buffer: &str) -> String {
    let mut plain = String::with_capacity(buffer.len());
    let mut rest = buffer;
    while let Some(start) = rest.find('\x1b') {
        plain.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        // SGR sequences only: `\x1b[`, colour parameters, a final `m`.
        let Some(body) = after.strip_prefix('[') else {
            plain.push('\x1b');
            rest = after;
            continue;
        };
        match body.find('m') {
            Some(end) if body[..end].bytes().all(|b| b.is_ascii_digit() || b == b';') => {
                rest = &body[end + 1..];
            }
            _ => {
                plain.push_str("\x1b[");
                rest = body;
            }
        }
    }
    plain.push_str(rest);
    plain
}

fn setup() -> Setup {
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
        plan_enabled: false,
        run_mode: patok_core::config::RunMode::Sprint,
        skip_research_for_simple: true,
        review_in_loop: false,
        review_history: data.path().join("review-history.json"),
        config_files: patok_core::config::ConfigFiles::default(),
    };
    Setup {
        _dirs: vec![project, runtime, data],
        config,
    }
}

/// Writes a project-local `patok.config.toml` (the project's slot of the projects dir)
/// with the given `[daemon]` table.
fn project_config(setup: &Setup, daemon_toml: &str) {
    std::fs::write(
        setup.config.data_dir.join("patok.config.toml"),
        format!("[daemon]\n{daemon_toml}"),
    )
    .unwrap();
}

async fn connect_retrying(
    socket: &std::path::Path,
) -> patok_proto::engine_client::EngineClient<tonic::transport::Channel> {
    for _ in 0..100 {
        if let Ok(client) = patok_tui::client::connect(socket).await {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("engine never became reachable");
}

/// Runs `run_until` with a fresh token, capturing the streamed output.
async fn run_headless(
    client: patok_proto::engine_client::EngineClient<tonic::transport::Channel>,
    color: bool,
) -> (patok_tui::HeadlessOutcome, String) {
    let mut buffer = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        patok_tui::run_until(client, &mut buffer, CancellationToken::new(), color),
    )
    .await
    .expect("the headless run finishes")
    .unwrap();
    (outcome, String::from_utf8(buffer).unwrap())
}

/// Ends the engine: stop any running work, then exit the server task.
async fn finish(server: tokio::task::JoinHandle<anyhow::Result<()>>, engine: &Engine) {
    engine.request_stop(true);
    engine.wait_until_idle().await;
    engine.shutdown_token().cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn runs_to_completion_and_streams_the_frame_content() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "hello from the agent".into(),
        }),
        Step::Event(AgentEvent::Result { text: "ok".into() }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, buffer) = run_headless(client, false).await;

    assert_eq!(outcome, patok_tui::HeadlessOutcome::Completed);
    // The frame's exact lines, in order: the task heading, the lifecycle status
    // lines, the agent text, the result and the task line.
    let heading = buffer.find("── T1.1: add hello").expect(&buffer);
    let started = buffer.find("Builder started (mock)").expect(&buffer);
    let text = buffer.find("hello from the agent").expect(&buffer);
    let result = buffer.find("result: ok").expect(&buffer);
    let finished = buffer.find("Builder finished in").expect(&buffer);
    let done = buffer.find("✔ T1.1 done").expect(&buffer);
    assert!(
        heading < started && started < text && text < result && result < finished,
        "buffer: {buffer}"
    );
    assert!(finished < done, "buffer: {buffer}");
    assert!(buffer.ends_with('\n'), "buffer: {buffer:?}");
    assert!(
        !buffer.contains('\x1b'),
        "escape sequences in the output: {buffer:?}"
    );

    finish(server, &engine).await;
}

#[tokio::test]
async fn colored_output_uses_escapes_and_keeps_the_lines() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "hello from the agent".into(),
        }),
        Step::Event(AgentEvent::Result { text: "ok".into() }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, buffer) = run_headless(client, true).await;

    assert_eq!(outcome, patok_tui::HeadlessOutcome::Completed);
    assert!(
        buffer.contains("\x1b["),
        "the colored run must emit escape sequences: {buffer:?}"
    );
    // The escape sequences wrap pieces of a line — the agent name wears its
    // own colour inside the lifecycle lines (T110.1) — so the ordering and
    // substring assertions run on the escape-stripped text.
    let plain = strip_escapes(&buffer);
    let heading = plain.find("── T1.1: add hello").expect(&plain);
    let started = plain.find("Builder started (mock)").expect(&plain);
    let text = plain.find("hello from the agent").expect(&plain);
    let result = plain.find("result: ok").expect(&plain);
    let finished = plain.find("Builder finished in").expect(&plain);
    let done = plain.find("✔ T1.1 done").expect(&plain);
    assert!(
        heading < started && started < text && text < result && result < finished,
        "buffer: {buffer}"
    );
    assert!(finished < done, "buffer: {buffer}");
    assert!(buffer.ends_with('\n'), "buffer: {buffer:?}");

    finish(server, &engine).await;
}

#[tokio::test]
async fn failed_task_exits_failed() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![Step::Event(AgentEvent::Result { text: "no".into() })])
        .ending_with(ExitKind::Failed);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, buffer) = run_headless(client, false).await;

    let patok_tui::HeadlessOutcome::Failed(error) = outcome else {
        panic!("a failed task must end the run as Failed")
    };
    assert!(error.contains("T1.1 failed"), "{error}");
    assert!(buffer.contains("✘ T1.1 failed"), "buffer: {buffer}");

    finish(server, &engine).await;
}

#[tokio::test]
async fn forces_sprint_and_restores_the_previous_mode() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    // The project-local config keeps the loop in continuous mode; the resolved layer
    // the engine persists the run mode to.
    project_config(
        &setup,
        "plan_enabled = false\nreview_in_loop = false\nrun_mode = \"continuous\"\n",
    );
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![Step::Event(AgentEvent::Result { text: "ok".into() })]);
    let recorded = provider.clone();
    let engine = Engine::configured_with_env(
        setup.config.clone(),
        &patok_core::config::DaemonEnv::default(),
        move |_kind| Ok(Arc::new(provider.clone())),
    );
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, buffer) = run_headless(client, false).await;

    assert_eq!(outcome, patok_tui::HeadlessOutcome::Completed);
    // The forced sprint mode skips the scheduled discovery round the continuous
    // mode would run on the emptied queue (T103.1): exactly the task's own
    // builder session ran, and no discovery line reached the output.
    let recorded_sessions = recorded.sessions();
    let labels: Vec<_> = recorded_sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["T1.1"], "no discovery session may run: {labels:?}");
    assert!(
        !buffer.contains("Discovery started"),
        "no discovery lifecycle line: {buffer}"
    );
    let forced = buffer
        .find("Setting `run_mode` changed to `sprint`")
        .expect(&buffer);
    let restored = buffer
        .find("Setting `run_mode` changed to `continuous`")
        .expect(&buffer);
    let done = buffer.find("✔ T1.1 done").expect(&buffer);
    assert!(
        forced < done && done < restored,
        "the forcing precedes the build and the restore follows it: {buffer}"
    );
    let config = std::fs::read_to_string(setup.config.data_dir.join("patok.config.toml")).unwrap();
    assert!(
        config.contains("run_mode = \"continuous\""),
        "the config file ends with the previous mode: {config}"
    );

    finish(server, &engine).await;
}

#[tokio::test]
async fn nothing_pending_completes_without_a_build() {
    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![Step::Event(AgentEvent::Result { text: "ok".into() })]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider.clone()));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, _buffer) = run_headless(client, false).await;

    assert_eq!(outcome, patok_tui::HeadlessOutcome::Completed);
    assert!(
        provider.sessions().is_empty(),
        "no agent session may run without pending tasks"
    );

    finish(server, &engine).await;
}

#[tokio::test]
async fn interrupt_cancels_to_interrupted() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    // A session that sleeps far past the run, so only the token can end it.
    let provider = MockProvider::new(vec![Step::Sleep(Duration::from_secs(60))]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let mut buffer = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        patok_tui::run_until(client, &mut buffer, token, false),
    )
    .await
    .expect("the interrupted run returns promptly")
    .unwrap();

    assert_eq!(outcome, patok_tui::HeadlessOutcome::Interrupted);

    finish(server, &engine).await;
}

#[tokio::test]
async fn unavailable_provider_is_an_engine_error() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    // A provider spelling this build does not ship: every build command is refused
    // with a deterministic error.
    project_config(
        &setup,
        "plan_enabled = false\nreview_in_loop = false\nprovider = \"opencode\"\n",
    );
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![]);
    let engine = Engine::configured_with_env(
        setup.config.clone(),
        &patok_core::config::DaemonEnv::default(),
        move |_kind| -> Result<Arc<dyn Provider>, ProviderError> { Ok(Arc::new(provider.clone())) },
    );
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let client = connect_retrying(&socket).await;
    let (outcome, _buffer) = run_headless(client, false).await;

    let patok_tui::HeadlessOutcome::Failed(error) = outcome else {
        panic!("a refused build must end the run as Failed")
    };
    assert!(
        error.contains("not available in this build"),
        "the provider error reaches the operator: {error}"
    );

    finish(server, &engine).await;
}
