//! Rendering. Reads the [`App`], draws a frame, never blocks on I/O.

pub(crate) mod choice;
mod common;
mod dialogs;
pub(crate) mod modal;
mod output;
mod settings;
mod status;
mod tasks;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::Block;

use crate::app::{App, FrameFocus, Modal};
use crate::pipeline::{rail_width, render_rail};

pub use common::fitted_hints;
pub use dialogs::{DIALOG_WATERMARK, dialog_area, stop_area, theme_area};
pub use output::{format_elapsed, output_title, output_title_parts, output_title_spans};
pub use settings::{
    SETTINGS_HELP_BESIDE_WIDTH, SETTINGS_HELP_PANEL_HEIGHT, SETTINGS_HELP_PANEL_WIDTH,
    settings_area, settings_body_areas, settings_confirm_area,
};

use common::base_style;
use dialogs::{render_dialog, render_menu, render_stop_dialog, render_theme_modal};
use output::render_output;
use settings::{render_settings_confirm, render_settings_overlay};
use status::status_widget;
use tasks::render_tasks;

pub fn render(frame: &mut Frame, app: &App) {
    // The theme's base style first: it paints the shell's background and
    // foreground over the whole frame, so a light theme recolours everything
    // while unstyled text (plain output lines, task rows, borders, titles)
    // inherits the base foreground. For the default theme both colours are
    // `Reset`, so this is a no-op.
    frame.render_widget(Block::new().style(base_style(app.theme())), frame.area());
    let [body, status] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());

    // The pipeline rail of T50.1: the one
    // rendering of the engine-reported state, drawn in every engine state
    // (T53.1) -- a narrow fixed-width vertical rail to the left of the
    // frames, so nothing about it changes when the engine moves between
    // idle and running. The rail's fixed `Length` constraint makes its edge
    // not draggable, and nothing in the app moves a layout edge. The width
    // follows the rail mode (T57.1): the letter tiles' narrow column in
    // compact mode, the full-name boxes' wider one in normal mode -- read
    // live from the tui settings, so a mode change relayouts the next frame.
    let [rail, frames] = Layout::horizontal([
        Constraint::Length(rail_width(app.tui.rail_mode)),
        Constraint::Min(3),
    ])
    .areas(body);
    render_rail(frame, app, rail);
    // One merged view (T30.1): the agent output frame stacked above the task list
    // frame, sized by the frame focus — the focused frame takes all the free space,
    // the other shrinks to 5 content lines.
    let [output, tasks] = Layout::vertical(frame_constraints(app.focus)).areas(frames);
    app.layout.output_area.set(output);
    app.layout.tasks_area.set(tasks);
    render_output(frame, app, output);
    render_tasks(frame, app, tasks);
    frame.render_widget(status_widget(app, status), status);
    // The open modals on top, bottom layer first (finding 20): the order is
    // the shared [`Modal::PRECEDENCE`] table reversed -- the same table the
    // key and mouse dispatch derive the winning modal from -- so the
    // highest-precedence modal ends up on top and which modal wins when
    // several would be open is decided in one place.
    for modal in Modal::PRECEDENCE.iter().rev().copied() {
        if !modal.is_open(app) {
            continue;
        }
        match modal {
            Modal::Overlay => render_settings_overlay(frame, app),
            Modal::Confirm => render_settings_confirm(frame, app),
            Modal::Dialog => render_dialog(frame, app),
            Modal::Menu => render_menu(frame, app),
            Modal::Stop => render_stop_dialog(frame, app),
            Modal::Theme => render_theme_modal(frame, app),
        }
    }
}

/// The merged body's vertical split: the agent output frame above the task list
/// frame, with the focused frame taking all the free space and the other fixed at
/// 7 rows (5 content lines plus its border) — never an equal percentage split.
pub fn frame_constraints(focus: FrameFocus) -> [Constraint; 2] {
    match focus {
        FrameFocus::Output => [Constraint::Min(1), Constraint::Length(7)],
        FrameFocus::Tasks => [Constraint::Length(7), Constraint::Min(1)],
    }
}

#[cfg(test)]
mod tests {
    use super::common::{render_scrollbar, selected_row_style};
    use super::output::visual_lines;
    use super::settings::settings_row_line;
    use super::status::status_widget;
    use super::*;
    use crate::app::{LineKind, OutLine, agent_display, finished_line, started_line};
    use crate::hit::{button_text, confirm_row_at, stop_row_at};
    use crate::overlay::{Editor, Entry, FieldKind, SECTIONS};
    use crate::theme::Theme;
    use patok_core::config::{THEME_KEYS, Theme as ThemeKey};
    use patok_core::event::SessionOutcome;
    use patok_core::event::{EngineEvent, Phase, Snapshot};
    use patok_core::pipeline::PipelineState;
    use patok_core::task::Task;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Modifier;
    use ratatui::widgets::Widget;
    use std::time::Duration;

    /// Every built-in theme variant, in `THEME_KEYS` order.
    fn every_key() -> Vec<ThemeKey> {
        THEME_KEYS
            .iter()
            .map(|key| ThemeKey::parse(key).expect("THEME_KEYS holds valid names"))
            .collect()
    }

    /// A minimal app with one pending task, in the idle phase.
    fn app_with_one_task() -> App {
        App::new(
            Snapshot {
                project_dir: String::new(),
                phase: Phase::Startup,
                tasks: vec![Task {
                    id: "T1.1".into(),
                    origin: Some('T'),
                    description: String::new(),
                    done: false,
                    line: 1,
                    raw: String::new(),
                }],
                current_task: None,
                planning: false,
                discovering: false,
                provider: String::new(),
                model: String::new(),
                settings: Default::default(),
                pipeline: PipelineState::today(),
                recent: vec![],
            },
            "test".into(),
        )
    }

    /// The output frame title is the one surface keeping the fixated
    /// per-agent name colour (T117.1), on every built-in theme: the name span
    /// carries it (bold), every other span stays muted detail.
    #[test]
    fn output_frame_title_keeps_the_agent_colour_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            for agent in ["planner", "builder", "research"] {
                let colour = Theme::agent_name_color(theme, agent);
                let (left, right) = output_title_spans(
                    &agent_display(agent),
                    "Mock",
                    "mock-model",
                    Some("01:23"),
                    80,
                    colour,
                    theme.muted_text,
                );
                let name = &left[0];
                assert_eq!(name.style.fg, Some(colour), "{key:?}/{agent}");
                assert!(
                    name.style.add_modifier.contains(Modifier::BOLD),
                    "{key:?}/{agent}"
                );
                for span in left[1..].iter().chain(&right) {
                    assert_eq!(span.style.fg, Some(theme.muted_text), "{key:?}/{agent}");
                }
            }
        }
    }

    /// A pane line that names an agent wears its line kind's colour in full on
    /// every built-in theme (T117.1): the lifecycle status lines in the
    /// pane-status colour, the task heading in the heading colour (bold),
    /// and the name is never dropped or restyled on its own.
    #[test]
    fn agent_name_lines_wear_the_kind_colour_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            for (kind, text) in [
                (LineKind::Status, started_line("builder", "mock", None)),
                (
                    LineKind::Status,
                    finished_line("builder", SessionOutcome::Finished, Duration::from_secs(45)),
                ),
                (LineKind::Heading, "── T1.1: add a login page".to_string()),
            ] {
                let colour = Theme::line_color(theme, kind);
                let rows = visual_lines(
                    theme,
                    &OutLine {
                        kind,
                        text: text.clone(),
                    },
                    40,
                );
                let mut joined = String::new();
                for row in &rows {
                    for span in &row.spans {
                        // The kind's colour rides the line's style, patched by
                        // the span's own; the agent's name inherits it like the
                        // rest of the line instead of carrying its own colour.
                        let effective = row.style.patch(span.style);
                        assert_eq!(effective.fg, Some(colour), "{key:?}/{kind:?}: {text:?}");
                        assert_eq!(
                            effective.add_modifier.contains(Modifier::BOLD),
                            kind == LineKind::Heading,
                            "{key:?}/{kind:?}: {text:?}"
                        );
                        joined.push_str(&span.content);
                    }
                }
                assert_eq!(joined, text, "{key:?}/{kind:?}");
            }
        }
    }

    /// The status bar's status message wears the highlighted-text colour in
    /// full on every built-in theme (T117.1) — never the fixated per-agent
    /// name colour.
    #[test]
    fn status_messages_wear_the_highlighted_colour_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            app.run.planning = true;
            app.run.agent = "planner".into();
            let message = "The planner is already running.";
            app.status = Some(message.into());
            let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 1));
            status_widget(&app, buffer.area).render(buffer.area, &mut buffer);
            let row: String = (0..100).map(|x| buffer[(x, 0)].symbol()).collect();
            // A byte index would mis-column on multibyte symbols, so the
            // match's column is the character count before it.
            let x = row
                .find(message)
                .map(|at| row[..at].chars().count())
                .unwrap_or_else(|| panic!("{message:?} on {key:?}")) as u16;
            for (i, _) in message.chars().enumerate() {
                assert_eq!(
                    buffer[(x + i as u16, 0)].style().fg,
                    Some(theme.highlighted_text),
                    "cell ({}, 0) on {key:?}",
                    x + i as u16
                );
            }
        }
    }

    /// The status bar's interactive chips render exactly as modal buttons on
    /// every built-in theme (T133.1, T134.1, T141.1): the idle Enter button
    /// and the m menu button -- and the running Esc stop button -- spell the
    /// shared [`button_text`] form, with the brackets and the key in the
    /// button accent and the label in the regular foreground, no background,
    /// no bold -- the shared [`button_line`] look.
    #[test]
    fn the_interactive_chips_render_as_modal_buttons_on_every_theme() {
        // Draws the app's status row and checks one button: the row carries
        // its exact [`button_text`], every cell of the bracketed key span
        // wears the button accent, every cell of the label span the regular
        // foreground, none with a chip background or bold.
        fn assert_button(theme: Theme, key: ThemeKey, app: &App, key_name: &str, label: &str) {
            let button = button_text(key_name, label);
            let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 1));
            status_widget(app, buffer.area).render(buffer.area, &mut buffer);
            let row: String = (0..100).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(row.contains(&button), "the {button:?} button on {key:?}");
            // A byte index would mis-column on multibyte symbols, so the
            // match's column is the character count before it.
            let key_span = format!(" [ {key_name} ] ");
            let label_span = format!("{label} ");
            for (name, span, expected) in [
                ("key", key_span, theme.highlighted_text),
                ("label", label_span, theme.normal_text),
            ] {
                let x = row
                    .find(&span)
                    .map(|at| row[..at].chars().count())
                    .unwrap_or_else(|| panic!("the {span:?} {name} on {key:?}"))
                    as u16;
                for column in x..x + span.chars().count() as u16 {
                    let style = buffer[(column, 0)].style();
                    assert_eq!(
                        style.fg,
                        Some(expected),
                        "the {span:?} {name} cell ({column}, 0) on {key:?}"
                    );
                    // The button look: no chip background, no bold -- the
                    // bare buffer's cells carry Reset backgrounds.
                    assert_ne!(
                        style.bg,
                        Some(theme.chip_neutral),
                        "cell ({column}, 0) on {key:?}"
                    );
                    assert!(
                        !style.add_modifier.contains(Modifier::BOLD),
                        "cell ({column}, 0) on {key:?}"
                    );
                }
            }
        }
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            // Idle: the Enter `start` button and the m `menu` button.
            assert_eq!(app.run.phase, Phase::Startup);
            assert_button(theme, key, &app, "Enter", "start");
            assert_button(theme, key, &app, "m", "menu");
            // Running: the Esc `stop` button joins them.
            app.apply(EngineEvent::PhaseChanged {
                phase: Phase::Running,
            });
            assert_button(theme, key, &app, "Esc", "stop");
            assert_button(theme, key, &app, "m", "menu");
        }
    }

    /// The shared selected-row style (T118.1) on every built-in theme: the
    /// highlighted-text foreground on the normal background, bold -- only the
    /// foreground switches, the background never does.
    #[test]
    fn selected_row_style_wears_highlighted_text_on_the_normal_background() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let style = selected_row_style(theme);
            assert_eq!(style.fg, Some(theme.highlighted_text), "{key:?}");
            assert_eq!(style.bg, Some(theme.background), "{key:?}");
            assert!(
                style.add_modifier.contains(Modifier::BOLD),
                "the selected row stays bold on {key:?}"
            );
        }
    }

    /// A thumb cell and a rail cell from a deterministic overflow: body
    /// 20x6, `start = 0`, `total = 18` gives a thumb of 2 rows at the top,
    /// so `(19, 0)` is the thumb and `(19, 3)` the rail.
    fn scrollbar_cells(theme: Theme) -> (ratatui::buffer::Cell, ratatui::buffer::Cell) {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        terminal
            .draw(|frame| render_scrollbar(frame, theme, Rect::new(0, 0, 20, 6), 0, 18))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (buffer[(19, 0)].clone(), buffer[(19, 3)].clone())
    }

    /// The scrollbar thumb wears the `highlighted_text` class and the rail its
    /// `scrollbar_rail` class on every built-in theme, and the two stay
    /// distinct so the thumb reads against the rail (T121.1).
    #[test]
    fn scrollbar_thumb_wears_highlighted_text_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let (thumb, rail) = scrollbar_cells(theme);
            assert_eq!(thumb.symbol(), "█", "{key:?}");
            assert_eq!(thumb.style().fg, Some(theme.highlighted_text), "{key:?}");
            assert_eq!(rail.symbol(), "│", "{key:?}");
            assert_eq!(rail.style().fg, Some(theme.scrollbar_rail), "{key:?}");
            assert_ne!(
                theme.highlighted_text, theme.scrollbar_rail,
                "the thumb must stay distinct from the rail on {key:?}"
            );
        }
    }

    /// Switching the active theme recolours the scrollbar thumb: no two
    /// built-in themes share a thumb colour.
    #[test]
    fn switching_themes_recolours_the_scrollbar_thumb() {
        let thumbs: Vec<_> = every_key()
            .into_iter()
            .map(|key| {
                scrollbar_cells(Theme::resolve(key, Some(true)))
                    .0
                    .style()
                    .fg
            })
            .collect();
        for (i, left) in thumbs.iter().enumerate() {
            for right in &thumbs[i + 1..] {
                assert_ne!(
                    left, right,
                    "two built-in themes share a scrollbar thumb colour"
                );
            }
        }
    }

    /// The dialog input's cells on an 80x24 screen, rendered through
    /// `render_dialog` with the app's input state as the caller set it.
    fn dialog_buffer(app: &App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render_dialog(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The add-task dialog input's block cursor wears the `highlighted_text`
    /// class on every built-in theme (T138.1) — the colour selected rows and
    /// the scrollbar thumb already share — both over a typed character and as
    /// the styled space in the empty-input watermark row, with no accent
    /// background left behind.
    #[test]
    fn dialog_input_cursor_wears_highlighted_text_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let area = dialog_area(Rect::new(0, 0, 80, 24));
            let (x0, y0) = (area.x + 1, area.y + 1);
            // With text: the cursor cell covers the character under it.
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            app.dialog.open = true;
            app.dialog.editor.set_text("hello");
            app.dialog.editor.set_cursor(3);
            let buffer = dialog_buffer(&app);
            let under = &buffer[(x0 + 3, y0)];
            assert_eq!(under.symbol(), "l", "{key:?}");
            assert_eq!(under.style().fg, Some(theme.highlighted_text), "{key:?}");
            assert_ne!(under.style().bg, Some(theme.accent), "{key:?}");
            let neighbour = &buffer[(x0 + 1, y0)];
            assert_eq!(neighbour.symbol(), "e", "{key:?}");
            // The neighbour is typed text, not the cursor: it never wears the
            // highlighted-text foreground or an accent background.
            assert_ne!(
                neighbour.style().fg,
                Some(theme.highlighted_text),
                "the neighbour stays plain text: {:?} on {key:?}",
                neighbour.style().fg
            );
            assert_ne!(neighbour.style().bg, Some(theme.accent), "{key:?}");
            // Empty input: the watermark in the muted colour (T41.1), then
            // the block cursor's styled space.
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            app.dialog.open = true;
            let buffer = dialog_buffer(&app);
            for (i, ch) in DIALOG_WATERMARK.chars().enumerate() {
                let cell = &buffer[(x0 + i as u16, y0)];
                assert_eq!(cell.symbol(), ch.to_string(), "{key:?}");
                assert_eq!(cell.style().fg, Some(theme.muted_text), "{key:?}");
            }
            let width = DIALOG_WATERMARK.chars().count() as u16;
            let cursor = &buffer[(x0 + width, y0)];
            assert_eq!(cursor.symbol(), " ", "{key:?}");
            assert_eq!(cursor.style().fg, Some(theme.highlighted_text), "{key:?}");
            assert_ne!(cursor.style().bg, Some(theme.accent), "{key:?}");
        }
    }

    /// The unsaved-changes dialog's row hit-test (T146.1): the first three
    /// body rows are the three choices, the padding below them and every
    /// position outside the body miss.
    #[test]
    fn confirm_row_at_hits_only_the_three_choice_rows() {
        let body = Rect::new(20, 5, 40, 6);
        for row in 0..3u16 {
            assert_eq!(
                confirm_row_at(Position::new(20, body.y + row), body),
                Some(row as usize)
            );
        }
        assert_eq!(
            confirm_row_at(Position::new(20, body.y + 3), body),
            None,
            "the blank padding below the choices misses"
        );
        assert_eq!(
            confirm_row_at(Position::new(20, body.y + 5), body),
            None,
            "the body's last padding row misses"
        );
        assert_eq!(
            confirm_row_at(Position::new(20, body.y - 1), body),
            None,
            "above the body misses"
        );
        assert_eq!(
            confirm_row_at(Position::new(19, body.y), body),
            None,
            "left of the body misses"
        );
        assert_eq!(
            confirm_row_at(Position::new(20, body.y), Rect::default()),
            None,
            "a zero rect contains no position"
        );
    }

    /// The stop dialog's row hit-test (T148.1): the first three body rows
    /// are the three choices, the padding below them and every position
    /// outside the body miss.
    #[test]
    fn stop_row_at_hits_only_the_three_choice_rows() {
        let body = Rect::new(20, 5, 40, 6);
        for row in 0..3u16 {
            assert_eq!(
                stop_row_at(Position::new(20, body.y + row), body),
                Some(row as usize)
            );
        }
        assert_eq!(
            stop_row_at(Position::new(20, body.y + 3), body),
            None,
            "the blank padding below the choices misses"
        );
        assert_eq!(
            stop_row_at(Position::new(20, body.y + 5), body),
            None,
            "the body's last padding row misses"
        );
        assert_eq!(
            stop_row_at(Position::new(20, body.y - 1), body),
            None,
            "above the body misses"
        );
        assert_eq!(
            stop_row_at(Position::new(19, body.y), body),
            None,
            "left of the body misses"
        );
        assert_eq!(
            stop_row_at(Position::new(20, body.y), Rect::default()),
            None,
            "a zero rect contains no position"
        );
    }

    /// The settings overlay inline editor's end-of-buffer block cursor wears
    /// the `highlighted_text` class on every built-in theme (T138.1) instead
    /// of the REVERSED modifier, mirroring the selected-row and scrollbar
    /// style tests.
    #[test]
    fn inline_editor_cursor_wears_highlighted_text_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            app.overlay.editor = Some(Editor {
                field: "model",
                kind: FieldKind::Text,
                buffer: "abc".into(),
            });
            let row = SECTIONS
                .iter()
                .flat_map(|section| section.rows.iter())
                .find(|row| row.key == "model")
                .expect("model is a settings row");
            // Index 0 matches the overlay's default focus, so the row renders
            // focused like the real open-editor render does.
            let line = settings_row_line(&app, 0, &Entry::Row(row), 40);
            let cursor = line.spans.last().expect("the editor cursor span");
            assert_eq!(cursor.content, " ", "{key:?}");
            assert_eq!(cursor.style.fg, Some(theme.highlighted_text), "{key:?}");
            assert!(
                cursor.style.add_modifier.contains(Modifier::REVERSED),
                "the cursor stays a visible block on {key:?}"
            );
        }
    }
}
