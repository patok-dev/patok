//! The fleet monitor against real engines with a mock provider, under a temp-dir runtime root.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use patok_core::paths::{DaemonMeta, meta_path};
use patok_engine::Engine;
use patok_providers::mock::MockProvider;
use patok_tui::{FleetEvent, FleetMonitor, monitor, monitor_polling};
use support::{Setup, setup};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type Server = JoinHandle<anyhow::Result<()>>;

fn engine_in(root: &Path, name: &str) -> (Setup, Engine) {
    let mut setup = setup();
    setup.config.runtime_dir = root.join(name);
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    (setup, engine)
}

fn spawn(engine: Engine) -> (CancellationToken, Server) {
    let token = engine.shutdown_token();
    (token, tokio::spawn(patok_engine::serve(engine)))
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

async fn next_event(mon: &mut FleetMonitor) -> FleetEvent {
    tokio::time::timeout(Duration::from_secs(10), mon.next())
        .await
        .expect("a fleet event arrives")
        .expect("the monitor is running")
}

async fn assert_quiet(mon: &mut FleetMonitor, span: Duration) {
    if let Ok(event) = tokio::time::timeout(span, mon.next()).await {
        panic!("unexpected event: {event:?}");
    }
}

fn added_socket(event: FleetEvent) -> PathBuf {
    match event {
        FleetEvent::Added(status) => status.socket_path,
        other => panic!("expected Added, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_a_daemon_that_was_running_before_the_monitor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let (setup, engine) = engine_in(&root, "-a");
    let socket = setup.config.socket_path();
    let (token, server) = spawn(engine);
    wait_for(&socket).await;
    wait_for(&meta_path(&setup.config.runtime_dir)).await;

    let mut mon = monitor(root);
    let FleetEvent::Added(status) = next_event(&mut mon).await else {
        panic!("expected Added");
    };
    assert_eq!(status.socket_path, socket);
    assert_eq!(status.project_dir, setup.config.project_dir);
    assert_eq!(status.pid, Some(std::process::id()));
    assert_quiet(&mut mon, Duration::from_millis(500)).await;

    token.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn reports_a_daemon_started_after_the_monitor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let mut mon = monitor(root.clone());

    let (setup_a, engine_a) = engine_in(&root, "-a");
    let (token_a, server_a) = spawn(engine_a);
    assert_eq!(
        added_socket(next_event(&mut mon).await),
        setup_a.config.socket_path()
    );

    let (setup_b, engine_b) = engine_in(&root, "-b");
    let (token_b, server_b) = spawn(engine_b);
    assert_eq!(
        added_socket(next_event(&mut mon).await),
        setup_b.config.socket_path()
    );
    assert_quiet(&mut mon, Duration::from_millis(500)).await;

    token_a.cancel();
    token_b.cancel();
    server_a.await.unwrap().unwrap();
    server_b.await.unwrap().unwrap();
}

#[tokio::test]
async fn reports_a_clean_shutdown_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let mut mon = monitor(root.clone());
    let (setup, engine) = engine_in(&root, "-a");
    let socket = setup.config.socket_path();
    let (token, server) = spawn(engine);
    assert_eq!(added_socket(next_event(&mut mon).await), socket);

    token.cancel();
    assert_eq!(next_event(&mut mon).await, FleetEvent::Removed(socket));
    server.await.unwrap().unwrap();
    assert_quiet(&mut mon, Duration::from_secs(1)).await;
}

#[tokio::test]
async fn never_reports_a_stale_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let stale = root.join("-stale");
    std::fs::create_dir_all(&stale).unwrap();
    drop(std::os::unix::net::UnixListener::bind(stale.join("engine.sock")).unwrap());
    let meta = DaemonMeta {
        project_dir: "/nowhere".into(),
        pid: 1,
        started_at: 0,
        engine_version: "0".into(),
    };
    meta.write(&meta_path(&stale)).unwrap();

    let mut mon = monitor(root.clone());
    let (setup, engine) = engine_in(&root, "-live");
    let (token, server) = spawn(engine);
    // The live engine is the barrier: the stale socket was scanned before or alongside it.
    assert_eq!(
        added_socket(next_event(&mut mon).await),
        setup.config.socket_path()
    );

    meta.write(&meta_path(&stale)).unwrap();
    assert_quiet(&mut mon, Duration::from_secs(2)).await;

    token.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn polling_fallback_reports_add_and_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let mut mon = monitor_polling(root.clone());

    let (setup, engine) = engine_in(&root, "-a");
    let socket = setup.config.socket_path();
    let (token, server) = spawn(engine);
    assert_eq!(added_socket(next_event(&mut mon).await), socket);

    token.cancel();
    assert_eq!(next_event(&mut mon).await, FleetEvent::Removed(socket));
    server.await.unwrap().unwrap();
}
