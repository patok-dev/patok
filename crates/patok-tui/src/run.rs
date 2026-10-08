//! The shell's driver: terminal setup and the event loop merging keyboard input, the engine's
//! update stream and a tick.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use crossterm::event::{Event as TermEvent, EventStream};
use futures::StreamExt;
use futures::future::BoxFuture;
use patok_core::config::{SettingValue, Theme as ThemeKey};
use patok_core::event::{EngineEvent, Snapshot};
use patok_proto::engine_client::EngineClient;
use patok_proto::{
    AddTasks, AddTasksKind, AttachRequest, CancelSoftStop, CommandRequest, InjectTask,
    RunDiscovery, SettingsChange, ShutdownRequest, StartBuild, command_request, engine_update,
    shutdown_request, shutdown_update,
};
use ratatui::backend::Backend;
use ratatui::{DefaultTerminal, Terminal};
use tonic::Streaming;
use tonic::transport::Channel;

use crate::app::{Action, App, QueueRun};
use crate::overlay::Schema;
use crate::project;
use crate::settings::{ShellSettings, to_proto};
use crate::ui;

const TICK: Duration = Duration::from_millis(100);
/// Config files are polled every ~20 ticks, about 2 seconds, like the task file
///.
const CONFIG_FILE_POLL_TICKS: u32 = 20;

/// How the shell session ended.
#[derive(Debug)]
pub enum Outcome {
    /// The engine stopped (requested here, elsewhere, or by itself).
    EngineStopped { reason: String, completed: usize },
    /// The shell left; the engine keeps running.
    Detached { completed: usize },
}

/// Starts a fresh engine (after the old one has exited) and connects to it.
pub type Spawner =
    Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<EngineClient<Channel>>> + Send + Sync>;

/// XTerm's modifyOtherKeys mode (T65.1): level 2 makes terminals without the
/// kitty keyboard protocol encode modified keys as CSI u sequences, so
/// Shift-Enter arrives as `CSI 13;2u`, which crossterm parses into Enter with
/// the SHIFT modifier and the dialog's line-break arm runs. 0 resets the mode.
struct ModifyOtherKeys(u8);

impl crossterm::Command for ModifyOtherKeys {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        if self.0 == 0 {
            write!(f, "\x1b[>4m")
        } else {
            write!(f, "\x1b[>{};{}m", 4, self.0)
        }
    }

    // Windows reports Shift natively, so the ANSI path needs no WinAPI work.
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Attaches to the engine and runs the interactive UI until the user leaves or the engine exits.
/// `spawn` backs the restart command; `project_dir` locates the project-local config file.
pub async fn run(
    mut client: EngineClient<Channel>,
    spawn: Spawner,
    project_dir: PathBuf,
) -> anyhow::Result<Outcome> {
    let (mut app, updates) = attach(&mut client).await?;
    // The project facts behind the idle scenario are scanned before the first
    // frame renders.
    app.project_status = probe_status(&project_dir).await;

    let mut terminal = ratatui::init();
    // Keyboard encoding (T62.1, T65.1): the kitty flags make kitty-capable
    // terminals encode every key press as a CSI u sequence, so Shift-Enter
    // arrives as Enter with the SHIFT modifier. Terminals without the kitty
    // protocol ignore that push and would send a bare `\r` for Shift-Enter,
    // so XTerm's modifyOtherKeys level 2 is enabled too -- xterm, VTE,
    // iTerm2 and friends then encode Shift-Enter as `CSI 13;2u`, which
    // crossterm parses into Enter with the SHIFT modifier. A terminal with
    // neither mode still sends a bare `\r`, where Shift-Enter submits.
    let _ = crossterm::execute!(
        std::io::stdout(),
        ModifyOtherKeys(2),
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        ),
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture
    );
    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut client,
        updates,
        &spawn,
        &project_dir,
    )
    .await;
    let _ = crossterm::execute!(
        std::io::stdout(),
        ModifyOtherKeys(0),
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture
    );
    ratatui::restore();
    result
}

/// Attaches and builds the shell state from the engine's first (info) message. Shared
/// with the headless driver, so both attach through the identical snapshot decode.
pub(crate) async fn attach(
    client: &mut EngineClient<Channel>,
) -> anyhow::Result<(App, Streaming<patok_proto::EngineUpdate>)> {
    let mut updates = client
        .attach(AttachRequest {
            shell_version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .await
        .map_err(|status| anyhow!("{}", status.message()))?
        .into_inner();

    // The first update is always the engine info; the first frame renders from it.
    let first = updates
        .message()
        .await?
        .context("the engine closed the connection during attach")?;
    let Some(engine_update::Payload::Info(info)) = first.payload else {
        anyhow::bail!("the engine did not start with its info message");
    };
    let snapshot =
        Snapshot::from_state(&info.snapshot.context("attach carried no snapshot")?.state)
            .context("unreadable engine snapshot")?;
    let mut app = App::new(snapshot, info.engine_version);
    app.check_version(env!("CARGO_PKG_VERSION"));
    Ok((app, updates))
}

/// The restart command: soft-stop handshake, fresh engine, reattach. The shell stays open.
pub async fn restart<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    client: &mut EngineClient<Channel>,
    updates: &mut Streaming<patok_proto::EngineUpdate>,
    spawn: &Spawner,
) -> anyhow::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    // A SOFT stop only winds the build loop down (the engine keeps serving), so a
    // second, NOW shutdown makes the old process actually exit before the fresh
    // one binds the socket. Nothing runs anymore at that point, so the NOW stop
    // lands while the engine is idle -- the true quit, not an interrupt.
    for scope in [shutdown_request::Scope::Soft, shutdown_request::Scope::Now] {
        let mut progress = client
            .shutdown(ShutdownRequest {
                scope: scope as i32,
            })
            .await
            .context("could not request engine shutdown")?
            .into_inner();
        // The drain draws render with a fresh clock, so the timer does not sit on
        // the stale tick from before the restart.
        app.now.set(Instant::now());
        while let Some(update) = progress
            .message()
            .await
            .context("lost the engine connection while restarting")?
        {
            app.status = Some(update.message);
            terminal.draw(|frame| ui::render(frame, app))?;
        }
    }
    app.status = Some("Starting a fresh engine...".into());
    terminal.draw(|frame| ui::render(frame, app))?;
    *client = spawn().await.context("could not start a fresh engine")?;
    let (mut fresh, fresh_updates) = attach(client).await?;
    fresh.completed = app.completed;
    fresh.status = Some("Engine restarted.".into());
    *app = fresh;
    *updates = fresh_updates;
    // The fresh engine may see a different project state; rescan before drawing.
    let dir = std::path::PathBuf::from(app.project.clone());
    app.project_status = probe_status(&dir).await;
    Ok(())
}

/// Scans the project's files off the render path; a failed scan leaves whatever
/// the app already held.
async fn probe_status(dir: &std::path::Path) -> patok_core::scenario::ProjectScan {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || patok_core::scenario::scan_project(&dir))
        .await
        .unwrap_or_default()
}

/// What running one shutdown-family action left the event loop with.
enum ShutdownFlow {
    /// The loop returns this outcome: the shell leaves, the engine keeps
    /// running.
    Detach(Outcome),
    /// A shutdown was requested; the caller stores this pair and its
    /// `next_shutdown` branch polls it. The stream is boxed to keep the enum
    /// small.
    Pending(
        shutdown_request::Scope,
        Box<Streaming<patok_proto::ShutdownUpdate>>,
    ),
    /// Keep looping as before: the action was not a shutdown-family one, or an
    /// interrupt hit an engine that is already exiting -- the pending
    /// shutdown stream finishes the cleanup.
    Keep,
}

/// The shutdown RPC's result, as the shared dispatch asks the engine for it.
type ShutdownCall = Result<tonic::Response<Streaming<patok_proto::ShutdownUpdate>>, tonic::Status>;

/// The status line the driver shows while an interrupt runs: the true quit's
/// stopping message while idle, the keep-running notice otherwise.
fn interrupt_status(app: &App) -> String {
    if app.is_idle() {
        "Stopping the engine...".to_string()
    } else {
        "Interrupting -- the current task is cancelled and the app keeps running.".to_string()
    }
}

/// Runs one shutdown-family action's dispatch (T143.1): the one dispatch both
/// the key arm and the mouse arm of [`event_loop`] route `Detach`, `Quit` and
/// `Interrupt` through, so a click runs exactly its key's flow. `request` is
/// the shutdown RPC, a generic function instead of a concrete `EngineClient`,
/// so tests pass fakes; the event loop needs a terminal and a live engine,
/// and tonic's `Streaming` has no public constructor, so the Ok paths stay
/// covered by the e2e protocol tests.
async fn run_shutdown_family<Req, Fut>(
    app: &mut App,
    action: &Action,
    request: Req,
) -> anyhow::Result<ShutdownFlow>
where
    Req: FnOnce(shutdown_request::Scope) -> Fut,
    Fut: std::future::Future<Output = ShutdownCall>,
{
    match action {
        // The `d` key: the shell leaves, the engine keeps running.
        Action::Detach => Ok(ShutdownFlow::Detach(Outcome::Detached {
            completed: app.completed,
        })),
        // A quit (the first `q` while busy): a SOFT-scope shutdown -- the
        // current task finishes, no further task starts, the engine keeps
        // running and the shell stays open. The app state already carries the
        // pending-stop status, so it is left alone; a transport failure
        // propagates with the `q` key's message.
        Action::Quit => {
            let stream = request(shutdown_request::Scope::Soft)
                .await
                .context("could not request engine shutdown")?
                .into_inner();
            Ok(ShutdownFlow::Pending(
                shutdown_request::Scope::Soft,
                Box::new(stream),
            ))
        }
        // An interrupt (the second `q`, or the interrupt choice of the stop
        // dialog): cancel the current task and stop the build loop without
        // waiting. While idle (the plain `q`) it is the true quit: the engine
        // exits and the shell ends with it. A failed request is tolerated:
        // the engine is likely already exiting (the soft-stop stream is still
        // draining), so the loop keeps polling that stream instead of failing
        // the shell.
        Action::Interrupt => {
            app.status = Some(interrupt_status(app));
            match request(shutdown_request::Scope::Now).await {
                Ok(stream) => Ok(ShutdownFlow::Pending(
                    shutdown_request::Scope::Now,
                    Box::new(stream.into_inner()),
                )),
                Err(status) => {
                    app.status = Some(format!("interrupt failed: {}", status.message()));
                    Ok(ShutdownFlow::Keep)
                }
            }
        }
        _ => Ok(ShutdownFlow::Keep),
    }
}

/// The submit RPC's result, as the shared dispatch asks the engine for it.
type CommandCall = Result<tonic::Response<patok_proto::CommandResponse>, tonic::Status>;

/// Whether the shared submit dispatch consumed the action.
enum SubmitFlow {
    /// The action was one of the submit family and its flow ran.
    Handled,
    /// Not a submit-family action: the caller's own arms handle it.
    Keep,
}

/// Runs one submit-family action's dispatch (T144.1): the one dispatch both
/// the key arm and the mouse arm of [`event_loop`] route `SubmitTasks`,
/// `ResearchQueue`, `SaveBrief` and `InjectTask` through, so a click on the
/// add-task dialog's or the inject-task modal's primary footer button runs
/// exactly its Enter key's flow. `submit` is the submit RPC, a generic
/// function instead of a concrete `EngineClient`, so tests pass fakes (the
/// Ok paths are unit-testable here: `CommandResponse` is a plain message).
async fn run_submit_family<Req, Fut>(
    app: &mut App,
    project_dir: &std::path::Path,
    action: &Action,
    submit: Req,
) -> SubmitFlow
where
    Req: FnOnce(CommandRequest) -> Fut,
    Fut: std::future::Future<Output = CommandCall>,
{
    match action {
        // The add-task dialog's submit with text: the planner expands the
        // user's request into appended T tasks.
        Action::SubmitTasks(request) => {
            add_tasks(app, submit, request.clone(), AddTasksKind::AddTasksPlanner).await;
        }
        // The research queue-creation runs (T69.1): the Research agent
        // investigates the project and appends the tasks.
        Action::ResearchQueue(run) => {
            let kind = match run {
                QueueRun::Bootstrap => AddTasksKind::AddTasksBootstrap,
                QueueRun::Scan => AddTasksKind::AddTasksScan,
            };
            add_tasks(app, submit, run.request().to_string(), kind).await;
        }
        // The brief is written by the shell itself (like its config files);
        // the dialog stays open with the created message and the re-scanned
        // facts re-detect the scenario as NeedsQueue.
        Action::SaveBrief(text) => {
            let path = project_dir.join(patok_core::scenario::SPEC_FILE);
            let contents = project::brief_content(text);
            let write = tokio::task::spawn_blocking(move || {
                std::fs::write(&path, contents).map_err(|e| e.to_string())
            })
            .await;
            match write {
                Ok(Ok(())) => {
                    app.project_status = probe_status(project_dir).await;
                    app.on_brief_saved();
                }
                Ok(Err(error)) => {
                    app.on_submit_failed(format!("could not write SPEC.md: {error}"));
                }
                Err(_) => {
                    app.on_submit_failed("the SPEC.md write did not finish".into());
                }
            }
        }
        // The inject-task modal's confirm (T76.1, routed through the engine
        // for T77.1): the engine normalizes the typed text into a
        // well-formed unchecked task line -- adding the checkbox and the
        // next `T<N>.1` id when missing -- and appends it to TASKS.md under
        // its task-file lock, so the append never interleaves with the
        // engine's own rewrites and the engine reconciles the queue right
        // away -- even mid-build, since the lock is never held across an
        // agent session. The modal closes with the focus back on the task
        // list; a rejection keeps it open on the input with the error.
        Action::InjectTask(text) => {
            let command = CommandRequest {
                action: Some(command_request::Action::InjectTask(InjectTask {
                    text: text.clone(),
                })),
            };
            let error = match submit(command).await {
                Ok(response) => {
                    let response = response.into_inner();
                    (!response.accepted).then_some(response.error)
                }
                Err(status) => Some(format!("inject failed: {}", status.message())),
            };
            match error {
                None => app.on_task_injected(),
                Some(error) => {
                    app.on_submit_failed(format!("could not append to TASKS.md: {error}"));
                }
            }
        }
        _ => return SubmitFlow::Keep,
    }
    SubmitFlow::Handled
}

/// Sends the add-tasks command with the run's `kind`: the planner for a user
/// request, the research agent for the queue-creation runs (T69.1). The
/// submit RPC is a generic function instead of a concrete `EngineClient`, so
/// the dispatch tests pass fakes.
async fn add_tasks<Req, Fut>(app: &mut App, submit: Req, request: String, kind: AddTasksKind)
where
    Req: FnOnce(CommandRequest) -> Fut,
    Fut: std::future::Future<Output = CommandCall>,
{
    let command = CommandRequest {
        action: Some(command_request::Action::AddTasks(AddTasks {
            request,
            kind: kind as i32,
        })),
    };
    let error = match submit(command).await {
        Ok(response) => {
            let response = response.into_inner();
            (!response.accepted).then_some(response.error)
        }
        Err(status) => Some(format!("add failed: {}", status.message())),
    };
    match error {
        None => app.on_tasks_submitted(),
        Some(error) => app.on_submit_failed(error),
    }
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    client: &mut EngineClient<Channel>,
    mut updates: Streaming<patok_proto::EngineUpdate>,
    spawn: &Spawner,
    project_dir: &std::path::Path,
) -> anyhow::Result<Outcome> {
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    // The in-flight shutdown, tagged with the scope it was requested with: a
    // completed shutdown is a stop for both scopes, and the shell ends only
    // when the attach stream ends (the engine's ShutdownNotice).
    let mut shutdown: Option<(
        shutdown_request::Scope,
        Streaming<patok_proto::ShutdownUpdate>,
    )> = None;
    let mut stopped_reason: Option<String> = None;
    // The shell's own tui-schema settings: loaded here,
    // applied and persisted by the shell itself, and reloaded when a config file
    // changes on disk.
    let (mut shell_settings, warnings) = ShellSettings::load(project_dir);
    for warning in warnings {
        app.shell_notice(warning);
    }
    app.tui = shell_settings.settings().clone();
    let mut ticks: u32 = 0;
    // Set in the shutdown arm when the stream completes (either scope: a SOFT
    // stop or a NOW interrupt): the stream is dropped after the `select!`
    // (its pinned future still borrows `shutdown`), so `next_shutdown` pends
    // again and the shell keeps serving keys.
    let mut shutdown_done;

    loop {
        // The timer recomputes on every render: refresh the clock first.
        app.now.set(Instant::now());
        terminal.draw(|frame| ui::render(frame, app))?;
        shutdown_done = false;
        tokio::select! {
            _ = tick.tick() => {
                ticks = ticks.wrapping_add(1);
                if ticks.is_multiple_of(CONFIG_FILE_POLL_TICKS)
                    && let Some(warnings) = shell_settings.reload_if_changed()
                {
                    for warning in warnings {
                        app.shell_notice(warning);
                    }
                    app.tui = shell_settings.settings().clone();
                }
            }
            key = keys.next() => match key {
                Some(Ok(TermEvent::Key(key))) => match app.on_key(key) {
                    Action::None => {}
                    Action::StartBuild => start_build(client, app).await,
                    Action::RunDiscovery => run_discovery(client, app).await,
                    // Esc while a soft stop is pending: the engine clears the
                    // stop, so the loop continues with the next task; the
                    // pending SOFT shutdown stream delivers the cancelled
                    // Complete update that follows.
                    Action::CancelSoftStop => cancel_soft_stop(client, app).await,
                    // The submit family (T144.1): the add-task dialog's and
                    // the inject-task modal's Enter keys -- and every mouse
                    // path that runs that key, the primary footer button's
                    // click -- go through the one shared dispatch.
                    action @ (Action::SubmitTasks(_)
                    | Action::ResearchQueue(_)
                    | Action::SaveBrief(_)
                    | Action::InjectTask(_)) => {
                        run_submit_family(app, project_dir, &action, |command| {
                            client.submit_command(command)
                        })
                        .await;
                    }
                    Action::Restart => {
                        restart(terminal, app, client, &mut updates, spawn).await?;
                        stopped_reason = None;
                    }
                    // The shutdown family (T143.1): the `d` key, the first and
                    // second `q`, and every mouse path that runs one of those
                    // keys -- the m menu's Detach and Quit row clicks -- go
                    // through the one shared dispatch.
                    action @ (Action::Detach | Action::Quit | Action::Interrupt) => {
                        match run_shutdown_family(app, &action, |scope| {
                            client.shutdown(ShutdownRequest { scope: scope as i32 })
                        })
                        .await?
                        {
                            ShutdownFlow::Detach(outcome) => return Ok(outcome),
                            ShutdownFlow::Pending(scope, stream) => {
                                shutdown = Some((scope, *stream))
                            }
                            ShutdownFlow::Keep => {}
                        }
                    }
                    // The overlay closed with nothing left to apply (a clean close
                    // or a discard): the drafts are gone from the state already.
                    Action::CloseSettings => {}
                    // The close dialog's save choice, its Enter key (T149.1):
                    // the one shared dispatch below also runs the Save row's
                    // click (T146.1). Apply and persist every drafted change
                    // through the settings-change flow, one by one; the first
                    // rejection stops the loop, keeps the overlay open with
                    // the reason and the remaining drafts, and a retry saves
                    // only what is left.
                    Action::SaveSettings => {
                        save_settings(app, &mut shell_settings, client).await;
                    }
                    // The theme picker's Enter or click (T43.1): the app state
                    // already carries the theme; persist it through the same
                    // tui-field path the settings overlay's theme row takes.
                    Action::SaveTheme(theme) => save_theme(&mut shell_settings, app, theme),
                    // S-Tab's run-mode flip (T60.1): the same settings-change
                    // flow the overlay's save takes, so the change validates,
                    // persists and reports back; the chip follows immediately
                    // through `on_settings_applied`, a rejection lands in the
                    // status bar.
                    Action::SetRunMode(mode) => {
                        change_setting(
                            app,
                            &mut shell_settings,
                            Schema::Daemon,
                            "run_mode",
                            SettingValue::Str(mode),
                            client,
                        )
                        .await;
                    }
                },
                Some(Ok(TermEvent::Paste(text))) => app.on_paste(&text),
                // A click inside one of the two frames focuses it (T30.1);
                // the theme picker's row click saves its theme (T43.1); the
                // status bar's chips run their keys' actions (T134.1); the m
                // menu's Detach and Quit row clicks run their keys' shutdown
                // flows through the same shared dispatch the key arm takes
                // (T143.1); a click on the add-task dialog's or the
                // inject-task modal's primary footer button runs its Enter
                // key's submit flow through the same shared dispatch the key
                // arm takes (T144.1); the unsaved-changes dialog's Save row
                // click runs its Enter key's apply-and-persist flow through
                // the same shared dispatch the key arm takes (T149.1).
                Some(Ok(TermEvent::Mouse(mouse))) => match app.on_mouse(mouse) {
                    Action::None => {}
                    Action::StartBuild => start_build(client, app).await,
                    Action::RunDiscovery => run_discovery(client, app).await,
                    Action::SaveTheme(theme) => save_theme(&mut shell_settings, app, theme),
                    action @ (Action::SubmitTasks(_)
                    | Action::ResearchQueue(_)
                    | Action::SaveBrief(_)
                    | Action::InjectTask(_)) => {
                        run_submit_family(app, project_dir, &action, |command| {
                            client.submit_command(command)
                        })
                        .await;
                    }
                    action @ (Action::Detach | Action::Quit | Action::Interrupt) => {
                        match run_shutdown_family(app, &action, |scope| {
                            client.shutdown(ShutdownRequest { scope: scope as i32 })
                        })
                        .await?
                        {
                            ShutdownFlow::Detach(outcome) => return Ok(outcome),
                            ShutdownFlow::Pending(scope, stream) => {
                                shutdown = Some((scope, *stream))
                            }
                            ShutdownFlow::Keep => {}
                        }
                    }
                    // The unsaved-changes dialog's Save row click (T146.1,
                    // T149.1): the same shared dispatch the key arm's Enter
                    // takes, so a click applies and persists every draft
                    // exactly like the key.
                    Action::SaveSettings => {
                        save_settings(app, &mut shell_settings, client).await;
                    }
                    _ => {}
                },
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(Outcome::Detached { completed: app.completed }),
            },
            update = updates.message() => match update {
                Ok(Some(update)) => match update.payload {
                    Some(engine_update::Payload::Event(event)) => {
                        match EngineEvent::from_payload(&event.payload) {
                            Ok(event) => {
                                // Entering the idle phase rescans the project, so the
                                // scenario reflects the files as they are now
                                //.
                                let was_idle = app.is_idle();
                                app.apply(event);
                                if !was_idle && app.is_idle() {
                                    let dir = std::path::PathBuf::from(app.project.clone());
                                    app.project_status = probe_status(&dir).await;
                                }
                            }
                            Err(e) => app.status = Some(format!("unreadable {} event: {e}", event.kind)),
                        }
                    }
                    Some(engine_update::Payload::ShutdownNotice(notice)) => {
                        app.status = Some(format!("Engine stopping: {}", notice.reason));
                        stopped_reason = Some(notice.reason);
                    }
                    _ => {}
                },
                // The stream ends once the engine is gone.
                Ok(None) => {
                    let reason = stopped_reason.unwrap_or_else(|| "connection to the engine closed".into());
                    return Ok(Outcome::EngineStopped { reason, completed: app.completed });
                }
                Err(status) => return Err(anyhow!("lost the engine connection: {}", status.message())),
            },
            progress = next_shutdown(&mut shutdown) => match progress {
                Some(Ok(update)) => {
                    let scope = shutdown.as_ref().expect("a pending shutdown").0;
                    match shutdown_progress(app, scope, &update) {
                        Some(outcome) => return Ok(outcome),
                        // A completed shutdown is a stop, not a quit: drop the
                        // finished stream below so keys work again. The shell
                        // still ends through the attach stream when the engine
                        // really exits.
                        None => {
                            if update.phase == shutdown_update::Phase::Complete as i32 {
                                shutdown_done = true;
                            }
                        }
                    }
                }
                // Stream ended or failed: the engine is gone either way.
                _ => return Ok(Outcome::EngineStopped { reason: "stopped".into(), completed: app.completed }),
            },
        }
        if shutdown_done {
            shutdown = None;
        }
    }
}

/// Applies one shutdown-progress update. A completed shutdown is a stop for
/// both scopes, not a quit: the shell keeps running and keys work again. The
/// shell ends only when the attach stream ends -- the ShutdownNotice the
/// engine sends when it really exits (a NOW stop requested while idle, a
/// signal, or the idle timeout), or a lost connection, handled by the caller.
pub fn shutdown_progress(
    app: &mut App,
    scope: shutdown_request::Scope,
    update: &patok_proto::ShutdownUpdate,
) -> Option<Outcome> {
    // While a SOFT stop is pending, the two-keys message stays on screen for
    // the whole pending window; the engine's generic progress text does not
    // overwrite it. The Complete update still carries its own message.
    if scope == shutdown_request::Scope::Soft
        && update.phase != shutdown_update::Phase::Complete as i32
    {
        app.status = Some(crate::SOFT_STOP_PENDING.to_string());
    } else {
        app.status = Some(update.message.clone());
    }
    if update.phase == shutdown_update::Phase::Complete as i32 {
        // The loop has wound down; the engine is idle and keeps serving, so
        // the shell stays open (a SOFT stop finishes the running task first,
        // a NOW interrupt cancels it).
        app.stopping = false;
        None
    } else {
        None
    }
}

/// Pending forever when no shutdown is in progress.
async fn next_shutdown(
    shutdown: &mut Option<(
        shutdown_request::Scope,
        Streaming<patok_proto::ShutdownUpdate>,
    )>,
) -> Option<Result<patok_proto::ShutdownUpdate, tonic::Status>> {
    match shutdown {
        Some((_, stream)) => stream.message().await.transpose(),
        None => std::future::pending().await,
    }
}

async fn start_build(client: &mut EngineClient<Channel>, app: &mut App) {
    let request = CommandRequest {
        action: Some(command_request::Action::StartBuild(StartBuild {})),
    };
    app.status = match client.submit_command(request).await {
        Ok(response) => {
            let response = response.into_inner();
            (!response.accepted).then_some(response.error)
        }
        Err(status) => Some(format!("start failed: {}", status.message())),
    };
}

/// Starts one discovery round now (the Enter key while the engine is idle): the round
/// runs in the background, and its output and outcome reach the shell through the
/// attach stream; only a rejection (e.g. a run that just started) lands in the
/// status bar.
async fn run_discovery(client: &mut EngineClient<Channel>, app: &mut App) {
    let request = CommandRequest {
        action: Some(command_request::Action::RunDiscovery(RunDiscovery {})),
    };
    app.status = match client.submit_command(request).await {
        Ok(response) => {
            let response = response.into_inner();
            (!response.accepted).then_some(response.error)
        }
        Err(status) => Some(format!("discovery failed: {}", status.message())),
    };
}

/// Cancels a pending soft stop (Esc while one is pending). The app state was
/// already cleared optimistically by the key handler; a rejection (no soft
/// stop pending, e.g. a NOW stop or restart in flight) or a transport failure
/// puts it back: the stop is still in flight and its stream finishes the
/// cleanup.
async fn cancel_soft_stop(client: &mut EngineClient<Channel>, app: &mut App) {
    let request = CommandRequest {
        action: Some(command_request::Action::CancelSoftStop(CancelSoftStop {})),
    };
    let error = match client.submit_command(request).await {
        Ok(response) => {
            let response = response.into_inner();
            (!response.accepted).then_some(response.error)
        }
        Err(status) => Some(format!("cancel failed: {}", status.message())),
    };
    if let Some(error) = error {
        app.stopping = true;
        app.status = Some(error);
    }
}

/// Persists the theme picker's choice (T43.1): the same path the settings
/// overlay's theme row takes -- validated exactly as an on-disk field, written
/// to the user-local `config.local.toml` (the resolved layer of every tui
/// field), re-merged -- with no engine round trip, since the theme is a
/// tui-schema field. A failed write restores the theme the picker opened with,
/// so the shell matches the file that is still on disk.
pub fn save_theme(shell_settings: &mut ShellSettings, app: &mut App, theme: ThemeKey) {
    match shell_settings.apply("theme", SettingValue::Str(theme.as_str().into())) {
        Ok(warnings) => {
            for warning in warnings {
                app.shell_notice(warning);
            }
            app.tui = shell_settings.settings().clone();
            app.on_theme_saved();
        }
        Err(error) => app.on_theme_save_failed(error),
    }
}

/// The settings-change RPC as the shared dispatches ask the engine for it:
/// one method instead of a concrete `EngineClient`, so tests pass fakes. The
/// returned future borrows the submitter, so one save flow can run the RPC
/// once per drafted change in sequence.
trait SettingsSubmit {
    /// Sends one settings-change command and reports its call result.
    fn settings_change(
        &mut self,
        request: CommandRequest,
    ) -> impl std::future::Future<Output = CommandCall>;
}

impl SettingsSubmit for EngineClient<Channel> {
    async fn settings_change(&mut self, request: CommandRequest) -> CommandCall {
        self.submit_command(request).await
    }
}

/// Applies and persists one drafted change: a
/// daemon-schema field goes through the engine's settings-change flow, a tui-schema
/// field is applied by the shell itself with no round trip. Returns whether the
/// change applied; the overlay learns the outcome either way -- accepted or rejected
/// with the reason. The submit RPC goes through [`SettingsSubmit`] instead of a
/// concrete `EngineClient`, so tests pass fakes.
async fn change_setting<S: SettingsSubmit>(
    app: &mut App,
    shell_settings: &mut ShellSettings,
    schema: Schema,
    field: &str,
    value: patok_core::config::SettingValue,
    submitter: &mut S,
) -> bool {
    match schema {
        Schema::Daemon => {
            let change = match to_proto(field, value.clone()) {
                Ok(change) => change,
                Err(error) => {
                    app.on_settings_rejected(error);
                    return false;
                }
            };
            let request = CommandRequest {
                action: Some(command_request::Action::SettingsChange(SettingsChange {
                    change: Some(change),
                })),
            };
            match submitter.settings_change(request).await {
                Ok(response) => {
                    let response = response.into_inner();
                    if !response.accepted {
                        app.on_settings_rejected(response.error);
                        return false;
                    }
                    app.on_settings_applied(schema, field, value);
                    true
                }
                Err(status) => {
                    app.on_settings_rejected(format!(
                        "settings change failed: {}",
                        status.message()
                    ));
                    false
                }
            }
        }
        Schema::Tui => match shell_settings.apply(field, value.clone()) {
            Ok(warnings) => {
                for warning in warnings {
                    app.shell_notice(warning);
                }
                app.tui = shell_settings.settings().clone();
                app.on_settings_applied(schema, field, value);
                true
            }
            Err(error) => {
                app.on_settings_rejected(error);
                false
            }
        },
    }
}

/// Runs the close dialog's save choice (T149.1): the one dispatch both the
/// key arm and the mouse arm of [`event_loop`] route `SaveSettings` through,
/// so a click on the Save row runs exactly its Enter key's flow. Every
/// drafted change is applied and persisted one by one through the
/// settings-change flow; the first rejection stops the loop and keeps the
/// overlay open with the reason and the remaining drafts, and the overlay
/// closes only once nothing dirty remains. The submit RPC goes through
/// [`SettingsSubmit`] instead of a concrete `EngineClient`, so tests pass
/// fakes.
async fn save_settings<S: SettingsSubmit>(
    app: &mut App,
    shell_settings: &mut ShellSettings,
    submitter: &mut S,
) {
    let pending = app.overlay.pending();
    for (schema, field, value) in pending {
        if !change_setting(app, shell_settings, schema, &field, value, submitter).await {
            break;
        }
    }
    if !app.overlay.dirty() {
        app.overlay.close();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use patok_core::config::{ConfigFiles, RailMode, SettingValue, Theme as ThemeKey};
    use patok_core::event::{EngineEvent, Phase, Snapshot};
    use patok_core::pipeline::PipelineState;
    use patok_core::scenario::{SPEC_FILE, SpecState};
    use patok_core::task;
    use patok_proto::{
        AddTasksKind, CommandRequest, CommandResponse, SettingsChange, command_request,
        settings_change::Change,
    };
    use ratatui::layout::Rect;

    use crate::app::FrameFocus;
    use crate::overlay::StatusLevel;
    use crate::settings::ShellSettings;

    use super::{
        Action, App, CommandCall, Outcome, QueueRun, Schema, SettingsSubmit, ShutdownCall,
        ShutdownFlow, SubmitFlow, interrupt_status, run_shutdown_family, run_submit_family,
        save_settings,
    };
    use super::{ModifyOtherKeys, shutdown_request};

    fn ansi(command: &ModifyOtherKeys) -> String {
        let mut bytes = String::new();
        crossterm::Command::write_ansi(command, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn modify_other_keys_sets_and_resets_the_mode() {
        assert_eq!(ansi(&ModifyOtherKeys(2)), "\x1b[>4;2m");
        assert_eq!(ansi(&ModifyOtherKeys(0)), "\x1b[>4m");
    }

    /// An idle shell, the state a fresh attach builds.
    fn app() -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: task::parse("## Phase 1\n- [ ] T1.1: scaffold the workspace\n"),
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    /// A running engine, so `is_idle` is false and the interrupt status is the
    /// keep-running notice.
    fn running() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app
    }

    /// A shutdown-request fake that always fails: it records the scope it was
    /// asked for and answers with the given status. tonic's `Streaming` has no
    /// public constructor, so only the failure paths are unit-testable here;
    /// the Ok paths are the key arm's moved code plus the e2e protocol tests.
    fn failing_request(
        scopes: &Arc<Mutex<Vec<shutdown_request::Scope>>>,
        message: &'static str,
    ) -> impl FnOnce(shutdown_request::Scope) -> std::future::Ready<ShutdownCall> {
        let scopes = Arc::clone(scopes);
        move |scope| {
            scopes.lock().unwrap().push(scope);
            let answer: ShutdownCall = Err(tonic::Status::unavailable(message));
            std::future::ready(answer)
        }
    }

    /// A shutdown-request fake that is never supposed to run: it panics, so
    /// any call fails the test.
    fn unused_request() -> impl FnOnce(shutdown_request::Scope) -> std::future::Pending<ShutdownCall>
    {
        move |scope| panic!("no shutdown request was expected, got {scope:?}")
    }

    /// A submit fake that records the command it was asked for and answers
    /// with the given outcome: an accepting response when `accepted`, a
    /// rejected one carrying `error` otherwise -- so the dispatch's accept
    /// and rejection paths are unit-testable here (the transport failures
    /// take [`failing_submit`]).
    fn recording_submit(
        commands: &Arc<Mutex<Vec<CommandRequest>>>,
        accepted: bool,
        error: &'static str,
    ) -> impl FnOnce(CommandRequest) -> std::future::Ready<CommandCall> {
        let commands = Arc::clone(commands);
        move |command| {
            commands.lock().unwrap().push(command);
            std::future::ready(Ok(tonic::Response::new(CommandResponse {
                accepted,
                error: error.into(),
                ..Default::default()
            })))
        }
    }

    /// A submit fake that always fails with the given status.
    fn failing_submit(
        message: &'static str,
    ) -> impl FnOnce(CommandRequest) -> std::future::Ready<CommandCall> {
        move |_command| {
            let answer: CommandCall = Err(tonic::Status::unavailable(message));
            std::future::ready(answer)
        }
    }

    /// A submit fake that is never supposed to run: it panics, so any call
    /// fails the test.
    fn unused_submit() -> impl FnOnce(CommandRequest) -> std::future::Pending<CommandCall> {
        move |command| panic!("no submit was expected, got {command:?}")
    }

    /// A shell settings holder whose user-local layer is a tempdir file (the
    /// settings.rs `holder` pattern), so tui drafts persist somewhere real.
    fn shell() -> (tempfile::TempDir, ShellSettings) {
        let dir = tempfile::tempdir().unwrap();
        let (settings, warnings) = ShellSettings::with_files(
            ConfigFiles {
                user_global: None,
                user_local: Some(dir.path().join("config.local.toml")),
            },
            dir.path(),
            None,
        );
        assert!(warnings.is_empty());
        (dir, settings)
    }

    /// A save-flow submit fake: it records the command it was asked for into
    /// the shared list and answers with the given outcome -- the same
    /// recording-and-answer shape as [`recording_submit`], on the
    /// [`SettingsSubmit`] trait the multi-draft save dispatch takes.
    struct RecordingSave {
        commands: Arc<Mutex<Vec<CommandRequest>>>,
        accepted: bool,
        error: &'static str,
    }

    impl SettingsSubmit for RecordingSave {
        async fn settings_change(&mut self, request: CommandRequest) -> CommandCall {
            self.commands.lock().unwrap().push(request);
            Ok(tonic::Response::new(CommandResponse {
                accepted: self.accepted,
                error: self.error.into(),
                ..Default::default()
            }))
        }
    }

    /// A save-flow submit fake that is never supposed to run: its RPC panics,
    /// so any call fails the test.
    struct UnusedSave;

    impl SettingsSubmit for UnusedSave {
        async fn settings_change(&mut self, request: CommandRequest) -> CommandCall {
            panic!("no submit was expected, got {request:?}")
        }
    }

    /// The app at the save dispatch: the dialog gone (its Save choice, key
    /// or click, already dismissed it), the overlay open with the drafted
    /// changes pending.
    fn save_app(drafts: &[(&str, &str)]) -> App {
        let mut app = app();
        app.overlay.open();
        for (field, value) in drafts {
            app.overlay
                .drafts
                .insert((*field).into(), SettingValue::Str((*value).into()));
        }
        app
    }

    /// The app at the close dialog's Save row (T146.1): the dialog open, row
    /// 0 (Save) selected, the choice rows' area recorded. The body rect lies
    /// outside any other recorded rect, so its rows 0-2 are the choices.
    fn confirm_app() -> (App, Rect) {
        let mut app = save_app(&[("model", "opus")]);
        app.overlay.confirm_open = true;
        app.overlay.confirm_selected = 0;
        let body = Rect::new(0, 20, 50, 6);
        app.overlay.confirm_body.set(body);
        (app, body)
    }

    /// A mouse event of `kind` at (`column`, `row`).
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Detach maps to the detached outcome with the completed count, and no
    /// shutdown request is sent -- the engine keeps running.
    #[tokio::test]
    async fn detach_maps_to_the_detached_outcome_without_a_request() {
        let mut app = app();
        app.completed = 3;
        let flow = run_shutdown_family(&mut app, &Action::Detach, unused_request())
            .await
            .unwrap();
        assert!(matches!(
            flow,
            ShutdownFlow::Detach(Outcome::Detached { completed: 3 })
        ));
    }

    /// Quit asks for a SOFT stop, leaves the app state's status alone, and
    /// propagates a transport failure with the `q` key's message.
    #[tokio::test]
    async fn quit_requests_a_soft_stop_and_propagates_a_failure() {
        let mut app = app();
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let error = run_shutdown_family(
            &mut app,
            &Action::Quit,
            failing_request(&scopes, "unavailable"),
        )
        .await
        .err()
        .expect("quit propagates a failed shutdown request");
        assert_eq!(error.to_string(), "could not request engine shutdown");
        assert_eq!(*scopes.lock().unwrap(), vec![shutdown_request::Scope::Soft]);
        assert_eq!(app.status, None);
    }

    /// Interrupt asks for a NOW stop and tolerates an engine that is already
    /// exiting: the failure lands in the status bar and the loop keeps going,
    /// polling the earlier shutdown's stream.
    #[tokio::test]
    async fn interrupt_requests_a_now_stop_and_tolerates_an_exiting_engine() {
        let mut app = app();
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let flow = run_shutdown_family(
            &mut app,
            &Action::Interrupt,
            failing_request(&scopes, "the engine is exiting"),
        )
        .await
        .unwrap();
        assert!(matches!(flow, ShutdownFlow::Keep));
        assert_eq!(*scopes.lock().unwrap(), vec![shutdown_request::Scope::Now]);
        assert_eq!(
            app.status.as_deref(),
            Some("interrupt failed: the engine is exiting")
        );
    }

    /// The interrupt status line is the true quit's stopping message while
    /// idle and the keep-running notice otherwise -- the key arm's exact
    /// texts.
    #[test]
    fn interrupt_status_names_the_true_quit_while_idle() {
        assert_eq!(interrupt_status(&app()), "Stopping the engine...");
        assert_eq!(
            interrupt_status(&running()),
            "Interrupting -- the current task is cancelled and the app keeps running."
        );
    }

    /// Every other action keeps its own arm in the event loop: the shared
    /// dispatch returns `Keep` for them without a request, so the mouse
    /// arm's StartBuild/RunDiscovery/SaveTheme handling and the wildcard are
    /// unchanged (the submit family goes through `run_submit_family`).
    #[tokio::test]
    async fn other_actions_keep_looping_without_a_request() {
        for action in [
            Action::None,
            Action::StartBuild,
            Action::SaveTheme(ThemeKey::default()),
        ] {
            let mut app = app();
            let flow = run_shutdown_family(&mut app, &action, unused_request())
                .await
                .unwrap();
            assert!(matches!(flow, ShutdownFlow::Keep), "{action:?}");
            assert_eq!(app.status, None);
        }
    }

    /// The add-task dialog's submit (T144.1) sends the planner-kind AddTasks
    /// command with the typed text, and the driver's accepted-submit cleanup
    /// closes the dialog with the output frame focused.
    #[tokio::test]
    async fn submit_tasks_sends_the_planner_add_tasks_and_closes_on_accept() {
        let mut app = app();
        app.dialog_open = true;
        app.dialog_text = "add a login page".into();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let flow = run_submit_family(
            &mut app,
            std::path::Path::new("/home/user/demo"),
            &Action::SubmitTasks("add a login page".into()),
            recording_submit(&commands, true, ""),
        )
        .await;
        assert!(matches!(flow, SubmitFlow::Handled));
        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        match &commands[0].action {
            Some(command_request::Action::AddTasks(add)) => {
                assert_eq!(add.request, "add a login page");
                assert_eq!(add.kind, AddTasksKind::AddTasksPlanner as i32);
            }
            other => panic!("expected an AddTasks command, got {other:?}"),
        }
        assert!(!app.dialog_open);
        assert_eq!(app.focus, FrameFocus::Output);
        assert_eq!(app.dialog_text, "");
    }

    /// The research queue-creation runs (T69.1) send the AddTasks command
    /// with their run's kind and their request text.
    #[tokio::test]
    async fn research_queue_sends_each_run_s_kind_with_its_request_text() {
        for (run, kind) in [
            (QueueRun::Bootstrap, AddTasksKind::AddTasksBootstrap),
            (QueueRun::Scan, AddTasksKind::AddTasksScan),
        ] {
            let mut app = app();
            let commands = Arc::new(Mutex::new(Vec::new()));
            let flow = run_submit_family(
                &mut app,
                std::path::Path::new("/home/user/demo"),
                &Action::ResearchQueue(run),
                recording_submit(&commands, true, ""),
            )
            .await;
            assert!(matches!(flow, SubmitFlow::Handled), "{run:?}");
            let commands = commands.lock().unwrap();
            match &commands[0].action {
                Some(command_request::Action::AddTasks(add)) => {
                    assert_eq!(add.request, run.request(), "{run:?}");
                    assert_eq!(add.kind, kind as i32, "{run:?}");
                }
                other => panic!("expected an AddTasks command, got {other:?}"),
            }
            assert!(!app.dialog_open, "{run:?}");
        }
    }

    /// A rejected or failed add keeps the dialog open on the input with its
    /// error -- the same cleanup the Enter key's failure takes.
    #[tokio::test]
    async fn a_failed_add_keeps_the_dialog_open_with_the_error() {
        // The engine rejected the request: its reason lands in the dialog.
        let mut app = app();
        app.dialog_open = true;
        app.dialog_text = "add a login page".into();
        run_submit_family(
            &mut app,
            std::path::Path::new("/home/user/demo"),
            &Action::SubmitTasks("add a login page".into()),
            recording_submit(&Arc::new(Mutex::new(Vec::new())), false, "planner busy"),
        )
        .await;
        assert!(app.dialog_open);
        assert_eq!(app.dialog_text, "add a login page");
        assert_eq!(app.dialog_status.as_deref(), Some("planner busy"));

        // The transport failed: the add's failure message lands instead.
        run_submit_family(
            &mut app,
            std::path::Path::new("/home/user/demo"),
            &Action::SubmitTasks("add a login page".into()),
            failing_submit("nope"),
        )
        .await;
        assert!(app.dialog_open);
        assert_eq!(app.dialog_text, "add a login page");
        assert_eq!(app.dialog_status.as_deref(), Some("add failed: nope"));
    }

    /// The inject-task modal's confirm (T76.1) sends the InjectTask command
    /// and closes the modal with the task list focused on accept; a
    /// rejection or transport failure keeps it open with the error.
    #[tokio::test]
    async fn inject_sends_the_inject_command_and_closes_the_modal_on_accept() {
        let mut accepted = app();
        accepted.dialog_open = true;
        let commands = Arc::new(Mutex::new(Vec::new()));
        let flow = run_submit_family(
            &mut accepted,
            std::path::Path::new("/home/user/demo"),
            &Action::InjectTask("- [ ] T78.1: Polish the README".into()),
            recording_submit(&commands, true, ""),
        )
        .await;
        assert!(matches!(flow, SubmitFlow::Handled));
        {
            let commands = commands.lock().unwrap();
            match &commands[0].action {
                Some(command_request::Action::InjectTask(inject)) => {
                    assert_eq!(inject.text, "- [ ] T78.1: Polish the README");
                }
                other => panic!("expected an InjectTask command, got {other:?}"),
            }
        }
        assert!(!accepted.dialog_open);
        assert_eq!(accepted.focus, FrameFocus::Tasks);
        // A rejected append keeps the modal open with the engine's reason.
        let mut rejected = app();
        rejected.dialog_open = true;
        run_submit_family(
            &mut rejected,
            std::path::Path::new("/home/user/demo"),
            &Action::InjectTask("- [ ] T78.1: Polish the README".into()),
            recording_submit(&Arc::new(Mutex::new(Vec::new())), false, "empty text"),
        )
        .await;
        assert!(rejected.dialog_open);
        assert_eq!(
            rejected.dialog_status.as_deref(),
            Some("could not append to TASKS.md: empty text")
        );

        // A failed append keeps the modal open with the transport's error.
        run_submit_family(
            &mut rejected,
            std::path::Path::new("/home/user/demo"),
            &Action::InjectTask("- [ ] T78.1: Polish the README".into()),
            failing_submit("nope"),
        )
        .await;
        assert!(rejected.dialog_open);
        assert_eq!(
            rejected.dialog_status.as_deref(),
            Some("could not append to TASKS.md: inject failed: nope")
        );
    }

    /// The empty project's brief submit writes SPEC.md with the brief content
    /// (no engine round trip), reports the created message on the still-open
    /// dialog, and the re-scan updates the project facts.
    #[tokio::test]
    async fn save_brief_writes_the_spec_file_and_reports_the_created_status() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut app = app();
        app.dialog_open = true;
        let flow = run_submit_family(
            &mut app,
            temp.path(),
            &Action::SaveBrief("A web service for recipes.".into()),
            unused_submit(),
        )
        .await;
        assert!(matches!(flow, SubmitFlow::Handled));
        let contents = std::fs::read_to_string(temp.path().join(SPEC_FILE)).unwrap();
        assert_eq!(
            contents,
            crate::project::brief_content("A web service for recipes.")
        );
        assert!(app.dialog_open);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("SPEC.md created -- review it, then press Enter to start.")
        );
        assert_eq!(app.project_status.spec, SpecState::Content);
    }

    /// A failed brief write keeps the dialog open with the write's error.
    #[tokio::test]
    async fn a_failed_brief_write_keeps_the_dialog_open_with_the_error() {
        let temp = tempfile::TempDir::new().unwrap();
        // A directory at the SPEC.md path makes the write fail.
        std::fs::create_dir(temp.path().join(SPEC_FILE)).unwrap();
        let mut app = app();
        app.dialog_open = true;
        run_submit_family(
            &mut app,
            temp.path(),
            &Action::SaveBrief("A web service for recipes.".into()),
            unused_submit(),
        )
        .await;
        assert!(app.dialog_open);
        assert!(
            app.dialog_status
                .as_deref()
                .unwrap_or_default()
                .starts_with("could not write SPEC.md: ")
        );
    }

    /// Every other action keeps its own arm in the event loop: the shared
    /// submit dispatch returns `Keep` for them without an RPC, leaving the
    /// app state untouched.
    #[tokio::test]
    async fn non_submit_actions_keep_their_own_arms() {
        for action in [
            Action::None,
            Action::StartBuild,
            Action::Detach,
            Action::Quit,
        ] {
            let mut app = app();
            app.dialog_open = true;
            let flow = run_submit_family(
                &mut app,
                std::path::Path::new("/home/user/demo"),
                &action,
                unused_submit(),
            )
            .await;
            assert!(matches!(flow, SubmitFlow::Keep), "{action:?}");
            assert!(app.dialog_open, "{action:?}");
            assert_eq!(app.dialog_status, None, "{action:?}");
        }
    }

    /// The shared save dispatch (T149.1) applies and persists every drafted
    /// change one by one -- the daemon field through the engine's
    /// settings-change command, the tui field by the shell itself -- and the
    /// overlay closes once nothing dirty remains.
    #[tokio::test]
    async fn save_settings_applies_and_persists_every_draft_and_closes_the_overlay() {
        let (temp, mut shell) = shell();
        let mut app = save_app(&[("model", "opus"), ("rail_mode", "detailed")]);
        let commands = Arc::new(Mutex::new(Vec::new()));
        let mut submit = RecordingSave {
            commands: Arc::clone(&commands),
            accepted: true,
            error: "",
        };
        save_settings(&mut app, &mut shell, &mut submit).await;
        // The tui field takes no RPC: exactly the one daemon command ran.
        let commands = commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        match &commands[0].action {
            Some(command_request::Action::SettingsChange(change)) => {
                assert_eq!(
                    change.change,
                    Some(Change::ModelOverride("opus".into())),
                    "{change:?}"
                );
            }
            other => panic!("expected a SettingsChange command, got {other:?}"),
        }
        assert!(!app.overlay.open);
        assert!(app.overlay.drafts.is_empty());
        assert_eq!(
            app.settings.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );
        assert_eq!(shell.settings().rail_mode, RailMode::Detailed);
        assert_eq!(app.tui.rail_mode, RailMode::Detailed);
        let text = std::fs::read_to_string(temp.path().join("config.local.toml")).unwrap();
        assert!(text.contains("rail_mode = \"detailed\""), "{text}");
    }

    /// A rejected change stops the save loop: the overlay stays open with
    /// the reason and the unspent drafts, and a retry saves only what is
    /// left.
    #[tokio::test]
    async fn a_rejection_keeps_the_overlay_open_and_a_retry_saves_the_rest() {
        let (_temp, mut shell) = shell();
        let mut app = save_app(&[("provider", "mistral"), ("model", "opus")]);
        // `provider` precedes `model` in the pending rows, so the provider
        // draft applies and the model draft takes the rejection.
        let commands = Arc::new(Mutex::new(Vec::new()));
        /// Records every command and rejects anything but a provider change.
        struct RejectingModels {
            commands: Arc<Mutex<Vec<CommandRequest>>>,
        }

        impl SettingsSubmit for RejectingModels {
            async fn settings_change(&mut self, request: CommandRequest) -> CommandCall {
                let accepted = matches!(
                    request.action,
                    Some(command_request::Action::SettingsChange(SettingsChange {
                        change: Some(Change::Provider(_)),
                    }))
                );
                self.commands.lock().unwrap().push(request);
                Ok(tonic::Response::new(CommandResponse {
                    accepted,
                    error: if accepted {
                        String::new()
                    } else {
                        "no such model".into()
                    },
                    ..Default::default()
                }))
            }
        }

        let mut submit = RejectingModels {
            commands: Arc::clone(&commands),
        };
        save_settings(&mut app, &mut shell, &mut submit).await;
        assert!(app.overlay.open);
        assert!(app.overlay.dirty());
        assert_eq!(
            app.overlay.status,
            Some(("no such model".to_string(), StatusLevel::Error))
        );
        assert_eq!(
            app.overlay.pending(),
            vec![(
                Schema::Daemon,
                "model".to_string(),
                SettingValue::Str("opus".into())
            )]
        );
        assert_eq!(
            app.settings.get("provider"),
            Some(&SettingValue::Str("mistral".into()))
        );
        assert_eq!(commands.lock().unwrap().len(), 2);
        // The retry accepts what is left: one command, the overlay closes.
        let retry = Arc::new(Mutex::new(Vec::new()));
        let mut retrying = RecordingSave {
            commands: Arc::clone(&retry),
            accepted: true,
            error: "",
        };
        save_settings(&mut app, &mut shell, &mut retrying).await;
        let retry = retry.lock().unwrap();
        assert_eq!(retry.len(), 1);
        match &retry[0].action {
            Some(command_request::Action::SettingsChange(change)) => {
                assert_eq!(
                    change.change,
                    Some(Change::ModelOverride("opus".into())),
                    "{change:?}"
                );
            }
            other => panic!("expected a SettingsChange command, got {other:?}"),
        }
        assert!(!app.overlay.open);
        assert!(app.overlay.drafts.is_empty());
        assert_eq!(
            app.settings.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );
    }

    /// A save with nothing drafted just closes the overlay: no RPC, no
    /// request.
    #[tokio::test]
    async fn save_settings_with_no_drafts_closes_the_overlay_without_a_request() {
        let (_temp, mut shell) = shell();
        let mut app = app();
        app.overlay.open();
        let mut submit = UnusedSave;
        save_settings(&mut app, &mut shell, &mut submit).await;
        assert!(!app.overlay.open);
        assert!(app.overlay.drafts.is_empty());
    }

    /// A click and the Enter key on the Save row are indistinguishable end
    /// to end: both return `SaveSettings` over the identical pre-dispatch
    /// state, and the one shared dispatch both event-loop arms call leaves
    /// the identical applied state with the identical recorded command.
    #[tokio::test]
    async fn a_click_and_the_enter_key_on_the_save_row_run_the_same_save_flow() {
        // The mouse path: a left click on the dialog's Save row (row 0).
        let (mut clicked, body) = confirm_app();
        assert_eq!(
            clicked.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y
            )),
            Action::SaveSettings
        );
        // The keyboard path: Enter on the selected Save row.
        let (mut pressed, _body) = confirm_app();
        assert_eq!(
            pressed.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::SaveSettings
        );
        // Pre-dispatch: identical state, the dialog gone in both.
        for app in [&mut clicked, &mut pressed] {
            assert!(!app.overlay.confirm_open);
            assert!(app.overlay.open);
            assert_eq!(
                app.overlay.pending(),
                vec![(
                    Schema::Daemon,
                    "model".to_string(),
                    SettingValue::Str("opus".into())
                )]
            );
        }
        // Both dispatches run the same flow with the same kind of applier.
        let (_temp_clicked, mut shell_clicked) = shell();
        let (_temp_pressed, mut shell_pressed) = shell();
        let clicked_commands = Arc::new(Mutex::new(Vec::new()));
        let pressed_commands = Arc::new(Mutex::new(Vec::new()));
        let mut clicked_submit = RecordingSave {
            commands: Arc::clone(&clicked_commands),
            accepted: true,
            error: "",
        };
        let mut pressed_submit = RecordingSave {
            commands: Arc::clone(&pressed_commands),
            accepted: true,
            error: "",
        };
        save_settings(&mut clicked, &mut shell_clicked, &mut clicked_submit).await;
        save_settings(&mut pressed, &mut shell_pressed, &mut pressed_submit).await;
        assert!(!clicked.overlay.open);
        assert!(!pressed.overlay.open);
        assert!(clicked.overlay.drafts.is_empty());
        assert!(pressed.overlay.drafts.is_empty());
        assert_eq!(clicked.settings, pressed.settings);
        let clicked_commands = clicked_commands.lock().unwrap();
        let pressed_commands = pressed_commands.lock().unwrap();
        assert_eq!(*clicked_commands, *pressed_commands);
        match &clicked_commands[0].action {
            Some(command_request::Action::SettingsChange(change)) => {
                assert_eq!(
                    change.change,
                    Some(Change::ModelOverride("opus".into())),
                    "{change:?}"
                );
            }
            other => panic!("expected a SettingsChange command, got {other:?}"),
        }
    }
}
