//! The m menu modal (T128.1): the status bar's secondary key hints moved
//! behind one `m` chip, so this covers the modal's rendered rows (and the
//! stop row that joins only while a build runs), its key handling per
//! entry -- each entry runs exactly the action its direct key binding
//! triggers -- the rows' mouse handling -- a left click on a rendered row
//! moves the highlight there and runs that row exactly like `m` plus
//! Enter (T137.1), and a pointer move onto a rendered row moves only the
//! highlight, never the dispatch (T140.1) -- and the status bar's
//! slimmed-down hint strip.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use patok_core::event::{EngineEvent, Phase, Snapshot};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_tui::{Action, App, render};

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";

fn app() -> App {
    let mut app = App::new(
        Snapshot {
            project_dir: "/home/user/demo".into(),
            phase: Phase::Startup,
            tasks: task::parse(TASKS),
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

/// A running engine, so the menu's stop entry joins the list.
fn running_app() -> App {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app
}

fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
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

fn open() -> App {
    let mut app = app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    assert!(app.menu_open);
    app
}

/// Runs the menu's selected entry: `index` Down presses then Enter.
fn confirm_entry(app: &mut App, index: usize) -> Action {
    assert!(app.menu_open);
    for _ in 0..index {
        assert_eq!(press(app, KeyCode::Down), Action::None);
    }
    press(app, KeyCode::Enter)
}

/// The menu's rendered rows while the engine is idle: the four secondary
/// entries in order, no stop row, the focus marker on the first row and
/// the Confirm/Close footer buttons.
#[test]
fn the_idle_menu_renders_its_four_entries() {
    let app = open();
    let screen = draw(&app, 80, 14);
    assert!(screen.contains(" Menu "), "{screen}");
    assert!(
        screen.contains("▶ Settings -- open the settings overlay"),
        "{screen}"
    );
    assert!(
        screen.contains("  Theme -- pick the colour theme"),
        "{screen}"
    );
    assert!(
        screen.contains("  Detach -- leave the engine running"),
        "{screen}"
    );
    assert!(screen.contains("  Quit -- stop the app"), "{screen}");
    assert!(!screen.contains("Stop --"), "{screen}");
    assert!(screen.contains("Enter"), "{screen}");
    assert!(screen.contains("Confirm"), "{screen}");
    assert!(screen.contains("Esc"), "{screen}");
    assert!(screen.contains("Close"), "{screen}");
    insta::assert_snapshot!(screen);
}

/// While a build runs the menu gains its fifth row, the stop entry that
/// opens the stop dialog -- the Esc binding's scope is Running-only, so the
/// menu mirrors it.
#[test]
fn the_running_menu_renders_the_stop_build_entry() {
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("  Stop -- open the stop dialog"),
        "{screen}"
    );
    insta::assert_snapshot!(screen);
}

/// Esc closes the menu with no effect, Up/Down move the selection and Enter
/// runs the selected entry -- exactly the action its direct key binding
/// triggers, with the same side effects.
#[test]
fn the_menu_keys_run_the_entries_actions() {
    // Esc closes without acting.
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.menu_open);
    assert!(!app.overlay.open);
    assert!(!app.theme_modal.open);

    // Up clamps at the first row, Down walks the list and clamps at the last
    // (the settings overlay and the theme picker both stay closed).
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Up), Action::None);
    assert_eq!(app.menu_selected, 0);
    for expected in 1..=3 {
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.menu_selected, expected);
    }
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.menu_selected, 3);
    assert!(!app.overlay.open);
    assert!(!app.theme_modal.open);

    // Settings runs the `?` binding's action: the overlay opens, the menu
    // closes.
    let mut app = open();
    assert_eq!(confirm_entry(&mut app, 0), Action::None);
    assert!(!app.menu_open);
    assert!(app.settings_open());

    // Theme runs the `t` binding's action: the theme picker opens on the
    // active theme, the menu closes.
    let mut app = open();
    assert_eq!(confirm_entry(&mut app, 1), Action::None);
    assert!(!app.menu_open);
    assert!(app.theme_modal.open);

    // Detach runs the `d` binding's action.
    let mut app = open();
    assert_eq!(confirm_entry(&mut app, 2), Action::Detach);
    assert!(!app.menu_open);

    // Quit while idle runs the `q` binding's idle action: the engine stops
    // now and the app exits.
    let mut app = open();
    assert_eq!(confirm_entry(&mut app, 3), Action::Interrupt);
    assert!(!app.menu_open);
    assert!(app.stopping);
    assert_eq!(app.status.as_deref(), Some("Stopping the engine..."));

    // Quit while a build runs runs the `q` binding's soft stop.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    assert_eq!(confirm_entry(&mut app, 3), Action::Quit);
    assert!(!app.menu_open);
    assert!(app.stopping);
    assert_eq!(app.status.as_deref(), Some(patok_tui::SOFT_STOP_PENDING));

    // Quit while a soft stop is already pending runs the second-`q` action:
    // the current task is cancelled and the loop stops.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
    assert!(app.stopping);
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    assert_eq!(confirm_entry(&mut app, 3), Action::Interrupt);

    // Stop (running only) runs the Esc binding's action: the stop
    // dialog opens on its first choice, the menu closes.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    assert_eq!(confirm_entry(&mut app, 4), Action::None);
    assert!(!app.menu_open);
    assert!(app.stop_open);
    assert_eq!(app.stop_selected, 0);

    // Every other key is swallowed while the menu is open: nothing underneath
    // reacts, so the scroll and the focus stay put.
    let mut app = open();
    for code in [
        KeyCode::Char('a'),
        KeyCode::Char('i'),
        KeyCode::Char('d'),
        KeyCode::Char('t'),
        KeyCode::Char('q'),
        KeyCode::Char('?'),
        KeyCode::Tab,
    ] {
        assert_eq!(press(&mut app, code), Action::None, "{code:?}");
    }
    assert!(app.menu_open);
    assert!(!app.overlay.open);
    assert!(!app.theme_modal.open);
    assert!(!app.stop_open);
    assert!(!app.stopping);
}

/// j and k alias Up and Down while the menu is open -- the theme picker's
/// convention -- so the highlight moves with them, clamps at both ends
/// exactly like the arrows, Enter still runs the highlighted entry and Esc
/// still closes without acting.
#[test]
fn the_menu_moves_with_j_and_k() {
    // k clamps at the first row, the menu staying open.
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
    assert_eq!(app.menu_selected, 0);
    assert!(app.menu_open);

    // j walks the idle list and clamps at the last row: four entries, so
    // the fourth press stays on Quit.
    let mut app = open();
    for expected in [1, 2, 3, 3] {
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
        assert_eq!(app.menu_selected, expected);
    }
    assert!(!app.overlay.open);
    assert!(!app.theme_modal.open);
    assert!(!app.stop_open);

    // k moves back up one row.
    assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
    assert_eq!(app.menu_selected, 2);

    // Enter runs the j-moved entry: two j presses land on Detach.
    let mut app = open();
    for _ in 0..2 {
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    }
    assert_eq!(press(&mut app, KeyCode::Enter), Action::Detach);
    assert!(!app.menu_open);

    // Esc still closes without acting after a j press.
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.menu_open);
    assert!(!app.overlay.open);
    assert!(!app.theme_modal.open);
    assert!(!app.stop_open);

    // Running: five entries, so the j clamp reaches the stop row and k
    // walks back up.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    for _ in 0..6 {
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    }
    assert_eq!(app.menu_selected, 4);
    assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
    assert_eq!(app.menu_selected, 3);
    assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
    assert_eq!(app.menu_selected, 2);

    // Enter on the running list's stop row opens the stop dialog.
    for _ in 0..2 {
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    }
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(!app.menu_open);
    assert!(app.stop_open);
    assert_eq!(app.stop_selected, 0);
}

/// The stop entry joins only while a build runs, so the Down clamp
/// stops one row earlier while idle: no stop row hides below the fold.
#[test]
fn the_stop_build_entry_is_running_only() {
    // Idle: four entries, so five Down presses stay on the last row.
    let mut idle = open();
    for _ in 0..5 {
        assert_eq!(press(&mut idle, KeyCode::Down), Action::None);
    }
    assert_eq!(idle.menu_selected, 3);
    let screen = draw(&idle, 80, 14);
    assert!(!screen.contains("Stop --"), "{screen}");

    // Running: five entries, so the clamp reaches the stop row.
    let mut running = running_app();
    assert_eq!(press(&mut running, KeyCode::Char('m')), Action::None);
    for _ in 0..6 {
        assert_eq!(press(&mut running, KeyCode::Down), Action::None);
    }
    assert_eq!(running.menu_selected, 4);
    let screen = draw(&running, 80, 14);
    assert!(screen.contains("Stop -- open the stop dialog"), "{screen}");
    // The planning and discovery runs get no stop row either: the stop
    // entry keeps its build-only scope even though the Esc binding covers
    // these runs too (T142.1).
    let mut app = app();
    app.apply(EngineEvent::PlanningChanged { planning: true });
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    for _ in 0..5 {
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    }
    assert_eq!(app.menu_selected, 3);
}

/// The status bar's slimmed-down strip: the Enter hint leads while the engine
/// is idle, the Esc stop hint (T134.1) and the m menu chip follow while a
/// build runs, and none of the secondary chips the menu replaced survive --
/// while the direct `?`, `t`, `d`, `q` and Esc bindings keep working
/// unchanged.
#[test]
fn the_status_bar_keeps_the_enter_and_menu_hints() {
    // Idle: the Enter button and the menu button, none of the chips the menu
    // replaced.
    let idle = app();
    let screen = draw(&idle, 80, 14);
    let strip = screen.lines().last().unwrap();
    assert!(strip.contains(" [ Enter ] start "), "{strip}");
    assert!(strip.contains(" [ m ] menu"), "{strip}");
    assert!(strip.trim_end().ends_with(" [ m ] menu"), "{strip}");
    for gone in [
        " settings ",
        " theme ",
        " detach ",
        " quit ",
        " Esc ",
        " stop build ",
    ] {
        assert!(!strip.contains(gone), "{strip}");
    }

    // Running: the Enter button leaves, the Esc stop button (T134.1) joins
    // and the menu button stays.
    let running = running_app();
    let screen = draw(&running, 80, 14);
    let strip = screen.lines().last().unwrap();
    assert!(!strip.contains("Enter"), "{strip}");
    assert!(!strip.contains(" stop build "), "{strip}");
    assert!(strip.contains(" [ Esc ] stop "), "{strip}");
    assert!(strip.contains(" [ m ] menu"), "{strip}");
    assert!(strip.trim_end().ends_with(" [ m ] menu"), "{strip}");

    // The direct bindings still work: `?` opens the overlay, `t` the theme
    // picker, `d` detaches and `q` soft-stops a running build.
    let mut app = app();
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    assert!(app.settings_open());
    // A clean overlay's Esc closes it and reports the close to the driver.
    assert_eq!(press(&mut app, KeyCode::Esc), Action::CloseSettings);
    assert!(!app.settings_open());
    assert_eq!(press(&mut app, KeyCode::Char('t')), Action::None);
    assert!(app.theme_modal.open);
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.theme_modal.open);
    assert_eq!(press(&mut app, KeyCode::Char('d')), Action::Detach);

    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
    assert!(app.stopping);
    // Esc cancels the pending soft stop, the second `q` interrupts.
    assert_eq!(press(&mut app, KeyCode::Esc), Action::CancelSoftStop);
    assert!(!app.stopping);
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Interrupt);

    // Esc while a build runs still opens the stop dialog directly.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(app.stop_open);
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.stop_open);
}

/// The menu swallows every mouse event while it is open: a click on the title
/// row's close button runs the menu's Esc key, a click on the footer's
/// Confirm button runs Enter, and everything else changes nothing.
#[test]
fn the_menu_handles_its_own_mouse_events() {
    let mut app = open();
    draw(&app, 80, 14);
    // A click on the shell behind the modal changes nothing.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 2)),
        Action::None
    );
    assert!(app.menu_open);
    // The close button runs the menu's Esc key.
    let close = app.menu_close.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 1,
            close.y
        )),
        Action::None
    );
    assert!(!app.menu_open);

    // The footer's Confirm button runs Enter, so the selected entry acts: the
    // settings entry (row 0) opens the overlay.
    let mut app = open();
    draw(&app, 80, 14);
    let rects = patok_tui::footer_button_rects(
        app.menu_footer.get(),
        &[("Enter", "Confirm"), ("Esc", "Close")],
    );
    let confirm = rects[0];
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x + 2,
            confirm.y
        )),
        Action::None
    );
    assert!(!app.menu_open);
    assert!(app.settings_open());
}

/// The status bar's m menu chip is a real button (T133.1, T141.1): a left
/// click anywhere inside its rect -- which covers the full ` [ m ] menu `
/// button text -- opens the menu with the m key's open path, the same click
/// while the menu is open closes it, Esc still closes it, and clicks outside
/// the rect -- the status row left of the strip and the column past the
/// strip's right edge -- leave the menu closed.
#[test]
fn the_menu_chip_click_opens_the_menu_and_the_second_click_closes_it() {
    // Idle with one pending task, so the strip is
    // " STOPPED  sprint  [ Enter ] start  [ m ] menu" on the 80-column row:
    // the m button is the twelve columns at x = 68.
    let mut app = app();
    draw(&app, 80, 14);
    let chip = app.menu_chip.get();
    assert_eq!(chip, ratatui::layout::Rect::new(68, 13, 12, 1));

    // A click inside the chip opens the menu, the m key's action verbatim.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chip.x + 1,
            chip.y
        )),
        Action::None
    );
    assert!(app.menu_open);
    assert_eq!(app.menu_selected, 0);

    // The status bar renders behind the modal and re-records the chip, so
    // the same click closes it: the toggle.
    draw(&app, 80, 14);
    assert_eq!(app.menu_chip.get(), chip);
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chip.x + 1,
            chip.y
        )),
        Action::None
    );
    assert!(!app.menu_open);

    // Reopened by a click, the menu still closes with Esc.
    draw(&app, 80, 14);
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chip.x + 1,
            chip.y
        )),
        Action::None
    );
    assert!(app.menu_open);
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.menu_open);

    // Clicks outside the chip rect do not open the menu: the status row left
    // of the strip, and the column past the rect's right edge -- the strip's
    // last column, so the exact chip width is pinned.
    draw(&app, 80, 14);
    for (column, row) in [(0, 13), (chip.x + chip.width, chip.y)] {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
            Action::None
        );
        assert!(
            !app.menu_open,
            "a click at ({column}, {row}) must not open the menu"
        );
    }
}

/// The chip's rect is zero whenever the chip does not render -- a status
/// message replaces the whole hint zone, and a narrow strip drops the hint
/// pairs -- so clicks at the chip's usual place leave the menu closed.
#[test]
fn the_menu_chip_is_inert_while_a_status_message_shows() {
    let mut app = app();
    app.status = Some("The planner is already running.".into());
    draw(&app, 80, 14);
    assert_eq!(app.menu_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 72, 13)),
        Action::None
    );
    assert!(!app.menu_open);

    // A narrow terminal fits no hint pair (" STOPPED  sprint" only), so the
    // dropped chip leaves the zero rect too.
    app.status = None;
    draw(&app, 18, 10);
    assert_eq!(app.menu_chip.get(), ratatui::layout::Rect::default());
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 17, 9)),
        Action::None
    );
    assert!(!app.menu_open);
}

/// Each rendered row is a real button (T137.1): a left click inside a
/// row's rect moves the selection highlight to that row and immediately
/// runs it -- exactly the `m` plus Enter path, with the same action and
/// side effects per entry.
#[test]
fn a_menu_row_click_runs_that_entry() {
    // The body rect the render records, pinned: the modal's inner area
    // below the title border, one row per idle entry.
    let probe = open();
    draw(&probe, 80, 14);
    assert_eq!(
        probe.menu_body.get(),
        ratatui::layout::Rect::new(14, 4, 52, 4)
    );

    // Row 0 (Settings) runs the `?` binding's action: the overlay opens,
    // the menu closes.
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 3,
            body.y
        )),
        Action::None
    );
    assert_eq!(app.menu_selected, 0);
    assert!(!app.menu_open);
    assert!(app.settings_open());

    // Row 1 (Theme) runs the `t` binding's action: the theme picker opens,
    // the menu closes.
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 3,
            body.y + 1
        )),
        Action::None
    );
    assert_eq!(app.menu_selected, 1);
    assert!(!app.menu_open);
    assert!(app.theme_modal.open);

    // Row 2 (Detach) runs the `d` binding's action.
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 3,
            body.y + 2
        )),
        Action::Detach
    );
    assert_eq!(app.menu_selected, 2);
    assert!(!app.menu_open);

    // Row 3 (Quit, idle) runs the `q` binding's idle action: the engine
    // stops now and the app exits.
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 3,
            body.y + 3
        )),
        Action::Interrupt
    );
    assert_eq!(app.menu_selected, 3);
    assert!(!app.menu_open);
    assert!(app.stopping);
    assert_eq!(app.status.as_deref(), Some("Stopping the engine..."));
}

/// While a build runs the fifth row joins the body, and a click on it
/// runs the Esc binding's action -- the same state the key path reaches
/// with `m` plus Enter on that row.
#[test]
fn a_menu_row_click_runs_the_stop_build_entry_while_running() {
    // The running body rect, pinned: one row taller than the idle one.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    draw(&app, 80, 14);
    assert_eq!(
        app.menu_body.get(),
        ratatui::layout::Rect::new(14, 4, 52, 5)
    );
    let body = app.menu_body.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 3,
            body.y + 4
        )),
        Action::None
    );
    assert_eq!(app.menu_selected, 4);
    assert!(!app.menu_open);
    assert!(app.stop_open);
    assert_eq!(app.stop_selected, 0);

    // The key path gives the same state: `m` plus four Downs and Enter.
    let mut keyed = running_app();
    assert_eq!(press(&mut keyed, KeyCode::Char('m')), Action::None);
    assert_eq!(confirm_entry(&mut keyed, 4), Action::None);
    assert_eq!(keyed.menu_selected, 4);
    assert!(!keyed.menu_open);
    assert!(keyed.stop_open);
    assert_eq!(keyed.stop_selected, 0);
}

/// The stop entry joins only while a build runs, so the idle body ends at
/// the fourth row: a click on the line below it -- the footer's
/// non-button columns -- hits no entry and changes nothing.
#[test]
fn the_stop_build_entry_is_not_clickable_when_idle() {
    let mut app = open();
    draw(&app, 80, 14);
    assert_eq!(patok_tui::menu_entries(false).len(), 4);
    assert_eq!(app.menu_body.get().height, 4);
    // The footer line left of its buttons, where the selection hint
    // renders: outside every clickable rect.
    let footer = app.menu_footer.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            footer.x + 1,
            footer.y
        )),
        Action::None
    );
    assert!(app.menu_open);
    assert_eq!(app.menu_selected, 0);
    assert!(!app.stop_open);
    assert!(!app.stopping);
}

/// A click inside the modal but outside any entry rect does nothing: the
/// border, the title row and the shell behind the modal are not buttons,
/// and a click while the menu is closed cannot open it through the row
/// path. The key bindings are unaffected: after the swallowed clicks,
/// Down and Enter still run the entry they always did.
#[test]
fn a_click_in_the_modal_padding_does_nothing() {
    // While the menu is closed a click anywhere leaves it closed: the row
    // branch runs only while the menu is open.
    let mut app = app();
    draw(&app, 80, 14);
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 17, 5)),
        Action::None
    );
    assert!(!app.menu_open);

    // Open, then click the top border line and the title row: both outside
    // the body, the close button and the footer buttons.
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    for (column, row) in [(body.x + 3, body.y - 1), (body.x + 10, body.y - 1), (2, 2)] {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
            Action::None
        );
        assert!(app.menu_open);
        assert_eq!(app.menu_selected, 0);
        assert!(!app.overlay.open);
        assert!(!app.theme_modal.open);
        assert!(!app.stop_open);
    }

    // The key path is intact after the swallowed clicks: two Downs then
    // Enter run the entry at index 2.
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::Detach);
    assert!(!app.menu_open);
}

/// The pointer moving onto a rendered row moves only the highlight (T140.1):
/// each idle row follows the pointer without running anything, and a
/// subsequent Enter runs the hovered row, exactly like the click path.
#[test]
fn hovering_a_menu_row_moves_the_highlight_without_running_it() {
    // The body rect the render records, pinned: the modal's inner area
    // below the title border, one row per idle entry.
    let probe = open();
    draw(&probe, 80, 14);
    assert_eq!(
        probe.menu_body.get(),
        ratatui::layout::Rect::new(14, 4, 52, 4)
    );

    // Each of the four idle rows: the move lands on the row, the highlight
    // follows, and nothing runs -- the menu stays open and every modal
    // a row would open stays closed.
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    for (i, offset) in (0u16..4).enumerate() {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, body.x + 3, body.y + offset)),
            Action::None
        );
        assert_eq!(app.menu_selected, i);
        assert!(app.menu_open);
        assert!(!app.settings_open());
        assert!(!app.theme_modal.open);
        assert!(!app.stop_open);
    }

    // A subsequent Enter runs the hovered row: hover row 1 (Theme), then
    // Enter opens the theme picker, exactly the `m` plus Enter path.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 3, body.y + 1)),
        Action::None
    );
    assert_eq!(app.menu_selected, 1);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(!app.menu_open);
    assert!(app.theme_modal.open);
}

/// A pointer move inside the modal but outside every row rect -- the border
/// above the body, the footer below it, the shell behind the modal and the
/// status bar's m chip -- leaves the selection alone (T140.1).
#[test]
fn hovering_the_menu_padding_or_outside_changes_nothing() {
    let mut app = open();
    draw(&app, 80, 14);
    let body = app.menu_body.get();
    // Hover row 2 (Detach) first, so every later miss is observable.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 3, body.y + 2)),
        Action::None
    );
    assert_eq!(app.menu_selected, 2);

    let chip = app.menu_chip.get();
    for (column, row) in [
        (body.x + 3, body.y - 1),
        (body.x + 10, body.y + body.height),
        (0, 0),
        (chip.x + 1, chip.y),
    ] {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
            Action::None
        );
        assert_eq!(app.menu_selected, 2, "a move at ({column}, {row})");
        assert!(app.menu_open);
        assert!(!app.overlay.open);
        assert!(!app.theme_modal.open);
        assert!(!app.stop_open);
    }
}

/// While a build runs the fifth row joins the body, and a pointer move onto
/// it moves the highlight there without opening the stop dialog (T140.1) --
/// a subsequent Enter opens it, the same state the click and key paths
/// reach.
#[test]
fn hovering_the_stop_build_row_selects_it_without_running_it() {
    // The running body rect, pinned: one row taller than the idle one.
    let mut app = running_app();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    draw(&app, 80, 14);
    assert_eq!(
        app.menu_body.get(),
        ratatui::layout::Rect::new(14, 4, 52, 5)
    );
    let body = app.menu_body.get();

    // The hover selects the stop row but never runs it: the menu stays
    // open, the stop dialog stays closed.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 3, body.y + 4)),
        Action::None
    );
    assert_eq!(app.menu_selected, 4);
    assert!(app.menu_open);
    assert!(!app.stop_open);

    // Enter on the hovered row opens the stop dialog, the key path's state.
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(!app.menu_open);
    assert!(app.stop_open);
    assert_eq!(app.stop_selected, 0);
}

/// Pointer moves never open the closed menu or disturb any other
/// selection (T140.1): the merged view's move handling stays inert, and
/// the settings overlay keeps its own focus through moves sent while it
/// is open.
#[test]
fn pointer_moves_while_the_menu_is_closed_change_nothing() {
    // While the menu is closed a move over where the body would be and
    // over the m chip leaves it closed: the row hover runs only while
    // the menu is open.
    let mut app = app();
    draw(&app, 80, 14);
    let chip = app.menu_chip.get();
    for (column, row) in [(17, 5), (chip.x + 1, chip.y)] {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
            Action::None
        );
        assert!(!app.menu_open);
    }

    // The same moves while the settings overlay is open leave its focus
    // alone: the overlay's mouse path has no row hover, so nothing
    // underneath moves either.
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    assert!(app.settings_open());
    assert_eq!(app.overlay.focus, 0);
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.overlay.focus, 1);
    draw(&app, 80, 14);
    for (column, row) in [(17, 5), (chip.x + 1, chip.y)] {
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
            Action::None
        );
        assert_eq!(app.overlay.focus, 1);
        assert!(!app.menu_open);
    }
}
