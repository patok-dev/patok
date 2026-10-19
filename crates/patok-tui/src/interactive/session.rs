//! A child process on a PTY, its output parsed into a vt100 screen.

use std::io::{Read, Write};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const READ_CHUNK: usize = 8 * 1024;
/// Bound on queued writes to the child.
const INPUT_QUEUE: usize = 256;
/// Lines of local history, for wheel scrolling.
const SCROLLBACK: usize = 1000;
/// How long the group gets after SIGHUP before SIGKILL.
const KILL_GRACE: Duration = Duration::from_millis(200);

fn lock(parser: &Mutex<vt100::Parser>) -> MutexGuard<'_, vt100::Parser> {
    // A panicked reader must not take down rendering.
    parser.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct PtySession {
    parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    input: SyncSender<Vec<u8>>,
    /// (rows, cols), last applied.
    size: (u16, u16),
    killed: bool,
}

impl PtySession {
    /// Spawns `cmd` on a fresh PTY of `rows` x `cols`. The child runs in its
    /// own session (portable-pty `setsid`s it), so its pid is the process
    /// group id that [`kill`](Self::kill) signals. The receiver gets a
    /// coalesced `()` whenever new output has been parsed.
    pub(crate) fn spawn(
        cmd: CommandBuilder,
        rows: u16,
        cols: u16,
    ) -> anyhow::Result<(Self, tokio::sync::mpsc::Receiver<()>)> {
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

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let (redraw_tx, redraw_rx) = tokio::sync::mpsc::channel::<()>(1);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let reader_parser = Arc::clone(&parser);
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
                    lock(&reader_parser).process(&buf[..n]);
                    // A full channel already means a redraw is pending.
                    let _ = redraw_tx.try_send(());
                }
            })
            .context("spawn pty reader thread")?;

        let mut writer = pair.master.take_writer().context("take pty writer")?;
        let (input, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE);
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

        let session = Self {
            parser,
            master: pair.master,
            child,
            input,
            size: (rows, cols),
            killed: false,
        };
        Ok((session, redraw_rx))
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
    /// SIGKILL, and reaps the leader. Idempotent.
    pub(crate) fn kill(&mut self) {
        if self.killed {
            return;
        }
        self.killed = true;
        if let Some(pid) = self.child.process_id() {
            signal_group("-HUP", pid);
            let deadline = Instant::now() + KILL_GRACE;
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Grandchildren may ignore HUP, or outlive an exited leader.
            signal_group("-KILL", pid);
        }
        let _ = self.child.wait();
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
        redraw: &mut Receiver<()>,
        pred: impl Fn(&vt100::Screen) -> bool,
    ) {
        loop {
            if session.with_screen(&pred) {
                return;
            }
            match tokio::time::timeout(Duration::from_secs(5), redraw.recv()).await {
                Ok(Some(())) => {}
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
        let pid = s.child.process_id().unwrap().to_string();
        drop(s);
        assert!(pid_gone(&pid), "child {pid} survived drop");
    }
}
