//! The headless driver: the agent output frame's content, line by line on a writer,
//! with the build run to completion. Renders nothing; `App` is reused as the line
//! builder so the streamed lines are exactly the ones the frame displays.

use std::io::Write;
use std::time::Duration;

use patok_core::config::SettingValue;
use patok_core::event::{EngineEvent, Phase, TaskOutcome};
use patok_proto::engine_client::EngineClient;
use patok_proto::{CommandRequest, SettingsChange, StartBuild, command_request, engine_update};
use tokio_util::sync::CancellationToken;
use tonic::Streaming;
use tonic::transport::Channel;

use crate::app::App;
use crate::settings::to_proto;

/// How long the run-mode restore waits for the engine's confirmation event.
const RESTORE_CONFIRM: Duration = Duration::from_secs(5);

/// How the headless run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum HeadlessOutcome {
    /// Every task in the store is done.
    Completed,
    /// The build failed or an engine error stopped it; the message is for the operator.
    Failed(String),
    /// Interrupted (SIGINT/SIGTERM); the engine still needs stopping by the caller.
    Interrupted,
}

/// Attaches, forces the build loop into sprint mode, starts the build and streams the
/// agent output frame's content -- agent messages, task lines and agent lifecycle
/// status lines -- to `out` line by line as it arrives. Ends once every task in the
/// store is done, the build fails, an engine error stops it, or `interrupt` is
/// cancelled. The previous run mode is restored on every exit path.
pub async fn run_until<W: Write>(
    mut client: EngineClient<Channel>,
    out: &mut W,
    interrupt: CancellationToken,
) -> anyhow::Result<HeadlessOutcome> {
    // The same attach path the shell takes, so the snapshot decode and the recent-event
    // replay are identical: a headless run attaching mid-build shows the prior output
    // first, like the frame does.
    let (mut app, mut updates) = crate::run::attach(&mut client).await?;
    let mut printer = Printer::default();
    printer.drain(out, &app)?;

    // Sprint while headless is active: the build loop must run the whole store down
    // without waiting between tasks. Forced through the settings-change flow so it
    // validates and persists like a user change, and put back on every exit path.
    let mut restore = None;
    if app.run_mode() != "sprint" {
        let previous = app.run_mode().to_string();
        // A rejected forcing is an engine error that stops the build before it began.
        if let Err(error) = set_run_mode(&mut client, "sprint").await {
            printer.finish(out)?;
            return Ok(HeadlessOutcome::Failed(error));
        }
        restore = Some(previous);
    }

    // Locally recorded completions: the store's TasksChanged can lag the final
    // PhaseChanged, so the last task's Done is recorded here too.
    let mut done: Vec<String> = Vec::new();
    let mut failed: Option<String> = None;
    let mut build_requested = false;
    // The engine flips its phase synchronously when a build is accepted, but the
    // PhaseChanged event arrives on the stream afterwards -- until it does, the
    // snapshot still says idle and a requested build must not read as stopped.
    let mut build_started = false;
    let mut stopped_reason: Option<String> = None;

    // The attach state decides the first move: follow a running build, complete a
    // finished store, or start the build on a pending one.
    if let Some(outcome) = advance(
        &mut client,
        &app,
        &done,
        &mut failed,
        &mut build_requested,
        build_started,
    )
    .await
    {
        return Ok(exit(
            out,
            &mut printer,
            &mut client,
            &mut app,
            &mut updates,
            restore,
            outcome,
        )
        .await);
    }

    loop {
        tokio::select! {
            _ = interrupt.cancelled() => {
                return Ok(exit(
                    out, &mut printer, &mut client, &mut app, &mut updates, restore,
                    HeadlessOutcome::Interrupted,
                )
                .await);
            }
            update = updates.message() => match update {
                Ok(Some(update)) => match update.payload {
                    Some(engine_update::Payload::Event(event)) => {
                        match EngineEvent::from_payload(&event.payload) {
                            Ok(event) => {
                                match &event {
                                    EngineEvent::PhaseChanged { phase: Phase::Running } => {
                                        build_started = true
                                    }
                                    EngineEvent::TaskFinished {
                                        id,
                                        outcome: TaskOutcome::Done,
                                        ..
                                    } => done.push(id.clone()),
                                    EngineEvent::TaskFinished {
                                        id,
                                        outcome: TaskOutcome::Failed,
                                        ..
                                    } => {
                                        failed =
                                            Some(format!("task {id} failed; the build stopped"))
                                    }
                                    _ => {}
                                }
                                app.apply(event);
                                printer.drain(out, &app)?;
                                if let Some(outcome) = advance(
                                    &mut client,
                                    &app,
                                    &done,
                                    &mut failed,
                                    &mut build_requested,
                                    build_started,
                                )
                                .await
                                {
                                    return Ok(exit(
                                        out, &mut printer, &mut client, &mut app,
                                        &mut updates, restore, outcome,
                                    )
                                    .await);
                                }
                            }
                            // The frame would show a status note; without one the run
                            // keeps going on the events that do decode.
                            Err(e) => {
                                eprintln!("headless: unreadable {} event: {e}", event.kind)
                            }
                        }
                    }
                    Some(engine_update::Payload::ShutdownNotice(notice)) => {
                        printer.note(out, format!("Engine stopping: {}", notice.reason))?;
                        stopped_reason = Some(notice.reason);
                    }
                    _ => {}
                },
                Ok(None) => {
                    let outcome = if all_done(&app, &done) {
                        HeadlessOutcome::Completed
                    } else {
                        HeadlessOutcome::Failed(stopped_reason.unwrap_or_else(|| {
                            "the engine stopped before the build completed".into()
                        }))
                    };
                    return Ok(exit(
                        out, &mut printer, &mut client, &mut app, &mut updates, restore,
                        outcome,
                    )
                    .await);
                }
                Err(status) => {
                    // The connection is gone; the restore below cannot reach the engine.
                    printer.finish(out)?;
                    return Ok(HeadlessOutcome::Failed(format!(
                        "lost the engine connection: {}",
                        status.message()
                    )));
                }
            },
        }
    }
}

/// The idle-state decision: complete when every task is done, fail when the build
/// failed or already stopped with work pending, and start the build on a pending
/// store -- once. A build already running at attach never passes through here until
/// it ends, so headless follows it instead of starting a second one. `build_started`
/// marks that the requested build's PhaseChanged has arrived; before it does, an
/// idle-looking snapshot is the race above, not a stopped build.
async fn advance(
    client: &mut EngineClient<Channel>,
    app: &App,
    done: &[String],
    failed: &mut Option<String>,
    build_requested: &mut bool,
    build_started: bool,
) -> Option<HeadlessOutcome> {
    if !app.is_idle() {
        return None;
    }
    if all_done(app, done) {
        return Some(HeadlessOutcome::Completed);
    }
    if let Some(error) = failed.take() {
        return Some(HeadlessOutcome::Failed(error));
    }
    if *build_requested {
        if !build_started {
            return None;
        }
        let pending = app.tasks.iter().filter(|t| !t.done).count();
        return Some(HeadlessOutcome::Failed(format!(
            "the build stopped with {pending} task(s) pending"
        )));
    }
    match submit(client, command_request::Action::StartBuild(StartBuild {})).await {
        Ok(()) => {
            *build_requested = true;
            None
        }
        Err(error) => Some(HeadlessOutcome::Failed(error)),
    }
}

/// Every task in the store is done: per the latest `TasksChanged`, or per the locally
/// recorded completions when that list lags the final `PhaseChanged`.
fn all_done(app: &App, done: &[String]) -> bool {
    app.tasks.iter().all(|t| t.done)
        || app
            .tasks
            .iter()
            .filter(|t| !t.done)
            .all(|t| done.contains(&t.id))
}

/// The shared exit path: the last partial line is terminated, and the previous run
/// mode is put back so the override never outlives the headless run. Best effort on
/// the failure paths -- the engine may already be gone.
async fn exit<W: Write>(
    out: &mut W,
    printer: &mut Printer,
    client: &mut EngineClient<Channel>,
    app: &mut App,
    updates: &mut Streaming<patok_proto::EngineUpdate>,
    restore: Option<String>,
    outcome: HeadlessOutcome,
) -> HeadlessOutcome {
    // The restore first: its confirmation is the last streamed line, and the finish
    // below terminates it so whatever the caller prints next starts on a fresh line.
    if let Some(mode) = restore
        && let Err(error) = restore_mode(client, app, out, printer, updates, &mode).await
    {
        eprintln!("headless: could not restore the `{mode}` run mode: {error}");
    }
    let _ = printer.finish(out);
    outcome
}

/// Submits one command; the error is the engine's rejection reason or the transport
/// failure, both of which stop a headless build.
async fn submit(
    client: &mut EngineClient<Channel>,
    action: command_request::Action,
) -> Result<(), String> {
    let response = client
        .submit_command(CommandRequest {
            action: Some(action),
        })
        .await
        .map_err(|status| format!("command failed: {}", status.message()))?
        .into_inner();
    if response.accepted {
        Ok(())
    } else {
        Err(response.error)
    }
}

/// One daemon-schema field change through the engine's settings flow.
async fn set_run_mode(client: &mut EngineClient<Channel>, mode: &str) -> Result<(), String> {
    let change = to_proto("run_mode", SettingValue::Str(mode.to_string()))?;
    submit(
        client,
        command_request::Action::SettingsChange(SettingsChange {
            change: Some(change),
        }),
    )
    .await
}

/// Puts the previous run mode back and waits for the engine's confirmation event, so
/// the restore surfaces as a notice line in the output like the forcing did. The
/// engine may already be stopping (or gone), so every failure ends the wait quietly.
async fn restore_mode<W: Write>(
    client: &mut EngineClient<Channel>,
    app: &mut App,
    out: &mut W,
    printer: &mut Printer,
    updates: &mut Streaming<patok_proto::EngineUpdate>,
    mode: &str,
) -> Result<(), String> {
    set_run_mode(client, mode).await?;
    let deadline = tokio::time::Instant::now() + RESTORE_CONFIRM;
    loop {
        let update = match tokio::time::timeout_at(deadline, updates.message()).await {
            Ok(Ok(Some(update))) => update,
            _ => return Err("the engine did not confirm the run-mode restore".into()),
        };
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = match EngineEvent::from_payload(&event.payload) {
            Ok(event) => event,
            Err(_) => continue,
        };
        let restored = matches!(
            &event,
            EngineEvent::ConfigChanged {
                field,
                value: SettingValue::Str(value),
                ..
            } if field == "run_mode" && value == mode
        );
        app.apply(event);
        let _ = printer.drain(out, app);
        if restored {
            return Ok(());
        }
    }
}

/// Streams the output frame's lines to a writer as they are built: completed lines
/// are written whole, the still-streaming last line is written in tail deltas, so
/// what arrives on the writer matches what the frame displays at every moment.
#[derive(Default)]
struct Printer {
    /// Lines already written whole, with their newline.
    printed: usize,
    /// What has already been written of the current (last) line, without a newline.
    partial: String,
}

impl Printer {
    /// Writes everything new in `app`'s output lines. Completed lines never mutate
    /// (`Pane::append` only extends the last line while it is open), so the line at
    /// `printed` is final once a newer one exists behind it.
    fn drain<W: Write>(&mut self, out: &mut W, app: &App) -> std::io::Result<()> {
        let lines = &app.output.lines;
        if lines.len() < self.printed {
            // The 5000-line limit dropped the front; the old lines scroll off, as
            // in the frame.
            self.printed = lines.len();
            self.partial.clear();
        }
        while self.printed + 1 < lines.len() {
            self.complete(out, &lines[self.printed].text)?;
            self.printed += 1;
        }
        if let Some(last) = lines.back() {
            let text = &last.text;
            if self.partial.is_empty() {
                if !text.is_empty() {
                    out.write_all(text.as_bytes())?;
                    out.flush()?;
                    self.partial = text.clone();
                }
            } else if let Some(tail) = text.strip_prefix(self.partial.as_str()) {
                if !tail.is_empty() {
                    out.write_all(tail.as_bytes())?;
                    out.flush()?;
                }
                self.partial = text.clone();
            } else {
                // Not a prefix anymore (defensive): the line is rewritten whole.
                out.write_all(text.as_bytes())?;
                out.flush()?;
                self.partial = text.clone();
            }
        }
        Ok(())
    }

    /// Writes one completed line: the part of it not written yet, then the newline.
    fn complete<W: Write>(&mut self, out: &mut W, text: &str) -> std::io::Result<()> {
        match text.strip_prefix(self.partial.as_str()) {
            Some(tail) => out.write_all(tail.as_bytes())?,
            None => out.write_all(text.as_bytes())?,
        }
        out.write_all(b"\n")?;
        self.partial.clear();
        Ok(())
    }

    /// Terminates the still-streaming last line, so it is never left unflushed. The
    /// line counts as printed from here on: a later drain (the restore confirmation)
    /// must not write it again.
    fn finish<W: Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        if !self.partial.is_empty() {
            out.write_all(b"\n")?;
            out.flush()?;
            self.partial.clear();
            self.printed += 1;
        }
        Ok(())
    }

    /// One line that is not agent output (the engine's shutdown notice): the
    /// streaming line is terminated first so the note stands on its own.
    fn note<W: Write>(&mut self, out: &mut W, text: String) -> std::io::Result<()> {
        self.finish(out)?;
        out.write_all(text.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_core::event::{AgentEvent, Phase, Snapshot};
    use patok_core::task::Task;

    fn app_with_one_task() -> App {
        App::new(
            Snapshot {
                project_dir: String::new(),
                phase: Phase::Startup,
                tasks: vec![Task {
                    id: "T1.1".into(),
                    origin: Some('T'),
                    description: String::new(),
                    done: false,
                    line: 1,
                    raw: String::new(),
                }],
                current_task: None,
                planning: false,
                discovering: false,
                provider: String::new(),
                model: String::new(),
                settings: Default::default(),
                pipeline: Default::default(),
                recent: vec![],
            },
            "test".into(),
        )
    }

    /// A text line written in deltas completes without duplication.
    #[test]
    fn streamed_line_is_written_in_deltas() {
        let mut app = app_with_one_task();
        let mut out = Vec::new();
        let mut printer = Printer::default();
        for chunk in ["hello", " world", "!"] {
            app.apply(EngineEvent::Agent {
                event: AgentEvent::TextDelta { text: chunk.into() },
            });
            printer.drain(&mut out, &app).unwrap();
        }
        app.apply(EngineEvent::TaskFinished {
            id: "T1.1".into(),
            outcome: TaskOutcome::Done,
            commit: None,
        });
        printer.drain(&mut out, &app).unwrap();
        printer.finish(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "hello world!\n✔ T1.1 done\n"
        );
    }
}
