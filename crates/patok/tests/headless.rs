//! The headless mode's process-level contract: build output on stdout, no TUI, exit
//! 0 once the store is done, 1 when the build cannot run, engine stopped either way.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Spawns `patok headless` for `dir` with the XDG directories isolated in `data`
/// (the engine reads its project-local config from the data dir's projects slot).
/// Returns the child and the two log paths under `logs`.
fn spawn_headless(dir: &Path, data: &Path, logs: &Path) -> std::process::Child {
    let stdout = logs.join("headless.out");
    let stderr = logs.join("headless.err");
    Command::new(env!("CARGO_BIN_EXE_patok"))
        .args(["-d", dir.to_str().unwrap(), "headless"])
        .env("XDG_DATA_HOME", data)
        // No user config layers: the test's project-local config is the only one.
        .env("XDG_CONFIG_HOME", data.join("config"))
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap()
}

/// The engine socket for this project, resolved exactly as the CLI does: the project
/// dir canonicalized (main.rs `project_dir`), then the runtime-dir rules from `patok-core`.
fn socket(dir: &Path) -> PathBuf {
    let project = dir.canonicalize().unwrap();
    patok_core::paths::socket_path(&patok_core::paths::runtime_dir_from_env(&project))
}

/// One probe of the engine over the same channel the TUI client connects through.
async fn reachable(dir: &Path) -> bool {
    patok_tui::client::connect(&socket(dir)).await.is_ok()
}

fn read_log(logs: &Path, name: &str) -> String {
    std::fs::read_to_string(logs.join(name)).unwrap_or_default()
}

/// Waits for the child to exit on its own, the way a supervisor would.
async fn wait_for_exit(child: &mut std::process::Child, logs: &Path) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the headless process did not exit; stdout: {}",
            read_log(logs, "headless.out")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The project-local config file, in the project's slot of the isolated data dir.
fn project_config(data: &Path, dir: &Path, daemon_toml: &str) {
    let project = dir.canonicalize().unwrap();
    let config_dir =
        patok_core::paths::project_data_dir(Some(data.to_str().unwrap()), None, &project).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("patok.config.toml"),
        format!("[daemon]\n{daemon_toml}"),
    )
    .unwrap();
}

#[tokio::test]
async fn headless_completes_and_stops_the_engine_when_nothing_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let mut child = spawn_headless(dir.path(), data.path(), logs.path());

    let status = wait_for_exit(&mut child, logs.path()).await;
    assert_eq!(status.code(), Some(0), "headless exit status: {status}");

    let stdout = read_log(logs.path(), "headless.out");
    assert!(stdout.contains("Started the engine"), "stdout: {stdout}");
    assert!(stdout.contains("All tasks completed."), "stdout: {stdout}");
    assert!(
        !stdout.contains('\x1b'),
        "escape sequences were drawn on stdout: {stdout:?}"
    );
    let stderr = read_log(logs.path(), "headless.err");
    assert!(stderr.is_empty(), "stderr: {stderr}");

    // The headless exit stopped the engine it started.
    assert!(
        !reachable(dir.path()).await,
        "the engine is still running after the headless exit"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_patok"))
        .args(["-d", dir.path().to_str().unwrap(), "daemon", "stop"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No engine is running for"),
        "stdout: {stdout}"
    );
}

#[tokio::test]
async fn headless_exits_1_when_the_build_cannot_start() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("TASKS.md"), "- [ ] T1.1: add hello\n").unwrap();
    // A provider spelling this build does not ship: every build command is refused
    // with a deterministic error, so no provider CLI on PATH is needed.
    project_config(data.path(), dir.path(), "provider = \"opencode\"\n");
    let mut child = spawn_headless(dir.path(), data.path(), logs.path());

    let status = wait_for_exit(&mut child, logs.path()).await;
    assert_eq!(status.code(), Some(1), "headless exit status: {status}");

    let stdout = read_log(logs.path(), "headless.out");
    assert!(
        !stdout.contains('\x1b'),
        "escape sequences were drawn on stdout: {stdout:?}"
    );
    let stderr = read_log(logs.path(), "headless.err");
    assert!(stderr.contains("patok:"), "stderr: {stderr}");
    assert!(
        stderr.contains("not available in this build"),
        "the provider error reaches the operator: {stderr}"
    );

    // The failure path stops the engine too.
    assert!(
        !reachable(dir.path()).await,
        "the engine is still running after the failed headless run"
    );
}
