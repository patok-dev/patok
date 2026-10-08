//! The settings overlay: key handling through `App::on_key` and screen snapshots of the
//! overlay in its states (fresh, editing, validation error, drafted changes, the
//! unsaved-changes dialog).

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use patok_core::config::{ApplyTiming, SettingValue};
use patok_core::event::{EngineEvent, Phase, Snapshot};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_tui::{
    Action, App, Entry, FieldKind, SETTINGS_HELP_BESIDE_WIDTH, SETTINGS_HELP_PANEL_HEIGHT,
    SETTINGS_HELP_PANEL_WIDTH, settings_area, settings_body_areas,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";

/// The engine-reported readout the overlay renders from.
fn readout() -> BTreeMap<String, SettingValue> {
    let str = |value: &str| SettingValue::Str(value.into());
    [
        ("provider", str("claude")),
        ("model", SettingValue::Str(String::new())),
        ("research_provider", str("claude")),
        ("research_model", str("sonnet")),
        ("planner_provider", str("claude")),
        ("planner_model", str("opus")),
        ("builder_provider", str("claude")),
        ("builder_model", str("opus")),
        ("reviewer_provider", str("claude")),
        ("reviewer_model", str("sonnet")),
        ("discovery_provider", str("claude")),
        ("discovery_model", str("opus")),
        ("run_mode", str("sprint")),
        ("plan_enabled", SettingValue::Bool(false)),
        ("review_confidence_threshold", SettingValue::Uint(5)),
        ("review_multipass_threshold", SettingValue::Uint(8)),
        ("confidence_threshold", SettingValue::Float(0.5)),
        ("agent_timeout_secs", SettingValue::Uint(600)),
        ("pause_between_tasks_secs", SettingValue::Uint(10)),
        ("pause_between_agents_secs", SettingValue::Uint(3)),
        ("pause_between_cycles_secs", SettingValue::Uint(30)),
        ("engine_idle_timeout_secs", SettingValue::Uint(1800)),
        ("adaptive_pauses", SettingValue::Bool(true)),
        ("discovery_cooldown_secs", SettingValue::Uint(300)),
        ("discovery_cooldown_cap_secs", SettingValue::Uint(1800)),
        ("auto_push_remote", SettingValue::Str(String::new())),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

fn app() -> App {
    App::new(
        Snapshot {
            project_dir: "/home/user/demo".into(),
            phase: Phase::Startup,
            tasks: task::parse(TASKS),
            current_task: None,
            planning: false,
            discovering: false,
            provider: "claude".into(),
            model: String::new(),
            settings: readout(),
            pipeline: PipelineState::today(),
            recent: vec![],
        },
        "0.1.0".into(),
    )
}

fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| patok_tui::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
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

fn ctrl(app: &mut App, c: char) -> Action {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
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
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    assert!(app.settings_open());
    app
}

/// Puts the overlay's focus on the (visible) row of `field`.
fn focus_field(app: &mut App, field: &str) {
    let index = app
        .overlay
        .visible()
        .iter()
        .position(|entry| matches!(entry, Entry::Row(row) if row.key == field))
        .unwrap_or_else(|| panic!("`{field}` is not visible"));
    move_focus_to(app, index);
}

/// Puts the overlay's focus on a section header.
fn focus_header(app: &mut App, index: usize) {
    let header = app
        .overlay
        .visible()
        .iter()
        .position(|entry| *entry == Entry::Header(index))
        .expect("the header is visible");
    move_focus_to(app, header);
}

/// Walks the focus to `index` with the navigation keys, one row per press,
/// so the scroll offset follows the selection like it does in the shell
/// (T136.1) -- the pointer's row hover moves the focus too (T145.1), but
/// the keys stay the baseline. The draw first records the list viewport
/// the follow reads.
fn move_focus_to(app: &mut App, index: usize) {
    let _ = draw(app, W, H);
    while app.overlay.focus < index {
        press(app, KeyCode::Down);
    }
    while app.overlay.focus > index {
        press(app, KeyCode::Up);
    }
    assert_eq!(app.overlay.focus, index);
}

/// Puts focus on a section header and toggles it with Enter.
fn toggle_section(app: &mut App, index: usize) {
    focus_header(app, index);
    press(app, KeyCode::Enter);
}

const W: u16 = 100;
/// Tall enough that the 80%-height modal shows every row of the model.
const H: u16 = 40;

#[test]
fn settings_area_is_large_centered_and_inside_the_screen() {
    let large = ratatui::layout::Rect::new(0, 0, 200, 60);
    let rect = settings_area(large);
    assert!(rect.right() <= 200 && rect.bottom() <= 60, "{rect:?}");
    // 90% x 80% with an 80x24 minimum that a big screen does not hit.
    assert_eq!((rect.width, rect.height), (180, 48));
    assert!(rect.x.abs_diff(200 - rect.right()) <= 1);
    assert!(rect.y.abs_diff(60 - rect.bottom()) <= 1);
    // Below the minimum the modal is full screen instead of overflowing.
    let small = ratatui::layout::Rect::new(0, 0, 60, 20);
    assert_eq!(settings_area(small), small);
}

/// The description line's leading column in a drawn screen, proving which side
/// of the list the help box rendered on: the modal's inner edge in the under
/// placement, deep in the modal in the beside one.
fn description_column(screen: &str, phrase: &str) -> usize {
    let line = screen
        .split('\n')
        .find(|line| line.contains(phrase))
        .unwrap_or_else(|| panic!("no line contains `{phrase}`\n{screen}"));
    // The byte offset counts the multibyte box-drawing characters on the line;
    // the column is the character count up to it.
    let bytes = line.find(phrase).unwrap();
    line.char_indices().take_while(|(i, _)| *i < bytes).count()
}

#[test]
fn settings_body_areas_switches_on_width() {
    // A body at least SETTINGS_HELP_BESIDE_WIDTH wide puts the help box to the
    // right of the list.
    let body = ratatui::layout::Rect::new(0, 0, SETTINGS_HELP_BESIDE_WIDTH, 28);
    let (list, help) = settings_body_areas(body);
    assert_eq!((list.y, list.height), (body.y, body.height));
    assert_eq!(help.y, list.y);
    assert_eq!(help.x, list.right());
    assert_eq!(help.width, SETTINGS_HELP_PANEL_WIDTH);
    // One column short of the threshold the help box moves under the list.
    let body = ratatui::layout::Rect::new(0, 0, SETTINGS_HELP_BESIDE_WIDTH - 1, 28);
    let (list, help) = settings_body_areas(body);
    assert_eq!((list.x, list.width), (body.x, body.width));
    assert_eq!(help.x, list.x);
    assert_eq!(help.y, list.bottom());
    assert_eq!(help.height, SETTINGS_HELP_PANEL_HEIGHT);
    // Both placements keep both areas inside the body.
    for body in [
        ratatui::layout::Rect::new(0, 0, 96, 28),
        ratatui::layout::Rect::new(0, 0, 88, 28),
        ratatui::layout::Rect::new(0, 0, 78, 20),
        ratatui::layout::Rect::new(0, 0, 124, 28),
    ] {
        let (list, help) = settings_body_areas(body);
        for rect in [list, help] {
            assert!(
                rect.right() <= body.right() && rect.bottom() <= body.bottom(),
                "{rect:?} in {body:?}"
            );
        }
    }
}

#[test]
fn the_help_box_sits_under_the_list_on_a_narrow_body() {
    let app = open();
    // W = 100 gives a 90-column modal (88 inner), under the 96-column beside
    // threshold, so the help box sits under the list (T85.1) and spans the
    // modal's inner width: the description starts at the modal's inner edge.
    let screen = draw(&app, W, H);
    assert!(screen.contains("Settings -- Patok"), "{screen}");
    // The fresh overlay focuses the Provider and model header, so the box
    // carries that group's description.
    assert!(
        screen.contains("Which CLI provider and model the agent roles run on"),
        "{screen}"
    );
    assert_eq!(
        description_column(
            &screen,
            "Which CLI provider and model the agent roles run on"
        ),
        7,
        "{screen}"
    );
    insta::assert_snapshot!(screen);
}

#[test]
fn the_help_box_sits_beside_the_list_on_a_wide_body() {
    let app = open();
    // A 140-column screen gives a 126-column modal (124 inner), past the
    // 96-column threshold: the help box sits right of the list, its text deep
    // inside the modal rather than at its inner edge.
    let screen = draw(&app, 140, 40);
    // The 40-column box wraps the description; its first line proves both the
    // placement and the content.
    assert!(screen.contains("Which CLI provider and model"), "{screen}");
    assert!(
        description_column(&screen, "Which CLI provider and model") > 60,
        "{screen}"
    );
    // The list keeps its rows beside the box.
    assert!(screen.contains("▾ Provider and model"), "{screen}");
    assert!(screen.contains("‹ sprint ›"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn the_help_box_follows_the_focused_entry() {
    let mut app = open();
    // A focused row carries its own description, titled with its label.
    focus_field(&mut app, "run_mode");
    let screen = draw(&app, W, H);
    assert!(screen.contains("run mode"), "{screen}");
    assert!(
        screen.contains("sprint (default) or continuous"),
        "{screen}"
    );
    // A focused header carries the whole group's description.
    focus_header(&mut app, 1);
    let screen = draw(&app, W, H);
    assert!(
        screen.contains("How tasks flow through the pipeline"),
        "{screen}"
    );
    // A tui-schema row routes through the tui registry.
    focus_field(&mut app, "theme");
    let screen = draw(&app, W, H);
    assert!(
        screen.contains("Colour theme: dark is the default"),
        "{screen}"
    );
}

#[test]
fn the_minimum_size_screen_still_shows_the_help_box() {
    let app = open();
    // The 80x24 minimum modal keeps the help box under a 12-row list.
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("Settings -- Patok"), "{screen}");
    assert!(
        screen.contains("Which CLI provider and model the agent roles run on"),
        "{screen}"
    );
    assert!(
        screen.contains("↑↓ move · Enter/Space edit · ←→ fold/cycle"),
        "{screen}"
    );
}

#[test]
fn question_mark_opens_the_fresh_overlay() {
    let app = open();
    // A 75-row terminal: the per-role provider and model rows (T90.1) pushed the
    // expanded overlay past the 40-row body of a 60-row screen, and the help box
    // under the list (T85.1) costs 8 more, so the full walk needs the taller
    // modal's 48-row list.
    let screen = draw(&app, W, 75);
    assert!(screen.contains("Settings -- Patok"), "{screen}");
    assert!(screen.contains("[ x ]"), "{screen}");
    // Every section opens expanded, so all of them show their fold indicator.
    assert!(screen.contains("▾ Provider and model"), "{screen}");
    assert!(screen.contains("▾ Pipeline"), "{screen}");
    assert!(screen.contains("▾ Timeouts and pauses"), "{screen}");
    assert!(screen.contains("▾ Git"), "{screen}");
    assert!(screen.contains("▾ Display and theme"), "{screen}");
    assert!(screen.contains("[ ] plan enabled"), "{screen}");
    assert!(screen.contains("‹ sprint ›"), "{screen}");
    assert!(screen.contains("‹ claude ›"), "{screen}");
    assert!(screen.contains("engine version: 0.1.0"), "{screen}");
    assert!(screen.contains("auto-push remote"), "{screen}");
    insta::assert_snapshot!(screen);
}

/// The research-skip key's row carries its research name (T68.1, T69.1: the
/// role that once owned it is folded into the research agent, so no settings
/// surface names it any more).
#[test]
fn the_pipeline_section_shows_the_skip_research_row_and_no_folded_role_text() {
    let app = open();
    let screen = draw(&app, W, H);
    assert!(screen.contains("[ ] skip research for simple"), "{screen}");
    assert!(screen.contains("[ ] skip planner for simple"), "{screen}");
    // The folded role's old name is gone from every rendered settings surface;
    // the needle is assembled so the sweep does not find it here either.
    let folded = ["s", "c", "o", "u", "t"].concat();
    assert!(!screen.to_lowercase().contains(&folded), "{screen}");
}

#[test]
fn toggling_a_bool_drafts_the_change_without_sending_it() {
    let mut app = open();
    focus_field(&mut app, "plan_enabled");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    // Nothing was sent or applied: the readout keeps the old value, the overlay is
    // only dirty with the draft, and no save happened.
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );
    assert!(app.overlay.dirty());
    assert_eq!(
        app.overlay.drafts.get("plan_enabled"),
        Some(&SettingValue::Bool(true))
    );
    // The row shows the drafted value, with no saved status line.
    let screen = draw(&app, W, H);
    assert!(screen.contains("[x] plan enabled"), "{screen}");
    assert!(!screen.contains("Saved."), "{screen}");
    insta::assert_snapshot!(screen);
    // A rejected draft keeps the draft and shows the reason in the status line.
    app.on_settings_rejected("daemon.plan_enabled must be a boolean, got a string".into());
    assert!(app.overlay.dirty());
    let screen = draw(&app, W, H);
    assert!(screen.contains("must be a boolean"), "{screen}");
    assert!(screen.contains("[x] plan enabled"), "{screen}");
}

#[test]
fn enums_cycle_with_left_right_and_enter() {
    let mut app = open();
    focus_field(&mut app, "run_mode");
    // Right drafts the next choice; nothing is sent or applied.
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(
        app.overlay.drafts.get("run_mode"),
        Some(&SettingValue::Str("continuous".into()))
    );
    assert_eq!(app.run_mode(), "sprint");
    assert!(draw(&app, W, H).contains("‹ continuous ›"));
    // A later key cycles from the drafted value (here: back to sprint, the wrap).
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(
        app.overlay.drafts.get("run_mode"),
        Some(&SettingValue::Str("sprint".into()))
    );

    // A tui-schema enum drafts the same way, without touching the shell's mirror.
    focus_field(&mut app, "theme");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(
        app.overlay.drafts.get("theme"),
        Some(&SettingValue::Str("atom_one_dark".into()))
    );
    assert_eq!(app.tui.theme, patok_core::config::Theme::Dark);
    assert!(draw(&app, W, H).contains("‹ atom_one_dark ›"));
    assert!(!draw(&app, W, H).contains("Saved."));

    // truecolor: auto -> on -> off -> auto, drafted as Bool(true)/Bool(false)/Unset;
    // the row displays the drafted auto/on/off choice each time.
    focus_field(&mut app, "truecolor");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(
        app.overlay.drafts.get("truecolor"),
        Some(&SettingValue::Bool(true))
    );
    assert!(draw(&app, W, H).contains("‹ on ›"));
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(
        app.overlay.drafts.get("truecolor"),
        Some(&SettingValue::Bool(false))
    );
    assert!(draw(&app, W, H).contains("‹ off ›"));
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(
        app.overlay.drafts.get("truecolor"),
        Some(&SettingValue::Unset)
    );
    assert!(draw(&app, W, H).contains("‹ auto ›"));
    assert_eq!(app.tui.truecolor, None);
}

/// The theme row cycles through every built-in name, forward wrapping back to
/// `dark` and backward, drafting each choice without applying it.
#[test]
fn the_theme_row_cycles_through_every_builtin_name() {
    let mut app = open();
    focus_field(&mut app, "theme");
    // Forward from the current `dark`: atom_one_dark .. gruvbox_light, then the
    // wrap back to dark.
    let keys = patok_core::config::THEME_KEYS;
    let forward: Vec<&str> = keys[1..]
        .iter()
        .copied()
        .chain(keys[..1].iter().copied())
        .collect();
    for name in forward {
        assert_eq!(press(&mut app, KeyCode::Right), Action::None);
        assert_eq!(
            app.overlay.drafts.get("theme"),
            Some(&SettingValue::Str(name.into())),
            "cycling must draft {name}"
        );
    }
    // The last Right wrapped back to `dark`, and the shell's own mirror is
    // untouched throughout.
    assert_eq!(app.tui.theme, patok_core::config::Theme::Dark);

    // Left cycles backwards through the same list, wrapping the other way:
    // gruvbox_light .. atom_one_dark, then back to dark.
    let backward: Vec<&str> = keys[1..]
        .iter()
        .rev()
        .copied()
        .chain(keys[..1].iter().copied())
        .collect();
    for name in backward {
        assert_eq!(press(&mut app, KeyCode::Left), Action::None);
        assert_eq!(
            app.overlay.drafts.get("theme"),
            Some(&SettingValue::Str(name.into())),
            "cycling back must draft {name}"
        );
    }
}

#[test]
fn the_inline_editor_edits_numbers() {
    let mut app = open();
    focus_field(&mut app, "agent_timeout_secs");
    // Enter opens the editor prefilled with the current value.
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "600");
    insta::assert_snapshot!(draw(&app, W, H));
    // Digits type; Ctrl+U clears; Enter saves.
    for c in "2".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "6002");
    ctrl(&mut app, 'u');
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "");
    for c in "240".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    // Enter commits the buffer as a draft: no change is sent, applied or persisted.
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(app.overlay.editor.is_none());
    assert_eq!(
        app.overlay.drafts.get("agent_timeout_secs"),
        Some(&SettingValue::Uint(240))
    );
    assert_eq!(
        app.settings.get("agent_timeout_secs"),
        Some(&SettingValue::Uint(600))
    );
    assert!(draw(&app, W, H).contains("240"));
    assert!(!draw(&app, W, H).contains("Saved."));
    // Esc closes the editor without drafting anything.
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay.editor.is_some());
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(app.overlay.editor.is_none());
}

#[test]
fn a_local_validation_error_keeps_the_editor_open() {
    let mut app = open();
    focus_field(&mut app, "agent_timeout_secs");
    press(&mut app, KeyCode::Enter);
    ctrl(&mut app, 'u');
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(app.overlay.editor.is_some());
    let (text, level) = app.overlay.status.as_ref().unwrap();
    assert_eq!(*level, patok_tui::StatusLevel::Error);
    assert!(text.contains("greater than zero"), "{text}");
    insta::assert_snapshot!(draw(&app, W, H));
}

#[test]
fn sections_collapse_and_focus_skips_hidden_rows() {
    let mut app = open();
    toggle_section(&mut app, 2);
    let screen = draw(&app, W, H);
    assert!(screen.contains("▸ Timeouts and pauses"), "{screen}");
    assert!(!screen.contains("agent timeout"), "{screen}");
    // Moving down from the section's header lands on the next section, not a hidden row.
    let header = app
        .overlay
        .visible()
        .iter()
        .position(|entry| *entry == Entry::Header(2))
        .unwrap();
    app.overlay.focus = header;
    press(&mut app, KeyCode::Down);
    assert_eq!(app.overlay.focused(), Entry::Header(3));
    insta::assert_snapshot!(screen);
}

#[test]
fn left_on_a_header_hides_its_rows() {
    let mut app = open();
    focus_header(&mut app, 1);
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    let screen = draw(&app, W, H);
    assert!(screen.contains("▸ Pipeline"), "{screen}");
    assert!(!screen.contains("run mode"), "{screen}");
    assert!(!screen.contains("plan enabled"), "{screen}");
    // An untouched section keeps its expanded indicator.
    assert!(screen.contains("▾ Provider and model"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn right_on_a_folded_header_bring_its_rows_back() {
    let mut app = open();
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    let folded = draw(&app, W, H);
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    let screen = draw(&app, W, H);
    assert!(screen.contains("▾ Pipeline"), "{screen}");
    assert!(screen.contains("run mode"), "{screen}");
    assert_ne!(screen, folded);
    // A second Right changes nothing: the section is already expanded.
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(draw(&app, W, H), screen);
}

#[test]
fn left_on_a_folded_header_is_ignored() {
    let mut app = open();
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    let folded = draw(&app, W, H);
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(draw(&app, W, H), folded);
}

#[test]
fn navigation_skips_folded_rows() {
    let mut app = open();
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    // Down from the folded Pipeline header lands on the next section header, not a
    // hidden row; Up skips back over them the same way.
    assert_eq!(press(&mut app, KeyCode::Down), Action::None);
    assert_eq!(app.overlay.focused(), Entry::Header(2));
    assert_eq!(press(&mut app, KeyCode::Up), Action::None);
    assert_eq!(app.overlay.focused(), Entry::Header(1));

    // With every section folded, Up/Down walk only the headers.
    for index in 0..5 {
        focus_header(&mut app, index);
        press(&mut app, KeyCode::Left);
    }
    assert_eq!(app.overlay.visible().len(), 5);
    app.overlay.focus = 0;
    for expected in [
        Entry::Header(1),
        Entry::Header(2),
        Entry::Header(3),
        Entry::Header(4),
    ] {
        press(&mut app, KeyCode::Down);
        assert_eq!(app.overlay.focused(), expected);
    }
    for expected in [
        Entry::Header(3),
        Entry::Header(2),
        Entry::Header(1),
        Entry::Header(0),
    ] {
        press(&mut app, KeyCode::Up);
        assert_eq!(app.overlay.focused(), expected);
    }
}

#[test]
fn row_key_handling_is_untouched_with_a_section_folded() {
    let mut app = open();
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    assert!(!draw(&app, W, H).contains("run mode"));
    // Enter on a bool row still drafts the flip.
    focus_field(&mut app, "adaptive_pauses");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(
        app.overlay.drafts.get("adaptive_pauses"),
        Some(&SettingValue::Bool(false))
    );
    // Left/Right on an enum row still cycle (drafting) and fold nothing.
    focus_field(&mut app, "provider");
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert_eq!(
        app.overlay.drafts.get("provider"),
        Some(&SettingValue::Str("codex".into()))
    );
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(
        app.overlay.drafts.get("provider"),
        Some(&SettingValue::Str("claude".into()))
    );
    // Enter on a number row still opens the inline editor, and while it is open
    // Left and Right keep going to the editor instead of folding.
    focus_field(&mut app, "agent_timeout_secs");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert!(app.overlay.editor.is_some());
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "600");
    let screen = draw(&app, W, H);
    assert!(screen.contains("▸ Pipeline"), "{screen}");
    assert!(screen.contains("▾ Timeouts and pauses"), "{screen}");
}

#[test]
fn all_sections_folded_show_only_their_headers() {
    let mut app = open();
    for index in 0..5 {
        focus_header(&mut app, index);
        assert_eq!(press(&mut app, KeyCode::Left), Action::None);
    }
    let screen = draw(&app, W, H);
    for title in [
        "Provider and model",
        "Pipeline",
        "Timeouts and pauses",
        "Git",
        "Display and theme",
    ] {
        assert!(screen.contains(&format!("▸ {title}")), "{screen}");
    }
    assert!(!screen.contains("Close settings"), "{screen}");
    assert!(!screen.contains("run mode"), "{screen}");
    assert!(!screen.contains("agent timeout"), "{screen}");
    assert!(!screen.contains("auto-push remote"), "{screen}");
    assert!(!screen.contains("‹ dark ›"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn every_section_expands_and_the_overflowing_body_gains_a_scrollbar() {
    let mut app = open();
    // Every section opens expanded, so the full row model -- the tui-schema fields
    // included -- is showing; the bottom rows only render once the body scrolls
    // to the focused last entry row, reached through the keys so the offset
    // follows the selection (T136.1).
    let _ = draw(&app, W, H);
    while app.overlay.focus < app.overlay.visible().len() - 1 {
        press(&mut app, KeyCode::PageDown);
    }
    let screen = draw(&app, W, H);
    assert!(screen.contains("auto-push remote"), "{screen}");
    assert!(screen.contains("‹ dark ›"), "{screen}");
    assert!(screen.contains("‹ auto ›"), "{screen}");
    assert!(screen.contains("[x] preview wrap"), "{screen}");
    assert!(screen.contains("‹ stable ›"), "{screen}");
    // The full row model overflows the 80%-height modal, so the scrollbar renders.
    assert!(screen.contains("█"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn a_clean_overlay_closes_directly() {
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Esc), Action::CloseSettings);
    assert!(!app.settings_open());
    // q behaves the same.
    let mut app = open();
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::CloseSettings);
    assert!(!app.settings_open());
}

#[test]
fn a_dirty_overlay_opens_the_three_choice_close_dialog() {
    let mut app = open();
    focus_field(&mut app, "plan_enabled");
    press(&mut app, KeyCode::Enter);
    assert!(app.overlay.dirty());

    // Esc opens the centered dialog with Save selected; the drafts stay.
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(app.overlay.confirm_open);
    assert_eq!(app.overlay.confirm_selected, 0);
    insta::assert_snapshot!(draw(&app, W, H));
    // Up/Down move through the three rows and clamp at the ends.
    assert_eq!(press(&mut app, KeyCode::Up), Action::None);
    assert_eq!(app.overlay.confirm_selected, 0);
    press(&mut app, KeyCode::Down);
    insta::assert_snapshot!(draw(&app, W, H));
    press(&mut app, KeyCode::Down);
    insta::assert_snapshot!(draw(&app, W, H));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.overlay.confirm_selected, 2);
    // Esc in the dialog cancels: back to the overlay, drafts intact.
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(!app.overlay.confirm_open);
    assert!(app.settings_open());
    assert!(app.overlay.dirty());
    // q opens the same dialog.
    assert_eq!(press(&mut app, KeyCode::Char('q')), Action::None);
    assert!(app.overlay.confirm_open);

    // Cancel (the third row) returns to the overlay the same way.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert!(!app.overlay.confirm_open);
    assert!(app.settings_open());
    assert!(app.overlay.dirty());

    // Enter on Discard throws the drafts away and closes the overlay; the readout
    // never moved, because nothing was applied.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::CloseSettings);
    assert!(!app.settings_open());
    assert!(app.overlay.drafts.is_empty());
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );

    // Enter on Save asks the driver to apply every draft; simulating it (the driver
    // loops pending() through the settings-change flow) persists the readout, syncs
    // the run-mode chip only then, and closes the overlay.
    let mut app = open();
    focus_field(&mut app, "plan_enabled");
    press(&mut app, KeyCode::Enter);
    focus_field(&mut app, "run_mode");
    press(&mut app, KeyCode::Right);
    assert_eq!(app.run_mode(), "sprint");
    press(&mut app, KeyCode::Esc);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::SaveSettings);
    assert!(!app.overlay.confirm_open);
    assert!(app.settings_open());
    for (schema, field, value) in app.overlay.pending() {
        app.on_settings_applied(schema, &field, value);
    }
    assert!(!app.overlay.dirty());
    app.overlay.close();
    assert!(!app.settings_open());
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(true))
    );
    assert_eq!(app.run_mode(), "continuous");
}

#[test]
fn a_config_changed_event_updates_the_open_overlay() {
    let mut app = open();
    focus_field(&mut app, "run_mode");
    assert!(draw(&app, W, H).contains("‹ sprint ›"));
    // A hand-edited config file picked up by the engine's reload arrives as a
    // ConfigChanged broadcast; the open overlay follows the reported value.
    app.apply(EngineEvent::ConfigChanged {
        field: "run_mode".into(),
        value: SettingValue::Str("continuous".into()),
        timing: ApplyTiming::NextUnitOfWork,
    });
    let screen = draw(&app, W, H);
    assert!(screen.contains("‹ continuous ›"), "{screen}");
    // A field with a drafted change keeps showing the draft, not the reported value.
    press(&mut app, KeyCode::Right);
    assert!(draw(&app, W, H).contains("‹ sprint ›"));
    // But a field whose editor is open keeps showing the buffer, not the new value.
    focus_field(&mut app, "agent_timeout_secs");
    press(&mut app, KeyCode::Enter);
    app.apply(EngineEvent::ConfigChanged {
        field: "agent_timeout_secs".into(),
        value: SettingValue::Uint(120),
        timing: ApplyTiming::NextUnitOfWork,
    });
    let screen = draw(&app, W, H);
    assert!(screen.contains("600"), "{screen}");
}

#[test]
fn overlay_keys_do_not_leak_and_ctrl_c_detaches() {
    let mut app = open();
    // q is the overlay's close, not the shell's quit.
    assert!(!app.stopping);
    // Ctrl+C still detaches from inside the overlay.
    assert_eq!(ctrl(&mut app, 'c'), Action::Detach);
}

#[test]
fn arrows_move_focus_and_pages_jump() {
    let mut app = open();
    assert_eq!(app.overlay.focused(), Entry::Header(0));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert!(matches!(app.overlay.focused(), Entry::Row(row) if row.key == "shell_version"));
    press(&mut app, KeyCode::Up);
    assert!(matches!(app.overlay.focused(), Entry::Row(row) if row.key == "engine_version"));
    press(&mut app, KeyCode::PageDown);
    assert!(app.overlay.focus > 10);
    press(&mut app, KeyCode::PageUp);
    assert!(app.overlay.focus < 10);
    // The focused row stays on screen when the body scrolls: the last row,
    // reached through the keys, drags the offset along with it (T136.1).
    let _ = draw(&app, W, H);
    while app.overlay.focus < app.overlay.visible().len() - 1 {
        press(&mut app, KeyCode::PageDown);
    }
    let screen = draw(&app, W, H);
    assert!(screen.contains("update channel"), "{screen}");
    assert!(screen.contains("│"), "{screen}");
}

#[test]
fn entry_rows_indent_under_their_section_headers() {
    let mut app = open();
    // Every section opens expanded, so all five sections show entry rows.
    focus_field(&mut app, "plan_enabled");
    // All five sections expanded need the 48-row list of a 75-row screen to keep
    // every row of the model on screen at once.
    let screen = draw(&app, W, 75);
    // At W = 100 the modal's body starts at column 6 of each screen line.
    let body = |text: &str| -> String {
        screen
            .split('\n')
            .find(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no line contains `{text}`\n{screen}"))
            .chars()
            .skip(6)
            .collect()
    };
    // Every entry row — read-only, boolean, enum, number and text — starts with the
    // two-column focus marker plus the four-space indent, in every section.
    for text in [
        "engine version",
        "shell version",
        "provider  ‹",
        "model  (not set)",
        "research provider  ‹",
        "research model",
        "planner provider  ‹",
        "planner model",
        "builder provider  ‹",
        "builder model",
        "reviewer provider  ‹",
        "reviewer model",
        "discovery provider  ‹",
        "discovery model",
        "run mode",
        "skip planner for simple",
        "confidence threshold",
        "agent timeout",
        "pause between tasks",
        "adaptive pauses",
        "discovery cooldown cap",
        "auto-push remote",
        "theme  ‹",
        "truecolor  ‹",
        "preview wrap",
        "update channel",
    ] {
        let row = body(text);
        assert!(
            row.starts_with("      "),
            "`{text}` is not indented: {row:?}"
        );
    }
    // The focused row keeps the cursor marker at the margin, then the indent.
    let focused = body("plan enabled");
    assert!(
        focused.starts_with("▶     "),
        "focused entry row: {focused:?}"
    );
    // The five section headers keep the bare marker margin.
    for title in [
        "Provider and model",
        "Pipeline",
        "Timeouts and pauses",
        "Git",
        "Display and theme",
    ] {
        let header = body(title);
        assert!(
            header.starts_with("  ▾")
                || header.starts_with("  ▸")
                || header.starts_with("▶ ▾")
                || header.starts_with("▶ ▸"),
            "header `{title}` is indented: {header:?}"
        );
    }
}

#[test]
fn a_narrow_terminal_clamps_the_editor_to_the_modal_body() {
    let mut app = open();
    // A 50-column screen makes the modal full width; the model editor gets a buffer
    // far longer than the row has room for.
    focus_field(&mut app, "model");
    press(&mut app, KeyCode::Enter);
    for c in "HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHTTTTTTTTTTTTTTTTTTTT".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let screen = draw(&app, 50, 20);
    // The buffer's tail — where the block cursor sits — stays inside the body...
    assert!(screen.contains("TTTTTTTT"), "{screen}");
    // ...while the head is clipped: the row holds the marker, indent, label and 20
    // tail chars, so at most 14 of the 40 leading H's survive (a 15-H run means the
    // row pushed past the modal's right edge).
    assert!(!screen.contains(&"H".repeat(15)), "{screen}");
}

/// The text the focused entry renders, so a screen check proves it is visible.
fn focused_text(app: &App) -> &'static str {
    match app.overlay.focused() {
        Entry::Row(row) => row.label,
        Entry::Header(index) => match index {
            0 => "Provider and model",
            1 => "Pipeline",
            2 => "Timeouts and pauses",
            3 => "Git",
            _ => "Display and theme",
        },
    }
}

/// Asserts the rendered state keeps the cursor inside the viewport: the offset
/// within the list's scrollable range, the focused row on screen.
fn assert_cursor_visible(app: &App, screen: &str, height: usize) {
    let len = app.overlay.visible().len();
    let scroll = app.overlay.scroll.get();
    assert!(
        scroll <= len.saturating_sub(height),
        "scroll {scroll}, len {len}"
    );
    assert!(
        app.overlay.focus - scroll < height,
        "focus {} is off screen at scroll {scroll}",
        app.overlay.focus
    );
    assert!(
        screen.contains(focused_text(app)),
        "focused row `{}` is not on screen:\n{screen}",
        focused_text(app)
    );
}

#[test]
fn down_scrolls_only_once_the_cursor_is_four_rows_above_the_bottom() {
    let mut app = open();
    // At W = 100, H = 40 the modal's body is 28 rows; the help box under the
    // list (T85.1) leaves the list 20 rows, and the expanded row model is 44
    // rows, so the bottom edge of the viewport is viewport row 19 and the
    // 4-row margin sits at row 15.
    let screen = draw(&app, W, H);
    assert_eq!(app.overlay.scroll.get(), 0);
    assert_cursor_visible(&app, &screen, 20);
    for focus in 1..=15 {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 0, "focus {focus}");
        assert_cursor_visible(&app, &screen, 20);
    }
    // The view never moved: the body still starts at the first section header and
    // the rows past the viewport are still hidden.
    let screen = draw(&app, W, H);
    assert!(screen.contains("▾ Provider and model"), "{screen}");
    assert!(!screen.contains("Close settings"), "{screen}");
    // From the margin on, each Down advances the offset by one, keeping the
    // cursor exactly 4 rows above the bottom edge (viewport row 15).
    for (focus, scroll) in (16..=39).map(|focus| (focus, focus - 15)) {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), scroll, "focus {focus}");
        assert_eq!(focus - scroll, 15);
        assert_cursor_visible(&app, &screen, 20);
    }
    // The list-end clamp: the offset stops at 24 and the cursor rides the last
    // rows down to the bottom edge.
    for focus in 40..=43 {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 24, "focus {focus}");
        assert_cursor_visible(&app, &screen, 20);
    }
    assert!(draw(&app, W, H).contains("rail mode"));
}

#[test]
fn up_scrolls_only_once_the_cursor_is_four_rows_below_the_top() {
    let mut app = open();
    // Walk to the last row first, drawing every step; the offset ends at its
    // maximum (24) with the last entry row in view.
    for _ in 0..43 {
        press(&mut app, KeyCode::Down);
        draw(&app, W, H);
    }
    assert_eq!(app.overlay.focus, 43);
    assert_eq!(app.overlay.scroll.get(), 24);
    // Up from the bottom keeps the view stationary while the cursor has room:
    // it climbs from viewport row 19 down to 4 -- 4 rows below the top edge.
    for focus in (28..=42).rev() {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 24, "focus {focus}");
        assert_cursor_visible(&app, &screen, 20);
    }
    // Past the margin the offset decreases one row per Up press, keeping the
    // cursor exactly 4 rows below the top edge (viewport row 4).
    for (focus, scroll) in (4..=27).rev().map(|focus| (focus, focus - 4)) {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), scroll, "focus {focus}");
        assert_eq!(focus - scroll, 4);
        assert_cursor_visible(&app, &screen, 20);
    }
    // The rest of the way up the offset is already 0 and the view stays put.
    for focus in (0..4).rev() {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, H);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 0, "focus {focus}");
        assert_cursor_visible(&app, &screen, 20);
    }
    assert!(draw(&app, W, H).contains("▾ Provider and model"));
}

/// Hovering the bottom viewport row scrolls the list exactly like the Down
/// key: the first hover reaches the state nineteen Down presses reach, and
/// keeping the pointer there walks down to the clamp at the last full
/// screen (T145.1).
#[test]
fn hovering_the_bottom_row_scrolls_the_list_like_the_down_key() {
    let mut app = open();
    draw(&app, W, H);
    // At W = 100, H = 40 the help box under the list (T85.1) leaves the list
    // 20 rows over the 44-row expanded model -- the same geometry the Down
    // key test pins.
    let list = app.overlay.list.get();
    assert_eq!(list.height, 20);
    let len = app.overlay.visible().len();
    assert_eq!(app.overlay.scroll.get(), 0);

    // The pointer lands on the bottom viewport row: the highlight moves
    // there without activating anything, and the follow scrolls exactly
    // like nineteen Down presses.
    assert_eq!(
        app.on_mouse(mouse(
            MouseEventKind::Moved,
            list.x + 3,
            list.y + list.height - 1
        )),
        Action::None
    );
    assert_eq!(app.overlay.focus, 19);
    assert_eq!(app.overlay.scroll.get(), 4);
    assert!(app.overlay.editor.is_none());
    assert!(app.overlay.drafts.is_empty());

    // Keeping the pointer on the bottom row walks the list down like the
    // Down key does, the hovered row always on screen, to the clamp at the
    // last full screen.
    while app.overlay.focus < len - 1 {
        let hovered = app.overlay.scroll.get() + usize::from(list.height - 1);
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Moved,
                list.x + 3,
                list.y + list.height - 1
            )),
            Action::None
        );
        assert_eq!(app.overlay.focus, hovered);
        assert!(app.overlay.focus >= app.overlay.scroll.get());
        assert!(app.overlay.focus - app.overlay.scroll.get() < usize::from(list.height));
    }
    assert_eq!(app.overlay.focus, len - 1);
    assert_eq!(app.overlay.scroll.get(), 24);

    // The hovered row's selection marker is on screen at the walk's end.
    let screen = draw(&app, W, H);
    assert_cursor_visible(&app, &screen, 20);
}

#[test]
fn the_margin_holds_with_sections_folded() {
    let mut app = open();
    // Fold the Pipeline section: 33 visible rows in a 12-row list (the 20-row
    // body of a full-screen modal at H = 24 minus the help box under it,
    // T85.1), so the margin band is viewport rows 4..=7.
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    app.overlay.focus = 0;
    let screen = draw(&app, W, 24);
    assert_eq!(app.overlay.scroll.get(), 0);
    assert_cursor_visible(&app, &screen, 12);
    for focus in 1..=7 {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 0, "focus {focus}");
        assert_cursor_visible(&app, &screen, 12);
    }
    // The band ends on a Provider-section row: the folded Pipeline left the
    // section longer than the band.
    assert!(matches!(
        app.overlay.focused(),
        Entry::Row(row) if row.key == "planner_provider"
    ));
    for (focus, scroll) in (8..=28).map(|focus| (focus, focus - 7)) {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), scroll, "focus {focus}");
        assert_eq!(focus - scroll, 7);
        assert_cursor_visible(&app, &screen, 12);
        // Navigation still walks only visible rows: the folded Pipeline
        // contributes its header alone, so the walk crosses straight from the
        // Pipeline header to the Timeouts header without landing on a hidden
        // row.
        if focus == 15 {
            assert_eq!(app.overlay.focused(), Entry::Header(1));
        }
        if focus == 16 {
            assert_eq!(app.overlay.focused(), Entry::Header(2));
        }
    }
    // The clamp: the offset stops at 21 and the cursor rides to the bottom edge.
    for focus in 29..=32 {
        press(&mut app, KeyCode::Down);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 21, "focus {focus}");
        assert_cursor_visible(&app, &screen, 12);
    }
    assert!(matches!(
        app.overlay.focused(),
        Entry::Row(row) if row.key == "rail_mode"
    ));
    // And back up: stationary to viewport row 4, then decreasing with the same
    // 4-row margin below the top edge.
    for focus in (25..=31).rev() {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 21, "focus {focus}");
        assert_cursor_visible(&app, &screen, 12);
    }
    for (focus, scroll) in (4..=24).rev().map(|focus| (focus, focus - 4)) {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), scroll, "focus {focus}");
        assert_eq!(focus - scroll, 4);
        assert_cursor_visible(&app, &screen, 12);
    }
    for focus in (0..4).rev() {
        press(&mut app, KeyCode::Up);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), 0, "focus {focus}");
        assert_cursor_visible(&app, &screen, 12);
    }
}

#[test]
fn the_cursor_stays_visible_after_jumps_and_folds() {
    let mut app = open();
    // At H = 24 the list is 12 rows (a 20-row body minus the help box under it,
    // T85.1) and the expanded row model 44 rows, so a PageDown jumps the cursor
    // far past the stationary band; every draw must pull the stale offset along.
    let screen = draw(&app, W, 24);
    assert_cursor_visible(&app, &screen, 12);
    for (focus, scroll) in [(10, 3), (20, 13), (30, 23), (40, 32), (43, 32)] {
        press(&mut app, KeyCode::PageDown);
        let screen = draw(&app, W, 24);
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.scroll.get(), scroll, "focus {focus}");
        assert_cursor_visible(&app, &screen, 12);
    }
    // Folding Pipeline while scrolled to the bottom shortens the list to 33
    // rows: walking the focus to the header and back down with the keys keeps
    // every step visible, and the offset clamps into the shortened range.
    let header = app
        .overlay
        .visible()
        .iter()
        .position(|entry| *entry == Entry::Header(1))
        .unwrap();
    while app.overlay.focus > header {
        press(&mut app, KeyCode::Up);
    }
    press(&mut app, KeyCode::Left);
    while app.overlay.focus < app.overlay.visible().len() - 1 {
        press(&mut app, KeyCode::Down);
    }
    let screen = draw(&app, W, 24);
    assert_eq!(app.overlay.visible().len(), 33);
    assert_eq!(app.overlay.focus, 32);
    assert_eq!(app.overlay.scroll.get(), 21);
    assert_cursor_visible(&app, &screen, 12);
    // Unfolding grows the list again; walking back down to the very bottom
    // keeps the cursor visible, back at the maximum offset.
    let header = app
        .overlay
        .visible()
        .iter()
        .position(|entry| *entry == Entry::Header(1))
        .unwrap();
    while app.overlay.focus > header {
        press(&mut app, KeyCode::Up);
    }
    press(&mut app, KeyCode::Right);
    while app.overlay.focus < app.overlay.visible().len() - 1 {
        press(&mut app, KeyCode::Down);
    }
    let screen = draw(&app, W, 24);
    assert_eq!(app.overlay.visible().len(), 44);
    assert_eq!(app.overlay.scroll.get(), 32);
    assert_cursor_visible(&app, &screen, 12);
}

#[test]
fn j_and_k_move_focus_exactly_like_down_and_up() {
    // Two freshly opened apps: one driven with the arrows, one with j/k, in
    // lockstep through the full walk down and back up, drawing every step so
    // the scroll offset (the T22.1 margin behaviour) is recomputed identically.
    let mut hjkl = open();
    let mut arrows = open();
    for _ in 0..44 {
        press(&mut hjkl, KeyCode::Char('j'));
        press(&mut arrows, KeyCode::Down);
        let screen = draw(&hjkl, W, H);
        draw(&arrows, W, H);
        assert_eq!(hjkl.overlay.focus, arrows.overlay.focus);
        assert_eq!(
            hjkl.overlay.scroll.get(),
            arrows.overlay.scroll.get(),
            "focus {}",
            hjkl.overlay.focus
        );
        assert_cursor_visible(&hjkl, &screen, 20);
    }
    // The bottom of the list is reached with j alone and the last entry row shows.
    assert!(draw(&hjkl, W, H).contains("update channel"));
    for _ in 0..44 {
        press(&mut hjkl, KeyCode::Char('k'));
        press(&mut arrows, KeyCode::Up);
        let screen = draw(&hjkl, W, H);
        draw(&arrows, W, H);
        assert_eq!(hjkl.overlay.focus, arrows.overlay.focus);
        assert_eq!(
            hjkl.overlay.scroll.get(),
            arrows.overlay.scroll.get(),
            "focus {}",
            hjkl.overlay.focus
        );
        assert_cursor_visible(&hjkl, &screen, 20);
    }
    assert_eq!(hjkl.overlay.focus, 0);
    assert_eq!(hjkl.overlay.scroll.get(), 0);
}

#[test]
fn j_and_k_skip_folded_sections() {
    let mut app = open();
    focus_header(&mut app, 1);
    press(&mut app, KeyCode::Left);
    // j from the folded Pipeline header lands on the next section header, not a
    // hidden row; k skips back over them the same way.
    assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
    assert_eq!(app.overlay.focused(), Entry::Header(2));
    assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
    assert_eq!(app.overlay.focused(), Entry::Header(1));

    // With every section folded, j/k walk only the headers.
    for index in 0..5 {
        focus_header(&mut app, index);
        press(&mut app, KeyCode::Left);
    }
    assert_eq!(app.overlay.visible().len(), 5);
    app.overlay.focus = 0;
    for expected in [
        Entry::Header(1),
        Entry::Header(2),
        Entry::Header(3),
        Entry::Header(4),
    ] {
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.overlay.focused(), expected);
    }
    for expected in [
        Entry::Header(3),
        Entry::Header(2),
        Entry::Header(1),
        Entry::Header(0),
    ] {
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.overlay.focused(), expected);
    }
}

#[test]
fn h_and_l_cycle_enums_like_left_and_right() {
    let mut app = open();
    focus_field(&mut app, "run_mode");
    assert_eq!(press(&mut app, KeyCode::Char('l')), Action::None);
    assert_eq!(
        app.overlay.drafts.get("run_mode"),
        Some(&SettingValue::Str("continuous".into()))
    );
    assert_eq!(press(&mut app, KeyCode::Char('h')), Action::None);
    assert_eq!(
        app.overlay.drafts.get("run_mode"),
        Some(&SettingValue::Str("sprint".into()))
    );

    // A tui-schema enum through h/l drafts the same way, and h wraps back to the
    // first choice from the drafted value.
    focus_field(&mut app, "theme");
    assert_eq!(press(&mut app, KeyCode::Char('l')), Action::None);
    assert_eq!(
        app.overlay.drafts.get("theme"),
        Some(&SettingValue::Str("atom_one_dark".into()))
    );
    assert_eq!(app.tui.theme, patok_core::config::Theme::Dark);
    assert_eq!(press(&mut app, KeyCode::Char('h')), Action::None);
    assert_eq!(
        app.overlay.drafts.get("theme"),
        Some(&SettingValue::Str("dark".into()))
    );
}

#[test]
fn h_and_l_fold_and_unfold_headers_like_left_and_right() {
    let mut app = open();
    focus_header(&mut app, 1);
    assert_eq!(press(&mut app, KeyCode::Char('h')), Action::None);
    let folded = draw(&app, W, H);
    assert!(folded.contains("▸ Pipeline"), "{folded}");
    assert!(!folded.contains("run mode"), "{folded}");
    // h on an already-folded header is ignored, like Left.
    assert_eq!(press(&mut app, KeyCode::Char('h')), Action::None);
    assert_eq!(draw(&app, W, H), folded);
    // l unfolds it, like Right.
    assert_eq!(press(&mut app, KeyCode::Char('l')), Action::None);
    let screen = draw(&app, W, H);
    assert!(screen.contains("▾ Pipeline"), "{screen}");
    assert!(screen.contains("run mode"), "{screen}");
    assert_ne!(screen, folded);
    // A second l changes nothing: the section is already expanded.
    assert_eq!(press(&mut app, KeyCode::Char('l')), Action::None);
    assert_eq!(draw(&app, W, H), screen);
}

#[test]
fn hjkl_type_as_characters_in_the_open_editors() {
    // In the free-text editor the four letters append to the buffer, and nothing
    // folds or moves while the editor is open.
    let mut app = open();
    focus_field(&mut app, "model");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "");
    let before = app.overlay.visible();
    let focus = app.overlay.focus;
    for c in ['h', 'j', 'k', 'l'] {
        assert_eq!(press(&mut app, KeyCode::Char(c)), Action::None);
    }
    assert!(app.overlay.editor.is_some());
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "hjkl");
    assert_eq!(app.overlay.focus, focus);
    assert_eq!(app.overlay.visible(), before);
    let screen = draw(&app, W, H);
    assert!(screen.contains("▾ Provider and model"), "{screen}");
    assert!(screen.contains("▾ Pipeline"), "{screen}");

    // In the number editor letters are rejected, exactly as today.
    let mut app = open();
    focus_field(&mut app, "agent_timeout_secs");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    for c in ['h', 'j', 'k', 'l'] {
        assert_eq!(press(&mut app, KeyCode::Char(c)), Action::None);
    }
    assert_eq!(app.overlay.editor.as_ref().unwrap().buffer, "600");
}

#[test]
fn the_list_ends_with_the_last_entry_and_no_enter_button() {
    // Expanded: the last row of the list is the last settings entry, and nothing
    // renders after it but the modal's status line and footer. The 45-row
    // model needs the 48-row list of a 75-row screen.
    let app = open();
    let screen = draw(&app, W, 75);
    assert!(screen.contains("rail mode"), "{screen}");
    assert!(!screen.contains("Close settings"), "{screen}");
    // No line carries a stray Enter button: the only Enter hints are the status
    // bar's "Enter start" and the footer's "Enter/Space edit".
    for line in screen.split('\n') {
        if line.contains("Enter") {
            assert!(
                line.contains("Enter/Space") || line.contains("start"),
                "an Enter button row rendered: {line:?}"
            );
        }
    }
    // The line after the last entry row is blank inside the modal body (the W =
    // 100 modal spans columns 5..=94, its body 6..=93), not a button row.
    let lines: Vec<&str> = screen.split('\n').collect();
    let last = lines
        .iter()
        .position(|line| line.contains("rail mode"))
        .expect("the last entry row is on screen");
    let after: String = lines[last + 1].chars().skip(6).take(88).collect();
    assert!(
        after.trim().is_empty(),
        "a row follows the last entry: {:?}",
        lines[last + 1]
    );

    // All folded: only the headers show, still with no button under them.
    let mut app = open();
    for index in 0..5 {
        focus_header(&mut app, index);
        press(&mut app, KeyCode::Left);
    }
    let screen = draw(&app, W, 60);
    for title in [
        "▸ Provider and model",
        "▸ Pipeline",
        "▸ Timeouts and pauses",
        "▸ Git",
        "▸ Display and theme",
    ] {
        assert!(screen.contains(title), "{screen}");
    }
    assert!(!screen.contains("Close settings"), "{screen}");
}

#[test]
fn navigation_stops_on_the_last_real_entry_without_a_gap() {
    // Down far past the end stops on the last real entry row and stays there.
    let mut app = open();
    for _ in 0..50 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.overlay.focus, app.overlay.visible().len() - 1);
    assert!(matches!(
        app.overlay.focused(),
        Entry::Row(row) if row.key == "rail_mode"
    ));
    // Further presses leave it there.
    press(&mut app, KeyCode::Down);
    assert_eq!(app.overlay.focus, app.overlay.visible().len() - 1);
    let screen = draw(&app, W, H);
    assert!(screen.contains("rail mode"), "{screen}");
    assert!(!screen.contains("Close settings"), "{screen}");

    // j and PageDown clamp to the same last row.
    let mut hjkl = open();
    for _ in 0..50 {
        press(&mut hjkl, KeyCode::Char('j'));
    }
    assert_eq!(hjkl.overlay.focus, hjkl.overlay.visible().len() - 1);
    let mut paged = open();
    for _ in 0..10 {
        press(&mut paged, KeyCode::PageDown);
    }
    assert_eq!(paged.overlay.focus, paged.overlay.visible().len() - 1);

    // All folded: the walk stops on the last header instead.
    let mut folded = open();
    for index in 0..5 {
        focus_header(&mut folded, index);
        press(&mut folded, KeyCode::Left);
    }
    for _ in 0..10 {
        press(&mut folded, KeyCode::Down);
    }
    assert_eq!(folded.overlay.focused(), Entry::Header(4));
}

#[test]
fn the_last_row_still_edits_and_drafts() {
    let mut app = open();
    // Focus the last entry row: rail mode, a tui-schema enum reading "normal"
    // -- the default since T58.1, shown when the key is unset. Enter cycles to
    // the detailed view mode (T89.1). The keys walk the focus there so the
    // offset follows the selection (T136.1).
    let _ = draw(&app, W, H);
    while app.overlay.focus < app.overlay.visible().len() - 1 {
        press(&mut app, KeyCode::PageDown);
    }
    assert!(matches!(
        app.overlay.focused(),
        Entry::Row(row) if row.key == "rail_mode"
    ));
    let screen = draw(&app, W, H);
    assert!(screen.contains("‹ normal ›"), "{screen}");
    assert_eq!(app.tui.rail_mode, patok_core::config::RailMode::Normal);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    // The change lands in the draft, not in the shell's own settings.
    assert_eq!(
        app.overlay.drafts.get("rail_mode"),
        Some(&SettingValue::Str("detailed".into()))
    );
    assert_eq!(app.tui.rail_mode, patok_core::config::RailMode::Normal);
    assert!(app.overlay.dirty());
    // The row shows the drafted choice, with no saved status line.
    let screen = draw(&app, W, H);
    assert!(screen.contains("‹ detailed ›"), "{screen}");
    assert!(!screen.contains("Saved."), "{screen}");
}

/// The settings overlay's and its unsaved-changes dialog's bottom lines
/// (T59.1, T64.1): hints on the left without brackets in the low-emphasis
/// footer colour, the action buttons right-aligned with their bracketed key in
/// the button accent and their label in the theme's foreground, and every
/// button answers a click on its rectangle with exactly its key's action --
/// the overlay's Close runs the Esc close flow (clean closes, dirty opens the
/// dialog) and the dialog's Confirm and Cancel run its Enter and Esc keys.
mod modal_footer {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use patok_core::config::Theme as ThemeKey;
    use patok_tui::{Theme, button_text, footer_button_rects};
    use ratatui::style::Modifier;

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn themed_open(theme: ThemeKey) -> App {
        let mut app = app();
        app.tui.theme = theme;
        app.tui.truecolor = Some(true);
        assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
        app
    }

    /// A dirty overlay with the unsaved-changes dialog open.
    fn dirty_confirm() -> App {
        let mut app = open();
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.dirty());
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.overlay.confirm_open);
        app
    }

    /// Asserts the two zones on one rendered bottom line (T59.1, T64.1): the
    /// hint text opens the line in the footer colour and without brackets, the
    /// buttons sit right-aligned each exactly the shared button text with
    /// its bracketed key in the button accent and its label in the theme's
    /// foreground, at least two blank columns separate the zones, and no cell
    /// of the left zone carries the accent or a bracket.
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

        let rects = footer_button_rects(footer, buttons);
        assert_eq!(rects.len(), buttons.len(), "one rect per button");
        assert_eq!(
            rects.last().expect("buttons exist").right(),
            footer.right(),
            "the buttons are right-aligned"
        );
        for ((key, label), rect) in buttons.iter().zip(&rects) {
            assert_eq!(line(rect.x, rect.right()), button_text(key, label));
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
    fn overlay_bottom_line_two_zones() {
        let app = open();
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.overlay.footer.get(),
            "↑↓ move · Enter/Space edit · ←→ fold/cycle",
            &[("Esc", "Close")],
            &app.theme(),
        );
    }

    #[test]
    fn overlay_bottom_line_two_zones_in_a_palette_theme() {
        let app = themed_open(ThemeKey::TokyoNightDark);
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.overlay.footer.get(),
            "↑↓ move · Enter/Space edit · ←→ fold/cycle",
            &[("Esc", "Close")],
            &Theme::resolve(ThemeKey::TokyoNightDark, Some(true)),
        );
    }

    #[test]
    fn overlay_close_button_answers_clicks() {
        // A clean overlay closes through the Esc path.
        let mut app = open();
        buffer(&app);
        let rects = footer_button_rects(app.overlay.footer.get(), &[("Esc", "Close")]);
        assert_eq!(
            app.on_mouse(click(rects[0].x + 2, rects[0].y)),
            Action::CloseSettings
        );
        assert!(!app.settings_open());

        // A dirty overlay opens the unsaved-changes dialog instead, drafts kept.
        let mut app = open();
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        buffer(&app);
        let rects = footer_button_rects(app.overlay.footer.get(), &[("Esc", "Close")]);
        assert_eq!(
            app.on_mouse(click(rects[0].x + 2, rects[0].y)),
            Action::None
        );
        assert!(app.overlay.confirm_open);
        assert!(app.overlay.dirty());

        // Clicks on the hint zone change nothing; since T136.1 a click inside
        // a list row selects it and runs its Enter -- the Bool row that spot
        // lands on drafts its toggle, like focusing it and pressing Enter.
        let mut app = open();
        buffer(&app);
        let footer = app.overlay.footer.get();
        assert_eq!(app.on_mouse(click(footer.x + 1, footer.y)), Action::None);
        assert!(app.settings_open());
        assert_eq!(
            app.on_mouse(click(footer.x + 3, footer.y - 10)),
            Action::None
        );
        assert!(app.settings_open());
        let clicked = app.overlay.focused();
        assert!(matches!(clicked, Entry::Row(row) if row.kind == FieldKind::Bool));
        assert_eq!(
            app.overlay.drafts.get(match clicked {
                Entry::Row(row) => row.key,
                _ => "",
            }),
            Some(&SettingValue::Bool(true))
        );
    }

    #[test]
    fn confirm_bottom_line_two_zones() {
        let app = dirty_confirm();
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.overlay.confirm_footer.get(),
            "↑↓ move",
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
            &app.theme(),
        );
    }

    #[test]
    fn confirm_bottom_line_two_zones_in_a_palette_theme() {
        let mut app = themed_open(ThemeKey::TokyoNightDark);
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.overlay.confirm_footer.get(),
            "↑↓ move",
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
            &Theme::resolve(ThemeKey::TokyoNightDark, Some(true)),
        );
    }

    #[test]
    fn confirm_buttons_answer_clicks() {
        // Enter on Save asks the driver to apply every draft.
        let mut app = dirty_confirm();
        buffer(&app);
        let rects = footer_button_rects(
            app.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::SaveSettings
        );
        assert!(!app.overlay.confirm_open);
        assert!(app.settings_open());

        // Enter on Discard throws the drafts away and closes the overlay.
        let mut app = dirty_confirm();
        press(&mut app, KeyCode::Down);
        buffer(&app);
        let rects = footer_button_rects(
            app.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::CloseSettings
        );
        assert!(!app.settings_open());
        assert!(app.overlay.drafts.is_empty());

        // Enter on Cancel returns to the overlay with the drafts intact.
        let mut app = dirty_confirm();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        buffer(&app);
        let rects = footer_button_rects(
            app.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::None
        );
        assert!(!app.overlay.confirm_open);
        assert!(app.settings_open());
        assert!(app.overlay.dirty());

        // Esc goes back to the overlay the same way.
        let mut app = dirty_confirm();
        buffer(&app);
        let rects = footer_button_rects(
            app.overlay.confirm_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Cancel")],
        );
        assert_eq!(
            app.on_mouse(click(rects[1].x + 2, rects[1].y)),
            Action::None
        );
        assert!(!app.overlay.confirm_open);
        assert!(app.settings_open());
        assert!(app.overlay.dirty());
    }
}

/// The overlay's and its unsaved-changes dialog's close buttons (T66.1): the
/// shared ` [ x ] ` in the theme's button accent on each title row, inside
/// the modal and clear of its title, and a click on its rectangle runs
/// exactly that window's Esc key -- a clean overlay closes, a dirty one opens
/// the unsaved-changes dialog, and the dialog's button returns to the overlay
/// with the drafts intact.
mod close_button {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use patok_tui::{
        CLOSE_BUTTON_WIDTH, Theme, button_text, close_button_rect, settings_confirm_area,
    };

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// A dirty overlay with the unsaved-changes dialog open.
    fn dirty_confirm() -> App {
        let mut app = open();
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.dirty());
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.overlay.confirm_open);
        app
    }

    /// Asserts one modal's title row: the close button sits flush inside the
    /// modal's top-right corner, spells the shared button text with every
    /// cell in the theme's button accent, and the title survives to its left.
    fn assert_close_button(
        buffer: &ratatui::buffer::Buffer,
        modal: ratatui::layout::Rect,
        title: &str,
        theme: &Theme,
    ) -> ratatui::layout::Rect {
        let close = close_button_rect(modal);
        assert_ne!(close, ratatui::layout::Rect::default());
        assert_eq!(close.y, modal.y, "on the title row");
        assert_eq!(
            close.right(),
            modal.right() - 1,
            "one column short of the top-right corner"
        );
        let line = |from: u16, to: u16| -> String {
            (from..to).map(|x| buffer[(x, close.y)].symbol()).collect()
        };
        assert_eq!(line(close.x, close.right()), button_text("x", ""));
        for i in 0..CLOSE_BUTTON_WIDTH {
            assert_eq!(
                buffer[(close.x + i, close.y)].style().fg,
                Some(theme.highlighted_text),
                "button cell {i} wears the accent"
            );
        }
        let title_row = line(modal.x + 1, close.x);
        assert!(
            title_row.contains(title),
            "title {title:?} intact in {title_row:?}"
        );
        close
    }

    #[test]
    fn overlay_shows_the_close_button_top_right() {
        let app = open();
        let buffer = buffer(&app);
        let modal = settings_area(ratatui::layout::Rect::new(0, 0, W, H));
        assert_close_button(&buffer, modal, "Settings -- Patok", &app.theme());
        assert_eq!(app.overlay.close.get(), close_button_rect(modal));
    }

    #[test]
    fn confirm_dialog_shows_the_close_button_top_right() {
        let app = dirty_confirm();
        let buffer = buffer(&app);
        // Both windows are open, so both title rows carry the button.
        let overlay = settings_area(ratatui::layout::Rect::new(0, 0, W, H));
        assert_close_button(&buffer, overlay, "Settings -- Patok", &app.theme());
        let confirm = settings_confirm_area(ratatui::layout::Rect::new(0, 0, W, H));
        assert_close_button(&buffer, confirm, "Unsaved changes", &app.theme());
        assert_eq!(app.overlay.confirm_close.get(), close_button_rect(confirm));
    }

    /// At the modal's minimum size (80x24, the full screen when clamped) the
    /// centered title and the button still do not collide.
    #[test]
    fn close_button_fits_the_overlay_at_its_minimum_width() {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, &app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_close_button(
            &buffer,
            ratatui::layout::Rect::new(0, 0, 80, 24),
            "Settings -- Patok",
            &app.theme(),
        );
    }

    #[test]
    fn overlay_close_button_click_runs_esc() {
        // A clean overlay closes through the Esc path.
        let mut app = open();
        buffer(&app);
        let close = app.overlay.close.get();
        assert_eq!(
            app.on_mouse(click(close.x + 3, close.y)),
            Action::CloseSettings
        );
        assert!(!app.settings_open());

        // A dirty overlay opens the unsaved-changes dialog instead, drafts kept.
        let mut app = open();
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        buffer(&app);
        let close = app.overlay.close.get();
        assert_eq!(app.on_mouse(click(close.x + 3, close.y)), Action::None);
        assert!(app.settings_open());
        assert!(app.overlay.confirm_open);
        assert_eq!(app.overlay.confirm_selected, 0);
        assert!(app.overlay.dirty());

        // Clicks on the title row outside the button change nothing; since
        // T136.1 a click inside a list row selects it and runs its Enter --
        // the enum row that spot lands on cycles and drafts its next choice.
        let mut app = open();
        buffer(&app);
        let close = app.overlay.close.get();
        assert_eq!(app.on_mouse(click(close.x - 1, close.y)), Action::None);
        assert!(app.settings_open());
        assert_eq!(app.on_mouse(click(close.x + 3, close.y + 10)), Action::None);
        assert!(app.settings_open());
        let clicked = app.overlay.focused();
        assert!(matches!(clicked, Entry::Row(row) if matches!(row.kind, FieldKind::Enum(_))));
        assert!(app.overlay.dirty());
    }

    #[test]
    fn confirm_close_button_click_returns_to_the_overlay() {
        let mut app = dirty_confirm();
        buffer(&app);
        let close = app.overlay.confirm_close.get();
        assert_eq!(app.on_mouse(click(close.x + 3, close.y)), Action::None);
        assert!(!app.overlay.confirm_open);
        assert!(app.settings_open());
        assert!(app.overlay.dirty(), "the drafts survive the round trip");

        // Esc from the dialog takes the identical path.
        let mut app = dirty_confirm();
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.overlay.confirm_open);
        assert!(app.settings_open());
        assert!(app.overlay.dirty());

        // While the dialog is open, the overlay's button behind it is inert:
        // like Esc, the click acts on the topmost window only.
        let mut app = dirty_confirm();
        buffer(&app);
        let overlay_close = app.overlay.close.get();
        assert_eq!(
            app.on_mouse(click(overlay_close.x + 3, overlay_close.y)),
            Action::None
        );
        assert!(app.overlay.confirm_open, "the dialog stays open");
        assert!(app.settings_open());
    }
}

/// The selection-bearing modals' selected row (T118.1): the focused row wears
/// the highlighted-text foreground on the normal modal background, bold, in
/// the settings overlay's list and the unsaved-changes dialog alike; every
/// unselected row keeps its current colours.
mod selected_row_colours {
    use super::*;
    use patok_tui::{SETTINGS_CHOICES, settings_confirm_area};
    use ratatui::layout::{Constraint, Layout, Rect};
    use ratatui::style::Modifier;

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The settings list's rect: the overlay's inner area minus the status
    /// line and the footer, with the help box under the list at this width
    /// (T85.1) -- the same math `render_settings_overlay` runs.
    fn list_rect() -> Rect {
        let area = settings_area(Rect::new(0, 0, W, H));
        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let [body, _status, _footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        settings_body_areas(body).0
    }

    /// Asserts every rendered settings row's colours (T118.1): the focused
    /// row's text cells wear the highlighted-text foreground on the normal
    /// background, bold; no other row's cell does; an unfocused section
    /// header keeps its bold. The list's last column holds the scrollbar, so
    /// the sweep stops short of it.
    fn assert_list_colours(buffer: &ratatui::buffer::Buffer, app: &App) {
        let theme = app.theme();
        let list = list_rect();
        let start = app.overlay.scroll.get();
        for (index, entry) in app.overlay.visible().iter().enumerate() {
            let row = index.saturating_sub(start);
            if row >= usize::from(list.height) {
                break;
            }
            let y = list.y + row as u16;
            for x in list.x..list.right() - 1 {
                let cell = &buffer[(x, y)];
                if cell.symbol() == " " {
                    continue;
                }
                if index == app.overlay.focus {
                    assert_eq!(
                        cell.style().fg,
                        Some(theme.highlighted_text),
                        "focused row {index}'s cell ({x}, {y}) wears the highlight"
                    );
                    assert_eq!(
                        cell.bg, theme.background,
                        "focused row {index}'s cell ({x}, {y}) keeps the modal background"
                    );
                    assert!(
                        cell.style().add_modifier.contains(Modifier::BOLD),
                        "focused row {index}'s cell ({x}, {y}) stays bold"
                    );
                } else {
                    assert_ne!(
                        cell.style().fg,
                        Some(theme.highlighted_text),
                        "row {index}'s cell ({x}, {y}) is not highlighted"
                    );
                    if matches!(entry, Entry::Header(_)) {
                        assert!(
                            cell.style().add_modifier.contains(Modifier::BOLD),
                            "unfocused header {index}'s cell ({x}, {y}) stays bold"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_focused_settings_row_wears_the_highlighted_text_colour() {
        // The focus starts on the first section header.
        let mut app = open();
        assert!(matches!(app.overlay.focused(), Entry::Header(_)));
        assert_list_colours(&buffer(&app), &app);
        // One Down moves the selection onto the first entry row, a read-only
        // report whose muted colour switches to the highlight too.
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert!(matches!(app.overlay.focused(), Entry::Row(_)));
        assert_list_colours(&buffer(&app), &app);
    }

    /// A dirty overlay with the unsaved-changes dialog open.
    fn dirty_confirm() -> App {
        let mut app = open();
        focus_field(&mut app, "plan_enabled");
        press(&mut app, KeyCode::Enter);
        assert!(app.overlay.dirty());
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.overlay.confirm_open);
        app
    }

    /// The confirm dialog's body rect: the modal's inner area minus the
    /// footer -- the same math `render_settings_confirm` runs.
    fn confirm_body() -> Rect {
        let area = settings_confirm_area(Rect::new(0, 0, W, H));
        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let [body, _footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        body
    }

    /// Asserts the confirm dialog's three rows (T118.1): the selected row's
    /// marker, label and detail cells wear the highlight on the normal
    /// background, bold; the unselected rows keep the label on the theme's
    /// foreground and the detail in the muted colour.
    fn assert_confirm_colours(buffer: &ratatui::buffer::Buffer, app: &App) {
        let theme = app.theme();
        let body = confirm_body();
        for (index, choice) in SETTINGS_CHOICES.iter().enumerate() {
            let y = body.y + index as u16;
            let selected = index == app.overlay.confirm_selected;
            let text = format!(
                "{}{} -- {}",
                if selected { "▶ " } else { "  " },
                choice.label(),
                choice.detail()
            );
            let label_end = 2 + choice.label().chars().count();
            for (i, _) in text.chars().enumerate() {
                let cell = &buffer[(body.x + i as u16, y)];
                if cell.symbol() == " " {
                    continue;
                }
                if selected {
                    assert_eq!(
                        cell.style().fg,
                        Some(theme.highlighted_text),
                        "selected choice {index}'s cell {i} wears the highlight"
                    );
                    assert_eq!(
                        cell.bg, theme.background,
                        "selected choice {index}'s cell {i} keeps the modal background"
                    );
                    assert!(
                        cell.style().add_modifier.contains(Modifier::BOLD),
                        "selected choice {index}'s cell {i} stays bold"
                    );
                } else {
                    let expected = if i < label_end {
                        theme.foreground
                    } else {
                        theme.muted_text
                    };
                    assert_eq!(
                        cell.style().fg,
                        Some(expected),
                        "unselected choice {index}'s cell {i} keeps its colour"
                    );
                }
            }
        }
    }

    #[test]
    fn the_confirm_dialogs_selected_choice_wears_the_highlighted_text_colour() {
        let mut app = dirty_confirm();
        assert_eq!(app.overlay.confirm_selected, 0);
        assert_confirm_colours(&buffer(&app), &app);
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_confirm_colours(&buffer(&app), &app);
    }
}
