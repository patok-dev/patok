//! Shell state and its transitions. Pure: no I/O, so it is tested without a terminal.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::{Duration, Instant, SystemTime};

use patok_config::{EnumSpelling, SettingValue, Theme as ThemeKey, TuiSettings};
use patok_core::event::{AgentRole, Phase, ProviderCatalog, Snapshot, Usage};
use patok_core::output::{LineKind, LineOrigin, Pane};
use patok_core::pipeline::PipelineState;
use patok_core::scenario::{ProjectScan, Scenario};
use patok_core::task::Task;
use patok_providers::ProviderKind;
use ratatui::layout::Rect;

use crate::dialog_editor::DialogEditor;
use crate::modal::{Modal, ModalStack};
use crate::model_picker::{ModelPicker, PickerOutcome};
use crate::overlay::{
    Entry, Row, Schema, SettingsOverlay, StatusLevel, clamp_focus, group_entries, truecolor_choice,
};
use crate::terminal::TerminalState;
use crate::theme::{Theme, theme_modal_groups};
use crate::view::FrameFocus;

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

/// The m menu's choices (the `m` key, which replaced the status bar's
/// secondary key-hint chips): what the rows do, in the order they render --
/// Interactive, Settings, Theme, Detach, Quit, then Stop only while a build
/// runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuChoice {
    /// Open the interactive provider picker -- the `I` binding's action.
    Interactive,
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
            MenuChoice::Interactive => "Interactive",
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
            MenuChoice::Interactive => "open a provider CLI session",
            MenuChoice::Settings => "open the settings overlay",
            MenuChoice::Theme => "pick the colour theme",
            MenuChoice::Detach => "leave the engine running",
            MenuChoice::Quit => "stop the app",
            MenuChoice::StopBuild => "open the stop dialog",
        }
    }
}

/// The m menu's rows, top to bottom (Interactive first, so the default
/// selection, `MenuState::selected = 0`, lands on it); the menu's selected row
/// indexes this list. The stop entry is appended last and joins only while a
/// build runs (`running`) -- it keeps its build-only scope even though the Esc binding
/// covers planner and discovery runs too (T142.1) -- so the key handler's
/// clamp and the renderer read one list.
pub fn menu_entries(running: bool) -> Vec<MenuChoice> {
    let mut entries = vec![
        MenuChoice::Interactive,
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

/// Which modal the shared dialog input drives (T76.1): both modals are the
/// same input box -- same styling, same cursor and multi-line editing, same
/// cancel behaviour -- they differ only in their title, watermark, primary
/// label and what confirming does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DialogKind {
    /// The add-task dialog (`a`): the scenario-driven submit of T74.1, which
    /// sends the text to the engine's planner or discovery flow.
    #[default]
    Add,
    /// The inject-task modal (`i`): the engine normalizes the text into a
    /// well-formed unchecked task line before appending it to TASKS.md,
    /// with no agent session.
    Inject,
}

/// The model picker's state (T4.4): the picker and the settings row it was
/// opened from, so a choice knows its draft key and `Custom` can open the
/// inline editor on that row.
pub(crate) struct ModelPickerState {
    pub(crate) row: &'static Row,
    pub(crate) picker: ModelPicker,
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
    pub(crate) fn toggle(&mut self, group: usize) {
        if let Some(flag) = self.expanded.get_mut(group) {
            *flag = !*flag;
        }
        let len = self.entries().len();
        self.selected = clamp_focus(self.selected, len);
    }
}

/// The run state the event reducer (`App::apply`) writes: the
/// engine's phase and run flags, the active agent, provider and model, the
/// task list and the output pane. Grouped (finding 19) so the reducer's state
/// reads as one thing instead of a dozen flat fields.
pub struct RunState {
    pub phase: Phase,
    /// The planner is running.
    pub planning: bool,
    /// A discovery round is running.
    pub discovering: bool,
    pub stopping: bool,
    /// The engine's active agent ("planner" during the plan stage, "builder" afterwards);
    /// announced by `AgentChanged` events and replayed with the recent events (T11.1).
    pub agent: String,
    /// Name of the engine's active provider; empty when unknown.
    pub provider: String,
    /// The engine's configured model name; empty when the provider picks its default.
    pub model: String,
    /// The configured provider and model behind [`RunState::provider`] and
    /// [`RunState::model`]: the baseline the output frame title restores to after
    /// a session that runs on another provider (a reviewer under a `claude`
    /// reviewer provider) ends.
    pub config_provider: String,
    pub(crate) config_model: String,
    pub current_task: Option<String>,
    pub completed: usize,
    pub(crate) usage: Option<Usage>,
    pub tasks: Vec<Task>,
    /// IDs of tasks appended since the last key press or build start; shown highlighted.
    pub(crate) new_tasks: Vec<String>,
    pub output: Pane,
}

/// The session timer's clock (T42.1): the active session's start, the
/// construction-time epoch anchor that maps the engine's epoch milliseconds
/// onto the shell's `Instant` clock, and the render clock the timer reads.
/// Grouped (finding 19) so the timer's state moves as one thing.
pub struct SessionClock {
    /// The active session's start: `Some` from the moment the engine leaves
    /// idle until it returns, `None` while idle. A session is a build, planner
    /// or discovery run (the same classification `is_idle` makes).
    /// `AgentChanged.started_ms` is authoritative and re-anchors the timer on
    /// every stage announcement mid-build (T42.1), so the timer and the
    /// finished line derive from the engine-recorded session timing; flag
    /// flips only fill a missing anchor or clear it (T131.1).
    pub session_start: Option<Instant>,
    /// The start of the whole run, for the rail's timer: set once by the
    /// first engine-recorded agent start (or the idle→busy flip) and kept
    /// across stage changes, so it spans plan, build and discovery. Cleared
    /// when the engine returns to idle.
    pub overall_start: Option<Instant>,
    /// One epoch anchor captured at construction, mapping the engine's epoch
    /// milliseconds onto the shell's `Instant` clock (T42.1).
    epoch_anchor: (SystemTime, Instant),
    /// The render clock the timer reads; the driver refreshes it before every
    /// frame. Tests that leave it still get a deterministic `00:00`.
    pub now: Cell<Instant>,
}

impl SessionClock {
    /// Captures the epoch anchor and starts the render clock.
    fn new() -> Self {
        Self {
            session_start: None,
            overall_start: None,
            epoch_anchor: (SystemTime::now(), Instant::now()),
            now: Cell::new(Instant::now()),
        }
    }

    /// The instant the engine's epoch-millisecond timestamp falls on, mapped
    /// through the construction-time anchor (T42.1). A session that started
    /// before this shell attached maps to the instant before it, clamped to
    /// the anchor when the clock cannot reach back that far.
    pub(crate) fn anchored(&self, started_ms: u64) -> Instant {
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

    /// The active session's elapsed time on the render clock; `None` with no
    /// anchor. The idle gate stays with the caller, which owns the
    /// classification.
    fn elapsed(&self) -> Option<Duration> {
        self.since(self.session_start)
    }

    /// The whole run's elapsed time on the render clock; `None` with no anchor.
    fn overall_elapsed(&self) -> Option<Duration> {
        self.since(self.overall_start)
    }

    fn since(&self, start: Option<Instant>) -> Option<Duration> {
        start.map(|start| {
            let now = self.now.get();
            if now >= start {
                now - start
            } else {
                Duration::ZERO
            }
        })
    }
}

/// The render-side layout caches the ui module writes through `&App` at every
/// render (finding 19): the frames' largest useful scrolls, the rects the
/// mouse hit-tests read and the status bar's chip rects. The `Cell`
/// interior mutability is load-bearing -- `render` takes `&App`.
#[derive(Default)]
pub struct LayoutCache {
    /// Largest useful scroll at the last render; keeps `scroll` from running past the top.
    pub max_scroll: Cell<usize>,
    /// Largest useful task scroll at the last render.
    pub task_max_scroll: Cell<usize>,
    /// The agent output frame's rect at the last render; mouse hit-testing reads it.
    pub output_area: Cell<Rect>,
    /// The task list frame's rect at the last render; mouse hit-testing reads it.
    pub tasks_area: Cell<Rect>,
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
    /// The rail's rect at the last render.
    pub pipeline_area: Cell<Rect>,
    /// The whole screen's rect at the last render; the terminal modal sizes
    /// its first PTY from it.
    pub screen: Cell<Rect>,
}

/// The stop dialog's state (Esc pressed while a build runs, T46.1): its
/// selected row and the rects its mouse hit-testing reads. Grouped (finding
/// 19) like the modal's own state; whether it is open lives on the modal
/// stack.
#[derive(Default)]
pub struct StopState {
    /// The stop dialog's selected row; indexes [`STOP_CHOICES`].
    pub selected: usize,
    /// The stop dialog's bottom line rect at the last render; its buttons'
    /// mouse hit-testing reads it (T59.1).
    pub footer: Cell<Rect>,
    /// The stop dialog's close button rect at the last render (T66.1); a click
    /// on it runs the dialog's Esc key.
    pub close: Cell<Rect>,
    /// The stop dialog's body rect at the last render (T148.1); its rows'
    /// mouse hit-testing reads it -- a pointer move over a choice row moves
    /// the selection there, and a left click selects and runs that row's
    /// Enter.
    pub body: Cell<Rect>,
}

/// The m menu's state (the `m` key, from any engine state): its selected
/// row and the rects its mouse hit-testing reads. Grouped (finding 19) like
/// the modal's own state; whether it is open lives on the modal stack.
#[derive(Default)]
pub struct MenuState {
    /// The m menu's selected row; indexes [`menu_entries`].
    pub selected: usize,
    /// The m menu's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it (T59.1).
    pub footer: Cell<Rect>,
    /// The m menu's close button rect at the last render (T66.1); a click on
    /// it runs the menu's Esc key.
    pub close: Cell<Rect>,
    /// The m menu's body rect at the last render (T137.1); its rows' mouse
    /// hit-testing reads it -- a left click on a rendered row selects and
    /// runs that entry.
    pub body: Cell<Rect>,
}

/// The interactive provider picker's state (the `I` key): the providers it
/// lists with their availability, its selected row and the rects its mouse
/// hit-testing reads. Grouped (finding 19) like the other modals' state;
/// whether it is open lives on the modal stack.
#[derive(Default)]
pub struct InteractivePickerState {
    /// The picker's selected row; indexes [`Self::providers`].
    pub selected: usize,
    /// Every provider of the capability table, in table order, with whether
    /// its program was on the search path when the picker opened.
    pub providers: Vec<(ProviderKind, bool)>,
    /// The `PATH` the availability lookup searches when the picker opens:
    /// seeded from the environment, overridable so tests need no `set_var`.
    pub search_path: Option<OsString>,
    /// The picker's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it.
    pub footer: Cell<Rect>,
    /// The picker's close button rect at the last render; a click on it runs
    /// the picker's Esc key.
    pub close: Cell<Rect>,
    /// The picker's body rect at the last render; its rows' mouse
    /// hit-testing reads it -- a pointer move over a row moves the selection
    /// there, and a left click selects and runs that row's Enter.
    pub body: Cell<Rect>,
}

/// The add-task / inject-task dialog's state (T74.1, T76.1): the shared
/// multi-line editor, which modal the open dialog is, the dialog's status
/// line and the rects its mouse hit-testing reads. Grouped (finding 19) like
/// the other modals' state; whether it is open lives on the modal stack.
#[derive(Default)]
pub struct DialogState {
    /// The dialog's multi-line text editor: the buffer, its cursor,
    /// the Up/Down goal column and the wrap width the renderer reports
    /// (T154.2). The buffer is kept when the dialog is closed.
    pub editor: DialogEditor,
    /// Which modal the open dialog is (T76.1): the add-task dialog or the
    /// inject-task modal. Pinned back to [`DialogKind::Add`] whenever the
    /// add-task dialog opens, so a reopen never inherits the inject kind.
    pub kind: DialogKind,
    /// A one-line message in the dialog (empty input, command errors).
    pub status: Option<String>,
    /// The dialog's bottom line rect at the last render; its buttons' mouse
    /// hit-testing reads it (T59.1).
    pub footer: Cell<Rect>,
    /// The dialog's close button rect at the last render (T66.1); a click on
    /// it runs the dialog's Esc key.
    pub close: Cell<Rect>,
}

pub struct App {
    pub(crate) project: String,
    /// The open modals, one stack (finding 20, the rest): the bottom modal
    /// first, the top modal -- the one that wins keys, mouse and the top
    /// pixel layer -- last. See [`crate::modal::ModalStack`] for which
    /// combinations are legitimately open together.
    pub(crate) modals: ModalStack,
    /// The focused frame of the merged view; it takes all the free space. Defaults
    /// to the task list while the engine is idle and the agent output while a run
    /// is active.
    pub focus: FrameFocus,
    /// The scanned project facts behind the idle scenario.
    pub project_status: ProjectScan,
    pub engine_version: String,
    /// The engine's build differs from this shell's (set once, on attach).
    pub version_mismatch: bool,
    /// A one-line message in the status bar (rejections, quit progress).
    pub status: Option<String>,
    /// Visual lines scrolled up from the bottom; 0 follows the live output.
    pub scroll: usize,
    /// Task rows scrolled up from the auto-follow view; 0 keeps the first pending
    /// task a third of the way down.
    pub task_scroll: usize,
    /// The engine-reduced run state: the phase, the run flags, the active
    /// agent, provider and model, the task list and the output pane.
    pub run: RunState,
    /// The session timer's clock.
    pub clock: SessionClock,
    /// The render-side layout caches the ui module writes.
    pub layout: LayoutCache,
    /// The stop dialog's state.
    pub stop: StopState,
    /// The m menu's state.
    pub menu: MenuState,
    /// The interactive provider picker's state.
    pub interactive: InteractivePickerState,
    /// The embedded provider terminal, while its modal is open.
    pub(crate) terminal: Option<TerminalState>,
    /// The add-task / inject-task dialog's state.
    pub dialog: DialogState,
    /// The settings overlay's interaction state.
    pub overlay: SettingsOverlay,
    /// The theme picker modal's state (T43.1), distinct from the overlay.
    pub theme_modal: ThemeModal,
    /// The model picker's state while its modal is open (T4.4).
    pub(crate) model_picker: Option<ModelPickerState>,
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
    /// The engine-reported model catalog, keyed by provider slug: seeded from the
    /// attach snapshot and updated by every `ModelCatalogChanged`. Read by the model
    /// picker (T4.3); a provider without an entry has not been discovered.
    pub model_catalog: BTreeMap<String, ProviderCatalog>,
}

impl App {
    pub fn new(snapshot: Snapshot, engine_version: String) -> Self {
        let mut app = Self {
            project: snapshot.project_dir,
            modals: ModalStack::new(),
            focus: FrameFocus::Tasks,
            project_status: ProjectScan::default(),
            engine_version,
            version_mismatch: false,
            status: None,
            scroll: 0,
            task_scroll: 0,
            run: RunState {
                phase: snapshot.phase,
                planning: false,
                discovering: false,
                stopping: false,
                agent: AgentRole::Builder.as_str().into(),
                provider: snapshot.provider.clone(),
                model: snapshot.model.clone(),
                config_provider: snapshot.provider,
                config_model: snapshot.model,
                current_task: snapshot.current_task,
                completed: 0,
                usage: None,
                tasks: snapshot.tasks,
                new_tasks: Vec::new(),
                output: Pane::default(),
            },
            clock: SessionClock::new(),
            layout: LayoutCache::default(),
            stop: StopState::default(),
            menu: MenuState::default(),
            interactive: InteractivePickerState {
                search_path: std::env::var_os("PATH"),
                ..InteractivePickerState::default()
            },
            terminal: None,
            dialog: DialogState {
                editor: DialogEditor::default(),
                kind: DialogKind::Add,
                status: None,
                footer: Cell::new(Rect::default()),
                close: Cell::new(Rect::default()),
            },
            overlay: SettingsOverlay::default(),
            theme_modal: ThemeModal::default(),
            model_picker: None,
            tui: TuiSettings::default(),
            settings: snapshot.settings,
            pipeline: snapshot.pipeline,
            model_catalog: snapshot
                .model_catalog
                .into_iter()
                .map(|catalog| (catalog.provider.clone(), catalog))
                .collect(),
        };
        for event in snapshot.recent {
            app.apply(event);
        }
        // Replayed history is not this session's work.
        app.run.completed = 0;
        app.run.new_tasks.clear();
        app.run.planning = snapshot.planning;
        app.run.discovering = snapshot.discovering;
        // A replayed `AgentChanged` already anchored the timer to the engine's
        // session start, so a shell attaching mid-run shows the true elapsed
        // time; only an unanchored busy attach counts from here (T42.1). An
        // idle snapshot clears any anchor a replayed `AgentChanged` from a
        // finished session left behind, so the next run starts fresh (T131.1).
        if app.is_idle() {
            app.clock.session_start = None;
            app.clock.overall_start = None;
        } else {
            let now = app.clock.now.get();
            app.clock.session_start.get_or_insert(now);
            app.clock.overall_start.get_or_insert(now);
        }
        app.focus = app.state_focus();
        app
    }

    /// Compares the engine's reported build version with this shell's own.
    pub fn check_version(&mut self, shell_version: &str) {
        self.version_mismatch = self.engine_version != shell_version;
    }

    /// A shell-side notice line in the main pane (e.g. a config reload warning).
    pub(crate) fn shell_notice(&mut self, text: impl Into<String>) {
        self.push(LineOrigin::Orchestrator, LineKind::Notice, text.into());
    }

    pub fn is_new_task(&self, id: &str) -> bool {
        self.run.new_tasks.iter().any(|t| t == id)
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
        let key = match (self.modals.contains(Modal::Theme), self.theme_modal.preview) {
            (true, Some(preview)) => preview,
            _ => self.tui.theme,
        };
        Theme::resolve(key, self.tui.truecolor)
    }

    /// The engine is idle: no build, planner or discovery run is active. The scenario
    /// is re-derived and the task list takes the default focus in this state.
    pub fn is_idle(&self) -> bool {
        self.run.phase == Phase::Startup && !self.run.planning && !self.run.discovering
    }

    /// The active session's elapsed time, recomputed on every render; `None`
    /// while the engine is idle.
    pub fn session_elapsed(&self) -> Option<Duration> {
        if self.is_idle() {
            return None;
        }
        self.clock.elapsed()
    }

    /// The whole run's elapsed time (plan, build and discovery together),
    /// recomputed on every render; `None` while the engine is idle. The rail
    /// shows this, the output frame shows the active agent's
    /// [`session_elapsed`](Self::session_elapsed).
    pub fn overall_elapsed(&self) -> Option<Duration> {
        if self.is_idle() {
            return None;
        }
        self.clock.overall_elapsed()
    }

    /// The live startup scenario: the scanned file facts
    /// combined with the engine-reported task counts, so every queue change re-derives
    /// it without a rescan.
    pub fn scenario(&self) -> Scenario {
        let pending = self.run.tasks.iter().filter(|t| !t.done).count();
        patok_core::scenario::classify(
            self.project_status.has_code,
            self.project_status.task_file,
            pending,
            self.run.tasks.len(),
        )
    }

    /// The dialog's primary button label: "Inject"
    /// for the inject-task modal (T76.1), otherwise "Start" when the queue is
    /// ready and the input is empty, "Scan" when the queue is complete and the
    /// input is empty, "Submit" otherwise.
    pub fn primary_label(&self) -> &'static str {
        if self.dialog.kind == DialogKind::Inject {
            return "Inject";
        }
        let empty = self.dialog.editor.text().trim().is_empty();
        match (self.scenario(), empty) {
            (Scenario::QueueReady, true) => "Start",
            (Scenario::QueueComplete, true) => "Scan",
            _ => "Submit",
        }
    }

    /// Whether the settings overlay is open (rendered above everything else).
    #[cfg(any(test, feature = "test-support"))]
    pub fn settings_open(&self) -> bool {
        self.modals.contains(Modal::Overlay)
    }

    /// Whether the stop dialog is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stop_open(&self) -> bool {
        self.modals.contains(Modal::Stop)
    }

    /// Whether the m menu is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn menu_open(&self) -> bool {
        self.modals.contains(Modal::Menu)
    }

    /// Whether the interactive provider picker is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn picker_open(&self) -> bool {
        self.modals.contains(Modal::InteractivePicker)
    }

    /// Whether the add-task / inject-task dialog is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn dialog_open(&self) -> bool {
        self.modals.contains(Modal::Dialog)
    }

    /// Whether the theme picker is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn theme_open(&self) -> bool {
        self.modals.contains(Modal::Theme)
    }

    /// Whether the model picker is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn model_picker_open(&self) -> bool {
        self.modals.contains(Modal::ModelPicker)
    }

    /// Whether the settings overlay's unsaved-changes dialog is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn confirm_open(&self) -> bool {
        self.modals.contains(Modal::Confirm)
    }

    /// The open modals, bottom first: the render order, the bottom layer
    /// drawn first and the top modal last.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_modals(&self) -> Vec<Modal> {
        self.modals.iter().collect()
    }

    /// Opens the m menu (the `m` key): a refused push (another modal
    /// already open) is a no-op, so the selection is left alone.
    pub(crate) fn open_menu(&mut self) {
        if self.modals.push(Modal::Menu) {
            self.menu.selected = 0;
        }
    }

    /// Opens interactive mode: the `I` key, the m menu's Interactive entry
    /// and the status bar's `I` chip all call it. It opens the provider
    /// picker, reading each provider's availability from the search path
    /// once; a refused push (another modal already open) is a no-op. Choosing
    /// a provider opens its terminal.
    pub(crate) fn open_interactive(&mut self) {
        if !self.modals.push(Modal::InteractivePicker) {
            return;
        }
        let path = self.interactive.search_path.as_deref();
        self.interactive.providers = ProviderKind::VARIANTS
            .iter()
            .map(|&(kind, _)| (kind, kind.interactive_available_in(path)))
            .collect();
        self.interactive.selected = self
            .interactive
            .providers
            .iter()
            .position(|&(_, available)| available)
            .unwrap_or(0);
    }

    /// Opens the stop dialog (Esc while a run is active): a refused push
    /// (another modal already open) is a no-op, so the selection is left
    /// alone.
    pub(crate) fn open_stop(&mut self) {
        if self.modals.push(Modal::Stop) {
            self.stop.selected = 0;
        }
    }

    /// Opens the settings overlay with fresh per-open state: a refused push
    /// (another modal already open) is a no-op, so the overlay's state is
    /// left alone.
    pub(crate) fn open_settings(&mut self) {
        if self.modals.push(Modal::Overlay) {
            self.overlay.reset();
            self.overlay.sync_readout(&self.settings);
        }
    }

    /// Closes the settings overlay, whatever state it is in, together with
    /// the unsaved-changes dialog stacked on it.
    pub fn close_settings(&mut self) {
        self.modals.pop(Modal::Overlay);
        self.model_picker = None;
        self.overlay.discard();
    }

    /// Opens the model picker for `provider` over the settings overlay, on
    /// `row` with `current` as its value (T4.4). A refused push leaves the
    /// state untouched.
    pub(crate) fn open_model_picker(&mut self, row: &'static Row, provider: &str, current: &str) {
        if self.modals.push(Modal::ModelPicker) {
            self.model_picker = Some(ModelPickerState {
                row,
                picker: ModelPicker::new(provider, current),
            });
        }
    }

    /// Applies one picker outcome (T4.4): a choice drafts the row, `Custom`
    /// closes the picker and opens the inline editor, `Close` just closes.
    ///
    /// A choice edits only the overlay draft (T4.5): nothing is applied until
    /// the close dialog saves, which is also why Esc has nothing to restore.
    pub(crate) fn finish_model_picker(&mut self, outcome: PickerOutcome) -> Action {
        if matches!(outcome, PickerOutcome::Stay) {
            return Action::None;
        }
        self.modals.pop(Modal::ModelPicker);
        let Some(state) = self.model_picker.take() else {
            return Action::None;
        };
        match outcome {
            PickerOutcome::Stay | PickerOutcome::Close => {}
            PickerOutcome::Choose(value) => {
                self.overlay
                    .drafts
                    .insert(state.row.key.into(), SettingValue::Str(value));
                self.overlay.status = None;
            }
            PickerOutcome::Custom => {
                let current = SettingValue::Str(state.picker.current().to_owned());
                self.overlay.open_editor(state.row, Some(current));
            }
        }
        Action::None
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
                    truecolor_choice(self.tui.truecolor).into(),
                )),
                "preview_wrap" => Some(SettingValue::Bool(self.tui.preview_wrap)),
                "update_channel" => {
                    Some(SettingValue::Str(self.tui.update_channel.as_str().into()))
                }
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
        if self.modals.contains(Modal::Overlay) {
            self.overlay.settle_draft(field, &self.settings);
        }
    }

    /// One drafted change was rejected (validation or persistence error): the
    /// reason shows in the overlay's status line and the draft stays, so a retry
    /// can pick the save choice again. With the overlay closed (a run-mode flip
    /// sent from the main view, T60.1), the reason shows in the status bar.
    pub fn on_settings_rejected(&mut self, error: String) {
        if self.modals.contains(Modal::Overlay) {
            self.overlay.status = Some((error, StatusLevel::Error));
        } else {
            self.status = Some(error);
        }
    }
}

/// Turns a drafted value into what the overlay renders and cycles: `truecolor` is
/// drafted in its stored shape (`Bool`/`Unset`) but displays as its auto/on/off
/// choice, like the shell's live mirror does.
fn draft_display(key: &str, value: &SettingValue) -> SettingValue {
    if key == "truecolor" {
        return SettingValue::Str(truecolor_choice(value.as_bool()).into());
    }
    value.clone()
}
