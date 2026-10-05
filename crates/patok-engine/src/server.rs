//! The gRPC server: a thin adapter between the wire protocol and [`Engine`].

use std::os::unix::fs::DirBuilderExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use patok_core::config::ApplyTiming;
use patok_core::event::EngineEvent;
use patok_core::pipeline::{PipelineState, Stage, TileStatus};
use patok_proto::engine_server::{Engine as EngineApi, EngineServer};
use patok_proto::{
    AddTasksKind, AttachRequest, CommandRequest, CommandResponse, EngineInfo, EngineUpdate, Event,
    PipelineStage, PipelineState as ProtoPipelineState, PipelineTile, RunState, RunStateUpdate,
    SettingsChangeResult, ShutdownNotice, ShutdownRequest, ShutdownUpdate, Snapshot,
    TileStatus as ProtoTileStatus, command_request, engine_update, event, settings_change_result,
    shutdown_request, shutdown_update,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tonic::{Request, Response, Status};

use crate::engine::{Attachment, Engine, QueueCreation};

type Stream<T> = ReceiverStream<Result<T, Status>>;

/// How long open streams get to drain after the engine decides to exit.
const DRAIN: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct Service {
    engine: Engine,
    /// Why the engine is going away, reported to attached shells.
    reason: Arc<Mutex<String>>,
}

#[tonic::async_trait]
impl EngineApi for Service {
    type AttachStream = Stream<EngineUpdate>;
    type ShutdownStream = Stream<ShutdownUpdate>;

    async fn attach(
        &self,
        request: Request<AttachRequest>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        let shell_version = request.into_inner().shell_version;
        let attachment = self
            .engine
            .attach()
            .map_err(|e| Status::failed_precondition(e.to_string()))?;
        tracing::info!("shell attached (version {shell_version})");

        let (tx, rx) = mpsc::channel(256);
        let info = EngineInfo {
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            snapshot: Some(Snapshot {
                state: attachment.snapshot.to_state(),
                run_state: run_state(&self.engine) as i32,
                pipeline: Some(pipeline_proto(&attachment.snapshot.pipeline)),
            }),
        };
        let first = EngineUpdate {
            payload: Some(engine_update::Payload::Info(info)),
        };
        tx.try_send(Ok(first)).expect("fresh channel has room");
        tokio::spawn(forward(
            attachment,
            tx,
            self.engine.clone(),
            self.reason.clone(),
        ));
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn submit_command(
        &self,
        request: Request<CommandRequest>,
    ) -> Result<Response<CommandResponse>, Status> {
        let response = match request.into_inner().action {
            Some(command_request::Action::StartBuild(_)) => match self.engine.start_build() {
                Ok(()) => accepted(),
                Err(error) => rejected(error),
            },
            Some(command_request::Action::AddTasks(add)) => {
                // Which agent runs the append (T69.1): the planner expands a
                // user request; the two research kinds run the research
                // agent's queue-creation duty.
                let kind =
                    AddTasksKind::try_from(add.kind).unwrap_or(AddTasksKind::AddTasksPlanner);
                match kind {
                    AddTasksKind::AddTasksPlanner => {
                        if add.request.trim().is_empty() {
                            rejected("the request is empty".into())
                        } else {
                            // Runs in the background; planner output and the outcome reach the
                            // shell through the attach stream.
                            match self.engine.start_add_tasks(&add.request) {
                                Ok(()) => accepted(),
                                Err(error) => rejected(error),
                            }
                        }
                    }
                    AddTasksKind::AddTasksBootstrap => {
                        match self.engine.start_queue_creation(QueueCreation::Bootstrap) {
                            Ok(()) => accepted(),
                            Err(error) => rejected(error),
                        }
                    }
                    AddTasksKind::AddTasksScan => {
                        match self.engine.start_queue_creation(QueueCreation::Scan) {
                            Ok(()) => accepted(),
                            Err(error) => rejected(error),
                        }
                    }
                }
            }
            // Runs in the background; discovery output, the run state and the outcome
            // reach the shell through the attach stream.
            Some(command_request::Action::RunDiscovery(_)) => match self.engine.start_discovery() {
                Ok(()) => accepted(),
                Err(error) => rejected(error),
            },
            // Cancels a pending soft stop (T33.1): the loop continues with the
            // next pending task; rejected when no soft stop is pending.
            Some(command_request::Action::CancelSoftStop(_)) => {
                match self.engine.cancel_soft_stop() {
                    Ok(()) => accepted(),
                    Err(error) => rejected(error),
                }
            }
            // Injects one task line into TASKS.md under the engine's
            // task-file lock (T77.1): the typed text is normalized into a
            // well-formed unchecked task line first, and accepted in every
            // engine state, so the shell's inject modal confirms even
            // mid-build; the appended line reaches the shell as a task-list
            // change right away.
            Some(command_request::Action::InjectTask(inject)) => {
                match self.engine.inject_task(&inject.text).await {
                    Ok(()) => accepted(),
                    Err(error) => rejected(error),
                }
            }
            // One daemon-schema field change: validated,
            // persisted to the resolved layer and merged into the engine's central
            // config; the new value reaches the shell as a ConfigChanged event.
            Some(command_request::Action::SettingsChange(change)) => match change.change {
                Some(change) => {
                    let (field, value) = crate::settings::from_proto(&change);
                    match self.engine.apply_settings_change(&field, value) {
                        Ok(ApplyTiming::Immediate) => {
                            settings_accepted(settings_change_result::ApplyTiming::Immediate)
                        }
                        Ok(ApplyTiming::NextUnitOfWork) => {
                            settings_accepted(settings_change_result::ApplyTiming::NextUnitOfWork)
                        }
                        Err(error) => rejected(error),
                    }
                }
                None => rejected("the settings change is empty".into()),
            },
            Some(_) => rejected("this command is not supported yet".into()),
            None => return Err(Status::invalid_argument("empty command")),
        };
        Ok(Response::new(response))
    }

    async fn shutdown(
        &self,
        request: Request<ShutdownRequest>,
    ) -> Result<Response<Self::ShutdownStream>, Status> {
        let scope = request.into_inner().scope();
        let now = scope == shutdown_request::Scope::Now;
        // A NOW stop on a busy engine is an interrupt (T35.1): the running unit is
        // cancelled and the engine returns to idle and keeps serving. Only a NOW
        // stop on an idle engine quits the process, so only that case labels the
        // exit -- leaving the reason set on an interrupt would mislabel a later
        // signal-driven exit.
        let busy =
            self.engine.is_running() || self.engine.is_planning() || self.engine.is_discovering();
        let (tx, rx) = mpsc::channel(8);
        let engine = self.engine.clone();
        if now && !busy {
            *self.reason.lock().expect("reason lock") = "requested via shutdown request".into();
        }
        tokio::spawn(async move {
            let update = |phase, message: &str| {
                Ok(ShutdownUpdate {
                    phase: phase as i32,
                    message: message.into(),
                })
            };
            // Registered and enabled before the stop is requested, so a
            // CancelSoftStop landing immediately after it is not missed (the
            // same pattern `wait_until_idle` uses).
            let soft_stop_cancelled = engine.soft_stop_cancelled().notified();
            tokio::pin!(soft_stop_cancelled);
            soft_stop_cancelled.as_mut().enable();
            engine.request_stop(now);
            let started = match (busy, now) {
                (false, true) => "Shutting down.",
                (true, true) => {
                    "Interrupting -- cancelling the current task; the app keeps running."
                }
                (false, false) => "Soft stop requested -- nothing is running.",
                (true, false) => "Soft stop requested -- the build stops after the current task.",
            };
            let _ = tx
                .send(update(shutdown_update::Phase::Started, started))
                .await;
            // A soft stop waits for the running task; a NOW stop does not, so the
            // shell's interrupting message stays on screen until the stream ends.
            if busy && !now {
                let _ = tx
                    .send(update(
                        shutdown_update::Phase::InProgress,
                        "Waiting for the running task to end...",
                    ))
                    .await;
                // The pending soft stop can be cancelled (T33.1): the loop
                // keeps going, so end the stream with a cancelled update
                // instead of lingering until the whole build finishes.
                tokio::select! {
                    () = engine.wait_until_idle() => {}
                    () = &mut soft_stop_cancelled => {
                        let _ = tx
                            .send(update(
                                shutdown_update::Phase::Complete,
                                "Soft stop cancelled -- the build keeps running.",
                            ))
                            .await;
                        return;
                    }
                }
            } else {
                engine.wait_until_idle().await;
            }
            if now && !busy {
                let _ = tx
                    .send(update(shutdown_update::Phase::Complete, "Engine stopped."))
                    .await;
                engine.shutdown_token().cancel();
            } else {
                // A stop is not a quit: release the stop so commands work again
                // and report the idle state. The shutdown token stays intact --
                // the engine keeps serving.
                engine.end_stop();
                let message = if now {
                    "Interrupted -- the current task was cancelled; the engine is idle and keeps running."
                } else {
                    "Stopped after the current task -- the engine is idle and keeps running."
                };
                let _ = tx
                    .send(update(shutdown_update::Phase::Complete, message))
                    .await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

fn accepted() -> CommandResponse {
    CommandResponse {
        accepted: true,
        error: String::new(),
        settings_change_result: None,
    }
}

/// A settings change applied by the engine: every daemon-schema field resolves to the
/// project-local `patok.config.toml` today.
fn settings_accepted(timing: settings_change_result::ApplyTiming) -> CommandResponse {
    CommandResponse {
        accepted: true,
        error: String::new(),
        settings_change_result: Some(SettingsChangeResult {
            written_layer: settings_change_result::Layer::ProjectLocal as i32,
            apply_timing: timing as i32,
        }),
    }
}

fn rejected(error: String) -> CommandResponse {
    CommandResponse {
        accepted: false,
        error,
        settings_change_result: None,
    }
}

/// The engine's current run state for the wire protocol: the planner or a discovery round
/// first (a scheduled round runs in a session's empty-queue gap, while the phase is still
/// `Running`), else the build loop, else idle.
fn run_state(engine: &Engine) -> RunState {
    if engine.is_planning() {
        RunState::Planning
    } else if engine.is_discovering() {
        RunState::Discovering
    } else if engine.is_running() {
        RunState::Running
    } else {
        RunState::Idle
    }
}

/// The typed run-state update implied by an event, if it can change the run state.
fn run_state_update(engine: &Engine, event: &EngineEvent) -> Option<RunState> {
    match event {
        EngineEvent::PhaseChanged { .. }
        | EngineEvent::PlanningChanged { .. }
        | EngineEvent::DiscoveryChanged { .. } => Some(run_state(engine)),
        _ => None,
    }
}

/// The pipeline rail for the wire protocol: the typed
/// form of the state the engine recomputes on every rail change.
fn pipeline_proto(state: &PipelineState) -> ProtoPipelineState {
    ProtoPipelineState {
        stages: state
            .stages
            .iter()
            .map(|tile| PipelineTile {
                stage: stage_proto(tile.stage) as i32,
                status: tile_status_proto(tile.status) as i32,
            })
            .collect(),
        ship: tile_status_proto(state.ship) as i32,
        discover: tile_status_proto(state.discover) as i32,
        learnings: tile_status_proto(state.learnings) as i32,
    }
}

fn stage_proto(stage: Stage) -> PipelineStage {
    match stage {
        Stage::Research => PipelineStage::Research,
        Stage::Plan => PipelineStage::Plan,
        Stage::Build => PipelineStage::Build,
        Stage::Review => PipelineStage::Review,
    }
}

fn tile_status_proto(status: TileStatus) -> ProtoTileStatus {
    match status {
        TileStatus::Muted => ProtoTileStatus::Muted,
        TileStatus::Pending => ProtoTileStatus::Pending,
        TileStatus::Active => ProtoTileStatus::Active,
        TileStatus::Done => ProtoTileStatus::Done,
    }
}

/// Pushes live events to the attached shell until it goes away or the engine shuts down.
/// Dropping `attachment` on the way out frees the attach slot.
async fn forward(
    mut attachment: Attachment,
    tx: mpsc::Sender<Result<EngineUpdate, Status>>,
    engine: Engine,
    reason: Arc<Mutex<String>>,
) {
    let shutdown = engine.shutdown_token();
    loop {
        let (payload, run_state) = tokio::select! {
            () = tx.closed() => break,
            () = shutdown.cancelled() => {
                let reason = reason.lock().expect("reason lock").clone();
                let _ = tx.send(Ok(EngineUpdate {
                    payload: Some(engine_update::Payload::ShutdownNotice(ShutdownNotice { reason })),
                })).await;
                break;
            }
            received = attachment.events.recv() => match received {
                Ok(event) => (wire(&event), run_state_update(&engine, &event)),
                Err(broadcast::error::RecvError::Lagged(n)) => (wire(&EngineEvent::Notice {
                    level: patok_core::event::NoticeLevel::Warning,
                    text: format!("The shell fell behind; {n} events were dropped."),
                }), None),
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        if tx
            .send(Ok(EngineUpdate {
                payload: Some(payload),
            }))
            .await
            .is_err()
        {
            break;
        }
        // A run-state change arrives as its own typed update right after the event.
        if let Some(state) = run_state
            && tx
                .send(Ok(EngineUpdate {
                    payload: Some(engine_update::Payload::RunState(RunStateUpdate {
                        state: state as i32,
                    })),
                }))
                .await
                .is_err()
        {
            break;
        }
    }
    tracing::info!("shell detached");
}

fn wire(event: &EngineEvent) -> engine_update::Payload {
    // The pipeline-state change rides the typed oneof;
    // `kind`/`payload` stay set for the replay path through the recent events.
    let typed = match event {
        EngineEvent::PipelineChanged { state } => {
            Some(event::Typed::PipelineState(pipeline_proto(state)))
        }
        _ => None,
    };
    engine_update::Payload::Event(Event {
        kind: event.kind().into(),
        payload: event.to_payload(),
        typed,
    })
}

/// Creates the per-project runtime directory (and the runtime root) readable by this user only.
pub fn prepare_runtime_dir(dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))
}

/// Binds the engine socket and serves until the engine's shutdown token fires.
pub async fn serve(engine: Engine) -> anyhow::Result<()> {
    let config = engine.config().clone();
    prepare_runtime_dir(&config.runtime_dir)?;

    let socket = config.socket_path();
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!(
                "an engine is already running for {}",
                config.project_dir.display()
            );
        }
        // Left behind by an unclean shutdown.
        std::fs::remove_file(&socket)
            .with_context(|| format!("cannot remove stale {}", socket.display()))?;
    }
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("cannot bind {}", socket.display()))?;
    tracing::info!(
        "engine {} listening on {}",
        env!("CARGO_PKG_VERSION"),
        socket.display()
    );

    // The startup cleanup runs before the watcher's first reconcile and before
    // any build, planner or discovery run can start (T72.1).
    engine.cleanup_completed_tasks().await;
    engine.spawn_task_file_watch();
    engine.spawn_config_watch();
    spawn_idle_watch(engine.clone());
    spawn_signal_handlers(engine.clone())?;

    let shutdown = engine.shutdown_token();
    let service = Service {
        engine,
        reason: Arc::new(Mutex::new("engine shutting down".into())),
    };
    let mut server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(EngineServer::new(service))
            .serve_with_incoming_shutdown(
                UnixListenerStream::new(listener),
                shutdown.clone().cancelled_owned(),
            ),
    );

    tokio::select! {
        result = &mut server => result??,
        () = shutdown.cancelled() => {
            if tokio::time::timeout(DRAIN, &mut server).await.is_err() {
                tracing::warn!("streams did not drain within {DRAIN:?}; exiting anyway");
            }
        }
    }
    let _ = std::fs::remove_file(&socket);
    tracing::info!("engine stopped");
    Ok(())
}

/// Soft-shuts the engine down after it has been idle with no shell attached for the
/// configured duration. The limit is read from
/// the central settings on every poll -- a point-of-use read:
/// it only counts while no unit of work is active, so a change cannot contradict a
/// running unit's snapshot.
fn spawn_idle_watch(engine: Engine) {
    tokio::spawn(async move {
        let mut idle_since = None;
        loop {
            let limit = Duration::from_secs(engine.settings().engine_idle_timeout_secs);
            let poll = (limit / 10).clamp(Duration::from_millis(50), Duration::from_secs(30));
            tokio::time::sleep(poll).await;
            if !engine.is_idle() {
                idle_since = None;
            } else if idle_since
                .get_or_insert_with(tokio::time::Instant::now)
                .elapsed()
                >= limit
            {
                tracing::info!("idle for {limit:?} with no shell attached; shutting down");
                engine.request_stop(false);
                engine.shutdown_token().cancel();
                return;
            }
        }
    });
}

/// SIGHUP is ignored (the engine outlives its terminal); SIGTERM/SIGINT stop it now.
fn spawn_signal_handlers(engine: Engine) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::spawn(async move { while hangup.recv().await.is_some() {} });
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        tracing::info!("termination signal received");
        engine.request_stop(true);
        engine.wait_until_idle().await;
        engine.shutdown_token().cancel();
    });
    Ok(())
}
