//! `serve` publishes `meta.json` next to the socket and removes it on shutdown, and
//! idle-shuts-down when only observers (or no one) are attached.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use patok_core::paths::{DaemonMeta, meta_path};
use patok_engine::Engine;
use patok_providers::mock::MockProvider;
use support::Fixture;

const TASKS: &str = "# Tasks\n\n## Phase 1\n- [ ] T1.1: add the greeting file\n";

#[tokio::test]
async fn meta_json_exists_while_serving_and_is_gone_after_shutdown() {
    let fixture = Fixture::new(TASKS);
    let config = fixture.config();
    let meta = meta_path(&config.runtime_dir);
    let socket = fixture.socket();
    let engine = Engine::new(config, Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    tokio::time::timeout(Duration::from_secs(10), async {
        while !(meta.exists() && socket.exists()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("meta.json and the socket appear");

    let read = DaemonMeta::read(&meta).unwrap();
    assert_eq!(read.project_dir, fixture.path());
    assert_eq!(read.pid, std::process::id());
    assert_eq!(read.engine_version, env!("CARGO_PKG_VERSION"));
    assert!(read.started_at > 0);
    let mode = std::fs::metadata(&meta).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
    assert!(!meta.exists());
    assert!(!socket.exists());
}

/// Serves an empty-queue engine whose idle limit is one second.
async fn serve_with_short_idle(
    fixture: &Fixture,
) -> (Engine, tokio::task::JoinHandle<anyhow::Result<()>>) {
    let mut config = fixture.config();
    config.idle_shutdown = Duration::from_secs(1);
    let socket = fixture.socket();
    let engine = Engine::new(config, Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the socket appears");
    (engine, server)
}

#[tokio::test]
async fn an_engine_with_only_an_observer_attached_idle_shuts_down() {
    let fixture = Fixture::new("# Tasks\n");
    let (engine, server) = serve_with_short_idle(&fixture).await;
    let _observer = engine.attach_observer().unwrap();

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the engine idle-shuts-down")
        .unwrap()
        .unwrap();
    assert!(!fixture.socket().exists());
}

#[tokio::test]
async fn a_normal_attach_keeps_an_idle_engine_alive() {
    let fixture = Fixture::new("# Tasks\n");
    let (engine, server) = serve_with_short_idle(&fixture).await;
    let shell = engine.attach().unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!server.is_finished());
    assert!(!engine.shutdown_token().is_cancelled());

    drop(shell);
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the engine idle-shuts-down once the shell detaches")
        .unwrap()
        .unwrap();
}
