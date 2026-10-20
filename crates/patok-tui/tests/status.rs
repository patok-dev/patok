//! The per-daemon status client against a real engine with a mock provider.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use patok_core::event::AgentEvent;
use patok_engine::Engine;
use patok_proto::{CommandRequest, StartBuild, command_request};
use patok_providers::mock::{MockProvider, Step};
use patok_tui::{RunState, StatusUpdate, TaskProgress, probe};

use support::{connect_retrying, setup};

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

#[tokio::test]
async fn status_follows_run_state_and_task_progress() {
    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: add hello\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    let meta = patok_core::paths::meta_path(&setup.config.runtime_dir);
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "hello from the agent".into(),
        }),
        Step::Event(AgentEvent::Result { text: "ok".into() }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let token = engine.shutdown_token();
    let server = tokio::spawn(patok_engine::serve(engine));
    wait_for(&socket).await;
    wait_for(&meta).await;

    let mut stream = probe(&socket).await.unwrap();
    let status = stream.status();
    assert_eq!(status.run_state, RunState::Idle);
    assert_eq!(status.tasks, TaskProgress { done: 0, total: 1 });
    assert_eq!(status.project_dir, setup.config.project_dir);
    assert_eq!(status.engine_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.pid, Some(std::process::id()));
    assert!(status.started_at.is_some());
    assert_eq!(status.socket_path, socket);

    let mut client = connect_retrying(&socket).await;
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let mut saw_running = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .expect("a status update arrives");
        let StatusUpdate::Changed(status) = update else {
            panic!("the engine went away mid-build");
        };
        if status.run_state == RunState::Running {
            saw_running = true;
        } else if saw_running && status.run_state == RunState::Idle && status.tasks.done == 1 {
            break;
        }
    }

    token.cancel();
    let gone = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("shutdown ends the stream");
    assert_eq!(gone, StatusUpdate::Gone);
    assert_eq!(stream.next().await, StatusUpdate::Gone);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn probing_without_a_listener_fails() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sock");
    assert!(probe(&missing).await.is_err());

    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(stale.exists());
    assert!(probe(&stale).await.is_err());
}

#[tokio::test]
async fn an_observed_idle_engine_still_shuts_down_and_reports_gone() {
    let mut setup = setup();
    setup.config.idle_shutdown = Duration::from_secs(1);
    let socket = setup.config.socket_path();
    let meta = patok_core::paths::meta_path(&setup.config.runtime_dir);
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine));
    wait_for(&socket).await;
    wait_for(&meta).await;

    let mut stream = probe(&socket).await.unwrap();
    let update = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("idle shutdown ends the stream");
    assert_eq!(update, StatusUpdate::Gone);
    server.await.unwrap().unwrap();
}
