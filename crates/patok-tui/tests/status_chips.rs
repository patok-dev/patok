//! The status bar's contextual hint chips are real buttons (T134.1): the
//! idle Enter chip, the running Esc stop chip and the m menu chip each expose
//! their key span's rect, and a left click inside it runs exactly the action
//! its key triggers -- clicking Enter starts the build loop or runs a
//! discovery round, clicking Esc opens the stop dialog while a build runs.
//! Each chip only appears (and only clicks) in the engine state where its
//! key is active.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use patok_core::event::{EngineEvent, Phase, Snapshot};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_tui::{Action, App, render};

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";
const COMPLETE_TASKS: &str = "## Phase 1\n- [x] T1.1: scaffold the workspace\n";

fn app_with(tasks: &str) -> App {
    let mut app = App::new(
        Snapshot {
            project_dir: "/home/user/demo".into(),
            phase: Phase::Startup,
            tasks: task::parse(tasks),
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
    );
    // Deterministic colours: no COLORTERM auto-detection in tests.
    app.tui.truecolor = Some(true);
    app
}

/// An idle engine with one pending task, so the Enter chip says `start`.
fn app() -> App {
    app_with(TASKS)
}

/// A running engine, so the Esc stop chip joins the strip.
fn running_app() -> App {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app
}

fn draw(app: &App, width: u16, height: u16) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
}

fn press(app: &mut App, code: KeyCode) -> Action {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn click(app: &mut App, column: u16, row: u16) -> Action {
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row))
}

/// The idle Enter chip's click runs the Enter key's action: with a pending
/// task it starts the build loop, and the click moves nothing else -- no
/// frame focus change, no modal.
#[test]
fn the_enter_start_chip_click_starts_the_build() {
    let mut app = app();
    draw(&app, 80, 14);
    let chip = app.enter_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(57, 13, 7, 1));

    let focus = app.focus;
    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::StartBuild);
    assert_eq!(app.focus, focus, "the click must not move the frame focus");
    assert!(!app.menu_open);
    assert!(!app.stop_open);

    // The click aliases the key exactly: the Enter press returns the same
    // action on an identically prepared app.
    let mut key_app = app_with(TASKS);
    assert_eq!(press(&mut key_app, KeyCode::Enter), Action::StartBuild);
}

/// The idle Enter chip's click runs the Enter key's action with a complete
/// queue too: it starts a discovery round.
#[test]
fn the_run_discovery_chip_click_runs_discovery() {
    let mut app = app_with(COMPLETE_TASKS);
    draw(&app, 80, 14);
    let chip = app.enter_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(49, 13, 7, 1));

    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::RunDiscovery);
    assert!(!app.menu_open);
    assert!(!app.stop_open);

    // The click aliases the key exactly: the Enter press returns the same
    // action on an identically prepared app.
    let mut key_app = app_with(COMPLETE_TASKS);
    assert_eq!(press(&mut key_app, KeyCode::Enter), Action::RunDiscovery);
}

/// The running Esc chip's click runs the Esc key's action: it opens the stop
/// dialog with the first choice selected and the status line cleared --
/// exactly the state the Esc press leaves.
#[test]
fn the_esc_stop_chip_click_opens_the_stop_dialog() {
    let mut app = running_app();
    draw(&app, 80, 14);
    let chip = app.stop_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(60, 13, 5, 1));

    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::None);
    assert!(app.stop_open);
    assert_eq!(app.stop_selected, 0);
    assert_eq!(app.status, None);
    assert!(!app.menu_open);

    // The click aliases the key exactly: the Esc press on an identically
    // prepared running app leaves the same state.
    let mut key_app = running_app();
    assert_eq!(press(&mut key_app, KeyCode::Esc), Action::None);
    assert_eq!(key_app.stop_open, app.stop_open);
    assert_eq!(key_app.stop_selected, app.stop_selected);
    assert_eq!(key_app.status, app.status);
}

/// Each chip only clicks in the engine state where its key is active: the
/// Enter chip is the zero rect while a build runs, the Esc chip is the zero
/// rect while idle and while a soft stop is pending, and clicks at their
/// usual places start nothing and open nothing.
#[test]
fn the_chips_are_inert_in_the_wrong_state() {
    // Running: the Enter chip is gone, and a click at its idle place starts
    // nothing.
    let mut running = running_app();
    draw(&running, 80, 14);
    assert_eq!(running.enter_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(click(&mut running, 57, 13), Action::None);
    assert!(!running.menu_open);
    assert!(!running.stop_open);

    // Idle: the Esc chip is gone, and clicks at its running place -- which
    // the idle layout's start label and pad occupy -- open no stop dialog.
    // (The Enter chip covers columns 57-63 in the idle layout, so a click
    // there is the Enter chip's, not the Esc chip's would-be place.)
    let mut idle = app();
    draw(&idle, 80, 14);
    assert_eq!(idle.stop_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(click(&mut idle, 0, 13), Action::None);
    assert_eq!(click(&mut idle, 65, 13), Action::None);
    assert!(!idle.stop_open);
    assert!(!idle.menu_open);

    // A pending soft stop: Esc cancels the stop instead, so the chip hides
    // and a click at its place changes nothing.
    let mut stopping = running_app();
    assert_eq!(press(&mut stopping, KeyCode::Char('q')), Action::Quit);
    assert!(stopping.stopping);
    draw(&stopping, 80, 14);
    assert_eq!(stopping.stop_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(click(&mut stopping, 60, 13), Action::None);
    assert!(!stopping.stop_open);
    assert!(stopping.stopping);
}

/// The chips' rects are zero whenever a status message replaces the hint
/// zone, so clicks at their usual places change nothing.
#[test]
fn the_chips_are_inert_while_a_status_message_shows() {
    let mut idle = app();
    idle.status = Some("Planner running...".into());
    draw(&idle, 80, 14);
    assert_eq!(idle.enter_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(idle.stop_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(click(&mut idle, 57, 13), Action::None);
    assert!(!idle.menu_open);
    assert!(!idle.stop_open);

    let mut running = running_app();
    running.status = Some("Stopping after this task...".into());
    draw(&running, 80, 14);
    assert_eq!(running.stop_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(click(&mut running, 60, 13), Action::None);
    assert!(!running.stop_open);
}

/// Clicks outside the chips' rects -- on the strip's padding, on the label
/// spans, on each rect's right boundary -- do nothing: no action, no focus
/// move, no modal, no build.
#[test]
fn clicks_outside_the_chip_rects_do_nothing() {
    // Idle: the strip is
    // " STOPPED  sprint  Enter  start  m  menu" on the 80-column row.
    let mut idle = app();
    draw(&idle, 80, 14);
    let enter = idle.enter_chip.get();
    let menu = idle.menu_chip.get();
    assert_eq!(menu, ratatui::layout::Rect::new(71, 13, 3, 1));
    let focus = idle.focus;
    for (column, row) in [
        (0, 13),
        (56, 13),
        (enter.x + enter.width, 13),
        (menu.x + menu.width, 13),
    ] {
        assert_eq!(
            click(&mut idle, column, row),
            Action::None,
            "a click at ({column}, {row}) must do nothing"
        );
        assert!(!idle.menu_open, "at ({column}, {row})");
        assert!(!idle.stop_open, "at ({column}, {row})");
    }
    assert_eq!(idle.focus, focus, "no outside click moves the focus");

    // Running: the strip is
    // " RUNNING  sprint  Esc  stop  m  menu".
    let mut running = running_app();
    draw(&running, 80, 14);
    let stop = running.stop_chip.get();
    let menu = running.menu_chip.get();
    assert_eq!(menu, ratatui::layout::Rect::new(71, 13, 3, 1));
    let focus = running.focus;
    for (column, row) in [
        (0, 13),
        (stop.x + stop.width, 13),
        (menu.x + menu.width, 13),
    ] {
        assert_eq!(
            click(&mut running, column, row),
            Action::None,
            "a click at ({column}, {row}) must do nothing"
        );
        assert!(!running.stop_open, "at ({column}, {row})");
        assert!(!running.menu_open, "at ({column}, {row})");
    }
    assert_eq!(running.focus, focus, "no outside click moves the focus");
    assert_eq!(running.enter_chip.get(), ratatui::layout::Rect::default());
}
