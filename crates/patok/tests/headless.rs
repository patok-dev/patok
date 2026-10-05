//! The headless mode: engine up, nothing drawn, clean shutdown on interrupt.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const READINESS: &str = "Headless -- press Ctrl-C";

fn patok(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_patok"));
    command.args(["-d", dir.to_str().unwrap()]).args(args);
    command
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

/// Spawns `patok --headless` with stdout and stderr redirected to files (the child
/// stays alive after the kill checks, so a piped stdout could not be drained). Returns
/// the child and the two log paths.
fn spawn_headless(dir: &Path, logs: &Path) -> std::process::Child {
    let stdout = logs.join("headless.out");
    let stderr = logs.join("headless.err");
    patok(dir, &["--headless"])
        .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
        .spawn()
        .unwrap()
}

/// Polls the headless stdout file until it prints its readiness line, proving both
/// startup lines were flushed and the engine is up.
async fn wait_until_ready(logs: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stdout = std::fs::read_to_string(logs.join("headless.out")).unwrap_or_default();
        if stdout.contains(READINESS) {
            return stdout;
        }
        assert!(
            Instant::now() < deadline,
            "the headless process never announced readiness; stdout: {stdout}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn read_log(logs: &Path, name: &str) -> String {
    std::fs::read_to_string(logs.join(name)).unwrap_or_default()
}

/// Stops the engine via the CLI and confirms the process is really gone.
async fn cleanup(dir: &Path) {
    let out = patok(dir, &["daemon", "stop"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(out.success());
    assert!(!reachable(dir).await);
}

#[tokio::test]
async fn headless_starts_a_detached_engine_and_draws_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let mut child = spawn_headless(dir.path(), logs.path());

    wait_until_ready(logs.path()).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "the headless process exited while it should be waiting"
    );
    assert!(
        reachable(dir.path()).await,
        "the engine is not reachable on the socket"
    );

    // Abrupt death of the headless process: the engine has its own process group
    // and must survive it.
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "a killed child cannot report success: {status}"
    );
    assert!(
        reachable(dir.path()).await,
        "the engine died with the headless process"
    );

    let stdout = read_log(logs.path(), "headless.out");
    assert!(stdout.contains("Started the engine"), "stdout: {stdout}");
    assert!(
        !stdout.contains('\x1b'),
        "escape sequences were drawn on stdout: {stdout:?}"
    );
    let stderr = read_log(logs.path(), "headless.err");
    assert!(stderr.is_empty(), "stderr: {stderr}");

    cleanup(dir.path()).await;
}

#[tokio::test]
async fn headless_stops_the_engine_when_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let mut child = spawn_headless(dir.path(), logs.path());

    wait_until_ready(logs.path()).await;

    // SIGTERM, as a supervisor would send.
    let signal = Command::new("kill")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(signal.success());

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the headless process did not exit; stdout: {}",
            read_log(logs.path(), "headless.out")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(status.code(), Some(0), "headless exit status: {status}");

    let stdout = read_log(logs.path(), "headless.out");
    assert!(
        !stdout.contains('\x1b'),
        "escape sequences were drawn on stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("Soft stop requested"),
        "the clean-stop messages are missing: {stdout}"
    );

    assert!(
        !reachable(dir.path()).await,
        "the engine is still running after the headless exit"
    );
    let out = patok(dir.path(), &["daemon", "stop"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No engine is running for"),
        "stdout: {stdout}"
    );
}
