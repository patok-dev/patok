//! `serve` publishes `meta.json` next to the socket and removes it on shutdown.

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
