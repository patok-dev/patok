//! The theme picker modal (T43.1): key and mouse handling through `App::on_key`
//! and `App::on_mouse`, snapshots of the modal and of the preview-recoloured
//! shell, and the persistence the Enter and click paths go through. The
//! picker's list is two foldable groups, Dark then Light (T116.1), mirroring
//! the settings overlay's grouped entries.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use patok_core::config::{ConfigFiles, Theme as ThemeKey};
use patok_core::event::{EngineEvent, Phase, Snapshot};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_tui::{
    Action, App, FrameFocus, ShellSettings, Theme, render, theme_modal_groups, theme_row_at,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

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

fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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

/// Renders into an 80x24 TestBackend and returns the buffer, for cell-level
/// symbol and style assertions.
fn draw_buffer(app: &App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal.backend().buffer().clone()
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
    assert_eq!(press(&mut app, KeyCode::Char('t')), Action::None);
    assert!(app.theme_modal.open);
    app
}

fn theme_of(key: ThemeKey) -> Theme {
    Theme::resolve(key, Some(true))
}

/// The visible index of a theme name in the picker's two-group list (T114.1's
/// dark-then-light classification, T116.1's foldable Dark/Light groups): one
/// past the Dark header inside the dark block, one past both headers inside
/// the light block.
fn row_of(name: &str) -> usize {
    let groups = theme_modal_groups();
    groups[0]
        .1
        .iter()
        .position(|key| *key == name)
        .map(|i| 1 + i)
        .or_else(|| {
            groups[1]
                .1
                .iter()
                .position(|key| *key == name)
                .map(|i| 1 + groups[0].1.len() + 1 + i)
        })
        .unwrap_or_else(|| panic!("`{name}` is not a built-in theme"))
}

/// Moves the selection down onto `name`'s visible entry, one Down per entry.
fn press_down_to(app: &mut App, name: &str) {
    let target = row_of(name);
    while app.theme_modal.selected < target {
        assert_eq!(press(app, KeyCode::Down), Action::None);
    }
    assert_eq!(app.theme_modal.selected, target);
}

#[test]
fn t_opens_the_picker_from_every_app_state() {
    // Idle: the selection starts on the active theme, both groups expanded,
    // nothing previewed.
    let idle = open();
    assert_eq!(idle.theme_modal.selected, row_of("dark"));
    assert_eq!(idle.theme_modal.expanded, [true, true]);
    assert_eq!(idle.theme_modal.preview, None);
    assert_eq!(idle.theme(), theme_of(ThemeKey::Dark));

    // A running build.
    let mut running = app();
    running.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    running.apply(EngineEvent::TaskStarted {
        id: "T1.1".into(),
        description: "scaffold the workspace".into(),
    });
    assert_eq!(press(&mut running, KeyCode::Char('t')), Action::None);
    assert!(running.theme_modal.open);

    // Planning.
    let mut planning = app();
    planning.apply(EngineEvent::PlanningChanged { planning: true });
    assert_eq!(press(&mut planning, KeyCode::Char('t')), Action::None);
    assert!(planning.theme_modal.open);

    // Discovery.
    let mut discovering = app();
    discovering.apply(EngineEvent::DiscoveryChanged { discovering: true });
    assert_eq!(press(&mut discovering, KeyCode::Char('t')), Action::None);
    assert!(discovering.theme_modal.open);

    // Even while a soft stop is pending: the picker sits above it, and the
    // stop's keys work again once the picker closes.
    let mut stopping = app();
    stopping.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    assert_eq!(press(&mut stopping, KeyCode::Char('q')), Action::Quit);
    assert!(stopping.stopping);
    assert_eq!(press(&mut stopping, KeyCode::Char('t')), Action::None);
    assert!(stopping.theme_modal.open);
    assert!(stopping.stopping, "the pending stop survives underneath");
    // The picker swallows the stop's keys while it is open...
    assert_eq!(press(&mut stopping, KeyCode::Char('q')), Action::None);
    assert_eq!(press(&mut stopping, KeyCode::Esc), Action::None);
    assert!(stopping.stopping);
    assert!(!stopping.theme_modal.open);
    // ...and they reach the shell again after the picker closes.
    assert_eq!(press(&mut stopping, KeyCode::Char('q')), Action::Interrupt);
}

#[test]
fn t_stays_with_the_modal_that_has_the_keyboard() {
    // The add-task dialog types the t.
    let mut dialog = app();
    press(&mut dialog, KeyCode::Char('a'));
    assert!(dialog.dialog_open);
    assert_eq!(press(&mut dialog, KeyCode::Char('t')), Action::None);
    assert!(!dialog.theme_modal.open);
    assert_eq!(dialog.dialog_text, "t");

    // The settings overlay ignores it.
    let mut overlay = app();
    press(&mut overlay, KeyCode::Char('?'));
    assert!(overlay.settings_open());
    assert_eq!(press(&mut overlay, KeyCode::Char('t')), Action::None);
    assert!(!overlay.theme_modal.open);

    // The stop dialog swallows it.
    let mut stop = app();
    stop.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    press(&mut stop, KeyCode::Esc);
    assert!(stop.stop_open);
    assert_eq!(press(&mut stop, KeyCode::Char('t')), Action::None);
    assert!(!stop.theme_modal.open);
}

#[test]
fn moving_the_selection_previews_without_persisting() {
    let mut app = open();
    assert_eq!(app.tui.theme, ThemeKey::Dark);
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.theme_modal.selected, 2);
    assert_eq!(app.theme(), theme_of(ThemeKey::AtomOneDark));
    // j moves the same way Down does: within the dark block.
    assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    assert_eq!(app.theme(), theme_of(ThemeKey::TokyoNightDark));
    // k and Up move back up.
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.theme(), theme_of(ThemeKey::AtomOneDark));
    press(&mut app, KeyCode::Up);
    assert_eq!(app.theme(), theme_of(ThemeKey::Dark));
    // A header takes the highlight but never previews: crossing the Dark
    // header onto the Light header leaves the preview alone, and so does
    // coming back.
    press(&mut app, KeyCode::Up);
    assert_eq!(app.theme_modal.selected, 0, "the Dark header");
    assert_eq!(app.theme(), theme_of(ThemeKey::Dark));
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.theme_modal.selected, 1, "the active theme's row again");
    // The committed setting never moved: browsing persists nothing.
    assert_eq!(app.tui.theme, ThemeKey::Dark);
}

#[test]
fn hovering_a_row_previews_it_and_hovering_outside_changes_nothing() {
    let mut app = open();
    draw(&app, 80, 24);
    let body = app.theme_modal.area.get();
    // The pointer moving onto a row previews that row (T40.1's mouse capture):
    // with both groups expanded, body row 6 is the gruvbox_dark entry.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 4, body.y + 6)),
        Action::None
    );
    assert_eq!(app.theme_modal.selected, 6);
    assert_eq!(app.theme(), theme_of(ThemeKey::GruvboxDark));
    // On the footer -- outside the rows -- nothing changes.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Moved,
            body.x + 4,
            body.y + body.height
        )),
        Action::None
    );
    assert_eq!(app.theme_modal.selected, 6);
    // Outside the modal entirely: swallowed too.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, 0, 0)),
        Action::None
    );
    assert_eq!(app.tui.theme, ThemeKey::Dark);
}

#[test]
fn keys_and_mouse_are_swallowed_while_the_picker_is_open() {
    let mut app = open();
    for key in [
        KeyCode::Char('q'),
        KeyCode::Char('s'),
        KeyCode::Char('a'),
        KeyCode::Tab,
        KeyCode::Char('?'),
        KeyCode::Char('d'),
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::End,
    ] {
        assert_eq!(press(&mut app, key), Action::None);
    }
    assert!(!app.stopping);
    assert!(!app.dialog_open);
    assert!(!app.stop_open);
    assert!(!app.settings_open());
    assert_eq!(app.focus, FrameFocus::Tasks);
    assert_eq!(app.scroll, 0);
    assert_eq!(app.task_scroll, 0);

    // Mouse: wheel steps and clicks outside the rows change nothing either.
    draw(&app, 80, 24);
    let body = app.theme_modal.area.get();
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::ScrollUp, body.x + 1, body.y + 1)),
        Action::None
    );
    // A click on the modal's border -- over the output frame behind it --
    // focuses no frame.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 10,
            body.y - 2
        )),
        Action::None
    );
    assert_eq!(app.focus, FrameFocus::Tasks);
    assert_eq!(app.scroll, 0);
}

#[test]
fn esc_restores_the_theme_the_picker_opened_with() {
    let mut previewed = open();
    press_down_to(&mut previewed, "catppuccin_latte");
    assert_eq!(previewed.theme(), theme_of(ThemeKey::CatppuccinLatte));
    assert_eq!(press(&mut previewed, KeyCode::Esc), Action::None);
    assert!(!previewed.theme_modal.open);
    assert_eq!(previewed.theme_modal.preview, None);
    assert_eq!(previewed.theme(), theme_of(ThemeKey::Dark));
    assert_eq!(previewed.tui.theme, ThemeKey::Dark);

    // The same exactness from any committed theme, not just the default. The
    // selection starts on the last row here, so it moves up.
    let mut other = app();
    other.tui.theme = ThemeKey::GruvboxLight;
    press(&mut other, KeyCode::Char('t'));
    assert_eq!(other.theme_modal.selected, row_of("gruvbox_light"));
    for _ in 0..3 {
        press(&mut other, KeyCode::Up);
    }
    assert_ne!(other.theme(), theme_of(ThemeKey::GruvboxLight));
    press(&mut other, KeyCode::Esc);
    assert_eq!(other.theme(), theme_of(ThemeKey::GruvboxLight));
    assert_eq!(other.tui.theme, ThemeKey::GruvboxLight);
}

#[test]
fn enter_keeps_the_previewed_theme_and_hands_it_to_the_driver() {
    // Enter without browsing keeps the active theme.
    let mut app = open();
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::SaveTheme(ThemeKey::Dark)
    );

    // Enter after browsing keeps the previewed one.
    let mut app = open();
    press_down_to(&mut app, "gruvbox_dark");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::SaveTheme(ThemeKey::GruvboxDark)
    );
    assert!(!app.theme_modal.open);
    assert_eq!(app.tui.theme, ThemeKey::GruvboxDark);
    assert_eq!(app.theme(), theme_of(ThemeKey::GruvboxDark));
}

#[test]
fn clicking_a_row_previews_then_saves_like_enter() {
    // A click on the catppuccin_mocha entry (visible row 4, one past the Dark
    // header) previews and commits it, exactly like hovering plus Enter.
    let mut app = open();
    draw(&app, 80, 24);
    let body = app.theme_modal.area.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 4,
            body.y + 4
        )),
        Action::SaveTheme(ThemeKey::CatppuccinMocha)
    );
    assert!(!app.theme_modal.open);
    assert_eq!(app.theme_modal.selected, 4);
    assert_eq!(app.tui.theme, ThemeKey::CatppuccinMocha);
    assert_eq!(app.theme(), theme_of(ThemeKey::CatppuccinMocha));

    // A click outside the rows changes nothing.
    let mut app = open();
    draw(&app, 80, 24);
    let body = app.theme_modal.area.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x,
            body.y + body.height
        )),
        Action::None
    );
    assert!(app.theme_modal.open);
    assert_eq!(app.tui.theme, ThemeKey::Dark);
}

/// Folding a header hides its entries and unfolding brings them back (T116.1):
/// Enter on the Dark header removes every dark name from the screen and keeps
/// the Light group intact; Enter again restores them. Space toggles the same
/// way, and the Light header behaves identically.
#[test]
fn toggling_a_header_hides_and_shows_its_entries() {
    let mut app = open();
    press(&mut app, KeyCode::Up);
    assert_eq!(app.theme_modal.selected, 0, "the Dark header");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    assert_eq!(
        app.theme_modal.selected, 0,
        "the selection stays on the header"
    );
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("▸ Dark"), "{screen}");
    assert!(screen.contains("▾ Light"), "{screen}");
    for name in theme_modal_groups()[0].1.clone() {
        assert!(!screen.contains(name), "dark `{name}` stays visible");
    }
    for name in &theme_modal_groups()[1].1 {
        assert!(screen.contains(name), "light `{name}` stays visible");
    }
    // Enter again unfolds: every dark name is back.
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, true]);
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("▾ Dark"), "{screen}");
    for name in &theme_modal_groups()[0].1 {
        assert!(screen.contains(name), "dark `{name}` is back");
    }

    // Space toggles the same way, on the Light header.
    let mut app = open();
    press_down_to(&mut app, "gruvbox_dark");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.theme_modal.selected, 7, "the Light header");
    assert_eq!(press(&mut app, KeyCode::Char(' ')), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, false]);
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("▸ Light"), "{screen}");
    for name in &theme_modal_groups()[1].1 {
        assert!(!screen.contains(name), "light `{name}` stays visible");
    }
    for name in &theme_modal_groups()[0].1 {
        assert!(screen.contains(name), "dark `{name}` is back");
    }
    assert_eq!(press(&mut app, KeyCode::Char(' ')), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, true]);
}

/// Navigation walks the visible list only (T116.1): with the Dark group
/// folded, Down from the Dark header lands on the Light header and then the
/// first light theme -- never on a hidden dark row -- and headers never
/// preview. With both groups folded, movement runs only between the two
/// headers and clamps at both ends.
#[test]
fn collapsed_entries_are_skipped_during_navigation() {
    let mut app = open();
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    // Down from the Dark header: the Light header, preview untouched.
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.theme_modal.selected, 1);
    assert_eq!(app.theme_modal.preview, None);
    assert_eq!(app.theme(), theme_of(ThemeKey::Dark));
    // The next Down is the first light row -- never a hidden dark one.
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));
    assert_eq!(app.theme(), theme_of(ThemeKey::AtomOneLight));
    // Up from a light row lands on the Light header, not a dark row.
    assert_eq!(press(&mut app, KeyCode::Up), Action::None);
    assert_eq!(app.theme_modal.selected, 1);
    assert_eq!(app.theme_modal.preview, Some(ThemeKey::AtomOneLight));

    // Both groups folded: only the two headers, clamped at both ends.
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

/// The header fold keys mirror the settings overlay's (T116.1): Left folds an
/// expanded header, Right unfolds a folded one, the reverse directions are
/// no-ops, h/l alias the arrows -- and on a theme row all four are swallowed.
#[test]
fn left_folds_a_header_and_right_unfolds_it() {
    let mut app = open();
    press(&mut app, KeyCode::Up);
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    // Left on a folded header is a no-op; Right unfolds it.
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, true]);
    // Right on an expanded header is a no-op.
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, true]);
    // h/l alias Left/Right.
    assert_eq!(press(&mut app, KeyCode::Char('h')), Action::None);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    assert_eq!(press(&mut app, KeyCode::Char('l')), Action::None);
    assert_eq!(app.theme_modal.expanded, [true, true]);

    // On a theme row all four keys are swallowed: nothing folds, previews,
    // commits or closes.
    press(&mut app, KeyCode::Down);
    assert_eq!(app.theme_modal.preview, Some(ThemeKey::Dark));
    for key in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char('h'),
        KeyCode::Char('l'),
    ] {
        assert_eq!(press(&mut app, key), Action::None);
        assert_eq!(app.theme_modal.expanded, [true, true]);
        assert!(app.theme_modal.open);
        assert_eq!(app.tui.theme, ThemeKey::Dark);
        assert_eq!(app.theme_modal.preview, Some(ThemeKey::Dark));
    }
}

/// A header is a fold toggle, never a commit (T116.1): Enter and Space on
/// either header keep the modal open and the committed theme and preview
/// exactly what they were.
#[test]
fn a_header_never_commits() {
    let mut app = open();
    press(&mut app, KeyCode::Up);
    for key in [KeyCode::Enter, KeyCode::Char(' ')] {
        let preview = app.theme_modal.preview;
        assert_eq!(press(&mut app, key), Action::None);
        assert!(app.theme_modal.open);
        assert_eq!(app.tui.theme, ThemeKey::Dark);
        assert_eq!(app.theme_modal.preview, preview);
    }
    assert_eq!(app.theme_modal.expanded, [true, true], "toggled back");
    // The same on the Light header.
    while app.theme_modal.selected < 7 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.theme_modal.selected, 7);
    for key in [KeyCode::Enter, KeyCode::Char(' ')] {
        let preview = app.theme_modal.preview;
        assert_eq!(press(&mut app, key), Action::None);
        assert!(app.theme_modal.open);
        assert_eq!(app.tui.theme, ThemeKey::Dark);
        assert_eq!(app.theme_modal.preview, preview);
    }
    assert_eq!(app.theme_modal.expanded, [true, true], "toggled back");
}

/// The pointer on a header moves the highlight without previewing, and a
/// click on it toggles the fold without committing (T116.1).
#[test]
fn hovering_and_clicking_a_header_toggle_without_previewing_or_committing() {
    let mut app = open();
    draw(&app, 80, 24);
    let body = app.theme_modal.area.get();
    // Hovering the Dark header moves the selection there and previews nothing.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y)),
        Action::None
    );
    assert_eq!(app.theme_modal.selected, 0);
    assert_eq!(app.theme_modal.preview, None);
    // Clicking it folds the group and commits nothing.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 2,
            body.y
        )),
        Action::None
    );
    assert_eq!(app.theme_modal.expanded, [false, true]);
    assert!(app.theme_modal.open);
    assert_eq!(app.tui.theme, ThemeKey::Dark);
    // Hovering the folded header still previews nothing; clicking unfolds.
    assert_eq!(
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y)),
        Action::None
    );
    assert_eq!(app.theme_modal.preview, None);
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 2,
            body.y
        )),
        Action::None
    );
    assert_eq!(app.theme_modal.expanded, [true, true]);
    // The Light header behaves identically.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            body.x + 2,
            body.y + 7
        )),
        Action::None
    );
    assert_eq!(app.theme_modal.expanded, [true, false]);
    assert!(app.theme_modal.open);
    assert_eq!(app.tui.theme, ThemeKey::Dark);
}

/// The picker's top-right close button (T66.1): the shared ` [ x ] ` in the
/// button accent of the theme the shell wears -- the previewed one while
/// browsing -- and a click on its rectangle runs the picker's Esc key: the
/// preview is dropped, the opening theme restored, nothing persisted.
#[test]
fn the_picker_shows_a_close_button_top_right() {
    use ratatui::layout::Rect;

    let modal = patok_tui::theme_area(Rect::new(0, 0, 80, 24));
    let close = patok_tui::close_button_rect(modal);

    // Open on the default theme: the button wears that theme's accent.
    let app = open();
    let buffer = draw_buffer(&app);
    assert_eq!(app.theme_modal.close.get(), close);
    let line: String = (close.x..close.right())
        .map(|x| buffer[(x, close.y)].symbol())
        .collect();
    assert_eq!(line, patok_tui::button_text("x", ""));
    for i in 0..close.width {
        assert_eq!(
            buffer[(close.x + i, close.y)].style().fg,
            Some(theme_of(ThemeKey::Dark).highlighted_text),
            "button cell {i} wears the accent"
        );
    }

    // While previewing another theme the whole shell -- the button included --
    // recolours to it.
    let mut previewed = open();
    press_down_to(&mut previewed, "tokyo_night_dark");
    let buffer = draw_buffer(&previewed);
    for i in 0..close.width {
        assert_eq!(
            buffer[(close.x + i, close.y)].style().fg,
            Some(theme_of(ThemeKey::TokyoNightDark).highlighted_text),
            "button cell {i} wears the previewed theme's accent"
        );
    }
    assert_ne!(
        theme_of(ThemeKey::Dark).highlighted_text,
        theme_of(ThemeKey::TokyoNightDark).highlighted_text
    );
}

#[test]
fn clicking_the_close_button_restores_the_opening_theme_like_esc() {
    let mut app = open();
    press_down_to(&mut app, "catppuccin_latte");
    assert_eq!(app.theme(), theme_of(ThemeKey::CatppuccinLatte));
    draw(&app, 80, 24);
    let close = app.theme_modal.close.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 3,
            close.y
        )),
        Action::None
    );
    assert!(!app.theme_modal.open);
    assert_eq!(app.theme_modal.preview, None);
    assert_eq!(app.tui.theme, ThemeKey::Dark, "nothing is persisted");
    assert_eq!(app.theme(), theme_of(ThemeKey::Dark));

    // Esc from the same browsed state takes the identical path.
    let mut esc = open();
    press_down_to(&mut esc, "catppuccin_latte");
    assert_eq!(press(&mut esc, KeyCode::Esc), Action::None);
    assert!(!esc.theme_modal.open);
    assert_eq!(esc.theme_modal.preview, None);
    assert_eq!(esc.theme(), theme_of(ThemeKey::Dark));

    // A click on the picker's border outside the button changes nothing.
    let mut app = open();
    draw(&app, 80, 24);
    let close = app.theme_modal.close.get();
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x - 2,
            close.y
        )),
        Action::None
    );
    assert!(app.theme_modal.open);
    assert_eq!(app.tui.theme, ThemeKey::Dark);
}

#[test]
fn a_failed_save_restores_the_theme_the_picker_opened_with() {
    let mut app = open();
    press(&mut app, KeyCode::Down);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::SaveTheme(ThemeKey::AtomOneDark)
    );
    app.on_theme_save_failed("could not write config.local.toml".into());
    assert_eq!(app.tui.theme, ThemeKey::Dark);
    assert_eq!(app.theme(), theme_of(ThemeKey::Dark));
    assert_eq!(
        app.status.as_deref(),
        Some("could not write config.local.toml")
    );
}

#[test]
fn theme_row_at_maps_positions_onto_rows() {
    let body = ratatui::layout::Rect::new(16, 7, 46, 13);
    let at = |x: u16, y: u16| theme_row_at(ratatui::layout::Position::new(x, y), body, 13);
    assert_eq!(at(16, 7), Some(0));
    assert_eq!(at(40, 12), Some(5));
    assert_eq!(at(61, 19), Some(12));
    // Past the last visible row (the footer), on the border and outside the
    // modal; and a body row past a shortened (folded) visible list.
    assert_eq!(at(16, 20), None);
    assert_eq!(at(62, 7), None);
    assert_eq!(at(0, 0), None);
    assert_eq!(
        theme_row_at(ratatui::layout::Position::new(16, 14), body, 7),
        None
    );
    assert_eq!(
        theme_row_at(ratatui::layout::Position::new(16, 14), body, 13),
        Some(7)
    );
}

/// The persistence the Enter and click paths reach, against the real
/// user-local layer: the same read-modify-write the settings overlay's theme
/// row takes (T13.1), so what a fresh load resolves is what the overlay and
/// the config reload show afterwards.
#[test]
fn the_save_path_persists_the_theme_a_fresh_load_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let files = ConfigFiles {
        user_global: None,
        user_local: Some(dir.path().join("config.local.toml")),
    };
    let (mut shell, warnings) = ShellSettings::with_files(files.clone(), dir.path(), None);
    assert!(warnings.is_empty());

    // Browsing alone writes nothing: the preview never touches a config file.
    let mut app = open();
    press_down_to(&mut app, "gruvbox_dark");
    assert!(!dir.path().join("config.local.toml").exists());

    // The driver's save path (what Enter and a click reach).
    patok_tui::save_theme(&mut shell, &mut app, ThemeKey::GruvboxDark);
    assert_eq!(app.status.as_deref(), Some("Theme saved."));
    let text = std::fs::read_to_string(dir.path().join("config.local.toml")).unwrap();
    assert!(text.contains("theme = \"gruvbox_dark\""), "{text}");

    // A fresh load of the same files resolves it: what a restart, the settings
    // overlay's theme row and the config reload all show.
    let (fresh, warnings) = ShellSettings::with_files(files, dir.path(), None);
    assert!(warnings.is_empty());
    assert_eq!(fresh.settings().theme, ThemeKey::GruvboxDark);

    // A hand-edited theme key keeps working through the reload.
    std::fs::write(
        dir.path().join("config.local.toml"),
        "[tui]\ntheme = \"catppuccin_latte\"\n",
    )
    .unwrap();
    let reload = shell.reload_if_changed().expect("the change was picked up");
    assert!(reload.is_empty(), "{reload:?}");
    assert_eq!(shell.settings().theme, ThemeKey::CatppuccinLatte);
}

/// The picker's screen: the two group headers, the eleven built-ins nested
/// under them, the selection marker and the footer hints.
#[test]
fn the_picker_with_the_selection_on_a_dark_theme() {
    let mut app = open();
    press(&mut app, KeyCode::Down);
    assert_eq!(app.theme_modal.selected, 2);
    insta::assert_snapshot!(draw(&app, 80, 24));
}

#[test]
fn the_picker_with_the_selection_on_a_light_theme() {
    let mut app = open();
    press_down_to(&mut app, "catppuccin_latte");
    assert_eq!(app.theme_modal.selected, row_of("catppuccin_latte"));
    insta::assert_snapshot!(draw(&app, 80, 24));
}

/// The picker's screen with a folded group (T116.1): the Dark header folded,
/// its entries hidden, the Light group intact below.
#[test]
fn the_picker_with_the_dark_group_folded() {
    let mut app = open();
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.theme_modal.expanded, [false, true]);
    assert_eq!(app.theme_modal.selected, 0);
    insta::assert_snapshot!(draw(&app, 80, 24));
}

/// The picker's rows render as a normal list (T115.1): every entry -- headers
/// included -- wears the active theme's classes: the active `background`, the
/// names in `normal_text`, the selected row's name in `highlighted_text` --
/// never the entry's own palette.
#[test]
fn the_picker_rows_wear_the_active_theme_not_their_own() {
    use ratatui::style::Modifier;

    let app = open();
    let buffer = draw_buffer(&app);
    let body = app.theme_modal.area.get();
    let dark = theme_of(ThemeKey::Dark);
    let selected = row_of("dark");
    // The visible entries: the Dark header, the dark names, the Light header,
    // the light names (T116.1).
    let groups = theme_modal_groups();
    let mut rows: Vec<(usize, &str)> = Vec::new();
    rows.extend(
        groups[0]
            .1
            .iter()
            .enumerate()
            .map(|(i, name)| (1 + i, *name)),
    );
    rows.extend(
        groups[1]
            .1
            .iter()
            .enumerate()
            .map(|(i, name)| (1 + groups[0].1.len() + 1 + i, *name)),
    );
    for (index, _name) in rows {
        let row = index as u16;
        assert_eq!(
            buffer[(body.x, body.y + row)].bg,
            dark.background,
            "row {index} sits on the active background"
        );
        // The name sits indented past the focus marker under its header.
        let expected = if index == selected {
            dark.highlighted_text
        } else {
            dark.normal_text
        };
        assert_eq!(
            buffer[(body.x + 6, body.y + row)].style().fg,
            Some(expected),
            "row {index}'s name wears the active theme's class"
        );
    }
    // The two headers wear the active theme too and render bold, whether or
    // not they carry the selection marker; the unselected marker margin keeps
    // the glyph at the same column.
    for (header, title) in [(0usize, "  ▾ Dark"), (7, "  ▾ Light")] {
        let line: String = (body.x..body.x + title.chars().count() as u16)
            .map(|x| buffer[(x, body.y + header as u16)].symbol())
            .collect();
        assert_eq!(line, title);
        for i in 0..8u16 {
            let cell = &buffer[(body.x + i, body.y + header as u16)];
            assert_eq!(
                cell.bg, dark.background,
                "header cell {i} on the active background"
            );
            assert!(
                cell.style().add_modifier.contains(Modifier::BOLD),
                "header cell {i} is bold"
            );
        }
    }
    // Not vacuously: a row whose own palette differs from the active theme's
    // still wears the active theme, not its own background or text colour.
    let latte = row_of("catppuccin_latte") as u16;
    let latte_theme = theme_of(ThemeKey::CatppuccinLatte);
    assert_ne!(latte_theme.background, dark.background);
    assert_ne!(latte_theme.normal_text, dark.normal_text);
    assert_eq!(buffer[(body.x, body.y + latte)].bg, dark.background);
    assert_eq!(
        buffer[(body.x + 6, body.y + latte)].style().fg,
        Some(dark.normal_text)
    );
}

/// Browsing recolours the rows too (T115.1): with a theme previewed, the
/// rows wear the preview's classes -- the active theme is the previewed one
/// while the picker is open, so the list restyles live under navigation.
#[test]
fn previewing_recolours_the_rows_too() {
    let mut app = open();
    press_down_to(&mut app, "tokyo_night_dark");
    let tokyo = theme_of(ThemeKey::TokyoNightDark);
    assert_eq!(app.theme(), tokyo);
    assert_eq!(app.tui.theme, ThemeKey::Dark, "nothing is committed");
    let buffer = draw_buffer(&app);
    let body = app.theme_modal.area.get();
    // An unselected row wears the preview, not its own palette.
    let latte = row_of("catppuccin_latte") as u16;
    let latte_theme = theme_of(ThemeKey::CatppuccinLatte);
    assert_ne!(latte_theme.background, tokyo.background);
    assert_ne!(latte_theme.normal_text, tokyo.normal_text);
    assert_eq!(buffer[(body.x, body.y + latte)].bg, tokyo.background);
    assert_eq!(
        buffer[(body.x + 6, body.y + latte)].style().fg,
        Some(tokyo.normal_text)
    );
    // The selected row's name wears the preview's highlight.
    let selected = row_of("tokyo_night_dark") as u16;
    assert_eq!(
        buffer[(body.x + 6, body.y + selected)].style().fg,
        Some(tokyo.highlighted_text)
    );
}

/// The style dump of a rendered row: every cell as `symbol|fg|bg`, so a
/// snapshot captures the colours, not just the layout.
fn styled_rows(buffer: &ratatui::buffer::Buffer, rows: &[u16]) -> String {
    let spell = |colour: ratatui::style::Color| match colour {
        ratatui::style::Color::Reset => "-".to_string(),
        ratatui::style::Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
        ratatui::style::Color::Indexed(i) => format!("i{i}"),
        other => format!("{other:?}"),
    };
    rows.iter()
        .map(|&y| {
            (0..80usize)
                .map(|x| {
                    let cell = &buffer[(x as u16, y)];
                    format!("{}|{}|{}", cell.symbol(), spell(cell.fg), spell(cell.bg))
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A live preview recolours the whole shell (T43.1): the status chip carries
/// the preview's chip colour and the frame its background, with the committed
/// setting untouched.
#[test]
fn a_preview_recolours_the_whole_shell() {
    let mut app = open();
    press_down_to(&mut app, "catppuccin_latte");
    let preview = theme_of(ThemeKey::CatppuccinLatte);
    assert_eq!(app.theme(), preview);
    assert_eq!(app.tui.theme, ThemeKey::Dark);
    let buffer = draw_buffer(&app);
    // The status line's chip (row 23, T86.1) and a base row under the modal.
    assert_eq!(buffer[(0, 23)].bg, preview.chip_neutral);
    assert_eq!(buffer[(40, 21)].bg, preview.background);
    insta::assert_snapshot!(styled_rows(&buffer, &[23, 21]));
}

/// The picker's bottom line (T59.1, T64.1): the preview hints on the left
/// without brackets in the low-emphasis footer colour, the Save and Cancel
/// buttons right-aligned with their bracketed key in the button accent and
/// their label in the theme's foreground, and both buttons answer a click on
/// their rectangle with exactly their key's action.
mod modal_footer {
    use super::*;
    use ratatui::style::Modifier;

    fn click(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    /// Asserts the two zones on the picker's rendered bottom line (T59.1,
    /// T64.1): the hint text opens the line in the footer colour and without
    /// brackets, the buttons sit right-aligned each exactly the shared button
    /// text with its bracketed key in the button accent and its label in the
    /// theme's foreground, at least two blank columns separate the zones, and
    /// no cell of the left zone carries the accent or a bracket.
    fn assert_two_zones(
        buffer: &ratatui::buffer::Buffer,
        footer: ratatui::layout::Rect,
        hints: &str,
        buttons: &[(&str, &str)],
        theme: &Theme,
    ) {
        let row = footer.y;
        let line = |from: u16, to: u16| -> String {
            (from..to).map(|x| buffer[(x, row)].symbol()).collect()
        };

        let hint = format!(" {hints}");
        let hint_width = hint.chars().count() as u16;
        assert_eq!(line(footer.x, footer.x + hint_width), hint);
        for i in 0..hint_width {
            assert_eq!(
                buffer[(footer.x + i, row)].style().fg,
                Some(theme.muted_text),
                "hint cell {i} wears the footer colour"
            );
        }

        let rects = patok_tui::footer_button_rects(footer, buttons);
        assert_eq!(rects.len(), buttons.len(), "one rect per button");
        assert_eq!(
            rects.last().expect("buttons exist").right(),
            footer.right(),
            "the buttons are right-aligned"
        );
        for ((key, label), rect) in buttons.iter().zip(&rects) {
            assert_eq!(
                line(rect.x, rect.right()),
                patok_tui::button_text(key, label)
            );
            let accent_width =
                u16::try_from(format!(" [ {key} ] ").chars().count()).unwrap_or(rect.width);
            if !label.is_empty() {
                assert_ne!(
                    theme.foreground, theme.highlighted_text,
                    "the label colour differs from the accent"
                );
            }
            for i in 0..rect.width {
                let cell = &buffer[(rect.x + i, row)];
                let expected = if i < accent_width {
                    theme.highlighted_text
                } else {
                    theme.foreground
                };
                assert_eq!(
                    cell.style().fg,
                    Some(expected),
                    "button cell {i} wears its zone's colour"
                );
                assert!(
                    !cell.style().add_modifier.contains(Modifier::BOLD),
                    "buttons are never bold"
                );
            }
        }

        let left_end = rects[0].x;
        for x in footer.x + hint_width..left_end.saturating_sub(2) {
            let cell = &buffer[(x, row)];
            assert_ne!(
                cell.style().fg,
                Some(theme.highlighted_text),
                "no hint cell wears the accent at x={x}"
            );
            assert!(
                !matches!(cell.symbol(), "[" | "]"),
                "no bracket in the hint zone at x={x}"
            );
        }
        for x in left_end.saturating_sub(2)..left_end {
            assert_eq!(buffer[(x, row)].symbol(), " ", "a gap column at x={x}");
        }
    }

    #[test]
    fn picker_bottom_line_two_zones() {
        let app = open();
        let buffer = draw_buffer(&app);
        assert_two_zones(
            &buffer,
            app.theme_modal.footer.get(),
            "↑↓/jk previews",
            &[("Enter", "Save"), ("Esc", "Cancel")],
            &app.theme(),
        );
    }

    #[test]
    fn picker_bottom_line_two_zones_in_a_previewed_palette_theme() {
        let mut app = open();
        press_down_to(&mut app, "catppuccin_latte");
        assert_eq!(app.theme(), theme_of(ThemeKey::CatppuccinLatte));
        let buffer = draw_buffer(&app);
        assert_two_zones(
            &buffer,
            app.theme_modal.footer.get(),
            "↑↓/jk previews",
            &[("Enter", "Save"), ("Esc", "Cancel")],
            &app.theme(),
        );
    }

    #[test]
    fn picker_bottom_line_buttons_answer_clicks() {
        // A click on Save keeps the previewed theme and hands it to the driver.
        let mut app = open();
        press_down_to(&mut app, "gruvbox_dark");
        draw_buffer(&app);
        let rects = patok_tui::footer_button_rects(
            app.theme_modal.footer.get(),
            &[("Enter", "Save"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::SaveTheme(ThemeKey::GruvboxDark)
        );
        assert!(!app.theme_modal.open);
        assert_eq!(app.tui.theme, ThemeKey::GruvboxDark);

        // A click on Cancel restores the theme the picker opened with.
        let mut app = open();
        press_down_to(&mut app, "gruvbox_dark");
        draw_buffer(&app);
        let rects = patok_tui::footer_button_rects(
            app.theme_modal.footer.get(),
            &[("Enter", "Save"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[1].x + 2, rects[1].y)),
            Action::None
        );
        assert!(!app.theme_modal.open);
        assert_eq!(app.theme_modal.preview, None);
        assert_eq!(app.tui.theme, ThemeKey::Dark);
        assert_eq!(app.theme(), theme_of(ThemeKey::Dark));

        // A click on the hint zone and on the border change nothing, and a
        // hover over the footer previews no row.
        let mut app = open();
        draw_buffer(&app);
        let footer = app.theme_modal.footer.get();
        assert_eq!(app.on_mouse(click(footer.x + 1, footer.y)), Action::None);
        assert!(app.theme_modal.open);
        assert_eq!(app.tui.theme, ThemeKey::Dark);
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, footer.x + 1, footer.y)),
            Action::None
        );
        assert_eq!(app.theme_modal.selected, row_of("dark"));
    }
}
