//! Attach-or-spawn and the engine lifecycle commands.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use patok_core::paths;
use patok_engine::{Engine, EngineConfig};
use patok_proto::engine_client::EngineClient;
use patok_proto::{ShutdownRequest, shutdown_request};
use patok_tui::Outcome;
use std::os::unix::process::CommandExt;
use tonic::transport::Channel;

const POLL: Duration = Duration::from_millis(50);
const START_BUDGET: Duration = Duration::from_secs(8);
const STOP_BUDGET: Duration = Duration::from_secs(10);

type Client = EngineClient<Channel>;

fn socket(project: &Path) -> PathBuf {
    paths::socket_path(&paths::runtime_dir_from_env(project))
}

/// Connects to the running engine, if any.
pub(crate) async fn try_connect(project: &Path) -> Option<Client> {
    patok_tui::client::connect(&socket(project)).await.ok()
}

/// `patok` / `patok run`: attach to the engine, spawning it first when needed, then run the UI.
pub async fn run_shell(project: &Path) -> anyhow::Result<()> {
    let client = match try_connect(project).await {
        Some(client) => client,
        None => spawn_and_connect(project).await?,
    };
    let spawner: patok_tui::Spawner = {
        let project = project.to_path_buf();
        Arc::new(move || {
            let project = project.clone();
            Box::pin(async move {
                // The soft-stop handshake already ran; make sure the old process is really gone.
                wait_until_gone(&project).await?;
                spawn_and_connect(&project).await
            })
        })
    };
    let outcome = patok_tui::run(client, spawner, project.to_path_buf()).await?;
    match outcome {
        Outcome::EngineStopped { completed, .. } => {
            let noun = if completed == 1 { "task" } else { "tasks" };
            println!("Patok stopped. {completed} {noun} completed.");
        }
        Outcome::Detached { .. } => {
            println!("Detached. The engine keeps running; run `patok` to reattach.");
        }
    }
    Ok(())
}

/// `patok daemon`: idempotently make sure an engine is running.
pub async fn start(project: &Path) -> anyhow::Result<()> {
    if try_connect(project).await.is_some() {
        println!("An engine is already running for {}.", project.display());
        return Ok(());
    }
    spawn_and_connect(project).await?;
    println!("Engine started for {}.", project.display());
    Ok(())
}

/// `patok daemon stop`: request a stop and wait until the engine has exited.
pub async fn stop(project: &Path, now: bool) -> anyhow::Result<()> {
    let Some(mut client) = try_connect(project).await else {
        println!("No engine is running for {}.", project.display());
        return Ok(());
    };
    if now {
        println!("Stopping the engine now...");
        drain_shutdown(&mut client, shutdown_request::Scope::Now).await?;
        // A NOW stop on a busy engine is an interrupt, not a quit: the current
        // task is cancelled and the engine returns to idle. A second NOW then
        // lands while idle -- the true quit -- so `daemon stop --now` still
        // exits the engine.
        if try_connect(project).await.is_some() {
            drain_shutdown(&mut client, shutdown_request::Scope::Now).await?;
        }
    } else {
        // A SOFT stop only winds the build loop down (the engine keeps serving),
        // so a second, NOW shutdown makes the process exit once the current task
        // has finished.
        println!(
            "Soft stop requested -- the engine exits after the current task, which may take a while."
        );
        drain_shutdown(&mut client, shutdown_request::Scope::Soft).await?;
        drain_shutdown(&mut client, shutdown_request::Scope::Now).await?;
    }
    // The stream ends just before the process does; callers that start a new engine next
    // (restart) need the old one really gone.
    wait_until_gone(project).await
}

/// Runs one shutdown with the given scope, printing its progress messages.
async fn drain_shutdown(client: &mut Client, scope: shutdown_request::Scope) -> anyhow::Result<()> {
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: scope as i32,
        })
        .await
        .context("the engine rejected the shutdown request")?
        .into_inner();
    while let Some(update) = progress
        .message()
        .await
        .context("lost the engine connection while stopping")?
    {
        println!("{}", update.message);
    }
    Ok(())
}

async fn wait_until_gone(project: &Path) -> anyhow::Result<()> {
    let deadline = Instant::now() + STOP_BUDGET;
    while try_connect(project).await.is_some() {
        if Instant::now() >= deadline {
            bail!("the engine confirmed the stop but is still reachable");
        }
        tokio::time::sleep(POLL).await;
    }
    Ok(())
}

/// `patok daemon --foreground`: the engine process itself.
pub async fn run_engine(project: &Path) -> anyhow::Result<()> {
    // Logs go to stderr: the terminal in the foreground, `engine.log` when detached.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let config = EngineConfig::resolve(project.to_path_buf())?;
    patok_engine::serve(Engine::configured(config, patok_providers::resolve)).await
}

async fn spawn_and_connect(project: &Path) -> anyhow::Result<Client> {
    let runtime_dir = paths::runtime_dir_from_env(project);
    patok_engine::prepare_runtime_dir(&runtime_dir)?;
    let log_path = runtime_dir.join("engine.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("cannot open {}", log_path.display()))?;

    let exe = install_path();
    // Own process group: the engine must survive the terminal closing or the shell crashing.
    let mut child = std::process::Command::new(&exe)
        .arg("--dir")
        .arg(project)
        .args(["daemon", "--foreground"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start the engine ({})", exe.display()))?;

    let deadline = Instant::now() + START_BUDGET;
    while Instant::now() < deadline {
        if let Some(client) = try_connect(project).await {
            return Ok(client);
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "the engine exited during startup ({status}):\n{}",
                log_tail(&log_path)
            );
        }
        tokio::time::sleep(POLL).await;
    }
    bail!(
        "the engine did not become reachable within {} s; see {}",
        START_BUDGET.as_secs(),
        log_path.display()
    )
}

fn log_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(5)..].join("\n")
}

/// The path to launch the engine from: the install path as invoked (`argv[0]`, else a `PATH`
/// lookup), not the OS's current-executable lookup, which can report a "(deleted)" path after
/// a self-update replaced the binary.
fn install_path() -> PathBuf {
    resolve_install_path(
        std::env::args_os().next().map(PathBuf::from),
        std::env::var_os("PATH"),
        std::env::current_dir().ok(),
    )
    .or_else(|| std::env::current_exe().ok())
    .unwrap_or_else(|| PathBuf::from("patok"))
}

fn resolve_install_path(
    argv0: Option<PathBuf>,
    path_var: Option<std::ffi::OsString>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    let argv0 = argv0?;
    if argv0.components().count() > 1 {
        return Some(match (argv0.is_absolute(), cwd) {
            (true, _) => argv0,
            (false, Some(cwd)) => cwd.join(argv0),
            (false, None) => return None,
        });
    }
    std::env::split_paths(&path_var?)
        .map(|dir| dir.join(&argv0))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_path_prefers_argv0() {
        let cwd = Some(PathBuf::from("/work"));
        assert_eq!(
            resolve_install_path(Some("/usr/bin/patok".into()), None, cwd.clone()),
            Some("/usr/bin/patok".into())
        );
        assert_eq!(
            resolve_install_path(Some("./bin/patok".into()), None, cwd),
            Some("/work/./bin/patok".into())
        );
    }

    #[test]
    fn install_path_searches_path_for_bare_names() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        std::fs::write(&exe, "").unwrap();
        let path_var =
            std::env::join_paths(["/nonexistent", dir.path().to_str().unwrap()]).unwrap();
        assert_eq!(
            resolve_install_path(Some("patok".into()), Some(path_var.clone()), None),
            Some(exe)
        );
        assert_eq!(
            resolve_install_path(Some("other".into()), Some(path_var), None),
            None
        );
    }
}
