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

use crate::overlay::{
    Entry, Row, Schema, SettingsOverlay, StatusLevel, clamp_focus, group_entries, header_fold,
};
use crate::theme::{Theme, theme_modal_groups};
use crate::ui::{
    confirm_row_at, finished_line, footer_button_rects, frame_at, menu_row_at, settings_row_at,
    started_line, theme_row_at,
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
        self.add(kind, text);
    }

    fn add(&mut self, kind: LineKind, text: String) {
        if self.lines.len() == OUTPUT_LIMIT {
            self.lines.pop_front();
        }
        self.lines.push_back(OutLine { kind, text });
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
                _ => self.add(LineKind::Text, piece.to_string()),
            }
            self.open = true;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutLine {
    pub kind: LineKind,
    pub text: String,
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

/// The m menu's choices (the `m` key, which replaced the status bar's
/// secondary key-hint chips): what the rows do, in the order they render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuChoice {
    /// Open the settings overlay -- the `?` binding's action.
    Settings,
    /// Open the theme picker -- the `t` binding's action.
    Theme,
    /// Detach -- the `d` binding's action.
    Detach,
    /// Quit -- the `q` binding's action.
    Quit,
    /// Open the stop dialog -- the Esc binding's action, so it joins only
    /// while a build runs, matching that binding's scope.
    StopBuild,
}

impl MenuChoice {
    pub fn label(self) -> &'static str {
        match self {
            MenuChoice::Settings => "Settings",
            MenuChoice::Theme => "Theme",
            MenuChoice::Detach => "Detach",
            MenuChoice::Quit => "Quit",
            MenuChoice::StopBuild => "Stop",
        }
    }

    /// The one-line explanation rendered after the label.
    pub fn detail(self) -> &'static str {
        match self {
            MenuChoice::Settings => "open the settings overlay",
            MenuChoice::Theme => "pick the colour theme",
            MenuChoice::Detach => "leave the engine running",
            MenuChoice::Quit => "stop the app",
            MenuChoice::StopBuild => "open the stop dialog",
        }
    }
}

/// The m menu's rows while a build runs (`running`), top to bottom;
/// `menu_selected` indexes this list. The stop entry joins only while a
/// build runs -- it keeps its build-only scope even though the Esc binding
/// covers planner and discovery runs too (T142.1) -- so the key handler's
/// clamp and the renderer read one list.
pub fn menu_entries(running: bool) -> Vec<MenuChoice> {
    let mut entries = vec![
        MenuChoice::Settings,
        MenuChoice::Theme,
        MenuChoice::Detach,
        MenuChoice::Quit,
    ];
    if running {
        entries.push(MenuChoice::StopBuild);
    }
    entries
}

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

/// The theme picker modal's state (T43.1, T116.1) -- its own modal state,
/// distinct from the settings overlay's: the built-in themes grouped into a
/// foldable Dark group followed by a foldable Light group (both expanded on
/// open), the selection's position among the visible entries (the group
/// headers and the expanded groups' rows, in list order), the theme that was
/// active when the modal opened (Esc restores it exactly), the live preview
/// applied to the whole shell while browsing, and the body rect recorded at
/// the last render for mouse hit-testing.
#[derive(Default)]
pub struct ThemeModal {
    pub open: bool,
    /// The selected entry's index into the visible list (headers plus the
    /// expanded groups' rows); a header selection never previews or commits.
    pub selected: usize,
    /// Per-group expanded flag, parallel to [`theme_modal_groups`]; both
    /// groups open expanded.
    pub expanded: [bool; 2],
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

impl ThemeModal {
    /// The picker's visible entries in display order: the Dark header and its
    /// rows while expanded, then the Light header and its rows while expanded
    /// -- the same shared fold mechanism the settings overlay's sections use.
    pub(crate) fn entries(&self) -> Vec<Entry<&'static str>> {
        let groups = theme_modal_groups();
        let rows = [groups[0].1.as_slice(), groups[1].1.as_slice()];
        group_entries(&rows, &self.expanded)
            .into_iter()
            .map(|entry| entry.map_row(|name| *name))
            .collect()
    }

    /// The selected entry. The visible list is never empty -- every group
    /// always renders its header -- so this never fails.
    pub(crate) fn selected_entry(&self) -> Entry<&'static str> {
        let entries = self.entries();
        entries[clamp_focus(self.selected, entries.len())]
    }

    /// Flips a group's fold and clamps the selection into the (possibly
    /// shortened) visible list; folding never hides a header, so the
    /// selection always stays on a visible entry.
    fn toggle(&mut self, group: usize) {
        if let Some(flag) = self.expanded.get_mut(group) {
            *flag = !*flag;
        }
        let len = self.entries().len();
        self.selected = clamp_focus(self.selected, len);
    }
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
    /// or discovery run (the same classification `is_idle` makes).
    /// `AgentChanged.started_ms` is authoritative and re-anchors the timer on
    /// every stage announcement mid-build (T42.1), so the timer and the
    /// finished line derive from the engine-recorded session timing; flag
    /// flips only fill a missing anchor or clear it (T131.1).
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
    /// The m menu is open (the `m` key, from any engine state).
    pub menu_open: bool,
    /// The m menu's selected row; indexes [`menu_entries`].
    pub menu_selected: usize,
    /// The m menu's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it (T59.1).
    pub menu_footer: Cell<Rect>,
    /// The m menu's close button rect at the last render (T66.1); a click on
    /// it runs the menu's Esc key.
    pub menu_close: Cell<Rect>,
    /// The m menu's body rect at the last render (T137.1); its rows' mouse
    /// hit-testing reads it -- a left click on a rendered row selects and
    /// runs that entry.
    pub menu_body: Cell<Rect>,
    /// The status bar's m menu chip rect at the last render (T133.1); a left
    /// click on it opens the m menu (the m key's action) and a click while
    /// the menu is open closes it. The zero rect a chip-less render records
    /// -- a showing status message, a pair dropped for width -- contains no
    /// real position, so a click misses naturally.
    pub menu_chip: Cell<Rect>,
    /// The status bar's idle Enter chip rect at the last render (T134.1); a
    /// left click on it runs the Enter key's action through `enter_key` --
    /// it starts the build loop while pending tasks remain and runs a
    /// discovery round once the queue is complete. The zero rect a chip-less
    /// render records -- a showing status message, a pair dropped for width,
    /// a busy engine -- contains no real position, so a click misses
    /// naturally.
    pub enter_chip: Cell<Rect>,
    /// The status bar's running Esc stop chip rect at the last render
    /// (T134.1); a left click on it runs the Esc key's action while a build
    /// runs -- it opens the stop dialog. The zero rect a chip-less render
    /// records -- a showing status message, a pair dropped for width, any
    /// other engine state -- contains no real position, so a click misses
    /// naturally.
    pub stop_chip: Cell<Rect>,
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
            menu_open: false,
            menu_selected: 0,
            menu_footer: Cell::new(Rect::default()),
            menu_close: Cell::new(Rect::default()),
            menu_body: Cell::new(Rect::default()),
            menu_chip: Cell::new(Rect::default()),
            enter_chip: Cell::new(Rect::default()),
            stop_chip: Cell::new(Rect::default()),
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
        // time; only an unanchored busy attach counts from here (T42.1). An
        // idle snapshot clears any anchor a replayed `AgentChanged` from a
        // finished session left behind, so the next run starts fresh (T131.1).
        if app.is_idle() {
            app.session_start = None;
        } else if app.session_start.is_none() {
            app.session_start = Some(app.now.get());
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
            // The engine-recorded start anchors the running timer, so it and the
            // finished line derive from the same session timing (T42.1). The
            // engine announces the run's agent before the run's flag flips
            // (T24.1), so the anchor must not be gated on the busy
            // classification: `AgentChanged` lands while the shell still
            // classifies itself as idle, and the timer's visibility stays
            // handled by `session_elapsed`'s `is_idle` gate (T131.1). A
            // `started_ms` of 0 means unknown and is skipped.
            EngineEvent::AgentChanged { agent, started_ms } => {
                self.agent = agent;
                if started_ms > 0 {
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
                self.push(LineKind::Status, text);
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
                self.push(LineKind::Status, text);
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
                // A status message set during the run (a busy refusal)
                // clears with it; the run's own outcome is the notice line
                // in the output pane, not the status bar (T29.1).
                if !planning && self.planning {
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
    /// The idle→busy flip fills a missing anchor only (a build start before the
    /// first announce, or a run whose `AgentChanged` carried `started_ms == 0`);
    /// an engine-recorded anchor always wins (T131.1, T42.1).
    fn sync_session(&mut self, was_idle: bool) {
        if was_idle != self.is_idle() {
            if was_idle {
                self.session_start.get_or_insert_with(|| self.now.get());
            } else {
                self.session_start = None;
            }
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
        // The m menu swallows every other key while it is open, so nothing
        // leaks to the shell either.
        if self.menu_open {
            return self.on_menu_key(key);
        }
        if self.dialog_open {
            return self.on_dialog_key(key, ctrl);
        }
        if self.overlay.open {
            return self.on_overlay_key(key);
        }
        match key.code {
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
            // The m menu's key: it opens from any engine state except while
            // another modal has the keyboard (the guards above), so the
            // status bar's secondary hints (settings, theme, detach, quit and
            // the running Esc stop dialog) live behind one chip instead.
            KeyCode::Char('m') if !ctrl => {
                self.status = None;
                self.menu_open = true;
                self.menu_selected = 0;
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
            KeyCode::Char('q') => self.quit_action(),
            KeyCode::Char('r') if self.version_mismatch => {
                self.stopping = true;
                self.status = Some(
                    "Restarting the engine -- it finishes the current task first. [d] detach instead"
                        .into(),
                );
                Action::Restart
            }
            // Esc while a build, planner or discovery run is active opens
            // the stop dialog (T46.1, the key the `s` of T20.1 used to be,
            // extended to these runs in T142.1); the m menu's stop entry
            // keeps its build-only scope. The stopping guard above keeps a
            // pending soft stop's Esc.
            KeyCode::Esc if self.phase == Phase::Running || self.planning || self.discovering => {
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

    /// The quit action (the `q` key and the m menu's Quit entry, one shared
    /// path): while a run is active the first press soft-stops the build loop
    /// -- the current task finishes, no further task starts, and the app
    /// keeps running; Esc cancels the stop and a second press interrupts.
    /// Idle, nothing needs winding down, so it stops the app directly. A
    /// press while a soft stop is already pending interrupts instead; the
    /// main match's stopping guard reaches the `q` key before this helper,
    /// so that branch serves the menu's path, where no guard runs first.
    fn quit_action(&mut self) -> Action {
        if self.stopping {
            return Action::Interrupt;
        }
        self.stopping = true;
        if self.phase == Phase::Running || self.planning || self.discovering {
            self.status = Some(SOFT_STOP_PENDING.into());
            Action::Quit
        } else {
            self.status = Some("Stopping the engine...".into());
            Action::Interrupt
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

    /// The m menu's keys: Up/Down -- j/k aliasing them, the theme picker's
    /// convention -- move the selection, Enter runs the selected
    /// entry -- exactly the action its direct key binding triggers -- Esc
    /// closes with no effect, and everything else is swallowed. Ctrl+C is
    /// handled before this runs (it detaches from anywhere).
    fn on_menu_key(&mut self, key: KeyEvent) -> Action {
        let entries = menu_entries(self.phase == Phase::Running);
        match key.code {
            KeyCode::Esc => self.menu_open = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu_selected = self.menu_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu_selected = (self.menu_selected + 1).min(entries.len() - 1);
            }
            KeyCode::Enter => {
                self.menu_open = false;
                return match entries[self.menu_selected.min(entries.len() - 1)] {
                    MenuChoice::Settings => {
                        self.status = None;
                        self.overlay.open();
                        Action::None
                    }
                    MenuChoice::Theme => {
                        self.status = None;
                        self.open_theme();
                        Action::None
                    }
                    MenuChoice::Detach => Action::Detach,
                    MenuChoice::Quit => self.quit_action(),
                    MenuChoice::StopBuild => {
                        self.status = None;
                        self.stop_open = true;
                        self.stop_selected = 0;
                        Action::None
                    }
                };
            }
            _ => {}
        }
        Action::None
    }

    /// Opens the theme picker (the `t` key, T43.1): the selection starts on
    /// the active theme, and the theme active now is remembered, so Esc
    /// restores it exactly. Both groups open expanded (T116.1), the fold state
    /// resetting with every open. Nothing is previewed or persisted yet.
    fn open_theme(&mut self) {
        self.theme_modal.open = true;
        self.theme_modal.original = self.tui.theme;
        self.theme_modal.expanded = [true, true];
        let active = self.tui.theme.as_str();
        self.theme_modal.selected = self
            .theme_modal
            .entries()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(name) if *name == active))
            .unwrap_or(0);
        self.theme_modal.preview = None;
    }

    /// The theme picker's keys (T43.1, T116.1): Up/Down and j/k move the
    /// selection through the visible entries -- the group headers and the
    /// expanded groups' rows, in list order -- and immediately live-preview
    /// it when it lands on a row; a header takes only the highlight and
    /// leaves the preview alone. The acting keys follow the selected entry:
    /// on a header, Enter/Space toggle the fold and Left folds an expanded
    /// one while Right unfolds a folded one (the settings overlay's header
    /// rule, h/l aliasing the arrows); on a row, Enter keeps the previewed
    /// theme and hands it to the driver for persistence. Esc restores the
    /// theme the picker opened with and closes; everything else is swallowed.
    fn on_theme_key(&mut self, key: KeyEvent, ctrl: bool) -> Action {
        // h/l alias Left/Right on the headers (the settings overlay's hjkl
        // convention); j/k are matched directly below.
        let code = match key.code {
            KeyCode::Char('h') if !ctrl => KeyCode::Left,
            KeyCode::Char('l') if !ctrl => KeyCode::Right,
            code => code,
        };
        match code {
            KeyCode::Esc => {
                self.theme_modal.preview = None;
                self.theme_modal.open = false;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            code => match self.theme_modal.selected_entry() {
                Entry::Header(group) => {
                    if header_fold(code, self.theme_modal.expanded[group]).is_some() {
                        self.theme_modal.toggle(group);
                    }
                }
                // Enter on a theme row commits; every other key is swallowed.
                Entry::Row(_) if code == KeyCode::Enter && !ctrl => return self.commit_theme(),
                Entry::Row(_) => {}
            },
        }
        Action::None
    }

    /// Moves the selection one visible entry (`delta` sign) and previews the
    /// entry it lands on when it is a row; a header takes only the highlight.
    /// The visible list contains no entry of a collapsed group, so collapsed
    /// entries are skipped by construction; the clamp keeps both ends put.
    fn move_selection(&mut self, delta: isize) {
        let len = self.theme_modal.entries().len();
        let target = clamp_focus(self.theme_modal.selected.saturating_add_signed(delta), len);
        if let Entry::Row(name) = self.theme_modal.entries()[target] {
            self.theme_modal.preview =
                Some(ThemeKey::parse(name).unwrap_or(self.theme_modal.original));
        }
        self.theme_modal.selected = target;
    }

    /// The theme of the selected row, when the selection is on one: a
    /// built-in name, always parseable. On a header there is no row theme, so
    /// the remembered original serves as the same safety fallback a corrupt
    /// list could never hit.
    fn selected_theme(&self) -> ThemeKey {
        match self.theme_modal.selected_entry() {
            Entry::Row(name) => ThemeKey::parse(name).unwrap_or(self.theme_modal.original),
            Entry::Header(_) => self.theme_modal.original,
        }
    }

    /// Points the selection at visible entry `index` and immediately previews
    /// that theme when it is a row: the whole shell recolours on the next
    /// render with nothing persisted and no engine round trip, since the
    /// theme is a tui-schema field. A header takes only the highlight; the
    /// preview stays what it was.
    fn preview_entry(&mut self, index: usize) {
        let len = self.theme_modal.entries().len();
        self.theme_modal.selected = clamp_focus(index, len);
        self.theme_modal.preview = match self.theme_modal.selected_entry() {
            Entry::Row(name) => Some(ThemeKey::parse(name).unwrap_or(self.theme_modal.original)),
            Entry::Header(_) => self.theme_modal.preview,
        };
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

    /// The theme picker's mouse handling (T43.1, T116.1): the pointer moving
    /// onto a visible entry moves the selection there and previews it when it
    /// is a row -- a header takes only the highlight -- and a click on a row
    /// previews and then commits, exactly like hovering plus Enter, while a
    /// click on a header toggles its fold and commits nothing. A click on one
    /// of the footer's buttons runs that button's key (T59.1), and a click on
    /// the title row's close button runs the picker's Esc key -- the previewed
    /// theme is dropped and the opening one restored (T66.1). Events outside
    /// the visible entries and every other kind are swallowed, so nothing
    /// underneath scrolls or focuses.
    fn on_theme_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.theme_modal.close.get()) {
            return self.on_theme_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false);
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let row = theme_row_at(
            position,
            self.theme_modal.area.get(),
            self.theme_modal.entries().len(),
        );
        match mouse.kind {
            MouseEventKind::Moved => {
                if let Some(row) = row {
                    self.preview_entry(row);
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
                    match self.theme_modal.entries()[row] {
                        // A header click folds its group; it never commits.
                        Entry::Header(group) => self.theme_modal.toggle(group),
                        Entry::Row(_) => {
                            self.preview_entry(row);
                            return self.commit_theme();
                        }
                    }
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

    /// The m menu's mouse handling (T59.1): a click on one of the bottom
    /// line's buttons runs exactly that button's key through the menu's key
    /// path -- Enter runs the selected entry and Esc closes with no effect.
    /// A click on the title row's close button runs the menu's Esc key the
    /// same way (T66.1). A left click inside a rendered entry row moves the
    /// selection to that row and runs it -- exactly the `m` plus Enter
    /// path (T137.1). The pointer moving onto a rendered entry moves only
    /// the selection highlight there, without running it (T140.1), so a
    /// click is exactly hover plus Enter; a move inside the modal but
    /// outside every row -- or anywhere else -- leaves the selection
    /// unchanged. A click on the status bar's m menu chip closes the
    /// menu, the second half of the chip's toggle (T133.1). Everything else
    /// -- the border, the title, the footer's non-button columns, the shell
    /// behind the modal -- is swallowed, as before.
    fn on_menu_mouse(&mut self, mouse: MouseEvent) -> Action {
        if chip_clicked(&mouse, self.menu_chip.get()) {
            self.menu_open = false;
            return Action::None;
        }
        if close_clicked(&mouse, self.menu_close.get()) {
            return self.on_menu_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        if let Some(code) = footer_button_code(
            &mouse,
            self.menu_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        ) {
            return self.on_menu_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let body = self.menu_body.get();
        let entries = menu_entries(self.phase == Phase::Running);
        let visible = entries.len().min(usize::from(body.height));
        let row = menu_row_at(position, body, visible);
        match mouse.kind {
            MouseEventKind::Moved => {
                // The pointer move moves only the highlight, never the
                // dispatch (T140.1): a move that misses every row -- the
                // border, the title, the footer, the shell behind the
                // modal, the running-only stop row's absent slot -- leaves
                // the selection alone.
                if let Some(index) = row {
                    self.menu_selected = index;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = row {
                    // The click is the row's Enter key verbatim: it moves the
                    // highlight to the row, then runs the Enter dispatch -- so a
                    // click and `m` plus Enter on the same row are
                    // indistinguishable.
                    self.menu_selected = index;
                    return self.on_menu_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            _ => {}
        }
        Action::None
    }

    /// The settings overlay's mouse handling (T59.1, T136.1): a click on the
    /// bottom line's Close button runs exactly the Esc key through the
    /// overlay's key path -- a clean overlay closes, a dirty one opens the
    /// unsaved-changes dialog. A click on the title row's close button runs
    /// the same Esc key (T66.1). A wheel step scrolls the list one row per
    /// step without moving the selection, clamped at the list's ends, and a
    /// left click inside a rendered list row runs that row's Enter key
    /// verbatim -- it selects the row first, so a click and Enter on the
    /// same row are indistinguishable. A pointer move inside a rendered
    /// list row moves the selection highlight to that row with the same
    /// viewport follow the Up and Down keys run, without running it
    /// (T145.1). The inline editor is modal like its keys: while it is
    /// open, row clicks, wheel steps and pointer moves are swallowed.
    /// Everything else is swallowed, as before.
    fn on_overlay_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.overlay.close.get()) {
            return self.on_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        if let Some(code) =
            footer_button_code(&mouse, self.overlay.footer.get(), &[("Esc", "Close")])
        {
            return self.on_overlay_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        // The inline editor is modal: while it is open, row clicks, wheel
        // steps and pointer moves are swallowed, like every key that is
        // not the editor's.
        if self.overlay.editor.is_some() {
            return Action::None;
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let row = settings_row_at(
            position,
            self.overlay.list.get(),
            self.overlay.scroll.get(),
            self.overlay.visible().len(),
        );
        match mouse.kind {
            // The pointer move moves only the selection highlight and the
            // viewport's follow (T145.1): it never runs the row's Enter
            // key, and a move that misses every row -- the blank viewport
            // rows past the last entry, the help box, the border, the
            // footer, the shell outside -- leaves the focus and the
            // offset alone.
            MouseEventKind::Moved => {
                if let Some(index) = row {
                    self.overlay.hover_focus(index);
                }
            }
            MouseEventKind::ScrollUp => {
                self.overlay.scroll_by(-1);
            }
            MouseEventKind::ScrollDown => {
                self.overlay.scroll_by(1);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = row {
                    // The click is the row's Enter key verbatim: it moves
                    // the focus to the row, then runs the Enter dispatch,
                    // which reads the row's current value -- so a click and
                    // Enter on the same row are indistinguishable.
                    self.overlay.focus = index;
                    return self.on_overlay_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            _ => {}
        }
        Action::None
    }

    /// The unsaved-changes dialog's mouse handling (T59.1): a click on one of
    /// the bottom line's buttons runs exactly that button's key through the
    /// overlay's key path, which routes it to the dialog while it is open --
    /// Enter confirms the selected choice (save, discard or cancel) and Esc
    /// returns to the overlay with the drafts intact. A click on the title
    /// row's close button returns to the overlay the same way (T66.1). A
    /// pointer move over one of the dialog's three choice rows moves the
    /// selection highlight there without confirming it, and a left click
    /// inside a choice row runs that row's Enter key verbatim, so a click
    /// and Enter on a row are indistinguishable (T146.1). A move or click
    /// inside the dialog that misses every row -- the blank body padding
    /// below the choices, the border, the title -- or lands outside it does
    /// nothing. Everything else is swallowed, as before.
    fn on_confirm_mouse(&mut self, mouse: MouseEvent) -> Action {
        if close_clicked(&mouse, self.overlay.confirm_close.get()) {
            return self.on_overlay_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        if let Some(code) = footer_button_code(
            &mouse,
            self.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        ) {
            return self.on_overlay_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let row = confirm_row_at(position, self.overlay.confirm_body.get());
        match mouse.kind {
            MouseEventKind::Moved => {
                // The move moves only the highlight, never the dispatch:
                // a move that misses every row leaves the selection alone.
                if let Some(index) = row {
                    self.overlay.confirm_selected = index;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // The click is the row's Enter key verbatim: it moves the
                // selection to the row, then runs the Enter dispatch, so a
                // click and Enter on the same row are indistinguishable.
                if let Some(index) = row {
                    self.overlay.confirm_selected = index;
                    return self.on_overlay_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            _ => {}
        }
        Action::None
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
    pub(crate) fn setting_value(&self, entry: Entry<&'static Row>) -> Option<SettingValue> {
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
    /// focuses it; a click on the status bar's interactive chips runs the
    /// action its key triggers -- the m menu chip opens the m menu (T133.1),
    /// the idle Enter chip starts the build loop or runs a discovery round,
    /// and the running Esc stop chip opens the stop dialog (T134.1) -- and a
    /// wheel step scrolls the frame the pointer is over, one line per step,
    /// without moving the focus. Events outside both frames and open modals
    /// change nothing. Every open modal handles its own mouse events and
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
        if self.menu_open {
            return self.on_menu_mouse(mouse);
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
                if chip_clicked(&mouse, self.menu_chip.get()) {
                    // The m key's open path, verbatim: the chip is its
                    // mouse alias (T133.1).
                    self.status = None;
                    self.menu_open = true;
                    self.menu_selected = 0;
                    Action::None
                } else if chip_clicked(&mouse, self.enter_chip.get()) {
                    // The Enter chip is the Enter key's mouse alias
                    // (T134.1): start the build loop or run a discovery
                    // round, exactly the key's action -- `enter_key` carries
                    // the key's busy guard too.
                    self.enter_key()
                } else if chip_clicked(&mouse, self.stop_chip.get())
                    && (self.phase == Phase::Running || self.planning || self.discovering)
                {
                    // The Esc chip is the Esc key's mouse alias (T134.1):
                    // it opens the stop dialog, the key's action while a
                    // build, planner or discovery run is active. The guard
                    // mirrors the key arm, which covers these runs too
                    // (T142.1), so the chip stays inert in any other state
                    // -- the zero rect makes those miss naturally.
                    self.status = None;
                    self.stop_open = true;
                    self.stop_selected = 0;
                    Action::None
                } else {
                    if let Some(frame) = frame {
                        self.focus = frame;
                    }
                    Action::None
                }
            }
            // Wheel steps scroll the hovered frame only; the focus stays put.
            MouseEventKind::ScrollUp => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, 1);
                }
                Action::None
            }
            MouseEventKind::ScrollDown => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, -1);
                }
                Action::None
            }
            _ => Action::None,
        }
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

/// Whether a left click lands on one of the status bar's interactive chips
/// (T133.1, T134.1): the click must be a press inside the chip's rectangle,
/// which the renderer recorded at the last render through
/// [`crate::ui::status_widget`] -- the m menu chip, the idle Enter chip and
/// the running Esc stop chip are mouse aliases of their keys. The zero rect a
/// chip-less render records contains no real position, so the click misses
/// there naturally, like [`close_clicked`] for close buttons.
/// Every other mouse event misses too.
fn chip_clicked(mouse: &MouseEvent, chip: Rect) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return false;
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    chip.contains(position)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
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

    fn opened() -> App {
        let mut app = app();
        app.open_theme();
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn entries_of(app: &App) -> Vec<Entry<&'static str>> {
        app.theme_modal.entries()
    }

    /// The picker's structure (T116.1): with both groups expanded, the visible
    /// list is the Dark header, the dark themes, the Light header and the
    /// light themes, in `theme_modal_groups` order.
    #[test]
    fn the_picker_lists_two_groups_with_all_entries_visible() {
        let app = opened();
        let groups = theme_modal_groups();
        let mut expected = vec![Entry::Header(0)];
        expected.extend(groups[0].1.iter().map(|name| Entry::Row(*name)));
        expected.push(Entry::Header(1));
        expected.extend(groups[1].1.iter().map(|name| Entry::Row(*name)));
        assert_eq!(expected.len(), 13, "two headers plus the eleven built-ins");
        assert_eq!(entries_of(&app), expected);
        assert_eq!(entries_of(&app)[0], Entry::Header(0));
        assert_eq!(entries_of(&app)[7], Entry::Header(1));
        assert_eq!(app.theme_modal.expanded, [true, true]);
    }

    /// Toggling a header hides and shows its entries, and the selection stays
    /// on a visible entry (clamped into the shortened list).
    #[test]
    fn toggling_a_header_hides_and_shows_its_entries() {
        let mut app = opened();
        let is_dark = |entry: &Entry<&'static str>| match entry {
            Entry::Row(name) => ThemeKey::parse(name).is_some_and(ThemeKey::is_dark),
            Entry::Header(_) => false,
        };

        // Folding Dark leaves its header, the Light header and the light rows.
        app.theme_modal.toggle(0);
        let entries = entries_of(&app);
        assert_eq!(entries.len(), 7);
        assert!(!entries.iter().any(is_dark), "no dark row stays visible");
        assert_eq!(entries[0], Entry::Header(0));
        assert_eq!(entries[1], Entry::Header(1));
        // Unfolding brings every dark row back.
        app.theme_modal.toggle(0);
        assert_eq!(entries_of(&app).len(), 13);

        // Folding Light is symmetric: the two headers and the dark rows.
        app.theme_modal.toggle(1);
        let entries = entries_of(&app);
        assert_eq!(entries.len(), 8);
        assert_eq!(entries[7], Entry::Header(1));
        assert!(entries[1..7].iter().all(is_dark));
        app.theme_modal.toggle(1);
        assert_eq!(entries_of(&app).len(), 13);

        // A selection past the shortened list clamps onto the last entry.
        app.theme_modal.selected = 12;
        app.theme_modal.toggle(0);
        assert_eq!(app.theme_modal.selected, 6);
        app.theme_modal.toggle(0);
        assert_eq!(entries_of(&app).len(), 13);
    }

    /// Navigation walks the visible list only: entries of a collapsed group
    /// are never landed on, and moves that would leave the list clamp.
    #[test]
    fn collapsed_entries_are_skipped_during_navigation() {
        let mut app = opened();
        assert_eq!(app.theme_modal.selected, 1, "the active theme's row");

        // Fold Dark from its header.
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.theme_modal.selected, 0);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.theme_modal.expanded, [false, true]);

        // Down from the Dark header lands on the Light header -- never on a
        // hidden dark row -- and the preview is untouched on a header.
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.theme_modal.selected, 1);
        assert_eq!(app.theme_modal.preview, None);
        // The next Down lands on the first light row and previews it.
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.theme_modal.selected, 2);
        assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));
        // Up from a light row lands back on the Light header, not a dark row.
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.theme_modal.selected, 1);
        assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));

        // With both groups folded, navigation moves only between the headers
        // and clamps at both ends.
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.theme_modal.expanded, [false, false]);
        for _ in 0..3 {
            assert_eq!(press(&mut app, KeyCode::Down), Action::None);
            assert!(app.theme_modal.selected <= 1);
        }
        assert_eq!(app.theme_modal.selected, 1);
        for _ in 0..3 {
            assert_eq!(press(&mut app, KeyCode::Up), Action::None);
            assert!(app.theme_modal.selected <= 1);
        }
        assert_eq!(app.theme_modal.selected, 0);
    }

    /// The current epoch in milliseconds, the unit `AgentChanged.started_ms`
    /// carries.
    fn epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// A `started_ms` this many milliseconds before now.
    fn started_ms_ago(ms: u64) -> u64 {
        epoch_ms().saturating_sub(ms)
    }

    /// The engine announces the run's agent before the run's flag flips
    /// (T24.1), so `AgentChanged` lands while the shell still classifies
    /// itself as idle. The engine-recorded start anchors the timer anyway,
    /// and the flag flip that follows must not overwrite it with the local
    /// clock (T131.1).
    #[test]
    fn the_timer_anchors_at_the_engine_recorded_agent_start() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: started_ms_ago(10_000),
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(11),
            "elapsed {elapsed:?}"
        );

        // The run's end still hides the timer (T42.1's idle contract).
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.session_elapsed(), None);
    }

    /// A build's flag flip with no prior `AgentChanged` still anchors the
    /// timer at the local clock: the anchor is only filled when missing.
    #[test]
    fn the_flag_flip_fills_only_a_missing_anchor() {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(elapsed < Duration::from_secs(1), "elapsed {elapsed:?}");
    }

    /// A shell attaching mid-run replays the `AgentChanged` that names the
    /// running agent; with the guard gone the replayed engine start anchors
    /// the timer, so the timer counts from the run's true start, not from the
    /// attach (T131.1).
    #[test]
    fn an_attach_mid_run_counts_from_the_engine_start() {
        let app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: true,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![
                    EngineEvent::AgentChanged {
                        agent: "planner".into(),
                        started_ms: started_ms_ago(10_000),
                    },
                    EngineEvent::PlanningChanged { planning: true },
                ],
            },
            "0.1.0".into(),
        );
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(11),
            "elapsed {elapsed:?}"
        );
    }

    /// A snapshot that says the engine is idle clears any anchor a replayed
    /// `AgentChanged` from a finished session left behind, so the timer never
    /// shows a stale run's time and the next run starts fresh (T131.1).
    #[test]
    fn an_idle_attach_clears_a_stale_replayed_anchor() {
        let app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![EngineEvent::AgentChanged {
                    agent: "planner".into(),
                    started_ms: started_ms_ago(10_000),
                }],
            },
            "0.1.0".into(),
        );
        assert_eq!(app.session_elapsed(), None);
    }

    /// A `started_ms` of 0 means the engine could not record the start; the
    /// run's flag flip then fills the missing anchor at the local clock.
    #[test]
    fn an_unknown_start_relies_on_the_flag_flip() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: 0,
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(elapsed < Duration::from_secs(1), "elapsed {elapsed:?}");
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

    /// An app with the settings overlay open over a recorded 10-row list
    /// viewport (T136.1); the close and footer rects stay zero, so clicks
    /// there miss naturally.
    fn settings_app() -> (App, Rect) {
        let mut app = app();
        app.overlay.open();
        let list = Rect::new(10, 5, 60, 10);
        app.overlay.list.set(list);
        (app, list)
    }

    /// A dirty settings overlay with its unsaved-changes dialog open (T146.1):
    /// the app, the recorded list viewport and the recorded choice-rows area.
    /// The body rect lies deliberately outside the list rect so list
    /// hit-testing never interferes; its rows 0-2 are the three choices and
    /// rows 3-5 the blank padding below them.
    fn confirm_app() -> (App, Rect, Rect) {
        let (mut app, list) = settings_app();
        app.overlay
            .drafts
            .insert("model".into(), SettingValue::Str("opus".into()));
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.overlay.confirm_open);
        assert_eq!(app.overlay.confirm_selected, 0);
        let body = Rect::new(0, 20, 50, 6);
        app.overlay.confirm_body.set(body);
        (app, list, body)
    }

    /// A left click on the list's `index`-th visible row.
    fn click_row(app: &mut App, list: Rect, index: usize) -> Action {
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 2,
            list.y + index as u16,
        ))
    }

    /// The visible index of `field`'s row.
    fn visible_index(app: &App, field: &str) -> usize {
        app.overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == field))
            .unwrap_or_else(|| panic!("`{field}` is not visible"))
    }

    /// Wheel steps scroll the list one row per step without moving the
    /// selection, clamped at the first and the last row (T136.1).
    #[test]
    fn wheel_steps_scroll_the_list_without_moving_the_selection() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();
        let over = (list.x + 2, list.y + 2);

        // One wheel step moves the offset exactly one row; the selection,
        // the drafts and the editor stay put.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::ScrollDown, over.0, over.1)),
            Action::None
        );
        assert_eq!(app.overlay.scroll.get(), 1);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());

        // Scrolling down past the end clamps at the last offset that still
        // fills the viewport.
        for _ in 0..len {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, over.0, over.1));
        }
        assert_eq!(app.overlay.scroll.get(), len - 10);
        assert_eq!(app.overlay.focus, 0);

        // Scrolling back up clamps at the first row.
        for _ in 0..(len + 10) {
            app.on_mouse(mouse(MouseEventKind::ScrollUp, over.0, over.1));
        }
        assert_eq!(app.overlay.scroll.get(), 0);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// A left click inside a list row selects it and runs its Enter key
    /// verbatim: a header folds, a Text row opens the inline editor and a
    /// Bool row drafts the toggle (T136.1).
    #[test]
    fn a_click_on_a_row_selects_and_runs_its_enter() {
        let (mut app, list) = settings_app();
        let full = app.overlay.visible().len();

        // A header click folds its section, and a second one unfolds it --
        // exactly Enter's behaviour on a header.
        assert_eq!(click_row(&mut app, list, 0), Action::None);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.visible().len() < full);
        assert_eq!(click_row(&mut app, list, 0), Action::None);
        assert_eq!(app.overlay.visible().len(), full);

        // A Text row's click opens the same inline editor Enter opens,
        // prefilled with the row's current value (no readout yet: empty).
        let model = visible_index(&app, "model");
        assert_eq!(click_row(&mut app, list, model), Action::None);
        assert_eq!(app.overlay.focus, model);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("the click opened the editor");
        assert_eq!(editor.field, "model");
        assert_eq!(editor.buffer, "");

        // A Bool row's click drafts the toggle Enter drafts. The wheel
        // scrolled it into view first, so the click also proves the
        // hit-test sees the scrolled rows.
        for _ in 0..10 {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, list.x + 2, list.y + 2));
        }
        let plan = visible_index(&app, "plan_enabled");
        let scrolled = plan - app.overlay.scroll.get();
        assert_eq!(click_row(&mut app, list, scrolled), Action::None);
        assert_eq!(app.overlay.focus, plan);
        assert_eq!(
            app.overlay.drafts.get("plan_enabled"),
            Some(&SettingValue::Bool(true))
        );
    }

    /// A click on a blank viewport row below the list's last entry and a
    /// click outside the list rect entirely change nothing (T136.1).
    #[test]
    fn a_click_outside_the_list_rows_does_nothing() {
        let (mut app, list) = settings_app();
        // Fold every section by clicking its header: the list holds only
        // its five headers, leaving blank viewport rows below them.
        for index in 0..5 {
            let header = app
                .overlay
                .visible()
                .iter()
                .position(|entry| *entry == Entry::Header(index))
                .unwrap();
            assert_eq!(click_row(&mut app, list, header), Action::None);
        }
        assert_eq!(app.overlay.visible().len(), 5);

        let before = (app.overlay.focus, app.overlay.scroll.get());
        assert_eq!(click_row(&mut app, list, 7), Action::None, "a blank row");
        // (0, 1) is outside the list rect and below the footer line the zero
        // rects still derive their phantom buttons on.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 1)),
            Action::None,
            "outside the list rect"
        );
        assert_eq!((app.overlay.focus, app.overlay.scroll.get()), before);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// The inline editor is modal for the mouse too: wheel steps over the
    /// overlay are swallowed while it is open (T136.1).
    #[test]
    fn wheel_steps_are_swallowed_while_the_editor_is_open() {
        let (mut app, list) = settings_app();
        // The same Enter path a click on a Text row runs opens the editor.
        let model = visible_index(&app, "model");
        click_row(&mut app, list, model);
        assert!(app.overlay.editor.is_some());
        for _ in 0..5 {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, list.x + 2, list.y + 2));
        }
        assert_eq!(app.overlay.scroll.get(), 0, "the editor swallows the wheel");
    }

    /// A pointer move inside a visible list row moves the selection highlight
    /// to that row without running it -- no editor, no drafts, no folds --
    /// and Enter on a hovered row opens the same inline editor the click and
    /// the keyboard paths open (T145.1).
    #[test]
    fn hovering_a_row_moves_the_selection_without_running_it() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();
        for row in 0..10 {
            // The pointer rides viewport row `row`; the hit-test resolves it
            // against the current offset, so the highlight lands on the row
            // under the pointer and the follow keeps that row on screen.
            let hovered = app.overlay.scroll.get() + row;
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    list.x + 2,
                    list.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
            assert!(app.overlay.open);
            assert!(app.overlay.editor.is_none());
            assert!(app.overlay.drafts.is_empty());
            assert!(!app.overlay.confirm_open);
            assert_eq!(
                app.overlay.visible().len(),
                len,
                "a hover never folds a header"
            );
        }

        // Enter on a hovered row runs exactly the keyboard path: the same
        // inline editor the click opens, prefilled with the row's current
        // value (no readout yet: empty).
        let (mut app, list) = settings_app();
        let model = visible_index(&app, "model");
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Moved,
                list.x + 2,
                list.y + model as u16
            )),
            Action::None
        );
        assert_eq!(app.overlay.focus, model);
        assert!(
            app.overlay.editor.is_none(),
            "the hover never activates the row"
        );
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("Enter on the hovered row opens the editor");
        assert_eq!(editor.field, "model");
        assert_eq!(editor.buffer, "");

        // The click path is untouched by the hover (T136.1): a click on a
        // different row still selects and activates the clicked row, not
        // the hovered one.
        let (mut app, list) = settings_app();
        let provider = visible_index(&app, "provider");
        let model = visible_index(&app, "model");
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Moved,
                list.x + 2,
                list.y + provider as u16
            )),
            Action::None
        );
        assert_eq!(app.overlay.focus, provider);
        assert!(
            app.overlay.editor.is_none(),
            "the hover never activates the row"
        );
        assert_eq!(click_row(&mut app, list, model), Action::None);
        assert_eq!(app.overlay.focus, model);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("the click still opens the editor");
        assert_eq!(editor.field, "model");
        assert_eq!(editor.buffer, "");
    }

    /// A pointer move over the blank viewport rows past the last entry, over
    /// the modal above the list rect, or outside the overlay entirely leaves
    /// the selection and the offset alone (T145.1).
    #[test]
    fn hovering_between_the_rows_or_outside_the_list_changes_nothing() {
        let (mut app, list) = settings_app();
        // Fold every section by clicking its header, exactly the T136.1
        // path: the list holds only its five headers, leaving blank
        // viewport rows below them.
        for index in 0..5 {
            let header = app
                .overlay
                .visible()
                .iter()
                .position(|entry| *entry == Entry::Header(index))
                .unwrap();
            assert_eq!(click_row(&mut app, list, header), Action::None);
        }
        assert_eq!(app.overlay.visible().len(), 5);

        // An observable focus the misses must leave alone.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 1)),
            Action::None
        );
        assert_eq!(app.overlay.focus, 1);
        let before = (app.overlay.focus, app.overlay.scroll.get());

        // A blank viewport row below the last header, the border row above
        // the recorded list rect, and a point outside the overlay entirely.
        for (column, row) in [(list.x + 2, list.y + 7), (list.x + 2, list.y - 1), (0, 1)] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
                Action::None
            );
            assert_eq!((app.overlay.focus, app.overlay.scroll.get()), before);
        }
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// Hovering the viewport's edge rows scrolls the list exactly the way
    /// the navigation keys do: the offset follows with the margin band and
    /// clamps at the first and the last row, and the walk never activates
    /// anything (T145.1).
    #[test]
    fn hovering_the_edges_of_a_long_list_scrolls_it_like_the_keys() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();

        // Hovering the bottom viewport row from the top of the list lands
        // exactly where nine Down presses land: focus 9 with the offset 4.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 9)),
            Action::None
        );
        assert_eq!(app.overlay.focus, 9);
        assert_eq!(app.overlay.scroll.get(), 4);

        // Keeping the pointer on the bottom row walks the list down: each
        // move lands on the row the follow brought under the pointer, the
        // viewport keeps the hovered row visible, and the walk ends on the
        // last row at the last offset that still fills the viewport.
        while app.overlay.focus < len - 1 {
            assert!(app.overlay.editor.is_none());
            assert!(app.overlay.drafts.is_empty());
            let hovered = app.overlay.scroll.get() + 9;
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 9)),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
        }
        assert_eq!(app.overlay.focus, len - 1);
        assert_eq!(
            app.overlay.scroll.get(),
            len - 10,
            "never overshooting the clamp at the last full screen"
        );

        // Walking back up on the top viewport row ends at the first row
        // with the offset clamped at 0.
        while app.overlay.focus > 0 {
            let hovered = app.overlay.scroll.get();
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y)),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
        }
        assert_eq!(app.overlay.focus, 0);
        assert_eq!(
            app.overlay.scroll.get(),
            0,
            "never overshooting the clamp at the first row"
        );
    }

    /// The inline editor is modal for the pointer moves too: a hover is
    /// swallowed while it is open.
    #[test]
    fn hovering_is_swallowed_while_the_editor_is_open() {
        let (mut app, list) = settings_app();
        let model = visible_index(&app, "model");
        click_row(&mut app, list, model);
        assert!(app.overlay.editor.is_some());
        for row in 0..10 {
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    list.x + 2,
                    list.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.focus, model);
            assert!(app.overlay.editor.is_some());
        }

        // The draft stays drafted; the dialog's own hover behaviour is
        // covered by the T146.1 tests below.
        for c in "opus".chars() {
            assert_eq!(press(&mut app, KeyCode::Char(c)), Action::None);
        }
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.overlay.editor.is_none());
        assert_eq!(
            app.overlay.drafts.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );
    }

    /// The unsaved-changes dialog follows the pointer (T146.1): a move onto
    /// one of the three choice rows moves the selection highlight there
    /// without confirming it, and a move inside the dialog that misses every
    /// row -- the blank padding below the choices -- or over the list rect
    /// behind it, or outside the dialog, leaves the selection alone.
    #[test]
    fn hovering_the_confirm_dialog_follows_the_pointer() {
        let (mut app, list, body) = confirm_app();
        for row in 0..3 {
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    body.x + 2,
                    body.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.confirm_selected, row);
            assert!(app.overlay.confirm_open);
            assert_eq!(
                app.overlay.drafts.get("model"),
                Some(&SettingValue::Str("opus".into()))
            );
        }

        // The misses leave the selection on the last hovered row: the blank
        // body padding below the choices, the list rect behind the dialog and
        // the shell outside.
        for (column, row) in [
            (body.x + 2, body.y + 3),
            (body.x + 2, body.y + 5),
            (body.x + body.width, body.y),
            (list.x + 2, list.y + 3),
            (0, 0),
        ] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
                Action::None
            );
            assert_eq!(app.overlay.confirm_selected, 2);
            assert!(app.overlay.confirm_open);
        }
    }

    /// A hover picks the row the Enter key then confirms (T146.1): the dialog
    /// runs its Enter dispatch on the hovered row, exactly the key path.
    #[test]
    fn enter_after_a_hover_confirms_the_hovered_choice() {
        // Hovering Discard (row 1) and pressing Enter discards and closes.
        let (mut app, _list, body) = confirm_app();
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 1));
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::CloseSettings);
        assert!(!app.overlay.confirm_open);
        assert!(!app.overlay.open);
        assert!(app.overlay.drafts.is_empty());

        // Hovering Cancel (row 2) and pressing Enter returns to the overlay
        // with the dialog closed and the drafts intact.
        let (mut app, _list, body) = confirm_app();
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 2));
        assert_eq!(app.overlay.confirm_selected, 2);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(!app.overlay.confirm_open);
        assert!(app.overlay.open);
        assert_eq!(
            app.overlay.drafts.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );
    }

    /// A left click inside a choice row's rect runs exactly that row's Enter
    /// key (T146.1): Save asks the driver to apply the drafts, Discard throws
    /// them away and closes, Cancel returns to the overlay -- and a click in
    /// the padding between the rows or outside the dialog does nothing.
    #[test]
    fn a_click_on_a_confirm_choice_row_runs_its_enter() {
        // Save (row 0): the overlay stays open for the driver's
        // apply-and-persist flow, with the drafts still pending.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y
            )),
            Action::SaveSettings
        );
        assert!(!app.overlay.confirm_open);
        assert!(app.overlay.open);
        assert_eq!(
            app.overlay.drafts.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );
        assert_eq!(
            app.overlay.pending(),
            vec![(
                Schema::Daemon,
                "model".to_string(),
                SettingValue::Str("opus".into())
            )]
        );

        // Discard (row 1): the drafts are thrown away and the overlay closes.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y + 1
            )),
            Action::CloseSettings
        );
        assert!(!app.overlay.confirm_open);
        assert!(!app.overlay.open);
        assert!(app.overlay.drafts.is_empty());

        // Cancel (row 2): only the dialog closes; the overlay and the drafts
        // stay as they were.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y + 2
            )),
            Action::None
        );
        assert!(!app.overlay.confirm_open);
        assert!(app.overlay.open);
        assert_eq!(
            app.overlay.drafts.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );

        // The misses do nothing: the padding below the choices and the shell
        // outside the dialog leave the dialog open with the drafts intact.
        // The outside point sits off row 0, where the zero footer rect's
        // phantom button rects would otherwise catch the click.
        let (mut app, _list, body) = confirm_app();
        for (column, row) in [(body.x + 2, body.y + 3), (body.x + 2, body.y + 5), (0, 10)] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
                Action::None
            );
            assert!(app.overlay.confirm_open);
            assert_eq!(app.overlay.confirm_selected, 0);
            assert_eq!(
                app.overlay.drafts.get("model"),
                Some(&SettingValue::Str("opus".into()))
            );
        }
    }

    /// The dialog's keys are unchanged (T146.1): j, k and the arrows move the
    /// selection while it is open, Esc closes only the dialog with the drafts
    /// intact -- and with the dialog closed, a move over the recorded choice
    /// rows' coordinates goes through the overlay's mouse path, misses the
    /// list rect and touches neither the list selection nor the dialog's.
    #[test]
    fn the_confirm_dialog_keys_stay_unchanged() {
        let (mut app, _list, body) = confirm_app();
        assert_eq!(app.overlay.confirm_selected, 0);
        assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
        assert_eq!(app.overlay.confirm_selected, 0);
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
        assert_eq!(app.overlay.confirm_selected, 2);
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.overlay.confirm_open);
        assert!(app.overlay.open);
        assert_eq!(
            app.overlay.drafts.get("model"),
            Some(&SettingValue::Str("opus".into()))
        );

        // With the dialog closed, the same coordinates go through the
        // overlay's list hover and miss: the recorded choice-rows area lies
        // outside the list rect, so the focus and the dialog's selection
        // stay put.
        let focus = app.overlay.focus;
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 1)),
            Action::None
        );
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.confirm_selected, 1);
    }
}
