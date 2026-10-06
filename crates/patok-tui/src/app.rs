//! Shell state and its transitions. Pure: no I/O, so it is tested without a terminal.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use patok_core::config::{ApplyTiming, SettingValue, Theme as ThemeKey, TuiSettings};
use patok_core::event::{
    AgentEvent, EngineEvent, NoticeLevel, Phase, Snapshot, TaskOutcome, Usage,
};
use patok_core::pipeline::PipelineState;
use patok_core::scenario::{ProjectScan, Scenario, SpecState};
use patok_core::task::Task;
use ratatui::layout::Rect;

use crate::overlay::{Entry, Schema, SettingsOverlay, StatusLevel};
use crate::theme::{Theme, theme_modal_keys};
use crate::ui::{
    agent_display, finished_line, footer_button_rects, frame_at, started_line, theme_row_at,
};

/// Output lines kept; older ones scroll off for good.
const OUTPUT_LIMIT: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Text,
    /// The agent's thinking; rendered as markdown in the thinking colour.
    Thinking,
    Tool,
    Result,
    Error,
    Notice,
    /// A status line, not agent content: the agent-started line (T78.1) and
    /// the finished-agent line naming the agent type and the session's
    /// duration (T42.1), in the heading colour (T96.1).
    Status,
    Heading,
}

/// Agent output lines, with the streaming-line state deltas append to.
#[derive(Default)]
pub struct Pane {
    pub lines: VecDeque<OutLine>,
    /// Whether the last line is a streaming line that deltas append to.
    open: bool,
}

impl Pane {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn push(&mut self, kind: LineKind, text: String) {
        self.open = false;
        self.add(kind, text, None);
    }

    /// Pushes one complete line that names an agent (T110.1): the name keeps
    /// its fixed colour identity wherever the line renders.
    fn push_agent(&mut self, kind: LineKind, text: String, agent: &str) {
        self.open = false;
        self.add(kind, text, Some(agent.to_string()));
    }

    fn add(&mut self, kind: LineKind, text: String, agent: Option<String>) {
        if self.lines.len() == OUTPUT_LIMIT {
            self.lines.pop_front();
        }
        self.lines.push_back(OutLine { kind, text, agent });
    }

    /// Appends a streamed chunk to the current line, starting new lines at newlines.
    fn append(&mut self, chunk: &str) {
        for (i, piece) in chunk.split('\n').enumerate() {
            if i > 0 {
                self.open = false;
            }
            if piece.is_empty() {
                continue;
            }
            match self.lines.back_mut() {
                Some(line) if self.open => line.text.push_str(piece),
                _ => self.add(LineKind::Text, piece.to_string(), None),
            }
            self.open = true;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutLine {
    pub kind: LineKind,
    pub text: String,
    /// The agent whose name this line carries (T110.1): the lifecycle status
    /// lines and the planning heading; `None` on every other line.
    pub agent: Option<String>,
}

impl OutLine {
    /// Splits this line's text around the agent name it carries (T110.1):
    /// `(before, name, after)` as slices of `text`, or `None` on a line that
    /// carries no name. The lifecycle status lines lead with the display-cased
    /// name; the planning heading carries the raw name after the marker. Both
    /// producers build the text here, so the layouts are known — a mismatch
    /// falls back to `None` and the line renders like any other of its kind.
    pub fn name_pieces(&self) -> Option<(&str, &str, &str)> {
        let agent = self.agent.as_deref()?;
        match self.kind {
            LineKind::Status => {
                let name = agent_display(agent);
                let after = self.text.strip_prefix(name.as_str())?;
                let end = self.text.len() - after.len();
                Some(("", &self.text[..end], after))
            }
            LineKind::Heading => {
                let name = self.text.strip_prefix("── ")?;
                if name != agent {
                    return None;
                }
                Some(("── ", name, ""))
            }
            _ => None,
        }
    }
}

/// The two research queue-creation runs (T69.1): the Research agent's second
/// duty besides investigating a task is creating the project's initial task
/// queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueRun {
    /// The NeedsQueue scenario's empty submit: the research agent creates the
    /// initial queue from the spec.
    Bootstrap,
    /// The QueueComplete scenario's empty submit: the research agent scans
    /// for gaps and worthwhile follow-up work.
    Scan,
}

impl QueueRun {
    /// The informational request text the shell sends along with the run's
    /// kind; the engine's queue-creation prompt is self-contained.
    pub fn request(self) -> &'static str {
        match self {
            Self::Bootstrap => "Scan the project and create an initial task queue",
            Self::Scan => {
                "Scan the project for gaps and worthwhile follow-up work, and add tasks for what you find"
            }
        }
    }
}

/// What the key handler asks the driver to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    StartBuild,
    /// Soft-stop the build loop (SOFT scope): the current task finishes, no
    /// further task starts, the engine keeps running and the shell stays open.
    Quit,
    /// Interrupt the run: cancel the current task and stop the build loop
    /// now; the engine returns to idle and the app keeps running. While idle
    /// (the plain `q`) it is the true quit: the NOW shutdown exits the engine.
    Interrupt,
    /// Cancel a pending soft stop (Esc while one is pending): the driver sends
    /// the `CancelSoftStop` command, and the build loop continues with the
    /// next pending task once the running task finishes.
    CancelSoftStop,
    /// Leave the shell; the engine keeps running.
    Detach,
    /// Soft-stop the engine, start a fresh one and reattach. Only ever user-initiated.
    Restart,
    /// Send the add-task dialog's text to the engine as an `AddTasks` command.
    SubmitTasks(String),
    /// Start a research queue-creation run (T69.1): the NeedsQueue and
    /// QueueComplete scenarios' empty submits send the Research agent, which
    /// investigates the project and appends the resulting tasks.
    ResearchQueue(QueueRun),
    /// Run one discovery round now: Enter while the engine is idle sends the
    /// `RunDiscovery` command when no uncompleted task remains (T46.1).
    RunDiscovery,
    /// Inject the inject-task modal's text as the next task line of TASKS.md
    /// (T76.1): the engine normalizes the confirmed text into a
    /// well-formed unchecked task line -- adding the checkbox and the next
    /// `T<N>.1` id when missing, keeping an already formatted line -- and
    /// appends it under its task-file lock, with no agent session, no
    /// planner, no discovery; the engine's reconcile picks the line up
    /// right away.
    InjectTask(String),
    /// Save the dialog's text as the project brief into SPEC.md (EmptyProject submit);
    /// the dialog stays open with the created message.
    SaveBrief(String),
    /// The settings overlay closed with nothing left to apply (a clean close or a
    /// discard).
    CloseSettings,
    /// Apply and persist every drafted change: daemon fields through the engine's settings-change flow, tui
    /// fields directly by the shell; the driver closes the overlay once every
    /// draft is applied.
    SaveSettings,
    /// Keep the theme the picker previewed and persist it as the tui config's
    /// `theme` value through the shell's read-modify-write (T43.1). The app
    /// state already carries the theme; the driver only writes the file.
    SaveTheme(ThemeKey),
    /// S-Tab on the main view (T60.1): flip the run mode to its other value
    /// (`sprint` <-> `continuous`) by sending it through the engine's
    /// settings-change flow, so it validates, persists and reports back like
    /// an overlay save. Carries the newly selected mode's name.
    SetRunMode(String),
}

/// The stop dialog's choices (Esc while a build runs, T46.1): what the three
/// rows do, in the order they render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopChoice {
    /// The SOFT-scope shutdown: finish the current task, then stop the loop.
    SoftStop,
    /// The NOW-scope shutdown: cancel the running task and stop the loop
    /// without waiting; the app keeps running.
    Interrupt,
    /// Close the dialog; the build keeps running unchanged.
    Cancel,
}

impl StopChoice {
    pub fn label(self) -> &'static str {
        match self {
            StopChoice::SoftStop => "Soft stop",
            StopChoice::Interrupt => "Interrupt",
            StopChoice::Cancel => "Cancel",
        }
    }

    /// The one-line explanation rendered after the label.
    pub fn detail(self) -> &'static str {
        match self {
            StopChoice::SoftStop => "finish the current task, then stop",
            StopChoice::Interrupt => "cancel the current task, stay open",
            StopChoice::Cancel => "keep the build running",
        }
    }
}

/// The stop dialog's rows, top to bottom; `stop_selected` indexes this list.
pub const STOP_CHOICES: [StopChoice; 3] = [
    StopChoice::SoftStop,
    StopChoice::Interrupt,
    StopChoice::Cancel,
];

/// The status shown while a soft stop is pending (the first `q` or the stop
/// dialog's soft-stop choice, before the running task finishes): it names the
/// two keys that act on the pending stop.
pub const SOFT_STOP_PENDING: &str = "Soft stop requested -- the build stops after the current task and the app keeps running; press Esc to cancel the stop or q to cancel the current task now. [d] detach instead";

/// The merged view's focused frame: which of the two stacked frames takes all the
/// free space (the other shrinks to 5 content lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFocus {
    /// The agent output frame.
    Output,
    /// The task list frame.
    Tasks,
}

/// Which modal the shared dialog input drives (T76.1): both modals are the
/// same input box -- same styling, same cursor and multi-line editing, same
/// cancel behaviour -- they differ only in their title, watermark, primary
/// label and what confirming does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// The add-task dialog (`a`): the scenario-driven submit of T74.1, which
    /// sends the text to the engine's planner or discovery flow.
    Add,
    /// The inject-task modal (`i`): the engine normalizes the text into a
    /// well-formed unchecked task line before appending it to TASKS.md,
    /// with no agent session.
    Inject,
}

/// The theme picker modal's state (T43.1) -- its own modal state, distinct from
/// the settings overlay's: the selection's row among the built-in themes, the
/// theme that was active when the modal opened (Esc restores it exactly), the
/// live preview applied to the whole shell while browsing, and the body rect
/// recorded at the last render for mouse hit-testing.
#[derive(Default)]
pub struct ThemeModal {
    pub open: bool,
    pub selected: usize,
    pub original: ThemeKey,
    pub preview: Option<ThemeKey>,
    /// The modal's body rect at the last render; mouse hit-testing reads it.
    pub area: Cell<Rect>,
    /// The modal's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it (T59.1).
    pub footer: Cell<Rect>,
    /// The modal's close button rect at the last render (T66.1); a click on
    /// it runs the modal's Esc key.
    pub close: Cell<Rect>,
}

pub struct App {
    pub project: String,
    /// The focused frame of the merged view; it takes all the free space. Defaults
    /// to the task list while the engine is idle and the agent output while a run
    /// is active.
    pub focus: FrameFocus,
    /// The scanned project facts behind the idle scenario.
    pub project_status: ProjectScan,
    pub engine_version: String,
    /// Name of the engine's active provider; empty when unknown.
    pub provider: String,
    /// The engine's configured model name; empty when the provider picks its default.
    pub model: String,
    /// The configured provider and model behind [`App::provider`] and
    /// [`App::model`]: the baseline the output frame title restores to after
    /// a session that runs on another provider (a reviewer under a `claude`
    /// reviewer provider) ends.
    pub config_provider: String,
    pub config_model: String,
    /// The engine's active agent ("planner" during the plan stage, "builder" afterwards);
    /// announced by `AgentChanged` events and replayed with the recent events (T11.1).
    pub agent: String,
    /// The engine's build differs from this shell's (set once, on attach).
    pub version_mismatch: bool,
    pub phase: Phase,
    pub tasks: Vec<Task>,
    pub current_task: Option<String>,
    pub output: Pane,
    /// Visual lines scrolled up from the bottom; 0 follows the live output.
    pub scroll: usize,
    /// Largest useful scroll at the last render; keeps `scroll` from running past the top.
    pub max_scroll: Cell<usize>,
    /// Task rows scrolled up from the auto-follow view; 0 keeps the first pending
    /// task a third of the way down.
    pub task_scroll: usize,
    /// Largest useful task scroll at the last render.
    pub task_max_scroll: Cell<usize>,
    /// The agent output frame's rect at the last render; mouse hit-testing reads it.
    pub output_area: Cell<Rect>,
    /// The task list frame's rect at the last render; mouse hit-testing reads it.
    pub tasks_area: Cell<Rect>,
    pub usage: Option<Usage>,
    /// A one-line message in the status bar (rejections, quit progress).
    pub status: Option<String>,
    pub stopping: bool,
    /// The planner is running.
    pub planning: bool,
    /// A discovery round is running.
    pub discovering: bool,
    /// The active session's start: `Some` from the moment the engine leaves
    /// idle until it returns, `None` while idle. A session is a build, planner
    /// or discovery run (the same classification `is_idle` makes). Re-anchored
    /// by `AgentChanged.started_ms`, so the timer and the finished line derive
    /// from the engine-recorded session timing (T42.1).
    pub session_start: Option<Instant>,
    /// One epoch anchor captured at construction, mapping the engine's epoch
    /// milliseconds onto the shell's `Instant` clock (T42.1).
    epoch_anchor: (SystemTime, Instant),
    /// The render clock the timer reads; the driver refreshes it before every
    /// frame. Tests that leave it still get a deterministic `00:00`.
    pub now: Cell<Instant>,
    pub completed: usize,
    /// The add-task dialog is open (true for both dialog kinds: the add-task
    /// dialog and the inject-task modal share the input).
    pub dialog_open: bool,
    /// Which modal the open dialog is (T76.1): the add-task dialog or the
    /// inject-task modal. Pinned back to [`DialogKind::Add`] whenever the
    /// add-task dialog opens, so a reopen never inherits the inject kind.
    pub dialog_kind: DialogKind,
    /// The stop dialog is open (Esc pressed while a build runs, T46.1).
    pub stop_open: bool,
    /// The stop dialog's selected row; indexes [`STOP_CHOICES`].
    pub stop_selected: usize,
    /// The stop dialog's bottom line rect at the last render; its buttons'
    /// mouse hit-testing reads it (T59.1).
    pub stop_footer: Cell<Rect>,
    /// The stop dialog's close button rect at the last render (T66.1); a click
    /// on it runs the dialog's Esc key.
    pub stop_close: Cell<Rect>,
    /// Text typed into the add-task dialog; kept when the dialog is closed.
    pub dialog_text: String,
    /// Cursor in `dialog_text` as a character index; read it through [`App::cursor`].
    pub dialog_cursor: usize,
    /// Column Up/Down try to return to across rows of different lengths.
    dialog_goal_col: Option<usize>,
    /// Wrap width of the dialog input at the last render (0 before the first).
    pub dialog_width: Cell<usize>,
    /// The dialog's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it (T59.1).
    pub dialog_footer: Cell<Rect>,
    /// The dialog's close button rect at the last render (T66.1); a click on
    /// it runs the dialog's Esc key.
    pub dialog_close: Cell<Rect>,
    /// A one-line message in the dialog (empty input, command errors).
    pub dialog_status: Option<String>,
    /// IDs of tasks appended since the last key press or build start; shown highlighted.
    pub new_tasks: Vec<String>,
    /// The settings overlay's interaction state.
    pub overlay: SettingsOverlay,
    /// The theme picker modal's state (T43.1), distinct from the overlay.
    pub theme_modal: ThemeModal,
    /// The shell's tui-schema settings as the overlay renders them; refreshed by the
    /// driver after every load, reload and apply.
    pub tui: TuiSettings,
    /// The engine-reported daemon readout: every registry field with its effective value
    /// (seeded from the attach snapshot, updated by every `ConfigChanged`), which the
    /// overlay's rows and any run-mode display read from.
    pub settings: BTreeMap<String, SettingValue>,
    /// The engine-reported pipeline rail: seeded from the
    /// attach snapshot and updated by every `PipelineChanged`; rendered by the pipeline
    /// rail of T50.1.
    pub pipeline: PipelineState,
    /// The rail's rect at the last render.
    pub pipeline_area: Cell<Rect>,
}

/// One visual row of the dialog input: a character range of `dialog_text`.
/// `last` marks the final row of a logical line (the cursor may sit at its end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogRow {
    pub start: usize,
    pub end: usize,
    pub last: bool,
}

impl App {
    pub fn new(snapshot: Snapshot, engine_version: String) -> Self {
        let mut app = Self {
            project: snapshot.project_dir,
            focus: FrameFocus::Tasks,
            project_status: ProjectScan::default(),
            engine_version,
            provider: snapshot.provider.clone(),
            model: snapshot.model.clone(),
            config_provider: snapshot.provider,
            config_model: snapshot.model,
            agent: "builder".into(),
            version_mismatch: false,
            phase: snapshot.phase,
            tasks: snapshot.tasks,
            current_task: snapshot.current_task,
            output: Pane::default(),
            scroll: 0,
            max_scroll: Cell::new(0),
            task_scroll: 0,
            task_max_scroll: Cell::new(0),
            output_area: Cell::new(Rect::default()),
            tasks_area: Cell::new(Rect::default()),
            usage: None,
            status: None,
            stopping: false,
            planning: false,
            discovering: false,
            session_start: None,
            epoch_anchor: (SystemTime::now(), Instant::now()),
            now: Cell::new(Instant::now()),
            completed: 0,
            dialog_open: false,
            dialog_kind: DialogKind::Add,
            stop_open: false,
            stop_selected: 0,
            stop_footer: Cell::new(Rect::default()),
            stop_close: Cell::new(Rect::default()),
            dialog_text: String::new(),
            dialog_cursor: 0,
            dialog_goal_col: None,
            dialog_width: Cell::new(0),
            dialog_footer: Cell::new(Rect::default()),
            dialog_close: Cell::new(Rect::default()),
            dialog_status: None,
            new_tasks: Vec::new(),
            overlay: SettingsOverlay::default(),
            theme_modal: ThemeModal::default(),
            tui: TuiSettings::default(),
            settings: snapshot.settings,
            pipeline: snapshot.pipeline,
            pipeline_area: Cell::new(Rect::default()),
        };
        for event in snapshot.recent {
            app.apply(event);
        }
        // Replayed history is not this session's work.
        app.completed = 0;
        app.new_tasks.clear();
        app.planning = snapshot.planning;
        app.discovering = snapshot.discovering;
        // A replayed `AgentChanged` already anchored the timer to the engine's
        // session start, so a shell attaching mid-run shows the true elapsed
        // time; only an unanchored busy attach counts from here (T42.1).
        if app.session_start.is_none() {
            app.session_start = (!app.is_idle()).then(|| app.now.get());
        }
        app.focus = app.state_focus();
        app
    }

    /// Compares the engine's reported build version with this shell's own.
    pub fn check_version(&mut self, shell_version: &str) {
        self.version_mismatch = self.engine_version != shell_version;
    }

    /// A shell-side notice line in the main pane (e.g. a config reload warning).
    pub fn shell_notice(&mut self, text: impl Into<String>) {
        self.push(LineKind::Notice, text.into());
    }

    pub fn apply(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::PhaseChanged { phase } => {
                let was_idle = self.is_idle();
                if phase == Phase::Running {
                    self.new_tasks.clear();
                }
                self.phase = phase;
                // Leaving the idle state points the view at the run's output.
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::TaskStarted { id, description } => {
                self.current_task = Some(id.clone());
                self.push(LineKind::Heading, format!("── {id}: {description}"));
            }
            EngineEvent::Agent { event } => self.apply_agent(event),
            // Replayed with the recent events, so a reattaching shell shows the right one.
            // The engine-recorded start re-anchors the running timer, so it and the
            // finished line derive from the same session timing (T42.1).
            EngineEvent::AgentChanged { agent, started_ms } => {
                self.agent = agent;
                if started_ms > 0 && !self.is_idle() {
                    self.session_start = Some(self.anchored(started_ms));
                }
            }
            // One agent session began: one line naming the agent type, the
            // provider it runs on and the configured model when one exists,
            // pairing with the finished line at the session's end (T78.1). The
            // active agent and the timer stay owned by the adjacent
            // `AgentChanged` arm. The session's provider and model drive the
            // output frame title while it runs, so a reviewer on another
            // provider (a `claude` reviewer provider) is titled accordingly instead
            // of silently reusing the configured one (T81.1).
            EngineEvent::AgentStarted {
                agent,
                provider,
                model,
            } => {
                let text = started_line(&agent, &provider, model.as_deref());
                self.push_agent(LineKind::Status, text, &agent);
                self.provider = provider;
                self.model = model.unwrap_or_default();
            }
            // One agent session ended: one line naming the agent type and the
            // session's total duration lands at the end of the pane (T42.1). The
            // timer hides with the idle transition that follows, and the frame
            // title returns to the configured provider and model (T81.1).
            EngineEvent::AgentFinished {
                agent,
                outcome,
                duration_ms,
            } => {
                let text = finished_line(&agent, outcome, Duration::from_millis(duration_ms));
                self.push_agent(LineKind::Status, text, &agent);
                self.provider = self.config_provider.clone();
                self.model = self.config_model.clone();
            }
            // The running task (the pane's marker) is tracked by ID, so a replaced list keeps it.
            EngineEvent::TasksChanged { tasks } => {
                for task in &tasks {
                    if !self.tasks.iter().any(|t| t.id == task.id) {
                        self.new_tasks.push(task.id.clone());
                    }
                }
                self.tasks = tasks;
            }
            EngineEvent::PlanningChanged { planning } => {
                let was_idle = self.is_idle();
                if planning {
                    // The engine announces the run's agent (the planner, or the
                    // research agent for a queue-creation run, T69.1) before
                    // this event, so the status line and the heading name it.
                    self.status = Some(format!("{} running...", agent_display(&self.agent)));
                    // The run's output streams into the main pane next to earlier
                    // output; a heading keeps runs visually separate (T24.1),
                    // with the agent's name in its own fixed colour (T110.1).
                    let agent = self.agent.clone();
                    self.push_agent(LineKind::Heading, format!("── {agent}"), &agent);
                } else if self.planning {
                    // The run's outcome is the notice line in the output pane; the
                    // status bar returns to its normal content instead of repeating
                    // it (T29.1).
                    self.status = None;
                }
                self.planning = planning;
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::DiscoveryChanged { discovering } => {
                let was_idle = self.is_idle();
                self.discovering = discovering;
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::TaskFinished {
                id,
                outcome,
                commit,
            } => {
                self.current_task = None;
                let sha = commit.map(|c| format!(" ({c})")).unwrap_or_default();
                let (kind, text) = match outcome {
                    TaskOutcome::Done => {
                        self.completed += 1;
                        (LineKind::Notice, format!("✔ {id} done{sha}"))
                    }
                    TaskOutcome::Failed => (LineKind::Error, format!("✘ {id} failed{sha}")),
                    TaskOutcome::Cancelled => (LineKind::Notice, format!("■ {id} cancelled")),
                };
                self.push(kind, text);
            }
            // A daemon-schema field changed on the engine:
            // the readout the overlay renders from stays in sync, the routing fields
            // keep the display in sync, every other field is a notice line with its
            // apply timing.
            EngineEvent::ConfigChanged {
                field,
                value,
                timing,
            } => {
                self.settings.insert(field.clone(), value.clone());
                match field.as_str() {
                    "provider" => {
                        self.config_provider = value.to_string();
                        self.provider = self.config_provider.clone();
                    }
                    "model" => {
                        self.config_model = value.to_string();
                        self.model = self.config_model.clone();
                    }
                    _ => {
                        let mut text = format!("Setting `{field}` changed to `{value}`");
                        if timing == ApplyTiming::NextUnitOfWork {
                            text.push_str(" (takes effect on the next start)");
                        }
                        self.push(LineKind::Notice, text);
                    }
                }
            }
            // The pipeline rail changed: the full state
            // replaces the previous one. Rendered by the pipeline rail of T50.1.
            EngineEvent::PipelineChanged { state } => self.pipeline = state,
            EngineEvent::Notice { level, text } => {
                let kind = if level == NoticeLevel::Error {
                    LineKind::Error
                } else {
                    LineKind::Notice
                };
                self.push(kind, text);
            }
        }
    }

    fn apply_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta { text } => self.append(&text),
            AgentEvent::Text { text } => self.push(LineKind::Text, text),
            AgentEvent::Thinking { text } => self.push(LineKind::Thinking, text),
            AgentEvent::ToolUse { name, input } => {
                self.push(LineKind::Tool, format!("▸ {name} {input}"))
            }
            AgentEvent::ToolResult { output } => {
                self.push(LineKind::Result, format!("  ↳ {output}"))
            }
            AgentEvent::Stderr { text } | AgentEvent::Error { text, .. } => {
                self.push(LineKind::Error, text)
            }
            AgentEvent::Result { text } => {
                if !text.is_empty() {
                    self.push(LineKind::Result, format!("result: {text}"));
                }
            }
            AgentEvent::Usage(usage) if !self.planning => self.usage = Some(usage),
            AgentEvent::Usage(_) => {}
        }
    }

    fn push(&mut self, kind: LineKind, text: String) {
        self.output.push(kind, text);
    }

    /// Pushes a line that names an agent (T110.1): the name keeps its fixed
    /// colour identity in every surface that renders it.
    fn push_agent(&mut self, kind: LineKind, text: String, agent: &str) {
        self.output.push_agent(kind, text, agent);
    }

    fn append(&mut self, chunk: &str) {
        self.output.append(chunk);
    }

    pub fn is_new_task(&self, id: &str) -> bool {
        self.new_tasks.iter().any(|t| t == id)
    }

    /// The engine-reported run mode shown as the status line's second chip; "sprint"
    /// until the engine reports one, so old-engine
    /// attaches render sanely. `ConfigChanged` and `on_settings_applied` keep it
    /// in sync; the shell only mirrors the reported value.
    pub fn run_mode(&self) -> &str {
        self.settings
            .get("run_mode")
            .and_then(SettingValue::as_str)
            .unwrap_or("sprint")
    }

    /// The colour theme the renderers use, resolved from the shell's tui settings
    /// (T34.1). Every render path reads this; no code outside `theme.rs` constructs
    /// a colour literal. Resolved on call, so a config reload is picked up without
    /// extra plumbing. While the theme picker is open, its live preview overrides
    /// the committed setting (T43.1): the whole shell recolours from the preview,
    /// and `tui.theme` stays the persisted truth, so a config-file reload cannot
    /// race the preview and the preview cannot outlive the modal.
    pub fn theme(&self) -> Theme {
        let key = match (self.theme_modal.open, self.theme_modal.preview) {
            (true, Some(preview)) => preview,
            _ => self.tui.theme,
        };
        Theme::resolve(key, self.tui.truecolor)
    }

    /// The engine is idle: no build, planner or discovery run is active. The scenario
    /// is re-derived and the task list takes the default focus in this state.
    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Startup && !self.planning && !self.discovering
    }

    /// The frame focus the engine state implies: the task list while idle, the agent
    /// output while a build, planner or discovery run is active.
    fn state_focus(&self) -> FrameFocus {
        if self.is_idle() {
            FrameFocus::Tasks
        } else {
            FrameFocus::Output
        }
    }

    /// Restores the state-implied focus when the idle/busy classification flips; a
    /// manual Tab or mouse choice persists while the state stays.
    fn sync_focus(&mut self, was_idle: bool) {
        if was_idle != self.is_idle() {
            self.focus = self.state_focus();
        }
    }

    /// Starts or stops the session timer when the idle/busy classification flips:
    /// the session runs from the idle→busy transition to the busy→idle one, so
    /// the timer hides while the engine is idle and restarts for the next session.
    fn sync_session(&mut self, was_idle: bool) {
        if was_idle != self.is_idle() {
            self.session_start = if was_idle { Some(self.now.get()) } else { None };
        }
    }

    /// The instant the engine's epoch-millisecond timestamp falls on, mapped
    /// through the construction-time anchor (T42.1). A session that started
    /// before this shell attached maps to the instant before it, clamped to
    /// the anchor when the clock cannot reach back that far.
    fn anchored(&self, started_ms: u64) -> Instant {
        let (epoch, at) = self.epoch_anchor;
        let epoch_ms = epoch
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(started_ms);
        let delta = Duration::from_millis(epoch_ms.abs_diff(started_ms));
        if epoch_ms >= started_ms {
            at.checked_sub(delta).unwrap_or(at)
        } else {
            at + delta
        }
    }

    /// The active session's elapsed time, recomputed on every render; `None`
    /// while the engine is idle.
    pub fn session_elapsed(&self) -> Option<Duration> {
        if self.is_idle() {
            return None;
        }
        self.session_start.map(|start| {
            let now = self.now.get();
            if now >= start {
                now - start
            } else {
                Duration::ZERO
            }
        })
    }

    /// The live startup scenario: the scanned file facts
    /// combined with the engine-reported task counts, so every queue change re-derives
    /// it without a rescan.
    pub fn scenario(&self) -> Scenario {
        let pending = self.tasks.iter().filter(|t| !t.done).count();
        patok_core::scenario::classify(
            self.project_status.has_code,
            self.project_status.task_file,
            pending,
            self.tasks.len(),
        )
    }

    /// The dialog's primary button label: "Inject"
    /// for the inject-task modal (T76.1), otherwise "Start" when the queue is
    /// ready and the input is empty, "Scan" when the queue is complete and the
    /// input is empty, "Submit" otherwise.
    pub fn primary_label(&self) -> &'static str {
        if self.dialog_kind == DialogKind::Inject {
            return "Inject";
        }
        let empty = self.dialog_text.trim().is_empty();
        match (self.scenario(), empty) {
            (Scenario::QueueReady, true) => "Start",
            (Scenario::QueueComplete, true) => "Scan",
            _ => "Submit",
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        self.new_tasks.clear();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+C detaches from anywhere, overlay and dialog included.
        if key.code == KeyCode::Char('c') && ctrl {
            return Action::Detach;
        }
        // The theme picker swallows every other key while it is open, so
        // nothing underneath reacts (T43.1); Ctrl+C above still detaches, the
        // same convention as the other modals.
        if self.theme_modal.open {
            return self.on_theme_key(key, ctrl);
        }
        // The stop dialog swallows every other key while it is open, so nothing
        // leaks to the shell (no restart, no navigation, no second build).
        if self.stop_open {
            return self.on_stop_key(key);
        }
        if self.dialog_open {
            return self.on_dialog_key(key, ctrl);
        }
        if self.overlay.open {
            return self.on_overlay_key(key);
        }
        match key.code {
            KeyCode::Char('v') if !ctrl => {
                self.tui.rail_mode = if self.tui.rail_mode == patok_core::config::RailMode::Detailed
                {
                    patok_core::config::RailMode::Compact
                } else {
                    patok_core::config::RailMode::Detailed
                };
                Action::None
            }
            KeyCode::Char('d') => Action::Detach,
            // The theme picker's key (T43.1): it opens from any state except
            // while another modal has the keyboard (the guards above), and it
            // sits above even a pending soft stop, whose q/Esc keys work
            // again once the picker closes.
            KeyCode::Char('t') if !ctrl => {
                self.status = None;
                self.open_theme();
                Action::None
            }
            // S-Tab flips the run mode between `sprint` and `continuous`
            // (T60.1): the new value goes through the settings-change flow, so
            // the status line's chip follows as soon as the engine reports it, while
            // an already running loop keeps its snapshot. Sits above the plain
            // Tab arm so a shifted Tab never switches the frame focus.
            KeyCode::BackTab => self.flip_run_mode(),
            // Kitty-enhanced terminals report Shift plus Tab as a Tab with the
            // SHIFT modifier instead of BackTab; both encode the same key.
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.flip_run_mode(),
            // Tab switches the frame focus; the layout resizes around it.
            KeyCode::Tab => {
                self.focus = match self.focus {
                    FrameFocus::Output => FrameFocus::Tasks,
                    FrameFocus::Tasks => FrameFocus::Output,
                };
                Action::None
            }
            _ if self.stopping => match key.code {
                // A second q interrupts: the current task is cancelled and the
                // loop stops, but the app keeps running.
                KeyCode::Char('q') => Action::Interrupt,
                // Esc cancels a pending soft stop: the loop continues with the
                // next task once the running one finishes.
                KeyCode::Esc => {
                    self.stopping = false;
                    self.status = Some("Soft stop cancelled -- the build continues.".into());
                    Action::CancelSoftStop
                }
                _ => Action::None,
            },
            KeyCode::Char('q') => {
                self.stopping = true;
                if self.phase == Phase::Running || self.planning || self.discovering {
                    // While a run is active the first q soft-stops the build
                    // loop: the current task finishes, no further task starts,
                    // and the app keeps running. Esc cancels the stop, and a
                    // second q interrupts.
                    self.status = Some(SOFT_STOP_PENDING.into());
                    Action::Quit
                } else {
                    // Idle: nothing to wind down, so q quits the app directly.
                    self.status = Some("Stopping the engine...".into());
                    Action::Interrupt
                }
            }
            KeyCode::Char('r') if self.version_mismatch => {
                self.stopping = true;
                self.status = Some(
                    "Restarting the engine -- it finishes the current task first. [d] detach instead"
                        .into(),
                );
                Action::Restart
            }
            // Esc while a build runs opens the stop dialog (T46.1, the key
            // the `s` of T20.1 used to be); during a planner or discovery
            // run it does nothing. The stopping guard above keeps a pending
            // soft stop's Esc.
            KeyCode::Esc if self.phase == Phase::Running => {
                self.status = None;
                self.stop_open = true;
                self.stop_selected = 0;
                Action::None
            }
            // The add-task dialog's key (T74.1): it belongs to the task list
            // frame, so only that frame's focused state opens the dialog; the
            // agent output frame ignores it and the key falls through to the
            // no-op arm below, leaving the status line untouched.
            KeyCode::Char('a') if self.focus == FrameFocus::Tasks => self.open_dialog(),
            // The inject-task modal's key (T76.1): like `a`, it belongs to the
            // task list frame and only that frame's focused state opens the
            // modal; the agent output frame ignores it. Unlike `a`, no busy
            // state refuses it -- appending a line is safe in every engine
            // state, and the engine's task-file poll reconciles it.
            KeyCode::Char('i') if !ctrl && self.focus == FrameFocus::Tasks => self.open_inject(),
            // Enter is the primary action while the engine is idle (T46.1):
            // it starts the build loop while pending tasks remain and runs a
            // discovery round once the queue is complete.
            KeyCode::Enter if !ctrl => self.enter_key(),
            KeyCode::Char('?') if !ctrl => {
                self.status = None;
                self.overlay.open();
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(-1),
            KeyCode::PageUp => self.scroll_by(10),
            KeyCode::PageDown => self.scroll_by(-10),
            KeyCode::End => {
                self.scroll = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Flips the run mode to its other value (S-Tab, T60.1): the newly selected
    /// mode is named in the status bar and carried by the returned action, which
    /// the driver sends through the settings-change flow. The chip follows the
    /// engine-reported readout, so it updates once the change applies; a running
    /// loop keeps its snapshot until its next unit of work.
    fn flip_run_mode(&mut self) -> Action {
        let mode = if self.run_mode() == "sprint" {
            "continuous"
        } else {
            "sprint"
        };
        self.status = Some(format!("Run mode switched to {mode}."));
        Action::SetRunMode(mode.into())
    }

    /// The stop dialog's keys: Up/Down move the selection, Enter confirms it,
    /// Esc closes with no effect, and everything else is swallowed. Ctrl+C is
    /// handled before this runs (it detaches from anywhere).
    fn on_stop_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => self.stop_open = false,
            KeyCode::Up => self.stop_selected = self.stop_selected.saturating_sub(1),
            KeyCode::Down => {
                self.stop_selected = (self.stop_selected + 1).min(STOP_CHOICES.len() - 1);
            }
            KeyCode::Enter => {
                self.stop_open = false;
                return match STOP_CHOICES[self.stop_selected.min(STOP_CHOICES.len() - 1)] {
                    StopChoice::SoftStop => {
                        self.stopping = true;
                        self.status = Some(SOFT_STOP_PENDING.into());
                        Action::Quit
                    }
                    StopChoice::Interrupt => {
                        self.stopping = true;
                        self.status = Some(
                            "Interrupting -- the current task is cancelled and the app keeps running."
                                .into(),
                        );
                        Action::Interrupt
                    }
                    StopChoice::Cancel => Action::None,
                };
            }
            _ => {}
        }
        Action::None
    }

    /// Opens the theme picker (the `t` key, T43.1): the selection starts on the
    /// active theme, and the theme active now is remembered, so Esc restores it
    /// exactly. Nothing is previewed or persisted yet.
    fn open_theme(&mut self) {
        self.theme_modal.open = true;
        self.theme_modal.original = self.tui.theme;
        self.theme_modal.selected = theme_modal_keys()
            .iter()
            .position(|name| *name == self.tui.theme.as_str())
            .unwrap_or(0);
        self.theme_modal.preview = None;
    }

    /// The theme picker's keys (T43.1): Up/Down and j/k move the selection onto
    /// an entry and immediately live-preview it, Esc restores the theme the
    /// picker opened with and closes, Enter keeps the previewed theme and hands
    /// it to the driver for persistence; everything else is swallowed.
    fn on_theme_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.theme_modal.preview = None;
                self.theme_modal.open = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.preview_theme(self.theme_modal.selected.saturating_sub(1));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.preview_theme(
                    (self.theme_modal.selected + 1).min(theme_modal_keys().len() - 1),
                );
            }
            KeyCode::Enter if !ctrl => return self.commit_theme(),
            _ => {}
        }
        Action::None
    }

    /// The theme of the selected row: a built-in name, always parseable; the
    /// remembered original is a fallback a corrupt list could never hit.
    fn selected_theme(&self) -> ThemeKey {
        theme_modal_keys()
            .get(self.theme_modal.selected)
            .and_then(|name| ThemeKey::parse(name))
            .unwrap_or(self.theme_modal.original)
    }

    /// Points the selection at `row` and immediately previews that theme: the
    /// whole shell recolours on the next render with nothing persisted and no
    /// engine round trip, since the theme is a tui-schema field.
    fn preview_theme(&mut self, row: usize) {
        self.theme_modal.selected = row;
        self.theme_modal.preview = Some(self.selected_theme());
    }

    /// The Enter path: keep the currently previewed theme active, close the
    /// picker, and ask the driver to persist the same value through the tui
    /// field's read-modify-write. On a failed save the driver restores the
    /// remembered original.
    fn commit_theme(&mut self) -> Action {
        let theme = self.selected_theme();
        self.theme_modal.preview = None;
        self.theme_modal.open = false;
        self.tui.theme = theme;
        Action::SaveTheme(theme)
    }

    /// The theme choice was persisted (T43.1): the status bar confirms it.
    pub fn on_theme_saved(&mut self) {
        self.status = Some("Theme saved.".into());
    }

    /// Persisting the theme choice failed: the theme the picker opened with is
    /// restored, so the shell matches the config file that is still on disk.
    pub fn on_theme_save_failed(&mut self, error: String) {
        self.tui.theme = self.theme_modal.original;
        self.status = Some(error);
    }

    /// The theme picker's mouse handling (T43.1): the pointer moving onto a row
    /// moves the selection there and previews it, and a click on a row previews
    /// and then commits -- exactly like hovering plus Enter. A click on one of
    /// the footer's buttons runs that button's key (T59.1), and a click on the
    /// title row's close button runs the picker's Esc key -- the previewed
    /// theme is dropped and the opening one restored (T66.1). Events outside
    /// the rows and every other kind are swallowed, so nothing underneath
    /// scrolls or focuses.
    fn on_theme_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.theme_modal.close.get()) {
            return self.on_theme_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false);
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let row = theme_row_at(position, self.theme_modal.area.get());
        match mouse.kind {
            MouseEventKind::Moved => {
                if let Some(row) = row {
                    self.preview_theme(row);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // The footer's buttons sit outside the body rows, so they are
                // checked first (T59.1).
                if let Some(code) = footer_button_code(
                    &mouse,
                    self.theme_modal.footer.get(),
                    &[("Enter", "Save"), ("Esc", "Cancel")],
                ) {
                    return self.on_theme_key(KeyEvent::new(code, KeyModifiers::NONE), false);
                }
                if let Some(row) = row {
                    self.preview_theme(row);
                    return self.commit_theme();
                }
            }
            _ => {}
        }
        Action::None
    }

    /// The add-task dialog's mouse handling (T59.1): a click on one of the
    /// bottom line's buttons runs exactly that button's key through the
    /// dialog's key path -- Enter submits (with the empty-input refusal) and
    /// Esc closes keeping the text. A click on the title row's close button
    /// runs the dialog's Esc key the same way (T66.1). Everything else is
    /// swallowed, as before.
    fn on_dialog_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.dialog_close.get()) {
            return self.on_dialog_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false);
        }
        let code = footer_button_code(
            &mouse,
            self.dialog_footer.get(),
            &[("Enter", self.primary_label()), ("Esc", "Close")],
        );
        match code {
            Some(code) => self.on_dialog_key(KeyEvent::new(code, KeyModifiers::NONE), false),
            None => Action::None,
        }
    }

    /// The stop dialog's mouse handling (T59.1): a click on one of the bottom
    /// line's buttons runs exactly that button's key through the dialog's key
    /// path -- Enter confirms the selected choice and Esc closes with no
    /// effect. A click on the title row's close button runs the dialog's Esc
    /// key the same way, leaving the build unchanged (T66.1). Everything else
    /// is swallowed, as before.
    fn on_stop_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.stop_close.get()) {
            return self.on_stop_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        match footer_button_code(
            &mouse,
            self.stop_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        ) {
            Some(code) => self.on_stop_key(KeyEvent::new(code, KeyModifiers::NONE)),
            None => Action::None,
        }
    }

    /// The settings overlay's mouse handling (T59.1): a click on the bottom
    /// line's Close button runs exactly the Esc key through the overlay's key
    /// path -- a clean overlay closes, a dirty one opens the unsaved-changes
    /// dialog. A click on the title row's close button runs the same Esc key
    /// (T66.1). Everything else is swallowed, as before.
    fn on_overlay_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.overlay.close.get()) {
            return self.on_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        match footer_button_code(&mouse, self.overlay.footer.get(), &[("Esc", "Close")]) {
            Some(code) => self.on_overlay_key(KeyEvent::new(code, KeyModifiers::NONE)),
            None => Action::None,
        }
    }

    /// The unsaved-changes dialog's mouse handling (T59.1): a click on one of
    /// the bottom line's buttons runs exactly that button's key through the
    /// overlay's key path, which routes it to the dialog while it is open --
    /// Enter confirms the selected choice (save, discard or cancel) and Esc
    /// returns to the overlay with the drafts intact. A click on the title
    /// row's close button returns to the overlay the same way (T66.1).
    /// Everything else is swallowed, as before.
    fn on_confirm_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.overlay.confirm_close.get()) {
            return self.on_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        match footer_button_code(
            &mouse,
            self.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        ) {
            Some(code) => self.on_overlay_key(KeyEvent::new(code, KeyModifiers::NONE)),
            None => Action::None,
        }
    }

    /// The short inline refusal for a busy engine, shared by every key that
    /// starts something (Enter, `a`): `None` while the engine is idle.
    fn busy_status(&self) -> Option<&'static str> {
        if self.phase == Phase::Running {
            Some("A build is running; wait for it to finish.")
        } else if self.planning {
            Some("The planner is already running.")
        } else if self.discovering {
            Some("A discovery round is running; wait for it to finish.")
        } else {
            None
        }
    }

    /// Opens the add-task dialog when the engine is idle; a busy engine refuses with a
    /// short inline message (the same message the Enter key shows).
    fn open_dialog(&mut self) -> Action {
        self.status = None;
        self.dialog_kind = DialogKind::Add;
        match self.busy_status() {
            Some(message) => self.status = Some(message.into()),
            None => {
                self.dialog_open = true;
                self.dialog_status = None;
            }
        }
        Action::None
    }

    /// Opens the inject-task modal (the `i` key, T76.1) from any engine state:
    /// the shared input box with the inject kind, whose confirm sends the
    /// text to the engine, which normalizes it into a well-formed unchecked
    /// task line before appending it. No busy state refuses it -- the append
    /// is a plain file write, so the engine's task-file poll reconciles the
    /// line even while a run is active.
    fn open_inject(&mut self) -> Action {
        self.status = None;
        self.dialog_open = true;
        self.dialog_kind = DialogKind::Inject;
        self.dialog_status = None;
        Action::None
    }

    /// The Enter key (T46.1): while the engine is idle it starts the build
    /// loop when pending tasks remain and runs a discovery round when the
    /// queue is complete; a busy engine refuses with the same short message
    /// as the other keys.
    fn enter_key(&mut self) -> Action {
        self.status = None;
        match self.busy_status() {
            Some(message) => {
                self.status = Some(message.into());
                Action::None
            }
            None => {
                if self.tasks.iter().any(|t| !t.done) {
                    Action::StartBuild
                } else {
                    Action::RunDiscovery
                }
            }
        }
    }

    fn on_dialog_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        if key.code == KeyCode::Esc {
            self.dialog_open = false;
            return Action::None;
        }
        if key.code == KeyCode::Char('c') && ctrl {
            return Action::Detach;
        }
        self.dialog_status = None;
        if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
            self.dialog_goal_col = None;
        }
        let word = ctrl || key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('s') if ctrl => return self.submit_dialog(),
            KeyCode::Char('u') if ctrl => {
                self.dialog_text.clear();
                self.dialog_cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => self.insert_at_cursor(c.encode_utf8(&mut [0; 4])),
            // Shift-Enter breaks the line at the cursor (T41.1): multi-line
            // requests are typed inside the input.
            KeyCode::Enter if shift => self.insert_at_cursor("\n"),
            // Enter submits through the same path as the Submit button;
            // an empty input is refused inline. A submit that
            // launches a run closes the dialog; a command error keeps it open.
            KeyCode::Enter => {
                if self.dialog_text.trim().is_empty() {
                    self.dialog_status = Some(self.empty_input_message().into());
                } else {
                    return self.submit_dialog();
                }
            }
            KeyCode::Backspace => {
                let cursor = self.cursor();
                if cursor > 0 {
                    let start = self.byte_index(cursor - 1);
                    let end = self.byte_index(cursor);
                    self.dialog_text.replace_range(start..end, "");
                    self.dialog_cursor = cursor - 1;
                }
            }
            KeyCode::Delete => {
                let cursor = self.cursor();
                if cursor < self.dialog_text.chars().count() {
                    let start = self.byte_index(cursor);
                    let end = self.byte_index(cursor + 1);
                    self.dialog_text.replace_range(start..end, "");
                }
            }
            KeyCode::Left if word => self.dialog_cursor = self.word_left(),
            KeyCode::Right if word => self.dialog_cursor = self.word_right(),
            KeyCode::Left => self.dialog_cursor = self.cursor().saturating_sub(1),
            KeyCode::Right => {
                self.dialog_cursor = (self.cursor() + 1).min(self.dialog_text.chars().count());
            }
            KeyCode::Home => self.dialog_cursor = self.line_bounds().0,
            KeyCode::End => self.dialog_cursor = self.line_bounds().1,
            KeyCode::Up => self.move_vertically(-1),
            KeyCode::Down => self.move_vertically(1),
            _ => {}
        }
        Action::None
    }

    /// The cursor as a character index into `dialog_text`, clamped to its length.
    pub fn cursor(&self) -> usize {
        self.dialog_cursor.min(self.dialog_text.chars().count())
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.dialog_text
            .char_indices()
            .nth(char_index)
            .map_or(self.dialog_text.len(), |(b, _)| b)
    }

    fn insert_at_cursor(&mut self, text: &str) {
        let cursor = self.cursor();
        let at = self.byte_index(cursor);
        self.dialog_text.insert_str(at, text);
        self.dialog_cursor = cursor + text.chars().count();
    }

    /// Character range `(start, end)` of the logical line holding the cursor.
    fn line_bounds(&self) -> (usize, usize) {
        let cursor = self.cursor();
        let (mut start, mut index) = (0, 0);
        for c in self.dialog_text.chars() {
            if index >= cursor && c == '\n' {
                break;
            }
            index += 1;
            if c == '\n' {
                start = index;
            }
        }
        (start, index)
    }

    fn word_left(&self) -> usize {
        let chars: Vec<char> = self.dialog_text.chars().collect();
        let mut i = self.cursor();
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let chars: Vec<char> = self.dialog_text.chars().collect();
        let mut i = self.cursor();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Visual rows of the dialog text when wrapped at the last rendered width.
    pub fn dialog_rows(&self) -> Vec<DialogRow> {
        let width = match self.dialog_width.get() {
            0 => usize::MAX,
            w => w,
        };
        let mut rows = Vec::new();
        let mut start = 0;
        for line in self.dialog_text.split('\n') {
            let len = line.chars().count();
            let mut offset = 0usize;
            loop {
                let end = offset.saturating_add(width).min(len);
                rows.push(DialogRow {
                    start: start + offset,
                    end: start + end,
                    last: end == len,
                });
                offset = end;
                if end == len {
                    break;
                }
            }
            start += len + 1;
        }
        rows
    }

    /// Index of the visual row holding the cursor.
    pub fn cursor_row(&self, rows: &[DialogRow]) -> usize {
        let cursor = self.cursor();
        rows.iter()
            .position(|r| cursor < r.end || (cursor == r.end && r.last))
            .unwrap_or(rows.len().saturating_sub(1))
    }

    fn move_vertically(&mut self, delta: isize) {
        let rows = self.dialog_rows();
        let current = self.cursor_row(&rows);
        let Some(target) = current.checked_add_signed(delta).and_then(|i| rows.get(i)) else {
            return;
        };
        let col = self
            .dialog_goal_col
            .unwrap_or(self.cursor() - rows[current].start);
        self.dialog_goal_col = Some(col);
        let max = if target.last {
            target.end - target.start
        } else {
            target.end - target.start - 1
        };
        self.dialog_cursor = target.start + col.min(max);
    }

    /// The inline refusal for an empty input on the dialog's primary key:
    /// kind-dependent (T76.1), because the two modals submit different things.
    fn empty_input_message(&self) -> &'static str {
        match self.dialog_kind {
            DialogKind::Add => "Nothing to submit -- type a request first.",
            DialogKind::Inject => "Nothing to inject -- type a task line first.",
        }
    }

    /// The scenario-driven submit: what submitting does depends
    /// on the project scenario and whether the input is empty. A submit that launches
    /// a run or the build loop closes the dialog; one that only writes the spec file
    /// or refuses keeps it open with its message in the status line.
    fn submit_dialog(&mut self) -> Action {
        // The inject-task modal's submit (T76.1): the typed text goes to the
        // engine, which normalizes it into a well-formed unchecked task line
        // and appends it under its task-file lock -- never to an agent
        // session; the modal closes once the driver confirms the append
        // (`on_task_injected`). An empty input is refused inline, and the
        // add-task dialog's whole scenario flow below stays untouched.
        if self.dialog_kind == DialogKind::Inject {
            if self.dialog_text.trim().is_empty() {
                self.dialog_status = Some(self.empty_input_message().into());
                return Action::None;
            }
            return Action::InjectTask(self.dialog_text.clone());
        }
        let empty = self.dialog_text.trim().is_empty();
        match (self.scenario(), empty) {
            (Scenario::EmptyProject, false) => Action::SaveBrief(self.dialog_text.clone()),
            (Scenario::EmptyProject, true) => {
                self.dialog_status = Some(
                    "Describe what you want to build -- an empty project needs direction.".into(),
                );
                Action::None
            }
            (Scenario::NeedsQueue, true) => {
                if self.project_status.spec == SpecState::Content {
                    Action::ResearchQueue(QueueRun::Bootstrap)
                } else {
                    self.dialog_status =
                        Some("SPEC.md is empty -- describe what you want to build first.".into());
                    Action::None
                }
            }
            (Scenario::QueueReady, true) => {
                self.dialog_open = false;
                self.focus = FrameFocus::Output;
                Action::StartBuild
            }
            (Scenario::QueueComplete, true) => Action::ResearchQueue(QueueRun::Scan),
            (_, false) => Action::SubmitTasks(self.dialog_text.clone()),
        }
    }

    /// The engine accepted the request: the dialog closes immediately and the output
    /// frame takes the focus, where the planner's output streams (T24.1).
    pub fn on_tasks_submitted(&mut self) {
        self.dialog_open = false;
        self.dialog_text.clear();
        self.dialog_cursor = 0;
        self.dialog_status = None;
        self.focus = FrameFocus::Output;
    }

    /// The injected task line was appended to TASKS.md (T76.1): the modal
    /// closes and the task list frame keeps the focus, where the engine's
    /// task-file poll brings the new line in as a TasksChanged event.
    pub fn on_task_injected(&mut self) {
        self.dialog_open = false;
        self.dialog_text.clear();
        self.dialog_cursor = 0;
        self.dialog_status = None;
        self.focus = FrameFocus::Tasks;
    }

    /// The command failed: the dialog stays open on the input with the error.
    pub fn on_submit_failed(&mut self, error: String) {
        self.dialog_status = Some(error);
    }

    /// The brief was written to SPEC.md (EmptyProject submit): the text moved into the
    /// file, so the input clears and the dialog stays open with the created message;
    /// with the spec file now present the scenario re-detects as NeedsQueue.
    pub fn on_brief_saved(&mut self) {
        self.dialog_text.clear();
        self.dialog_cursor = 0;
        self.dialog_status =
            Some("SPEC.md created -- review it, then press Enter to start.".into());
    }

    /// Pasted text appends to the dialog's input; ignored while the dialog is closed.
    pub fn on_paste(&mut self, text: &str) {
        if self.dialog_open {
            self.dialog_goal_col = None;
            self.insert_at_cursor(&text.replace("\r\n", "\n").replace('\r', "\n"));
        }
    }

    /// Whether the settings overlay is open (rendered above everything else).
    pub fn settings_open(&self) -> bool {
        self.overlay.open
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Action {
        // The focused row's current value is read before the state is borrowed mutably.
        let entry = self.overlay.focused();
        let current = self.setting_value(entry);
        self.overlay.on_key(key, current)
    }

    /// One entry's current value: the drafted change when one exists (the overlay
    /// edits drafts, not the live settings), otherwise the engine-reported readout
    /// for daemon fields and the shell's tui mirror for tui fields (`truecolor` as
    /// its auto/on/off choice).
    pub(crate) fn setting_value(&self, entry: Entry) -> Option<SettingValue> {
        let Entry::Row(row) = entry else {
            return None;
        };
        if let Some(draft) = self.overlay.drafts.get(row.key) {
            return Some(draft_display(row.key, draft));
        }
        match row.schema {
            Schema::Daemon => self.settings.get(row.key).cloned(),
            Schema::Tui => match row.key {
                "theme" => Some(SettingValue::Str(self.tui.theme.as_str().into())),
                "truecolor" => Some(SettingValue::Str(
                    match self.tui.truecolor {
                        None => "auto",
                        Some(true) => "on",
                        Some(false) => "off",
                    }
                    .into(),
                )),
                "preview_wrap" => Some(SettingValue::Bool(self.tui.preview_wrap)),
                "update_channel" => {
                    Some(SettingValue::Str(self.tui.update_channel.as_str().into()))
                }
                "rail_mode" => Some(SettingValue::Str(self.tui.rail_mode.as_str().into())),
                _ => None,
            },
        }
    }

    /// One drafted change was applied and persisted (the driver calls this once per
    /// drafted change as the save flow reports success): the readout follows and the
    /// draft is spent. The engine's own `ConfigChanged` broadcast arrives later and
    /// is idempotent.
    pub fn on_settings_applied(&mut self, schema: Schema, field: &str, value: SettingValue) {
        if schema == Schema::Daemon {
            self.settings.insert(field.to_string(), value);
        }
        if self.overlay.open {
            self.overlay.drafts.remove(field);
        }
    }

    /// One drafted change was rejected (validation or persistence error): the
    /// reason shows in the overlay's status line and the draft stays, so a retry
    /// can pick the save choice again. With the overlay closed (a run-mode flip
    /// sent from the main view, T60.1), the reason shows in the status bar.
    pub fn on_settings_rejected(&mut self, error: String) {
        if self.overlay.open {
            self.overlay.status = Some((error, StatusLevel::Error));
        } else {
            self.status = Some(error);
        }
    }

    /// Scrolls one frame's content by `delta` visual lines, clamped at both ends by
    /// the frame's last-rendered range; a scroll back to 0 restores auto-follow.
    fn scroll_frame(&mut self, frame: FrameFocus, delta: isize) {
        match frame {
            FrameFocus::Output => {
                self.scroll = self
                    .scroll
                    .saturating_add_signed(delta)
                    .min(self.max_scroll.get());
            }
            FrameFocus::Tasks => {
                self.task_scroll = self
                    .task_scroll
                    .saturating_add_signed(delta)
                    .min(self.task_max_scroll.get());
            }
        }
    }

    /// Scrolls the focused frame's content; the unfocused frame ignores the keys.
    fn scroll_by(&mut self, delta: isize) -> Action {
        self.scroll_frame(self.focus, delta);
        Action::None
    }

    /// Mouse events on the merged view: a click inside one of the two frames
    /// focuses it; a wheel step scrolls the frame the pointer is over, one line
    /// per step, without moving the focus. Events outside both frames and open
    /// modals change nothing. Every open modal handles its own mouse events and
    /// swallows the rest, so nothing underneath reacts while it is open: the
    /// theme picker keeps its row hover and click behaviour (T43.1) and every
    /// modal's bottom-line buttons respond to a click on their rectangle with
    /// exactly the action their key triggers (T59.1).
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Action {
        if self.theme_modal.open {
            return self.on_theme_mouse(mouse);
        }
        if self.overlay.confirm_open {
            return self.on_confirm_mouse(mouse);
        }
        if self.stop_open {
            return self.on_stop_mouse(mouse);
        }
        if self.dialog_open {
            return self.on_dialog_mouse(mouse);
        }
        if self.overlay.open {
            return self.on_overlay_mouse(mouse);
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let frame = frame_at(position, self.output_area.get(), self.tasks_area.get());
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(frame) = frame {
                    self.focus = frame;
                }
            }
            // Wheel steps scroll the hovered frame only; the focus stays put.
            MouseEventKind::ScrollUp => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, 1);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, -1);
                }
            }
            _ => {}
        }
        Action::None
    }
}

/// Whether a left click lands on a modal's close button (T66.1): the click
/// must be a press inside the button's rectangle, which the renderer recorded
/// at the last render through [`crate::ui::close_button_rect`]. The zero rect
/// a too-narrow modal records contains no real position, so the click misses
/// there naturally. Every other mouse event misses too.
fn close_clicked(mouse: &MouseEvent, close: Rect) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return false;
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    close.contains(position)
}

/// The key a left click on a modal's bottom-line button stands for (T59.1):
/// the button whose rectangle (computed by [`footer_button_rects`] from the
/// same button list the renderer drew) contains the click. `None` for hint
/// clicks, clicks elsewhere and every other mouse event -- hints stay
/// non-interactive.
fn footer_button_code(
    mouse: &MouseEvent,
    footer: Rect,
    buttons: &[(&str, &str)],
) -> Option<KeyCode> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    let index = footer_button_rects(footer, buttons)
        .iter()
        .position(|rect| rect.contains(position))?;
    match buttons[index].0 {
        "Enter" => Some(KeyCode::Enter),
        "Esc" => Some(KeyCode::Esc),
        _ => None,
    }
}

/// Turns a drafted value into what the overlay renders and cycles: `truecolor` is
/// drafted in its stored shape (`Bool`/`Unset`) but displays as its auto/on/off
/// choice, like the shell's live mirror does.
fn draft_display(key: &str, value: &SettingValue) -> SettingValue {
    if key == "truecolor" {
        return SettingValue::Str(
            match value {
                SettingValue::Bool(true) => "on",
                SettingValue::Bool(false) => "off",
                _ => "auto",
            }
            .into(),
        );
    }
    value.clone()
}
