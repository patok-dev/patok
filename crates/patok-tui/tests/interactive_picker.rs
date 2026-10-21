//! T2.2: the interactive provider picker -- opening with each provider's
//! availability, the keyboard and mouse paths, the running-build warning and
//! the rendering. Every test sets the picker's search path to a tempdir, so
//! no result depends on the host's `PATH`.

mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;

use crossterm::event::{KeyCode, MouseButton, MouseEventKind};
use patok_config::ProviderKind;
use patok_core::event::{EngineEvent, Phase};
use patok_tui::{Action, App, Modal, interactive_picker_area};
use tempfile::TempDir;

use support::{draw, mouse, press};

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";
const WARNING: &str = "⚠ A build is running; the session shares its files.";

fn app() -> App {
    support::app(TASKS, "claude", BTreeMap::new(), Some(true))
}

fn running_app() -> App {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app
}

/// Writes executable stubs named `names` into a fresh tempdir and points the
/// picker's search path at it; keep the returned dir alive for the test.
fn with_programs(app: &mut App, names: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    add_programs(app, &dir, names);
    dir
}

fn add_programs(app: &mut App, dir: &TempDir, names: &[&str]) {
    for name in names {
        let file = dir.path().join(name);
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, PermissionsExt::from_mode(0o755)).unwrap();
    }
    let path: OsString = std::env::join_paths([dir.path()]).unwrap();
    app.interactive.search_path = Some(path);
}

fn open_with(app: &mut App, names: &[&str]) -> TempDir {
    let dir = with_programs(app, names);
    press(app, KeyCode::Char('I'));
    dir
}

fn availability(app: &App) -> Vec<(ProviderKind, bool)> {
    app.interactive.providers.clone()
}

fn left_down(app: &mut App, column: u16, row: u16) -> Action {
    app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row))
}

#[test]
fn i_opens_the_picker_listing_every_provider() {
    let mut app = app();
    let _dir = open_with(&mut app, &["claude", "codex", "vibe"]);
    assert_eq!(app.open_modals(), vec![Modal::InteractivePicker]);
    assert_eq!(
        availability(&app),
        vec![
            (ProviderKind::Claude, true),
            (ProviderKind::Codex, true),
            (ProviderKind::Mistral, true),
        ]
    );
    assert_eq!(app.interactive.selected, 0);
}

#[test]
fn the_selection_starts_on_the_first_available_provider() {
    let mut app = app();
    let _dir = open_with(&mut app, &["codex"]);
    assert_eq!(
        availability(&app),
        vec![
            (ProviderKind::Claude, false),
            (ProviderKind::Codex, true),
            (ProviderKind::Mistral, false),
        ]
    );
    assert_eq!(app.interactive.selected, 1);

    let mut app = self::app();
    let _dir = open_with(&mut app, &[]);
    assert_eq!(app.interactive.selected, 0);
    assert!(availability(&app).iter().all(|&(_, available)| !available));
    // Enter on a disabled row is swallowed: the picker stays, no status.
    press(&mut app, KeyCode::Enter);
    assert!(app.picker_open());
    assert_eq!(app.status, None);
}

#[test]
fn the_keys_move_and_clamp_over_every_row() {
    let mut app = app();
    let _dir = open_with(&mut app, &["codex"]);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.interactive.selected, 0);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.interactive.selected, 0);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.interactive.selected, 2);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.interactive.selected, 2);
    press(&mut app, KeyCode::Char('x'));
    assert!(app.picker_open());
}

#[test]
fn enter_on_an_available_row_closes_and_esc_closes_silently() {
    let mut app = app();
    let _dir = open_with(&mut app, &["claude", "codex"]);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // The empty stub runs as an (immediately exiting) shell script, so the
    // terminal modal opens in the picker's place.
    assert_eq!(app.open_modals(), vec![Modal::Terminal]);
    assert_eq!(app.status, None);

    let mut app = self::app();
    let _dir = open_with(&mut app, &["claude"]);
    press(&mut app, KeyCode::Esc);
    assert!(!app.picker_open());
    assert_eq!(app.status, None);
}

#[test]
fn the_menu_entry_opens_the_picker() {
    let mut app = app();
    let _dir = with_programs(&mut app, &["claude"]);
    press(&mut app, KeyCode::Char('m'));
    // `I` while the menu is open is swallowed by the menu.
    press(&mut app, KeyCode::Char('I'));
    assert_eq!(app.open_modals(), vec![Modal::Menu]);
    // Interactive is the first row, the default selection (T6.1).
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.open_modals(), vec![Modal::InteractivePicker]);
}

#[test]
fn the_status_bar_has_no_interactive_button_but_i_opens_the_picker() {
    for make in [app as fn() -> App, running_app as fn() -> App] {
        let mut app = make();
        let _dir = with_programs(&mut app, &["claude"]);
        for width in [80, 130] {
            let frame = draw(&app, width, 24);
            let bottom = frame.lines().last().unwrap();
            assert!(!bottom.contains("interactive"), "{bottom}");
            assert!(!bottom.contains("[ I ]"), "{bottom}");
            assert!(bottom.contains("[ m ] menu"), "{bottom}");
        }
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(app.open_modals(), vec![Modal::InteractivePicker]);
    }
}

#[test]
fn reopening_reads_the_path_again() {
    let mut app = app();
    let dir = open_with(&mut app, &[]);
    assert!(!availability(&app)[0].1);
    press(&mut app, KeyCode::Esc);
    add_programs(&mut app, &dir, &["claude"]);
    press(&mut app, KeyCode::Char('I'));
    assert!(availability(&app)[0].1);
}

#[test]
fn idle_rendering_shows_the_rows_without_a_warning() {
    let mut app = app();
    let _dir = open_with(&mut app, &["claude", "codex"]);
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("▶ Claude -- claude"), "{screen}");
    assert!(screen.contains("Codex -- codex"), "{screen}");
    assert!(
        screen.contains("Mistral -- `vibe` not found on PATH"),
        "{screen}"
    );
    assert!(!screen.contains(WARNING), "{screen}");
    insta::assert_snapshot!("idle_mixed_availability", screen);
}

#[test]
fn a_running_build_adds_the_warning_line() {
    let mut app = running_app();
    let _dir = open_with(&mut app, &["claude", "codex", "vibe"]);
    let screen = draw(&app, 80, 24);
    assert!(screen.contains(WARNING), "{screen}");
    let screen_rect = ratatui::layout::Rect::new(0, 0, 80, 24);
    assert_eq!(
        interactive_picker_area(screen_rect, true).height,
        interactive_picker_area(screen_rect, false).height + 2
    );
    insta::assert_snapshot!("running_warning", screen);
}

#[test]
fn the_mouse_hovers_clicks_and_closes() {
    // Hover moves only the selection.
    let mut app = app();
    let _dir = open_with(&mut app, &["claude", "codex"]);
    draw(&app, 80, 24);
    let body = app.interactive.body.get();
    app.on_mouse(mouse(MouseEventKind::Moved, body.x + 3, body.y + 2));
    assert_eq!(app.interactive.selected, 2);
    assert!(app.picker_open());

    // A click on a disabled row keeps the picker open.
    left_down(&mut app, body.x + 3, body.y + 2);
    assert!(app.picker_open());
    assert_eq!(app.status, None);

    // A click on an available row opens its terminal in the picker's place.
    left_down(&mut app, body.x + 3, body.y + 1);
    assert_eq!(app.open_modals(), vec![Modal::Terminal]);
    assert_eq!(app.status, None);

    // The warning line and the border are swallowed.
    let mut app = running_app();
    let _dir = open_with(&mut app, &["claude"]);
    draw(&app, 80, 24);
    let body = app.interactive.body.get();
    left_down(&mut app, body.x + 3, body.y + 4);
    let close = app.interactive.close.get();
    left_down(&mut app, close.x.saturating_sub(3), close.y);
    assert!(app.picker_open());
    assert_eq!(app.status, None);
}

#[test]
fn the_close_button_and_the_footer_buttons_close_or_open() {
    let buttons = [("Enter", "Open"), ("Esc", "Close")];

    let mut app = app();
    let _dir = open_with(&mut app, &["claude"]);
    draw(&app, 80, 24);
    let close = app.interactive.close.get();
    left_down(&mut app, close.x + 1, close.y);
    assert!(!app.picker_open());

    let mut app = self::app();
    let _dir = open_with(&mut app, &["claude"]);
    draw(&app, 80, 24);
    let rects = patok_tui::footer_button_rects(app.interactive.footer.get(), &buttons);
    left_down(&mut app, rects[1].x + 2, rects[1].y);
    assert!(!app.picker_open());
    assert_eq!(app.status, None);

    let mut app = self::app();
    let _dir = open_with(&mut app, &["claude"]);
    draw(&app, 80, 24);
    let rects = patok_tui::footer_button_rects(app.interactive.footer.get(), &buttons);
    left_down(&mut app, rects[0].x + 2, rects[0].y);
    assert_eq!(app.open_modals(), vec![Modal::Terminal]);
    assert_eq!(app.status, None);
}
