//! The m menu modal (T128.1): the status bar's secondary key hints moved
//! behind one `m` chip, so this covers the modal's rendered rows (and the
//! stop row that joins only while a build runs), its key handling per
//! entry -- each entry runs exactly the action its direct key binding
//! triggers -- and the status bar's slimmed-down hint strip.

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
    // The planning and discovery runs get no stop row either, matching the
    // Esc binding's scope.
    let mut app = app();
    app.apply(EngineEvent::PlanningChanged { planning: true });
    app.status = None;
    assert_eq!(press(&mut app, KeyCode::Char('m')), Action::None);
    for _ in 0..5 {
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    }
    assert_eq!(app.menu_selected, 3);
}

/// The status bar's slimmed-down strip: the Enter hint leads while the engine
/// is idle, the m menu chip follows, and none of the secondary chips the menu
/// replaced survive -- while the direct `?`, `t`, `d`, `q` and Esc bindings
/// keep working unchanged.
#[test]
fn the_status_bar_keeps_the_enter_and_menu_hints() {
    // Idle: both hints, none of the chips the menu replaced.
    let idle = app();
    let screen = draw(&idle, 80, 14);
    let strip = screen.lines().last().unwrap();
    assert!(strip.contains(" Enter "), "{strip}");
    assert!(strip.contains(" start "), "{strip}");
    assert!(strip.contains(" m "), "{strip}");
    assert!(strip.trim_end().ends_with(" menu"), "{strip}");
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

    // Running: the Enter hint leaves, the menu chip stays.
    let running = running_app();
    let screen = draw(&running, 80, 14);
    let strip = screen.lines().last().unwrap();
    assert!(!strip.contains(" Enter "), "{strip}");
    assert!(!strip.contains(" stop build "), "{strip}");
    assert!(strip.contains(" m "), "{strip}");
    assert!(strip.trim_end().ends_with(" menu"), "{strip}");

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
