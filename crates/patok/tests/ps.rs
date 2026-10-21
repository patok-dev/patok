//! `patok ps` against real mock-provider engines under a temp-dir runtime root: the one-shot
//! JSON listing and the watch stream's added/removed objects.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use patok_core::paths::meta_path;
use patok_engine::{Engine, EngineConfig};
use patok_providers::mock::MockProvider;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

type Server = JoinHandle<anyhow::Result<()>>;

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    config: EngineConfig,
}

impl Fixture {
    fn project(&self) -> String {
        self.config.project_dir.display().to_string()
    }
}

fn engine_in(root: &Path, name: &str) -> (Fixture, Engine) {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        project_dir: project.path().to_path_buf(),
        runtime_dir: root.join(name),
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
    let engine = Engine::new(config.clone(), Arc::new(MockProvider::new(vec![])));
    let fixture = Fixture {
        _dirs: vec![project, data],
        config,
    };
    (fixture, engine)
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

/// Starts an engine named `name` under `root` and waits until `ps` can discover it.
async fn start(root: &Path, name: &str) -> (Fixture, CancellationToken, Server) {
    let (fixture, engine) = engine_in(root, name);
    let (token, server) = spawn(engine);
    wait_for(&fixture.config.socket_path()).await;
    wait_for(&meta_path(&fixture.config.runtime_dir)).await;
    (fixture, token, server)
}

async fn stop(token: CancellationToken, server: Server) {
    token.cancel();
    server.await.unwrap().unwrap();
}

fn ps(xdg_runtime: &Path, env_dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_patok"));
    cmd.arg("ps")
        .args(args)
        .env("XDG_RUNTIME_DIR", xdg_runtime)
        .env("XDG_DATA_HOME", env_dir.join("data"))
        .env("XDG_CONFIG_HOME", env_dir.join("config"))
        .kill_on_drop(true);
    cmd
}

/// Reads watch lines until one that is not `changed` arrives, and asserts it is `want`.
async fn next_event(lines: &mut Lines<BufReader<ChildStdout>>, want: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read watch stdout")
                .expect("watch stdout closed before the expected event");
            let value: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("watch line is not JSON ({e}): {line}"));
            if value["event"] == "changed" {
                continue;
            }
            assert_eq!(value["event"], want, "unexpected watch line: {line}");
            return value;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no `{want}` event within 10s"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_lists_every_running_engine_with_its_project() {
    let tmp = tempfile::tempdir().unwrap();
    let env = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let (a, token_a, server_a) = start(&root, "-a").await;
    let (b, token_b, server_b) = start(&root, "-b").await;

    let out = ps(tmp.path(), env.path(), &["--json"])
        .output()
        .await
        .expect("run patok ps --json");
    assert!(
        out.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a JSON array ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(rows.len(), 2, "rows: {rows:?}");
    let mut projects: Vec<&str> = rows
        .iter()
        .map(|r| r["project"].as_str().unwrap())
        .collect();
    projects.sort_unstable();
    let mut expected = vec![a.project(), b.project()];
    expected.sort_unstable();
    assert_eq!(projects, expected);
    for row in &rows {
        assert_eq!(row["pid"], std::process::id(), "row: {row}");
    }

    stop(token_a, server_a).await;
    stop(token_b, server_b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_json_streams_added_and_removed_engines() {
    let tmp = tempfile::tempdir().unwrap();
    let env = tempfile::tempdir().unwrap();
    let root = tmp.path().join("patok");
    let (a, token_a, server_a) = start(&root, "-a").await;
    let (b, token_b, server_b) = start(&root, "-b").await;

    let mut child = ps(tmp.path(), env.path(), &["--watch", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn patok ps --watch --json");
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    let mut initial = vec![
        next_event(&mut lines, "added").await["project"]
            .as_str()
            .unwrap()
            .to_owned(),
        next_event(&mut lines, "added").await["project"]
            .as_str()
            .unwrap()
            .to_owned(),
    ];
    initial.sort_unstable();
    let mut expected = vec![a.project(), b.project()];
    expected.sort_unstable();
    assert_eq!(initial, expected);

    let (c, token_c, server_c) = start(&root, "-c").await;
    let added = next_event(&mut lines, "added").await;
    assert_eq!(added["project"], c.project());

    stop(token_a, server_a).await;
    let removed = next_event(&mut lines, "removed").await;
    assert_eq!(removed, json!({"event": "removed", "project": a.project()}));

    child.kill().await.unwrap();
    stop(token_b, server_b).await;
    stop(token_c, server_c).await;
}
