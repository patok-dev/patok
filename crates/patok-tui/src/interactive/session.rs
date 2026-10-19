//! A child process on a PTY, its output parsed into a vt100 screen.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use portable_pty::{CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

use super::replies::Replies;

const READ_CHUNK: usize = 8 * 1024;
/// Bound on queued writes to the child.
const INPUT_QUEUE: usize = 256;
/// Lines of local history, for wheel scrolling.
const SCROLLBACK: usize = 1000;
/// How long the group gets after SIGHUP before SIGKILL.
const KILL_GRACE: Duration = Duration::from_millis(200);

type Emulator = vt100::Parser<Replies>;

fn lock(parser: &Mutex<Emulator>) -> MutexGuard<'_, Emulator> {
    // A panicked reader must not take down rendering.
    parser.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the session reports to the event loop.
#[derive(Debug)]
pub(crate) enum PtyEvent {
    /// New output was parsed; coalesced, so one event may cover many reads.
    Output,
    /// The child exited (or could no longer be waited on).
    Exited(ExitStatus),
}

pub(crate) struct PtySession {
    parser: Arc<Mutex<Emulator>>,
    master: Box<dyn MasterPty + Send>,
    /// The child's pid, which is also its process group id.
    pid: Option<u32>,
    /// Set by the wait thread once the child has been reaped.
    exited: Arc<AtomicBool>,
    input: SyncSender<Vec<u8>>,
    /// (rows, cols), last applied.
    size: (u16, u16),
    killed: bool,
}

impl PtySession {
    /// Spawns `cmd` on a fresh PTY of `rows` x `cols`. The child runs in its
    /// own session (portable-pty `setsid`s it), so its pid is the process
    /// group id that [`kill`](Self::kill) signals. The receiver gets coalesced
    /// `Output` events whenever new output has been parsed, then exactly one
    /// `Exited` when the child is gone.
    pub(crate) fn spawn(
        cmd: CommandBuilder,
        rows: u16,
        cols: u16,
    ) -> anyhow::Result<(Self, tokio::sync::mpsc::Receiver<PtyEvent>)> {
        let program = cmd.get_argv().first().map_or_else(
            || "<empty>".to_string(),
            |a| a.to_string_lossy().into_owned(),
        );
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open pty")?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn {program}"))?;
        // Without this the master never sees EOF/EIO when the child exits,
        // so the reader thread would never end.
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            SCROLLBACK,
            Replies::default(),
        )));
        let (events_tx, events_rx) = tokio::sync::mpsc::channel::<PtyEvent>(1);
        let (input, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let reader_parser = Arc::clone(&parser);
        let replies_tx = input.clone();
        let reader_events = events_tx.clone();
        // A plain thread: the reads block for the session's whole lifetime
        // and should not hold a tokio blocking-pool slot.
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; READ_CHUNK];
                loop {
                    // EOF, or EIO on Linux once the slave side is closed.
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    // Programs block on DA/DSR/OSC queries until a terminal
                    // answers, so the emulator's replies go back to the child.
                    let replies = {
                        let mut p = lock(&reader_parser);
                        p.process(&buf[..n]);
                        p.callbacks_mut().take()
                    };
                    if !replies.is_empty() {
                        // Never block the reader: with a full queue the writer
                        // may be stuck on a child that is itself blocked on
                        // output only this thread drains.
                        let _ = replies_tx.try_send(replies);
                    }
                    // A full channel already means a redraw is pending.
                    let _ = reader_events.try_send(PtyEvent::Output);
                }
            })
            .context("spawn pty reader thread")?;

        let mut writer = pair.master.take_writer().context("take pty writer")?;
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for bytes in rx {
                    if writer
                        .write_all(&bytes)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .context("spawn pty writer thread")?;

        let pid = child.process_id();
        let exited = Arc::new(AtomicBool::new(false));
        let wait_exited = Arc::clone(&exited);
        let mut child = child;
        // A plain thread for the same reason as the reader: it blocks for the
        // session's whole lifetime.
        std::thread::Builder::new()
            .name("pty-wait".into())
            .spawn(move || {
                // An unwaitable child is unreachable, so treat it as exited.
                let status = child
                    .wait()
                    .unwrap_or_else(|_| ExitStatus::with_exit_code(1));
                // Flag first, so `kill` never depends on the receiver being
                // drained.
                wait_exited.store(true, Ordering::Release);
                // Unlike `Output`, this must not be coalesced away. Blocking is
                // safe because this is a std thread, not a runtime thread.
                let _ = events_tx.blocking_send(PtyEvent::Exited(status));
            })
            .context("spawn pty wait thread")?;

        let session = Self {
            parser,
            master: pair.master,
            pid,
            exited,
            input,
            size: (rows, cols),
            killed: false,
        };
        Ok((session, events_rx))
    }

    /// Queues `bytes` for the child. Never blocks the UI thread.
    pub(crate) fn write(&self, bytes: &[u8]) -> anyhow::Result<()> {
        match self.input.try_send(bytes.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => bail!("pty input queue full"),
            Err(TrySendError::Disconnected(_)) => bail!("pty writer closed"),
        }
    }

    /// Resizes the emulator, then the PTY (which sends SIGWINCH).
    pub(crate) fn resize(&mut self, rows: u16, cols: u16) -> anyhow::Result<()> {
        if (rows, cols) == self.size {
            return Ok(());
        }
        // Emulator first, deliberately: output the child redraws after
        // SIGWINCH is then parsed at the new size.
        lock(&self.parser).screen_mut().set_size(rows, cols);
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize pty")?;
        self.size = (rows, cols);
        Ok(())
    }

    pub(crate) fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        f(lock(&self.parser).screen())
    }

    /// Kills the child's whole process group: SIGHUP, a short grace, then
    /// SIGKILL; the wait thread reaps the leader. Idempotent.
    pub(crate) fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        if let Some(pid) = self.pid {
            signal_group("-HUP", pid);
            self.wait_exited();
            // Grandchildren may ignore HUP, or outlive an exited leader.
            signal_group("-KILL", pid);
        }
        self.wait_exited();
    }

    /// Waits up to [`KILL_GRACE`] for the wait thread to reap the child.
    fn wait_exited(&self) {
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline && !self.exited.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill();
    }
}

/// No libc or unsafe: signal the group through kill(1). portable-pty's own
/// killer HUPs only the leader pid.
fn signal_group(sig: &str, pid: u32) {
    let _ = std::process::Command::new("kill")
        .args([sig, "--", &format!("-{pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::Receiver;

    fn cmd(program: &str, args: &[&str]) -> CommandBuilder {
        let mut c = CommandBuilder::new(program);
        c.args(args);
        // portable-pty otherwise defaults to $HOME.
        c.cwd(std::env::temp_dir());
        c
    }

    async fn wait_until(
        session: &PtySession,
        redraw: &mut Receiver<PtyEvent>,
        pred: impl Fn(&vt100::Screen) -> bool,
    ) {
        loop {
            if session.with_screen(&pred) {
                return;
            }
            match tokio::time::timeout(Duration::from_secs(5), redraw.recv()).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if session.with_screen(&pred) {
                        return;
                    }
                    panic!(
                        "reader ended; screen: {:?}",
                        session.with_screen(|s| s.contents())
                    );
                }
                Err(_) => panic!(
                    "timed out; screen: {:?}",
                    session.with_screen(|s| s.contents())
                ),
            }
        }
    }

    fn pid_gone(pid: &str) -> bool {
        for _ in 0..200 {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !alive {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    async fn wait_exit(rx: &mut Receiver<PtyEvent>) -> ExitStatus {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(PtyEvent::Output)) => {}
                Ok(Some(PtyEvent::Exited(status))) => return status,
                Ok(None) => panic!("event channel closed without Exited"),
                Err(_) => panic!("timed out waiting for Exited"),
            }
        }
    }

    /// A child that sends `query`, reads exactly `reply_len` bytes of reply
    /// and prints them with ESC shown as `E`.
    fn query_cmd(query: &str, reply_len: usize) -> CommandBuilder {
        cmd(
            "sh",
            &[
                "-c",
                &format!(
                    "stty raw -echo; printf '{query}'; dd bs=1 count={reply_len} 2>/dev/null | tr '\\033' E"
                ),
            ],
        )
    }

    async fn assert_reply(query: &str, expected: &str, shown: &str) {
        let (s, mut rx) = PtySession::spawn(query_cmd(query, expected.len()), 24, 80).unwrap();
        wait_until(&s, &mut rx, |sc| sc.contents().contains(shown)).await;
    }

    #[tokio::test]
    async fn cursor_position_reply_reaches_the_child() {
        assert_reply("\\033[3;5H\\033[6n", "\x1b[3;5R", "E[3;5R").await;
    }

    #[tokio::test]
    async fn device_attributes_reply_reaches_the_child() {
        assert_reply("\\033[c", "\x1b[?1;2c", "E[?1;2c").await;
    }

    #[tokio::test]
    async fn osc_background_reply_reaches_the_child() {
        assert_reply(
            "\\033]11;?\\007",
            "\x1b]11;rgb:0000/0000/0000\x1b\\",
            "E]11;rgb:0000/0000/0000E\\",
        )
        .await;
    }

    #[tokio::test]
    async fn exit_is_reported() {
        let (_s, mut rx) = PtySession::spawn(cmd("sh", &["-c", "exit 3"]), 24, 80).unwrap();
        assert_eq!(wait_exit(&mut rx).await.exit_code(), 3);
    }

    #[tokio::test]
    async fn kill_reports_exit() {
        let (mut s, mut rx) = PtySession::spawn(cmd("cat", &[]), 24, 80).unwrap();
        s.kill();
        assert!(!wait_exit(&mut rx).await.success());
    }

    #[tokio::test]
    async fn sh_output_reaches_the_emulator() {
        let (s, mut rx) = PtySession::spawn(cmd("sh", &["-c", "printf hello"]), 24, 80).unwrap();
        wait_until(&s, &mut rx, |sc| sc.contents().contains("hello")).await;
    }

    #[tokio::test]
    async fn writes_reach_the_child() {
        let (s, mut rx) = PtySession::spawn(cmd("cat", &[]), 24, 80).unwrap();
        s.write(b"ping\r").unwrap();
        wait_until(&s, &mut rx, |sc| sc.contents().matches("ping").count() >= 2).await;
    }

    #[tokio::test]
    async fn resize_updates_emulator_and_pty() {
        let (mut s, mut rx) =
            PtySession::spawn(cmd("sh", &["-c", "read _; stty size"]), 24, 80).unwrap();
        s.resize(30, 100).unwrap();
        assert_eq!(s.with_screen(|sc| sc.size()), (30, 100));
        s.write(b"\r").unwrap();
        wait_until(&s, &mut rx, |sc| sc.contents().contains("30 100")).await;
    }

    #[tokio::test]
    async fn kill_takes_down_the_process_group() {
        let (mut s, mut rx) =
            PtySession::spawn(cmd("sh", &["-c", "sleep 30 & echo $!; wait"]), 24, 80).unwrap();
        wait_until(&s, &mut rx, |sc| {
            sc.contents()
                .lines()
                .any(|l| l.trim().parse::<u32>().is_ok())
        })
        .await;
        let pid = s
            .with_screen(|sc| sc.contents())
            .lines()
            .find(|l| l.trim().parse::<u32>().is_ok())
            .unwrap()
            .trim()
            .to_string();
        s.kill();
        assert!(pid_gone(&pid), "grandchild {pid} survived kill");
    }

    #[tokio::test]
    async fn drop_kills_the_child() {
        let (s, _rx) = PtySession::spawn(cmd("cat", &[]), 24, 80).unwrap();
        let pid = s.pid.unwrap().to_string();
        drop(s);
        assert!(pid_gone(&pid), "child {pid} survived drop");
    }
}
