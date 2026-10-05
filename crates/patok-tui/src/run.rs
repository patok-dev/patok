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

/// Attaches and builds the shell state from the engine's first (info) message.
async fn attach(
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
                    Action::Detach => return Ok(Outcome::Detached { completed: app.completed }),
                    Action::StartBuild => start_build(client, app).await,
                    Action::RunDiscovery => run_discovery(client, app).await,
                    // Esc while a soft stop is pending: the engine clears the
                    // stop, so the loop continues with the next task; the
                    // pending SOFT shutdown stream delivers the cancelled
                    // Complete update that follows.
                    Action::CancelSoftStop => cancel_soft_stop(client, app).await,
                    Action::SubmitTasks(request) => {
                        submit_tasks(client, app, request, AddTasksKind::AddTasksPlanner).await
                    }
                    // The research queue-creation runs (T69.1): the Research
                    // agent investigates the project and appends the tasks.
                    Action::ResearchQueue(run) => {
                        let kind = match run {
                            QueueRun::Bootstrap => AddTasksKind::AddTasksBootstrap,
                            QueueRun::Scan => AddTasksKind::AddTasksScan,
                        };
                        submit_tasks(client, app, run.request().to_string(), kind).await
                    }
                    // The brief is written by the shell itself (like its config
                    // files); the dialog stays open with the created message and
                    // the re-scanned facts re-detect the scenario as NeedsQueue.
                    Action::SaveBrief(text) => {
                        let path = project_dir.join(patok_core::scenario::SPEC_FILE);
                        let contents = project::brief_content(&text);
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
                    // The inject-task modal's confirm (T76.1, routed through
                    // the engine for T77.1): the engine normalizes the typed
                    // text into a well-formed unchecked task line -- adding
                    // the checkbox and the next `T<N>.1` id when missing --
                    // and appends it to TASKS.md under its task-file lock,
                    // so the append never interleaves with the engine's own
                    // rewrites and the engine reconciles the queue right away
                    // -- even mid-build, since the lock is never held across
                    // an agent session. The modal closes with the focus back
                    // on the task list; a rejection keeps it open on the
                    // input with the error.
                    Action::InjectTask(text) => {
                        let command = CommandRequest {
                            action: Some(command_request::Action::InjectTask(InjectTask {
                                text,
                            })),
                        };
                        let error = match client.submit_command(command).await {
                            Ok(response) => {
                                let response = response.into_inner();
                                (!response.accepted).then_some(response.error)
                            }
                            Err(status) => {
                                Some(format!("inject failed: {}", status.message()))
                            }
                        };
                        match error {
                            None => app.on_task_injected(),
                            Some(error) => {
                                app.on_submit_failed(format!(
                                    "could not append to TASKS.md: {error}"
                                ));
                            }
                        }
                    }
                    Action::Restart => {
                        restart(terminal, app, client, &mut updates, spawn).await?;
                        stopped_reason = None;
                    }
                    Action::Quit => {
                        shutdown = Some(
                            (
                                shutdown_request::Scope::Soft,
                                client
                                    .shutdown(ShutdownRequest {
                                        scope: shutdown_request::Scope::Soft as i32,
                                    })
                                    .await
                                    .context("could not request engine shutdown")?
                                    .into_inner(),
                            ),
                        );
                    }
                    // An interrupt (the second q, or the interrupt choice of
                    // the stop dialog): cancel the current task and stop the
                    // build loop without waiting. While idle (the plain q) it
                    // is the true quit: the engine exits and the shell ends
                    // with it.
                    Action::Interrupt => {
                        app.status = Some(if app.is_idle() {
                            "Stopping the engine...".to_string()
                        } else {
                            "Interrupting -- the current task is cancelled and the app keeps running.".to_string()
                        });
                        match client
                            .shutdown(ShutdownRequest {
                                scope: shutdown_request::Scope::Now as i32,
                            })
                            .await
                        {
                            Ok(stream) => {
                                shutdown =
                                    Some((shutdown_request::Scope::Now, stream.into_inner()))
                            }
                            // The engine is likely already exiting (the soft-stop
                            // stream is still draining); keep polling that stream
                            // instead of failing the shell.
                            Err(status) => {
                                app.status =
                                    Some(format!("interrupt failed: {}", status.message()));
                            }
                        }
                    }
                    // The overlay closed with nothing left to apply (a clean close
                    // or a discard): the drafts are gone from the state already.
                    Action::CloseSettings => {}
                    // The close dialog's save choice: apply and persist every
                    // drafted change through the settings-change flow, one by one;
                    // the first rejection stops the loop, keeps the overlay open
                    // with the reason and the remaining drafts, and a retry saves
                    // only what is left.
                    Action::SaveSettings => {
                        let pending = app.overlay.pending();
                        for (schema, field, value) in pending {
                            if !change_setting(
                                client,
                                app,
                                &mut shell_settings,
                                schema,
                                &field,
                                value,
                            )
                            .await
                            {
                                break;
                            }
                        }
                        if !app.overlay.dirty() {
                            app.overlay.close();
                        }
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
                            client,
                            app,
                            &mut shell_settings,
                            Schema::Daemon,
                            "run_mode",
                            SettingValue::Str(mode),
                        )
                        .await;
                    }
                },
                Some(Ok(TermEvent::Paste(text))) => app.on_paste(&text),
                // A click inside one of the two frames focuses it (T30.1); the
                // theme picker's row click saves its theme (T43.1).
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    if let Action::SaveTheme(theme) = app.on_mouse(mouse) {
                        save_theme(&mut shell_settings, app, theme);
                    }
                }
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

/// Sends the add-tasks command with the run's `kind`: the planner for a user
/// request, the research agent for the queue-creation runs (T69.1).
async fn submit_tasks(
    client: &mut EngineClient<Channel>,
    app: &mut App,
    request: String,
    kind: AddTasksKind,
) {
    let command = CommandRequest {
        action: Some(command_request::Action::AddTasks(AddTasks {
            request,
            kind: kind as i32,
        })),
    };
    let error = match client.submit_command(command).await {
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

/// Applies and persists one drafted change: a
/// daemon-schema field goes through the engine's settings-change flow, a tui-schema
/// field is applied by the shell itself with no round trip. Returns whether the
/// change applied; the overlay learns the outcome either way -- accepted or rejected
/// with the reason.
async fn change_setting(
    client: &mut EngineClient<Channel>,
    app: &mut App,
    shell_settings: &mut ShellSettings,
    schema: Schema,
    field: &str,
    value: patok_core::config::SettingValue,
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
            match client.submit_command(request).await {
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

#[cfg(test)]
mod tests {
    use super::ModifyOtherKeys;

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
}
