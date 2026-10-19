//! Fleet monitor: follows the runtime root and reports engines as they come and go. Filesystem
//! events are only hints; each one triggers a rescan and a status probe of any new socket.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use patok_core::fleet;
use patok_core::paths::{DaemonMeta, meta_path};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::status::{DaemonStatus, StatusStream, StatusUpdate, probe};

/// How often the root is rescanned when the filesystem watcher is unavailable.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Waits before each probe attempt, for daemons that have bound no listener yet.
const BACKOFF: [Duration; 6] = [
    Duration::ZERO,
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];
/// Upper bound on one probe attempt.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// A change in the fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetEvent {
    /// A live daemon was found.
    Added(DaemonStatus),
    /// A known daemon's status changed.
    Changed(DaemonStatus),
    /// A known daemon went away; carries its socket path.
    Removed(PathBuf),
}

/// A running fleet monitor. Dropping it stops the background task.
pub struct FleetMonitor {
    events: mpsc::Receiver<FleetEvent>,
    _stop: DropGuard,
}

impl FleetMonitor {
    /// The next fleet event; `None` once the monitor task has ended.
    pub async fn next(&mut self) -> Option<FleetEvent> {
        self.events.recv().await
    }
}

/// Starts monitoring the runtime `root`. The watch is in place before the first scan, so no
/// daemon is missed; if no watcher can be created the root is polled instead. Must be called
/// inside a tokio runtime.
pub fn monitor(root: PathBuf) -> FleetMonitor {
    start(root, true)
}

/// Like [`monitor`] but polls from the start, without a watcher. This runs exactly the code
/// path `monitor` takes when `notify::recommended_watcher` fails (`watcher == None`).
#[cfg(any(test, feature = "test-support"))]
pub fn monitor_polling(root: PathBuf) -> FleetMonitor {
    start(root, false)
}

fn start(root: PathBuf, watch: bool) -> FleetMonitor {
    let (events_tx, events) = mpsc::channel(64);
    let token = CancellationToken::new();
    let stop = token.clone().drop_guard();
    tokio::spawn(run(root, watch, events_tx, token));
    FleetMonitor {
        events,
        _stop: stop,
    }
}

/// Identity of a file on disk; a rebound socket gets a new one.
type FileId = (u64, u64, i64, i64);

fn file_id(path: &Path) -> Option<FileId> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some((meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()))
}

enum Msg {
    Hint,
    Probed {
        socket: PathBuf,
        id: Option<FileId>,
        result: Box<anyhow::Result<StatusStream>>,
    },
    Update {
        socket: PathBuf,
        generation: u64,
        update: StatusUpdate,
    },
}

struct Tracked {
    status: DaemonStatus,
    /// Identity of the socket file when adopted; retired if the daemon goes away.
    id: Option<FileId>,
    generation: u64,
    follower: AbortHandle,
}

struct State {
    root: PathBuf,
    tx: mpsc::UnboundedSender<Msg>,
    watcher: Option<RecommendedWatcher>,
    watched_root: Option<PathBuf>,
    watched_dirs: HashSet<PathBuf>,
    tracked: HashMap<PathBuf, Tracked>,
    pending: HashSet<PathBuf>,
    /// Sockets that failed a probe or reported `Gone`, by the file they were; never re-probed.
    retired: HashMap<PathBuf, FileId>,
    tasks: JoinSet<()>,
    next_generation: u64,
    out: Vec<FleetEvent>,
}

async fn run(
    root: PathBuf,
    watch: bool,
    events: mpsc::Sender<FleetEvent>,
    token: CancellationToken,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut state = State {
        root,
        tx: tx.clone(),
        watcher: None,
        watched_root: None,
        watched_dirs: HashSet::new(),
        tracked: HashMap::new(),
        pending: HashSet::new(),
        retired: HashMap::new(),
        tasks: JoinSet::new(),
        next_generation: 0,
        out: Vec::new(),
    };
    if watch {
        state.watcher = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            let _ = tx.send(Msg::Hint);
        })
        .ok();
    }
    if state.watcher.is_some() {
        state.ensure_watches();
        state.reconcile();
    }
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        if !flush(&mut state, &events).await {
            return;
        }
        let polling = state.watcher.is_none();
        let mut dirty = false;
        tokio::select! {
            () = token.cancelled() => return,
            _ = interval.tick(), if polling => dirty = true,
            Some(_) = state.tasks.join_next(), if !state.tasks.is_empty() => {}
            msg = rx.recv() => {
                let Some(msg) = msg else { return };
                dirty |= state.handle(msg);
                while let Ok(msg) = rx.try_recv() {
                    dirty |= state.handle(msg);
                }
            }
        }
        if dirty {
            state.reconcile();
        }
    }
}

async fn flush(state: &mut State, events: &mpsc::Sender<FleetEvent>) -> bool {
    for event in std::mem::take(&mut state.out) {
        if events.send(event).await.is_err() {
            return false;
        }
    }
    true
}

impl State {
    /// Handles one message; true when a reconcile should follow.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Hint => true,
            Msg::Probed { socket, id, result } => {
                self.pending.remove(&socket);
                match *result {
                    Ok(stream) => {
                        if self.tracked.contains_key(&socket) || !socket.exists() {
                            return true;
                        }
                        self.adopt(socket, id, stream);
                    }
                    Err(_) => {
                        if let Some(id) = id {
                            self.retired.insert(socket, id);
                        }
                    }
                }
                true
            }
            Msg::Update {
                socket,
                generation,
                update,
            } => {
                let Some(tracked) = self.tracked.get_mut(&socket) else {
                    return false;
                };
                if tracked.generation != generation {
                    return false;
                }
                match update {
                    StatusUpdate::Changed(status) => {
                        tracked.status = status.clone();
                        self.out.push(FleetEvent::Changed(status));
                    }
                    StatusUpdate::Gone => {
                        if let Some(id) = self.tracked.remove(&socket).and_then(|t| t.id) {
                            self.retired.insert(socket.clone(), id);
                        }
                        self.out.push(FleetEvent::Removed(socket));
                    }
                }
                false
            }
        }
    }

    fn adopt(&mut self, socket: PathBuf, id: Option<FileId>, mut stream: StatusStream) {
        let generation = self.next_generation;
        self.next_generation += 1;
        let status = stream.status().clone();
        self.out.push(FleetEvent::Added(status.clone()));
        let tx = self.tx.clone();
        let key = socket.clone();
        let follower = self.tasks.spawn(async move {
            loop {
                let update = stream.next().await;
                let gone = matches!(update, StatusUpdate::Gone);
                let msg = Msg::Update {
                    socket: key.clone(),
                    generation,
                    update,
                };
                if tx.send(msg).is_err() || gone {
                    break;
                }
            }
        });
        self.tracked.insert(
            socket,
            Tracked {
                status,
                id,
                generation,
                follower,
            },
        );
    }

    /// Watches the root (or the nearest existing ancestor while it is missing) and every
    /// project directory under it. Any failure drops the watcher, leaving polling.
    fn ensure_watches(&mut self) {
        if self.try_watches().is_none() {
            self.watcher = None;
        }
    }

    fn try_watches(&mut self) -> Option<()> {
        let watcher = self.watcher.as_mut()?;
        let target = if self.root.exists() {
            self.root.clone()
        } else {
            self.root
                .ancestors()
                .skip(1)
                .find(|p| p.exists())?
                .to_path_buf()
        };
        if self.watched_root.as_ref() != Some(&target) {
            watcher.watch(&target, RecursiveMode::NonRecursive).ok()?;
            if let Some(old) = self.watched_root.replace(target) {
                let _ = watcher.unwatch(&old);
            }
        }
        let mut dirs = HashSet::new();
        if let Ok(read_dir) = std::fs::read_dir(&self.root) {
            for entry in read_dir.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs.insert(entry.path());
                }
            }
        }
        for dir in &dirs {
            if !self.watched_dirs.contains(dir) {
                match watcher.watch(dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        self.watched_dirs.insert(dir.clone());
                    }
                    // Removed between listing and watching; the next hint sorts it out.
                    Err(_) if !dir.exists() => {}
                    Err(_) => return None,
                }
            }
        }
        self.watched_dirs.retain(|dir| {
            let keep = dirs.contains(dir);
            if !keep {
                let _ = watcher.unwatch(dir);
            }
            keep
        });
        Some(())
    }

    fn reconcile(&mut self) {
        if self.watcher.is_some() {
            self.ensure_watches();
        }
        let scanned: Vec<PathBuf> = fleet::scan(&self.root)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.socket_path)
            .collect();

        let gone: Vec<PathBuf> = self
            .tracked
            .keys()
            .filter(|socket| !scanned.contains(socket))
            .cloned()
            .collect();
        for socket in gone {
            if let Some(tracked) = self.tracked.remove(&socket) {
                tracked.follower.abort();
                self.out.push(FleetEvent::Removed(socket));
            }
        }

        // The socket is bound before `meta.json` is written; fill in the pid once it appears.
        for (socket, tracked) in &mut self.tracked {
            if tracked.status.pid.is_some() {
                continue;
            }
            let Some(dir) = socket.parent() else { continue };
            if let Ok(meta) = DaemonMeta::read(&meta_path(dir)) {
                tracked.status.pid = Some(meta.pid);
                tracked.status.started_at = Some(meta.started_at);
                self.out.push(FleetEvent::Changed(tracked.status.clone()));
            }
        }

        for socket in &scanned {
            if self.tracked.contains_key(socket) || self.pending.contains(socket) {
                continue;
            }
            let id = file_id(socket);
            if id.is_some() && self.retired.get(socket) == id.as_ref() {
                continue;
            }
            self.pending.insert(socket.clone());
            let tx = self.tx.clone();
            let socket = socket.clone();
            self.tasks.spawn(async move {
                let result = probe_with_retry(&socket).await;
                let result = Box::new(result);
                let _ = tx.send(Msg::Probed { socket, id, result });
            });
        }

        self.retired.retain(|socket, _| socket.exists());
    }
}

async fn probe_with_retry(socket: &Path) -> anyhow::Result<StatusStream> {
    let mut last = anyhow::anyhow!("no probe attempt was made");
    for delay in BACKOFF {
        tokio::time::sleep(delay).await;
        if !socket.exists() {
            break;
        }
        match tokio::time::timeout(PROBE_TIMEOUT, probe(socket)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(err)) => last = err,
            Err(_) => last = anyhow::anyhow!("probe of {} timed out", socket.display()),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delays_are_short_and_increasing() {
        assert!(BACKOFF.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(BACKOFF.iter().sum::<Duration>() < Duration::from_secs(2));
    }
}
