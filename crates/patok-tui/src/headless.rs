//! The headless driver: the agent output frame's content, line by line on a writer,
//! with the build run to completion. Renders nothing; `App` is reused as the line
//! builder so the streamed lines are exactly the ones the frame displays.

use std::io::Write;
use std::time::Duration;

use crossterm::style::Stylize;
use patok_core::config::SettingValue;
use patok_core::event::{EngineEvent, Phase, TaskOutcome};
use patok_proto::engine_client::EngineClient;
use patok_proto::{CommandRequest, SettingsChange, StartBuild, command_request, engine_update};
use tokio_util::sync::CancellationToken;
use tonic::Streaming;
use tonic::transport::Channel;

use crate::app::{App, LineKind, OutLine};
use crate::settings::to_proto;
use crate::theme::Theme;

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
/// cancelled. The previous run mode is restored on every exit path. With `color`,
/// the streamed lines wear the user's theme colours — the same kind-to-colour and
/// agent-name-to-colour mappings the frame renders with; without it the output
/// stays plain.
pub async fn run_until<W: Write>(
    mut client: EngineClient<Channel>,
    out: &mut W,
    interrupt: CancellationToken,
    color: bool,
) -> anyhow::Result<HeadlessOutcome> {
    // The same attach path the shell takes, so the snapshot decode and the recent-event
    // replay are identical: a headless run attaching mid-build shows the prior output
    // first, like the frame does.
    let (mut app, mut updates) = crate::run::attach(&mut client).await?;
    let mut printer = Printer {
        theme: color.then(|| app.theme()),
        ..Printer::default()
    };
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
/// With a theme, every written piece is wrapped in the line kind's theme colour and
/// an agent's name inside a line wears the agent's own fixed colour (T110.1);
/// without a theme the bytes are the plain text. Name-bearing lines are only ever
/// opened by a push, never by a streaming append, so they are always written whole.
#[derive(Default)]
struct Printer {
    /// The theme whose colours wrap the output, or `None` for plain output.
    theme: Option<Theme>,
    /// Lines already written whole, with their newline.
    printed: usize,
    /// What has already been written of the current (last) line, without a newline.
    /// Always the plain text: the colour escapes reach the writer but never this
    /// buffer, so the delta logic keeps comparing plain text against plain text.
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
            self.complete(out, &lines[self.printed])?;
            self.printed += 1;
        }
        if let Some(last) = lines.back() {
            let text = &last.text;
            let kind = last.kind;
            if self.partial.is_empty() {
                if !text.is_empty() {
                    out.write_all(self.whole(last).as_bytes())?;
                    out.flush()?;
                    self.partial = text.clone();
                }
            } else if let Some(tail) = text.strip_prefix(self.partial.as_str()) {
                if !tail.is_empty() {
                    out.write_all(self.wrap(tail, kind).as_bytes())?;
                    out.flush()?;
                }
                self.partial = text.clone();
            } else {
                // Not a prefix anymore (defensive): the line is rewritten whole.
                if !text.is_empty() {
                    out.write_all(self.whole(last).as_bytes())?;
                    out.flush()?;
                }
                self.partial = text.clone();
            }
        }
        Ok(())
    }

    /// Writes one completed line: the part of it not written yet, then the newline.
    fn complete<W: Write>(&mut self, out: &mut W, line: &OutLine) -> std::io::Result<()> {
        match line.text.strip_prefix(self.partial.as_str()) {
            Some(tail) if !tail.is_empty() => {
                // Nothing of this line was streamed before: it is written
                // whole, agent name and all (T110.1).
                let bytes = if self.partial.is_empty() {
                    self.whole(line)
                } else {
                    self.wrap(tail, line.kind).into_owned()
                };
                out.write_all(bytes.as_bytes())?
            }
            Some(_) => {}
            None => out.write_all(self.wrap(&line.text, line.kind).as_bytes())?,
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
        out.write_all(self.wrap(&text, LineKind::Notice).as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()
    }

    /// One whole line as written bytes: a line that names an agent (T110.1)
    /// colours the name in the agent's own fixed colour between the line
    /// kind's colour; every other line is wrapped in the kind's colour alone.
    fn whole(&self, line: &OutLine) -> String {
        let Some((before, name, after)) = line.name_pieces() else {
            return self.wrap(&line.text, line.kind).into_owned();
        };
        let Some(theme) = self.theme else {
            return line.text.clone();
        };
        let kind_colour = Theme::line_color(theme, line.kind);
        let name_colour = Theme::agent_name_color(theme, line.agent.as_deref().unwrap_or_default());
        let bold = line.kind == LineKind::Heading;
        let piece = |text: &str, colour: ratatui::style::Color| {
            let styled = crossterm::style::style(text).with(term_color(colour));
            let styled = if bold {
                styled.attribute(crossterm::style::Attribute::Bold)
            } else {
                styled
            };
            styled.to_string()
        };
        // Empty pieces emit nothing: the name line stays a tight two escapes.
        let mut bytes = String::new();
        if !before.is_empty() {
            bytes.push_str(&piece(before, kind_colour));
        }
        bytes.push_str(&piece(name, name_colour));
        if !after.is_empty() {
            bytes.push_str(&piece(after, kind_colour));
        }
        bytes
    }

    /// Wraps one written piece in the line kind's theme colour: the colour prefix,
    /// the plain text, a trailing reset. `Text` lines and a colourless printer
    /// return the text unchanged. Every wrapped piece ends with a reset, so a
    /// flush mid-line never leaves the terminal's colour state dangling.
    fn wrap<'a>(&self, text: &'a str, kind: LineKind) -> std::borrow::Cow<'a, str> {
        let Some(theme) = self.theme else {
            return std::borrow::Cow::Borrowed(text);
        };
        if kind == LineKind::Text {
            return std::borrow::Cow::Borrowed(text);
        }
        let styled = crossterm::style::style(text).with(term_color(Theme::line_color(theme, kind)));
        let styled = if kind == LineKind::Heading {
            styled.attribute(crossterm::style::Attribute::Bold)
        } else {
            styled
        };
        std::borrow::Cow::Owned(styled.to_string())
    }
}

/// Converts a render colour to the terminal colour: the two crates disagree on
/// naming -- ratatui's plain names are the standard 0-7 colours while crossterm's
/// are the bright 8-15 variants -- so the mapping is spelled out to keep the
/// streamed colours identical to the frame's.
fn term_color(color: ratatui::style::Color) -> crossterm::style::Color {
    use crossterm::style::Color as T;
    use ratatui::style::Color as R;
    match color {
        R::Reset => T::Reset,
        R::Black => T::Black,
        R::Red => T::DarkRed,
        R::Green => T::DarkGreen,
        R::Yellow => T::DarkYellow,
        R::Blue => T::DarkBlue,
        R::Magenta => T::DarkMagenta,
        R::Cyan => T::DarkCyan,
        R::Gray => T::Grey,
        R::DarkGray => T::DarkGrey,
        R::LightRed => T::Red,
        R::LightGreen => T::Green,
        R::LightYellow => T::Yellow,
        R::LightBlue => T::Blue,
        R::LightMagenta => T::Magenta,
        R::LightCyan => T::Cyan,
        R::White => T::White,
        R::Rgb(r, g, b) => T::Rgb { r, g, b },
        R::Indexed(i) => T::AnsiValue(i),
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

    /// With a theme, the deltas are wrapped in the line kinds' colours without
    /// being duplicated: the `Text` deltas stay escape-free, the completed status
    /// line carries its colour prefix and trailing reset, and no text is written
    /// twice.
    #[test]
    fn colored_lines_are_wrapped_and_deltas_are_not_duplicated() {
        let mut app = app_with_one_task();
        let mut out = Vec::new();
        let mut printer = Printer {
            theme: Some(Theme::DARK),
            ..Printer::default()
        };
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
        let buffer = String::from_utf8(out).unwrap();
        // The text line is a `Text` line: exactly the plain deltas, no escapes.
        let (text, status) = buffer
            .split_once('\n')
            .expect("the streamed line precedes the status line");
        assert_eq!(text, "hello world!", "buffer: {buffer:?}");
        assert!(
            !text.contains('\x1b'),
            "the Text line must stay plain: {text:?}"
        );
        // The status line is wrapped in its kind's theme colour and ends with
        // the colour reset before the newline. (The done line's kind resolves
        // to the dark theme's yellow in this snapshot: SGR 38;5;3.)
        assert!(
            status.starts_with("\x1b[38;5;3m"),
            "the status line carries its theme colour: {status:?}"
        );
        assert!(
            status.contains("✔ T1.1 done"),
            "the plain text survives the wrapping: {status:?}"
        );
        assert!(
            status.ends_with("\x1b[39m\n") || status.ends_with("\x1b[0m\n"),
            "the status line ends with a colour reset: {status:?}"
        );
        // The deltas were never written twice.
        assert_eq!(buffer.matches("hello").count(), 1, "buffer: {buffer:?}");
        assert_eq!(buffer.matches("T1.1 done").count(), 1, "buffer: {buffer:?}");
    }

    /// A line that names an agent (T110.1) writes the name in the agent's own
    /// fixed colour and the rest in the line kind's colour: the dark theme's
    /// builder green for `Builder`, the status magenta for the remainder. A
    /// colourless printer writes the raw text unchanged.
    #[test]
    fn agent_name_lines_colour_the_name() {
        let mut app = app_with_one_task();
        app.apply(EngineEvent::AgentStarted {
            agent: "builder".into(),
            provider: "mock".into(),
            model: None,
        });
        // Coloured: the whole line is exactly the two coloured pieces and
        // the newline, so the name span is the only builder-coloured text.
        let mut out = Vec::new();
        let mut printer = Printer {
            theme: Some(Theme::DARK),
            ..Printer::default()
        };
        printer.drain(&mut out, &app).unwrap();
        printer.finish(&mut out).unwrap();
        let name = crossterm::style::style("Builder")
            .with(term_color(Theme::agent_name_color(Theme::DARK, "builder")))
            .to_string();
        let rest = crossterm::style::style(" started (mock)")
            .with(term_color(Theme::line_color(Theme::DARK, LineKind::Status)))
            .to_string();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{name}{rest}\n"),
            "the name wears the agent colour, the rest the kind colour"
        );

        // Plain: no escapes at all.
        let mut out = Vec::new();
        let mut printer = Printer::default();
        printer.drain(&mut out, &app).unwrap();
        printer.finish(&mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "Builder started (mock)\n");
    }
}
