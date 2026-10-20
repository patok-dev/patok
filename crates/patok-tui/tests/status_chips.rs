//! The status bar's contextual hint chips are real buttons (T134.1, T141.1):
//! the idle Enter chip, the running Esc stop chip and the m menu chip each
//! render through the shared modal button formatting and expose a rect
//! covering their full button text -- key, brackets and label together --
//! and a left click anywhere inside it runs exactly the action its key
//! triggers -- clicking Enter starts the build loop or runs a discovery
//! round, clicking Esc opens the stop dialog while a build, planner or
//! discovery run is active (T142.1). Each chip only appears (and only
//! clicks) in the engine state where its key is active.

mod support;

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, MouseButton, MouseEventKind};
use patok_core::event::{EngineEvent, Phase};
use patok_tui::{Action, App, SOFT_STOP_PENDING, button_text};

use support::{draw, draw_buffer_of, mouse, press};

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";
const COMPLETE_TASKS: &str = "## Phase 1\n- [x] T1.1: scaffold the workspace\n";

fn app_with(tasks: &str) -> App {
    support::app(tasks, "claude", BTreeMap::new(), Some(true))
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

/// An idle engine mid planner run, so the Esc stop chip joins the strip
/// (T142.1).
fn planning_app() -> App {
    let mut app = app();
    app.apply(EngineEvent::PlanningChanged { planning: true });
    app
}

/// An idle engine mid discovery run, so the Esc stop chip joins the strip
/// (T142.1).
fn discovering_app() -> App {
    let mut app = app();
    app.apply(EngineEvent::DiscoveryChanged { discovering: true });
    app
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
    let chip = app.layout.enter_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(32, 13, 17, 1));

    let focus = app.focus;
    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::StartBuild);
    assert_eq!(app.focus, focus, "the click must not move the frame focus");
    assert!(!app.menu_open());
    assert!(!app.stop_open());

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
    let chip = app.layout.enter_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(24, 13, 25, 1));

    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::RunDiscovery);
    assert!(!app.menu_open());
    assert!(!app.stop_open());

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
    let chip = app.layout.stop_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(35, 13, 14, 1));

    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::None);
    assert!(app.stop_open());
    assert_eq!(app.stop.selected, 0);
    assert_eq!(app.status, None);
    assert!(!app.menu_open());

    // The click aliases the key exactly: the Esc press on an identically
    // prepared running app leaves the same state.
    let mut key_app = running_app();
    assert_eq!(press(&mut key_app, KeyCode::Esc), Action::None);
    assert_eq!(key_app.stop_open(), app.stop_open());
    assert_eq!(key_app.stop.selected, app.stop.selected);
    assert_eq!(key_app.status, app.status);
}

/// The m menu chip's click runs the m key's action: it opens the menu modal
/// with the first entry selected.
#[test]
fn the_menu_chip_click_opens_the_menu() {
    let mut app = app();
    draw(&app, 80, 14);
    let chip = app.layout.menu_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(49, 13, 12, 1));

    assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::None);
    assert!(app.menu_open());
    assert_eq!(app.menu.selected, 0);
    assert!(!app.stop_open());

    // The click aliases the key exactly: the m press on an identically
    // prepared app leaves the same state.
    let mut key_app = app_with(TASKS);
    assert_eq!(press(&mut key_app, KeyCode::Char('m')), Action::None);
    assert_eq!(key_app.menu_open(), app.menu_open());
    assert_eq!(key_app.menu.selected, app.menu.selected);
}

/// Each chip's rect covers its full button text -- the key, the brackets and
/// the label together, the same width [`button_text`] defines -- and a left
/// click at every column inside the rect fires its key's action.
#[test]
fn the_chip_rects_cover_the_full_button_and_click_anywhere_inside() {
    // One fresh app per click: each click fires the chip's action, which
    // changes the app's state. The idle Enter `start` button.
    for column in 32..49u16 {
        let mut app = app();
        draw(&app, 80, 14);
        let chip = app.layout.enter_chip.get();
        assert_eq!(
            chip.width as usize,
            button_text("Enter", "start").chars().count(),
            "the Enter rect covers the full button text"
        );
        assert_eq!(
            click(&mut app, column, chip.y),
            Action::StartBuild,
            "at {column}"
        );
    }

    // The complete-queue Enter button is wider (`run discovery` label).
    for column in 24..49u16 {
        let mut app = app_with(COMPLETE_TASKS);
        draw(&app, 80, 14);
        let chip = app.layout.enter_chip.get();
        assert_eq!(
            chip.width as usize,
            button_text("Enter", "run discovery").chars().count()
        );
        assert_eq!(
            click(&mut app, column, chip.y),
            Action::RunDiscovery,
            "at {column}"
        );
    }

    // The running Esc stop button.
    for column in 35..49u16 {
        let mut app = running_app();
        draw(&app, 80, 14);
        let chip = app.layout.stop_chip.get();
        assert_eq!(
            chip.width as usize,
            button_text("Esc", "stop").chars().count()
        );
        assert!(chip.contains(ratatui::layout::Position::new(column, 13)));
        click(&mut app, column, chip.y);
        assert!(app.stop_open(), "the stop dialog opens at {column}");
    }

    // The m menu button, in every state that shows it.
    for make in [app as fn() -> App, running_app as fn() -> App] {
        for column in 49..61u16 {
            let mut app = make();
            draw(&app, 80, 14);
            let chip = app.layout.menu_chip.get();
            assert_eq!(
                chip.width as usize,
                button_text("m", "menu").chars().count()
            );
            click(&mut app, column, chip.y);
            assert!(app.menu_open(), "the menu opens at {column}");
        }
    }

    // The rect's columns spell the full button text, key, brackets and label
    // together. The 80x14 shell keeps the status row at row 13.
    let buffer = draw_buffer_of(&app(), 80, 14);
    let row = 13;
    for (x, width, key, label) in [
        (32u16, 17u16, "Enter", "start"),
        (49, 12, "m", "menu"),
        (61, 19, "I", "interactive"),
    ] {
        let symbols: String = (x..x + width)
            .map(|column| buffer[(column, row)].symbol())
            .collect();
        assert_eq!(symbols, button_text(key, label));
    }
}

/// Each chip only clicks in the engine state where its key is active: the
/// Enter chip is the zero rect while a build runs, the Esc chip is the zero
/// rect while idle and while a soft stop is pending, and clicks at pad
/// columns start nothing and open nothing.
#[test]
fn the_chips_are_inert_in_the_wrong_state() {
    // Running: the Enter chip is gone, and a click at a pad column (the
    // strip is " RUNNING  sprint " then pad up to the Esc button at column
    // 35) starts nothing.
    let mut running = running_app();
    draw(&running, 80, 14);
    assert_eq!(
        running.layout.enter_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(click(&mut running, 34, 13), Action::None);
    assert!(!running.menu_open());
    assert!(!running.stop_open());

    // Idle: the Esc chip is gone, and clicks at pad columns open no stop
    // dialog. (The Enter button covers columns 32-48 and the m button
    // 49-60 in the idle layout, so a click there is theirs.)
    let mut idle = app();
    draw(&idle, 80, 14);
    assert_eq!(
        idle.layout.stop_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(click(&mut idle, 0, 13), Action::None);
    assert_eq!(click(&mut idle, 31, 13), Action::None);
    assert!(!idle.stop_open());
    assert!(!idle.menu_open());

    // A pending soft stop: Esc cancels the stop instead, so the chip hides
    // and a click at its place changes nothing.
    let mut stopping = running_app();
    assert_eq!(press(&mut stopping, KeyCode::Char('q')), Action::Quit);
    assert!(stopping.run.stopping);
    draw(&stopping, 80, 14);
    assert_eq!(
        stopping.layout.stop_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(click(&mut stopping, 60, 13), Action::None);
    assert!(!stopping.stop_open());
    assert!(stopping.run.stopping);
}

/// The chips' rects are zero whenever a status message replaces the hint
/// zone, so clicks at their usual places change nothing.
#[test]
fn the_chips_are_inert_while_a_status_message_shows() {
    let mut idle = app();
    idle.status = Some("The planner is already running.".into());
    draw(&idle, 80, 14);
    assert_eq!(
        idle.layout.enter_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(
        idle.layout.stop_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(click(&mut idle, 57, 13), Action::None);
    assert!(!idle.menu_open());
    assert!(!idle.stop_open());

    let mut running = running_app();
    running.status = Some("Stopping after this task...".into());
    draw(&running, 80, 14);
    assert_eq!(
        running.layout.stop_chip.get(),
        ratatui::layout::Rect::default()
    );
    assert_eq!(click(&mut running, 60, 13), Action::None);
    assert!(!running.stop_open());
}

/// Clicks outside the chips' rects -- on the strip's padding, on the line's
/// left end, past the strip's right edge -- do nothing: no action, no focus
/// move, no modal, no build.
#[test]
fn clicks_outside_the_chip_rects_do_nothing() {
    // Idle: the strip is
    // " STOPPED  sprint " then pad, then " [ Enter ] start  [ m ] menu  [ I ] interactive " on
    // the 80-column row -- the Enter button at columns 32-48, the m button
    // at 49-60, the I button at 61-79.
    let mut idle = app();
    draw(&idle, 80, 14);
    let enter = idle.layout.enter_chip.get();
    let menu = idle.layout.menu_chip.get();
    assert_eq!(enter, ratatui::layout::Rect::new(32, 13, 17, 1));
    assert_eq!(menu, ratatui::layout::Rect::new(49, 13, 12, 1));
    let focus = idle.focus;
    for (column, row) in [(0, 13), (31, 13), (80, 13)] {
        assert_eq!(
            click(&mut idle, column, row),
            Action::None,
            "a click at ({column}, {row}) must do nothing"
        );
        assert!(!idle.menu_open(), "at ({column}, {row})");
        assert!(!idle.stop_open(), "at ({column}, {row})");
    }
    assert_eq!(idle.focus, focus, "no outside click moves the focus");

    // Running: the strip is
    // " RUNNING  sprint " then pad, then " [ Esc ] stop  [ m ] menu  [ I ] interactive ".
    let mut running = running_app();
    draw(&running, 80, 14);
    let stop = running.layout.stop_chip.get();
    let menu = running.layout.menu_chip.get();
    assert_eq!(stop, ratatui::layout::Rect::new(35, 13, 14, 1));
    assert_eq!(menu, ratatui::layout::Rect::new(49, 13, 12, 1));
    let focus = running.focus;
    for (column, row) in [(0, 13), (34, 13), (80, 13)] {
        assert_eq!(
            click(&mut running, column, row),
            Action::None,
            "a click at ({column}, {row}) must do nothing"
        );
        assert!(!running.stop_open(), "at ({column}, {row})");
        assert!(!running.menu_open(), "at ({column}, {row})");
    }
    assert_eq!(running.focus, focus, "no outside click moves the focus");
    assert_eq!(
        running.layout.enter_chip.get(),
        ratatui::layout::Rect::default()
    );
}

/// The boundary column between two adjacent buttons belongs to the next
/// button: the column right after the Enter (or Esc) button is the m
/// button's first column, and clicking it opens the menu -- the neighbour's
/// action, never nothing and never both.
#[test]
fn the_button_boundary_click_fires_the_neighbour() {
    // Idle: the column past the Enter button's right edge is the m button's
    // first column.
    let mut idle = app();
    draw(&idle, 80, 14);
    let enter = idle.layout.enter_chip.get();
    click(&mut idle, enter.x + enter.width, enter.y);
    assert!(idle.menu_open(), "the boundary column opens the menu");
    assert!(!idle.stop_open());

    // The column past the m button's right edge is the I button's first
    // column (T2.1).
    let mut idle = app();
    draw(&idle, 80, 14);
    let menu = idle.layout.menu_chip.get();
    click(&mut idle, menu.x + menu.width, menu.y);
    assert!(!idle.menu_open());
    assert!(
        idle.status
            .as_deref()
            .is_some_and(|s| s.contains("Interactive mode")),
        "{:?}",
        idle.status
    );

    // Running: the column past the Esc button's right edge likewise.
    let mut running = running_app();
    draw(&running, 80, 14);
    let stop = running.layout.stop_chip.get();
    click(&mut running, stop.x + stop.width, stop.y);
    assert!(running.menu_open(), "the boundary column opens the menu");
    assert!(!running.stop_open());
}

/// The Esc stop chip shows and clicks during planner and discovery runs too
/// (T142.1): the chip renders at the same right-aligned place it occupies
/// while a build runs, `stop_chip` carries its rect, a click inside it
/// opens the stop dialog exactly like the Esc press, and both leave the
/// same state.
#[test]
fn the_esc_stop_chip_shows_and_clicks_during_planner_and_discovery_runs() {
    for make in [planning_app as fn() -> App, discovering_app as fn() -> App] {
        let mut app = make();
        assert!(app.run.planning || app.run.discovering);
        draw(&app, 80, 14);
        let chip = app.layout.stop_chip.get();
        assert_eq!(chip, ratatui::layout::Rect::new(35, 13, 14, 1));
        assert_eq!(
            chip.width as usize,
            button_text("Esc", "stop").chars().count()
        );

        assert_eq!(click(&mut app, chip.x + 1, chip.y), Action::None);
        assert!(app.stop_open());
        assert_eq!(app.stop.selected, 0);
        assert_eq!(app.status, None);
        assert!(!app.menu_open());

        // The click aliases the key exactly: the Esc press on an
        // identically prepared app leaves the same state.
        let mut key_app = make();
        assert_eq!(press(&mut key_app, KeyCode::Esc), Action::None);
        assert_eq!(key_app.stop_open(), app.stop_open());
        assert_eq!(key_app.stop.selected, app.stop.selected);
        assert_eq!(key_app.status, app.status);
    }
}

/// The stop dialog's choices act on a planner or discovery run the same way
/// they act on a build (T142.1): SoftStop soft-stops, Interrupt interrupts,
/// Cancel and the dialog's own Esc leave the run running.
#[test]
fn the_stop_dialog_choices_act_on_planner_and_discovery_runs() {
    for make in [planning_app as fn() -> App, discovering_app as fn() -> App] {
        // SoftStop (the first choice) soft-stops the run.
        let mut app = make();
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::Quit);
        assert!(app.run.stopping);
        assert_eq!(app.status, Some(SOFT_STOP_PENDING.into()));
        assert!(!app.stop_open());

        // Interrupt (the second choice) interrupts the run.
        let mut app = make();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::Interrupt);
        assert!(app.run.stopping);
        assert!(!app.stop_open());

        // Cancel (the third choice) closes the dialog with the run
        // untouched.
        let mut app = make();
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(!app.stop_open());
        assert!(!app.run.stopping);
        assert_eq!(app.status, None);
        assert!(
            app.run.planning || app.run.discovering,
            "the run keeps running"
        );

        // The dialog's own Esc closes it with the run untouched too.
        let mut app = make();
        press(&mut app, KeyCode::Esc);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.stop_open());
        assert!(!app.run.stopping);
        assert!(app.run.planning || app.run.discovering);
    }
}

/// Esc while a soft stop is pending during a planner or discovery run still
/// cancels the stop instead of opening the dialog (T142.1): the stopping
/// guard sits above the Esc arm.
#[test]
fn esc_cancels_a_pending_soft_stop_during_planner_and_discovery_runs() {
    for make in [planning_app as fn() -> App, discovering_app as fn() -> App] {
        let mut app = make();
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
        assert!(app.run.stopping);

        assert_eq!(press(&mut app, KeyCode::Esc), Action::CancelSoftStop);
        assert!(!app.run.stopping);
        assert!(!app.stop_open());
    }
}

/// The Esc chip stays hidden while the engine is idle with no run and while
/// a soft stop is pending during a planner or discovery run (T142.1).
#[test]
fn the_esc_chip_stays_hidden_while_idle_or_while_a_stop_is_pending() {
    // Idle with no run: the chip is the zero rect.
    let idle = app();
    assert!(!idle.run.planning && !idle.run.discovering);
    draw(&idle, 80, 14);
    assert_eq!(
        idle.layout.stop_chip.get(),
        ratatui::layout::Rect::default()
    );

    // A pending soft stop during a planner or discovery run hides the chip,
    // and a click at its usual place changes nothing.
    for make in [planning_app as fn() -> App, discovering_app as fn() -> App] {
        let mut app = make();
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
        assert!(app.run.stopping);
        draw(&app, 80, 14);
        assert_eq!(app.layout.stop_chip.get(), ratatui::layout::Rect::default());
        assert_eq!(click(&mut app, 60, 13), Action::None);
        assert!(!app.stop_open());
        assert!(app.run.stopping, "the pending stop survives the click");
    }
}

/// T2.1: the `I` chip's rect covers its full button, a click anywhere inside
/// it runs the `I` key's action, and the rect is zero while a status message
/// shows.
#[test]
fn the_interactive_chip_click_runs_the_i_key_action() {
    for make in [app as fn() -> App, running_app as fn() -> App] {
        let app = make();
        draw(&app, 80, 14);
        assert_eq!(
            app.layout.interactive_chip.get(),
            ratatui::layout::Rect::new(61, 13, 19, 1)
        );
        for column in 61..80u16 {
            let mut app = make();
            draw(&app, 80, 14);
            assert_eq!(click(&mut app, column, 13), Action::None);
            assert!(!app.menu_open() && !app.stop_open(), "at {column}");
            assert!(
                app.status
                    .as_deref()
                    .is_some_and(|s| s.contains("Interactive mode")),
                "at {column}: {:?}",
                app.status
            );
        }
    }

    let mut app = app();
    app.status = Some("The planner is already running.".into());
    draw(&app, 80, 14);
    assert_eq!(
        app.layout.interactive_chip.get(),
        ratatui::layout::Rect::default()
    );
}
