//! Supervision of a provider CLI subprocess: line streaming, transcript, timeouts, cancellation.
//! Shared by every CLI-backed provider. No PTY layer.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use patok_core::event::AgentEvent;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::{ExitKind, ProviderError, SessionRequest, SessionResult};

/// What a CLI-specific parser makes of one stdout line.
pub struct Parsed {
    pub events: Vec<AgentEvent>,
    /// The line was valid JSON; such lines count as progress even without a visible event, so
    /// thinking-only turns are not killed.
    pub json: bool,
}

pub trait LineParser: Send {
    fn parse_line(&mut self, line: &str) -> Parsed;
}

pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(&'static str, &'static str)>,
    /// Written to the child's stdin, which is then closed.
    pub stdin: String,
}

enum Line {
    Out(String),
    Err(String),
}

const POLL: Duration = Duration::from_millis(500);

pub async fn run(
    spec: ProcessSpec,
    request: &SessionRequest,
    mut parser: impl LineParser,
    provider: &str,
    model: &str,
) -> Result<SessionResult, ProviderError> {
    let log_path = open_log_path(&request.log_dir, &request.label, provider)?;
    let mut log =
        tokio::fs::File::create(&log_path)
            .await
            .map_err(|source| ProviderError::Log {
                dir: request.log_dir.clone(),
                source,
            })?;

    let mut child = spawn(&spec, &request.project_dir)?;
    let pid = child.id();
    if let Some(mut stdin) = child.stdin.take() {
        // A child that exits before reading its prompt shows up as a failed session below.
        let _ = stdin.write_all(spec.stdin.as_bytes()).await;
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_reader(child.stdout.take().expect("piped"), tx.clone(), Line::Out);
    spawn_reader(child.stderr.take().expect("piped"), tx, Line::Err);

    let started = Instant::now();
    let mut last_progress = started;
    let idle = request.idle_timeout;
    let mut tick = tokio::time::interval(POLL);
    let mut exit: Option<(ExitKind, Option<String>)> = None;
    let mut status = None;

    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some(Line::Out(text)) => {
                    write_log(&mut log, &text).await;
                    let parsed = parser.parse_line(&text);
                    if parsed.json || !parsed.events.is_empty() {
                        last_progress = Instant::now();
                    }
                    for event in parsed.events {
                        let _ = request.events.send(event);
                    }
                }
                Some(Line::Err(text)) => {
                    write_log(&mut log, &text).await;
                    let _ = request.events.send(AgentEvent::Stderr { text });
                }
                None => break,
            },
            waited = child.wait(), if status.is_none() => {
                // The pipes close with the process; keep looping until both readers drained.
                status = Some(waited);
            }
            () = request.cancel.cancelled(), if exit.is_none() => {
                exit = Some((ExitKind::Cancelled, Some("cancelled by shutdown".into())));
                kill(&mut child, pid).await;
            }
            _ = tick.tick(), if exit.is_none() => {
                let secs = idle.as_secs();
                if last_progress.elapsed() >= idle {
                    exit = Some((ExitKind::TimedOut, Some(format!("timed out after {secs} s without progress"))));
                } else if started.elapsed() >= idle * 4 {
                    exit = Some((ExitKind::TimedOut, Some(format!("timed out after {} s total runtime", secs * 4))));
                }
                if exit.is_some() {
                    kill(&mut child, pid).await;
                }
            }
        }
    }
    let _ = log.flush().await;

    let (exit, failure) = match exit {
        Some(pair) => pair,
        None => match status {
            Some(Ok(s)) if s.success() => (ExitKind::Completed, None),
            _ => match child.wait().await {
                Ok(s) if s.success() => (ExitKind::Completed, None),
                _ => (
                    ExitKind::Failed,
                    Some(format!("Agent {provider} exited unsuccessfully")),
                ),
            },
        },
    };
    Ok(SessionResult {
        success: exit == ExitKind::Completed,
        exit,
        failure,
        log_path: Some(log_path),
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

fn spawn(spec: &ProcessSpec, cwd: &Path) -> Result<Child, ProviderError> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group so the whole tree can be signalled; also reaps the child if the
        // engine task is dropped.
        .process_group(0)
        .kill_on_drop(true);
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command.spawn().map_err(|source| {
        let cli = spec.program.clone();
        if source.kind() == std::io::ErrorKind::NotFound {
            ProviderError::CliNotFound { cli }
        } else {
            ProviderError::Spawn { cli, source }
        }
    })
}

fn spawn_reader<R>(reader: R, tx: mpsc::UnboundedSender<Line>, wrap: fn(String) -> Line)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(wrap(line)).is_err() {
                break;
            }
        }
    });
}

/// Kills the child's whole process group (agents run tools as grandchildren), then the child.
async fn kill(child: &mut Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        // No libc dependency or unsafe: signal the group through kill(1).
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.start_kill();
}

async fn write_log(log: &mut tokio::fs::File, line: &str) {
    // The transcript is best-effort once the session is running.
    let _ = log.write_all(line.as_bytes()).await;
    let _ = log.write_all(b"\n").await;
}

pub fn open_log_path(dir: &Path, label: &str, provider: &str) -> Result<PathBuf, ProviderError> {
    std::fs::create_dir_all(dir).map_err(|source| ProviderError::Log {
        dir: dir.to_path_buf(),
        source,
    })?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(dir.join(format!("{millis}-{label}-{provider}.log")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct Lines;
    impl LineParser for Lines {
        fn parse_line(&mut self, line: &str) -> Parsed {
            Parsed {
                events: vec![AgentEvent::Text { text: line.into() }],
                json: false,
            }
        }
    }

    fn sh(script: &str) -> ProcessSpec {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            stdin: String::new(),
        }
    }

    fn request(
        dir: &Path,
        idle: Duration,
    ) -> (SessionRequest, mpsc::UnboundedReceiver<AgentEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        let request = SessionRequest {
            model: None,
            prompt: String::new(),
            system_prompt: None,
            project_dir: dir.to_path_buf(),
            events,
            log_dir: dir.join("logs"),
            label: "T1.1".into(),
            idle_timeout: idle,
            cancel: CancellationToken::new(),
            allowed_tools: vec![],
        };
        (request, rx)
    }

    #[tokio::test]
    async fn streams_lines_logs_them_and_completes() {
        let dir = tempfile::tempdir().unwrap();
        let (request, mut rx) = request(dir.path(), Duration::from_secs(30));
        let result = run(
            sh("echo one; echo two >&2; echo three"),
            &request,
            Lines,
            "sh",
            "m",
        )
        .await
        .unwrap();
        assert_eq!(result.exit, ExitKind::Completed);
        assert!(result.success);
        let mut texts = vec![];
        while let Ok(event) = rx.try_recv() {
            texts.push(format!("{event:?}"));
        }
        assert_eq!(texts.len(), 3, "{texts:?}");
        let log = std::fs::read_to_string(result.log_path.unwrap()).unwrap();
        assert!(log.contains("one") && log.contains("two") && log.contains("three"));
    }

    #[tokio::test]
    async fn nonzero_exit_fails() {
        let dir = tempfile::tempdir().unwrap();
        let (request, _rx) = request(dir.path(), Duration::from_secs(30));
        let result = run(sh("exit 3"), &request, Lines, "sh", "").await.unwrap();
        assert_eq!(result.exit, ExitKind::Failed);
        assert_eq!(
            result.failure.as_deref(),
            Some("Agent sh exited unsuccessfully")
        );
    }

    #[tokio::test]
    async fn idle_timeout_kills_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let (request, _rx) = request(dir.path(), Duration::from_millis(600));
        let result = run(sh("sleep 30"), &request, Lines, "sh", "")
            .await
            .unwrap();
        assert_eq!(result.exit, ExitKind::TimedOut);
        assert_eq!(
            result.failure.as_deref(),
            Some("timed out after 0 s without progress")
        );
    }

    #[tokio::test]
    async fn cancellation_kills_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let (request, _rx) = request(dir.path(), Duration::from_secs(30));
        let cancel = request.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let result = run(sh("sleep 30"), &request, Lines, "sh", "")
            .await
            .unwrap();
        assert_eq!(result.exit, ExitKind::Cancelled);
        assert_eq!(result.failure.as_deref(), Some("cancelled by shutdown"));
    }

    #[tokio::test]
    async fn missing_cli_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let (request, _rx) = request(dir.path(), Duration::from_secs(1));
        let spec = ProcessSpec {
            program: "patok-no-such-cli".into(),
            ..sh("")
        };
        let err = run(spec, &request, Lines, "x", "").await.unwrap_err();
        assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err}");
    }
}
