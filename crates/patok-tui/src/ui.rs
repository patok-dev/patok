//! Rendering. Reads the [`App`], draws a frame, never blocks on I/O.

use patok_core::config::SettingValue;
use patok_core::event::{Phase, SessionOutcome};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use std::time::Duration;

use crate::app::{App, DialogKind, FrameFocus, LineKind, OutLine, Pane, STOP_CHOICES};
use crate::markdown::markdown_lines;
use crate::overlay::{
    Entry, FieldKind, Row, SECTIONS, SETTINGS_CHOICES, StatusLevel, scroll_offset,
};
use crate::pipeline::{rail_width, render_rail};
use crate::theme::{Theme, theme_modal_groups};

/// Entry rows indent this many spaces past the focus marker, nesting them under
/// their section header; headers keep the bare marker margin.
const ENTRY_INDENT: usize = 4;

/// The placeholder hint rendered in the empty input area of the add-task dialog
/// (T41.1): a low-emphasis watermark that reads as a hint, not typed content.
/// It disappears with the first typed or pasted character and never enters
/// `dialog_text` or a submitted request.
pub const DIALOG_WATERMARK: &str = "Press Enter to run Discovery agent";

/// The inject-task modal's empty-input watermark (T76.1), in the place of the
/// add-task dialog's: it says what the confirm does with the typed text, so
/// the normalized append is never a surprise.
pub const INJECT_WATERMARK: &str = "Type the task; it lands as an unchecked task line";

/// The theme's base style: the shell's background and foreground. Painting it
/// over an area recolours that area (and everything unstyled rendered into it)
/// for the built-in themes, and is a no-op for the default one.
fn base_style(theme: Theme) -> Style {
    Style::new().fg(theme.foreground).bg(theme.background)
}

/// The selected row's style shared by every selection-bearing modal (T118.1):
/// the highlighted-text foreground on the normal modal background, bold --
/// only the foreground switches, the background never does.
fn selected_row_style(theme: Theme) -> Style {
    base_style(theme)
        .fg(theme.highlighted_text)
        .add_modifier(Modifier::BOLD)
}

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
    app.output_area.set(output);
    app.tasks_area.set(tasks);
    render_output(frame, app, output);
    render_tasks(frame, app, tasks);
    frame.render_widget(status_widget(app, usize::from(status.width)), status);
    if app.settings_open() {
        render_settings_overlay(frame, app);
    }
    if app.overlay.confirm_open {
        render_settings_confirm(frame, app);
    }
    if app.dialog_open {
        render_dialog(frame, app);
    }
    if app.stop_open {
        render_stop_dialog(frame, app);
    }
    if app.theme_modal.open {
        render_theme_modal(frame, app);
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

/// Which of the merged view's two frames the pointer is over; `None` outside both
/// (status line, past the right edge). The rects are adjacent and never
/// overlap, so the first match wins; `Rect::contains` is half-open, so the
/// boundary row between the frames belongs to exactly one of them.
pub fn frame_at(position: Position, output: Rect, tasks: Rect) -> Option<FrameFocus> {
    if output.contains(position) {
        Some(FrameFocus::Output)
    } else if tasks.contains(position) {
        Some(FrameFocus::Tasks)
    } else {
        None
    }
}

/// The settings overlay rect: Large --
/// 90% of the width and 80% of the height, at least 80x24, clamped to the screen and
/// centered.
pub fn settings_area(screen: Rect) -> Rect {
    let width = (screen.width * 9 / 10).max(80).min(screen.width);
    let height = (screen.height * 4 / 5).max(24).min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// One modal button's display text (T44.1, extended by T59.1): ` [ Key ] Label `
/// -- the key bracketed in the button style, a one-word label, one space of
/// padding inside the brackets and one either side -- or ` [ Key ] ` when the
/// button has no label. Shared by the renderer, the hit-test and the tests, so
/// every button's rectangle is agreed on by construction.
pub fn button_text(key: &str, label: &str) -> String {
    if label.is_empty() {
        format!(" [ {key} ] ")
    } else {
        format!(" [ {key} ] {label} ")
    }
}

/// Every modal's close button (T66.1): the display width of [`button_text`]
/// with the bare `x` key -- the seven columns of ` [ x ] `, shared by the
/// renderer and the mouse hit-test so the drawn button and its rectangle
/// agree by construction.
pub const CLOSE_BUTTON_WIDTH: u16 = 7;

/// A modal's top-right close button (T66.1): the bracketed ` [ x ] ` in the
/// theme's button accent colour, foreground only -- drawn through the shared
/// [`button_line`] on every modal's title row, so it recolours with the
/// active theme like every other button and no colour literal exists here.
/// A click on its rectangle ([`close_button_rect`]) runs the modal's Esc key.
pub fn close_button_line(theme: Theme) -> Line<'static> {
    button_line("x", "", theme)
}

/// The close button's hit-test rectangle on a modal's title row (T66.1): the
/// seven columns its right-aligned [`close_button_line`] occupies on the top
/// border, ending one column short of the modal's top-right corner -- so the
/// button stays inside the modal with its border corners intact. The zero
/// rect when the modal is too narrow to hold the button inside its borders:
/// nothing renders there and no real position ever falls inside a zero rect,
/// so the click test misses naturally.
pub fn close_button_rect(area: Rect) -> Rect {
    if area.width < CLOSE_BUTTON_WIDTH + 2 {
        return Rect::default();
    }
    Rect::new(
        area.right() - 1 - CLOSE_BUTTON_WIDTH,
        area.y,
        CLOSE_BUTTON_WIDTH,
        1,
    )
}

/// A modal button (T44.1, extended by T59.1, split by T64.1): the bracketed
/// ` [ Key ] Label ` style -- the brackets and the key in the theme's button
/// accent colour, the label after them in the theme's regular foreground
/// colour, foreground only (no background, no bold) -- shared by every modal's
/// bottom-line buttons and close buttons, so every modal draws the same
/// buttons and they recolour with the active theme. The two spans' text joins
/// into exactly [`button_text`], whose width sizes the hit-tested rectangles.
fn button_line(key: &str, label: &str, theme: Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" [ {key} ] "),
        Style::new().fg(theme.highlighted_text),
    )];
    if !label.is_empty() {
        spans.push(Span::styled(
            format!("{label} "),
            Style::new().fg(theme.normal_text),
        ));
    }
    Line::from(spans)
}

/// A modal bottom line's key-hint zone (T59.1): the hints joined with ` · `,
/// plain text without brackets, in the theme's low-emphasis footer colour, so
/// no hint ever carries the button accent.
fn hint_line(hints: &[&str], colour: Color) -> Line<'static> {
    Line::styled(format!(" {}", hints.join(" · ")), Style::new().fg(colour))
}

/// The hints that fit a frame's reserved bottom row (T71.1): the leading hints
/// while the strip the shared hint formatting builds -- one leading space and
/// ` · ` between hints -- stays within `width` columns; the first hint that
/// does not fit and every one after it are dropped, so a narrow frame sheds
/// trailing hints instead of overflowing its borders. Nothing fits when even
/// the first hint does not.
pub fn fitted_hints<'a>(hints: &[&'a str], width: usize) -> Vec<&'a str> {
    let mut used = 1;
    let mut fitted = Vec::new();
    for hint in hints {
        used += hint.chars().count() + usize::from(!fitted.is_empty()) * 3;
        if used > width {
            break;
        }
        fitted.push(*hint);
    }
    fitted
}

/// The task list frame's key hints (T71.1): the keys its focused state owns,
/// the most important leading so the trailing hints drop first on a narrow
/// frame. Every label matches what `App::on_key` does with the key while no
/// modal holds the keyboard and the task list frame has the focus: `a` opens
/// the add-task dialog (T74.1) and `i` the inject-task modal (T76.1), both
/// from the focused task list frame, Enter starts the build loop or runs a
/// discovery round while the engine is idle, the scroll keys move the task
/// list, and Tab moves the keys to the other frame.
fn task_hints(app: &App) -> Vec<&'static str> {
    let mut hints = vec!["a add tasks", "i inject task"];
    if app.is_idle() {
        hints.push(if app.tasks.iter().any(|t| !t.done) {
            "Enter start build"
        } else {
            "Enter run discovery"
        });
    }
    hints.push("↑↓ scroll");
    hints.push("Tab output");
    hints
}

/// The agent output frame's key hints (T71.1): the keys its focused state
/// owns -- the scroll keys move the output, PageUp/PageDown page through it,
/// `End` resumes the live follow, and Tab moves the keys to the other frame.
/// T124.1 dropped the leading `v rail view` entry -- the `v` key still flips
/// the rail view, the strip just does not say so (as T75.1 did for the idle
/// `q quit` hint).
fn output_hints() -> Vec<&'static str> {
    vec!["↑↓ scroll", "PgUp/PgDn page", "End follow", "Tab tasks"]
}

/// The focused frame's inner split (T71.1): the bottom inner row reserved for
/// the frame's key hints strip and the rest for its content -- or the whole
/// inner area with no hints row when the frame is unfocused, so the strip is
/// completely absent and the content keeps every row. A frame too short to
/// spare a row (a one-row interior) also renders no strip, so nothing ever
/// overflows or squeezes the content to nothing on a tiny terminal.
fn split_hints_row(inner: Rect, focused: bool) -> (Rect, Option<Rect>) {
    if !focused || inner.height < 2 {
        return (inner, None);
    }
    let [content, hints] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    (content, Some(hints))
}

/// Renders a frame's key hints into its reserved bottom row (T71.1): the
/// hints that fit the row's width joined through the shared modal hint
/// formatting in the theme's low-emphasis footer colour, so the strip
/// recolours with the active theme and never reaches the frame's borders. No
/// row reserved, nothing rendered.
fn render_hints_strip(frame: &mut Frame, hints_row: Option<Rect>, hints: &[&str], theme: Theme) {
    let Some(row) = hints_row else {
        return;
    };
    let fitted = fitted_hints(hints, usize::from(row.width));
    if fitted.is_empty() {
        return;
    }
    frame.render_widget(Paragraph::new(hint_line(&fitted, theme.muted_text)), row);
}

/// Every button's rectangle on a modal's bottom line (T59.1): the buttons
/// occupy one right-hand zone sized to their total width, laid out right to
/// left in the order given, so the renderer and the mouse hit-test agree on
/// every rectangle by construction. A pure companion of `rail_tile_rects`.
pub fn footer_button_rects(footer: Rect, buttons: &[(&str, &str)]) -> Vec<Rect> {
    let widths: Vec<u16> = buttons
        .iter()
        .map(|(key, label)| u16::try_from(button_text(key, label).chars().count()).unwrap_or(0))
        .collect();
    let total: u16 = widths.iter().sum();
    let mut x = footer.x + footer.width.saturating_sub(total);
    widths
        .into_iter()
        .map(|width| {
            let rect = Rect::new(x, footer.y, width, 1);
            x = x.saturating_add(width);
            rect
        })
        .collect()
}

/// A modal's bottom line (T59.1): one line in two zones -- the key hints that
/// act on the modal's content left-aligned in a left-hand zone, the
/// modal-level action buttons right-aligned in a fixed right-hand zone, the
/// two separated by at least two columns. Hints render without brackets in
/// the theme's low-emphasis footer colour; buttons render through the shared
/// [`button_line`] with their bracketed key in the theme's button accent and
/// their label in the theme's foreground (T64.1). Nothing else on the line.
fn render_modal_footer(
    frame: &mut Frame,
    footer: Rect,
    hints: &[&str],
    buttons: &[(&str, &str)],
    theme: Theme,
) {
    let rects = footer_button_rects(footer, buttons);
    let used: u16 = rects.iter().map(|rect| rect.width).sum();
    // The hint zone gives up its width first: it clips at two columns short of
    // the button zone, so the two kinds stay visually distinguishable.
    let hint_zone = Rect::new(footer.x, footer.y, footer.width.saturating_sub(used + 2), 1);
    frame.render_widget(
        Paragraph::new(hint_line(hints, theme.muted_text)),
        hint_zone,
    );
    for ((key, label), rect) in buttons.iter().zip(&rects) {
        frame.render_widget(Paragraph::new(button_line(key, label, theme)), *rect);
    }
}

/// The settings body width from which the help box (T85.1) fits beside the
/// settings list instead of under it.
pub const SETTINGS_HELP_BESIDE_WIDTH: u16 = 96;
/// The help box's (T85.1) width in the beside placement.
pub const SETTINGS_HELP_PANEL_WIDTH: u16 = 40;
/// The help box's (T85.1) height in the under placement.
pub const SETTINGS_HELP_PANEL_HEIGHT: u16 = 8;

/// The settings body's two areas (T85.1): the settings list and the help box --
/// side by side once the body is wide enough for both, the help box under the
/// list otherwise.
pub fn settings_body_areas(body: Rect) -> (Rect, Rect) {
    if body.width >= SETTINGS_HELP_BESIDE_WIDTH {
        let [list, help] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(SETTINGS_HELP_PANEL_WIDTH),
        ])
        .areas(body);
        (list, help)
    } else {
        let [list, help] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(SETTINGS_HELP_PANEL_HEIGHT),
        ])
        .areas(body);
        (list, help)
    }
}

/// The settings overlay: a Large modal titled "Settings --
/// Patok" with an accent-coloured [ X ] close button top right, a scrollable body of collapsible
/// sections, a status line and a bottom line of key hints left, Close button right (T59.1).
/// The body splits into the list and a help box describing the focused entry (T85.1):
/// beside the list on a wide body, under it on a narrow one. Edits show as drafted values;
/// nothing is applied until the close dialog's save choice.
fn render_settings_overlay(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = settings_area(frame.area());
    frame.render_widget(Clear, area);
    app.overlay.close.set(close_button_rect(area));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(" Settings -- Patok ").centered())
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, status_line, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let (list, help) = settings_body_areas(body);

    let entries = app.overlay.visible();
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| settings_row_line(app, index, entry, usize::from(list.width)))
        .collect();
    // Keep the focused row on screen with a 4-row margin: the view scrolls only
    // once the cursor approaches an edge, then follows keeping that margin.
    let height = usize::from(list.height.max(1));
    let start = scroll_offset(
        app.overlay.focus,
        lines.len(),
        height,
        app.overlay.scroll.get(),
    );
    app.overlay.scroll.set(start);
    frame.render_widget(Paragraph::new(lines[start..].to_vec()), list);
    render_scrollbar(frame, theme, list, start, lines.len());
    render_settings_help(frame, help, app, theme);

    match &app.overlay.status {
        Some((text, StatusLevel::Error)) => frame.render_widget(
            Paragraph::new(Line::styled(
                text.clone(),
                Style::new().fg(theme.highlighted_text),
            )),
            status_line,
        ),
        Some((text, StatusLevel::Info)) => frame.render_widget(
            Paragraph::new(Line::styled(
                text.clone(),
                Style::new().fg(theme.muted_text),
            )),
            status_line,
        ),
        None => {}
    }
    // The bottom line's two zones (T59.1): the content key hints on the left,
    // the Close button on the right, in the shared footer renderer.
    app.overlay.footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move", "Enter/Space edit", "←→ fold/cycle"],
        &[("Esc", "Close")],
        theme,
    );
}

/// The settings overlay's help box (T85.1): a bordered box titled with the
/// focused entry's label -- the section's title on a header -- holding the
/// focused entry's description from the owning schema's registry, wrapped to
/// the box's width in the theme's help colour. No focused entry (unreachable
/// while every section renders its header) renders the empty box.
fn render_settings_help(frame: &mut Frame, help: Rect, app: &App, theme: Theme) {
    frame.render_widget(Clear, help);
    let (label, text) = app.overlay.focused_help().unwrap_or(("", ""));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(format!(" {label} ")).centered());
    frame.render_widget(
        Paragraph::new(Line::styled(text, Style::new().fg(theme.muted_text)))
            .wrap(Wrap { trim: true }),
        block.inner(help),
    );
    frame.render_widget(block, help);
}

/// The unsaved-changes dialog's rect: a small centered modal, at least 54 columns
/// wide and 9 rows tall, clamped to the screen.
pub fn settings_confirm_area(screen: Rect) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = 9.min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// The unsaved-changes dialog (Esc or q on a dirty overlay): a centered
/// " Unsaved changes " modal with the three choices as a vertical list --
/// the selected choice in the shared selected-row style (T118.1) -- and a
/// two-zone bottom line of hints left, buttons right (T59.1), styled like
/// the stop dialog. Rendered on top of everything else.
fn render_settings_confirm(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = settings_confirm_area(frame.area());
    frame.render_widget(Clear, area);
    app.overlay.confirm_close.set(close_button_rect(area));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(" Unsaved changes ").centered())
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let lines: Vec<Line> = SETTINGS_CHOICES
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let focused = index == app.overlay.confirm_selected;
            let marker = if focused { "▶ " } else { "  " };
            let mut line = Line::from(vec![
                Span::raw(marker),
                Span::raw(choice.label()),
                Span::styled(
                    format!(" -- {}", choice.detail()),
                    Style::new().fg(if focused {
                        theme.highlighted_text
                    } else {
                        theme.muted_text
                    }),
                ),
            ]);
            if focused {
                line = line.style(selected_row_style(theme));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    // The bottom line's two zones (T59.1): the selection hint on the left, the
    // Confirm and Cancel buttons on the right.
    app.overlay.confirm_footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move"],
        &[("Enter", "Confirm"), ("Esc", "Cancel")],
        theme,
    );
}

/// One group header line: the focus marker, the fold glyph (▾ expanded, ▸
/// folded) and the title, bold -- shared by the settings overlay's sections
/// and the theme picker's Dark/Light groups (T116.1), so the fold glyph
/// logic exists in one place.
fn group_header_line(marker: &str, expanded: bool, title: &str) -> Line<'static> {
    Line::styled(
        format!("{marker}{} {title}", if expanded { '▾' } else { '▸' }),
        Style::new().add_modifier(Modifier::BOLD),
    )
}

/// One overlay row as a rendered line: section headers carry the collapse marker,
/// booleans a checkbox, enums their current choice in cycle markers, numbers and text
/// their value (or the live editor buffer while it is open), read-only rows a muted
/// report. Entry rows render indented past the focus marker so they nest under their
/// section header; headers keep the bare marker margin. The
/// focused row carries a cursor marker and renders in the shared selected-row
/// style (T118.1): the highlighted-text foreground on the normal modal
/// background, bold.
fn settings_row_line(
    app: &App,
    index: usize,
    entry: &Entry<&'static Row>,
    width: usize,
) -> Line<'static> {
    let theme = app.theme();
    let focused = index == app.overlay.focus;
    let marker = if focused { "▶ " } else { "  " };
    let row_marker = format!("{marker}{}", " ".repeat(ENTRY_INDENT));
    let mut line = match entry {
        Entry::Header(section) => {
            let expanded = app.overlay.expanded(*section);
            let title = SECTIONS
                .get(*section)
                .map(|section| section.title)
                .unwrap_or_default();
            group_header_line(marker, expanded, title)
        }
        Entry::Row(row) => {
            let editor = app
                .overlay
                .editor
                .as_ref()
                .filter(|editor| editor.field == row.key);
            match row.kind {
                FieldKind::ReadOnly => {
                    let value = match row.key {
                        "engine_version" => app.engine_version.clone(),
                        "shell_version" => env!("CARGO_PKG_VERSION").to_string(),
                        _ => String::new(),
                    };
                    Line::styled(
                        format!("{row_marker}{}: {value}", row.label),
                        Style::new().fg(if focused {
                            theme.highlighted_text
                        } else {
                            theme.muted_text
                        }),
                    )
                }
                FieldKind::Bool => {
                    let checked = setting_display_value(app, row.key)
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    Line::from(format!(
                        "{row_marker}{} {}",
                        if checked { "[x]" } else { "[ ]" },
                        row.label
                    ))
                }
                FieldKind::Enum(values) => {
                    let value = enum_display(app, row.key, values);
                    Line::from(format!("{row_marker}{}  ‹ {value} ›", row.label))
                }
                FieldKind::Number(_) | FieldKind::Text => {
                    // A field whose editor is open renders the buffer (mid-edit
                    // protection: a reload cannot clobber an in-flight edit).
                    let value = match editor {
                        Some(editor) => editor.buffer.clone(),
                        None => match setting_display_value(app, row.key) {
                            Some(SettingValue::Str(text)) if text.is_empty() => {
                                "(not set)".to_string()
                            }
                            Some(value) => value.to_string(),
                            None => "(not set)".to_string(),
                        },
                    };
                    let prefix = format!("{row_marker}{}  ", row.label);
                    // A narrow body clips the value's head so the tail — where the
                    // block cursor sits when the editor is open — stays inside the
                    // modal's right edge instead of being pushed off screen.
                    let avail = width
                        .saturating_sub(prefix.chars().count() + usize::from(editor.is_some()))
                        .max(1);
                    let skip = value.chars().count().saturating_sub(avail);
                    let shown: String = value.chars().skip(skip).collect();
                    let mut spans = vec![Span::raw(prefix), Span::raw(shown)];
                    if editor.is_some() {
                        // The block cursor at the end of the buffer.
                        spans.push(Span::styled(
                            " ",
                            Style::new().add_modifier(Modifier::REVERSED),
                        ));
                    }
                    Line::from(spans)
                }
            }
        }
    };
    if focused {
        line = line.style(selected_row_style(theme));
    }
    line
}

/// One entry's current display value: the engine-reported readout for daemon fields,
/// the shell's tui mirror for tui fields (rendering reads the same data the overlay's
/// key handling does).
fn setting_display_value(app: &App, key: &str) -> Option<SettingValue> {
    let row = SECTIONS
        .iter()
        .flat_map(|section| section.rows.iter())
        .find(|row| row.key == key)?;
    app.setting_value(Entry::Row(row))
}

/// An enum row's current choice, defaulting to the cycle's first value.
fn enum_display(app: &App, key: &str, values: &[&str]) -> String {
    let current =
        setting_display_value(app, key).and_then(|value| value.as_str().map(str::to_string));
    match current {
        Some(choice) if values.contains(&choice.as_str()) => choice,
        _ => values.first().copied().unwrap_or_default().to_string(),
    }
}

/// A hand-rendered scrollbar on the body's right edge when the rows overflow.
/// The thumb wears the theme's `highlighted_text` class and the rail its
/// `scrollbar_rail` class (T121.1).
fn render_scrollbar(frame: &mut Frame, theme: Theme, body: Rect, start: usize, total: usize) {
    let height = usize::from(body.height.max(1));
    if total <= height {
        return;
    }
    let thumb = (height * height / total).max(1);
    let thumb_start = start * height / total;
    let col = body.right() - 1;
    for i in 0..height {
        let in_thumb = i >= thumb_start && i < thumb_start + thumb;
        if let Some(cell) = frame.buffer_mut().cell_mut((col, body.y + i as u16)) {
            cell.set_symbol(if in_thumb { "█" } else { "│" });
            cell.set_style(if in_thumb {
                Style::new().fg(theme.highlighted_text)
            } else {
                Style::new().fg(theme.scrollbar_rail)
            });
        }
    }
}

/// The add-task dialog rect: about 90% of the width and 80% of the height, at least
/// 60x20, clamped to the screen and centered.
pub fn dialog_area(screen: Rect) -> Rect {
    let width = (screen.width * 9 / 10).max(60).min(screen.width);
    let height = (screen.height * 4 / 5).max(20).min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

fn render_dialog(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = dialog_area(frame.area());
    frame.render_widget(Clear, area);
    app.dialog_close.set(close_button_rect(area));
    // The two modals share everything but their chrome strings (T76.1): the
    // title and the empty-input watermark follow the dialog's kind; the
    // primary button's label flows from `App::primary_label`, which branches
    // on the same kind.
    let (title, watermark) = match app.dialog_kind {
        DialogKind::Add => (" What do you want to do? ", DIALOG_WATERMARK),
        DialogKind::Inject => (" Inject a task ", INJECT_WATERMARK),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title(Span::raw(title))
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [input, dialog_status, footer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    if let Some(message) = &app.dialog_status {
        frame.render_widget(
            Paragraph::new(Line::styled(
                message.clone(),
                Style::new().fg(theme.highlighted_text),
            )),
            dialog_status,
        );
    }

    let width = usize::from(input.width.max(2)) - 1;
    app.dialog_width.set(width);
    let chars: Vec<char> = app.dialog_text.chars().collect();
    let rows = app.dialog_rows();
    let cursor = app.cursor();
    let cursor_row = app.cursor_row(&rows);
    // The block cursor covers the cell at the cursor: the character under it
    // on the cursor style, or a styled space at the end of a logical line or
    // on an empty one. No cell is added, so the letters around the cursor
    // stay in place while it moves.
    let cursor_style = Style::new().fg(theme.contrast_text).bg(theme.accent);
    let mut lines: Vec<Line> = if app.dialog_text.is_empty() {
        // The empty-input watermark (T41.1): a dim hint followed by the block
        // cursor, which keeps the focus indicator visible.
        vec![Line::from(vec![
            Span::styled(watermark, Style::new().fg(theme.muted_text)),
            Span::styled(" ", cursor_style),
        ])]
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, row)| {
                let text = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
                if i == cursor_row {
                    let cell = if cursor < row.end {
                        chars[cursor].to_string()
                    } else {
                        " ".to_string()
                    };
                    Line::from(vec![
                        Span::raw(text(row.start, cursor)),
                        Span::styled(cell, cursor_style),
                        Span::raw(text((cursor + 1).min(row.end), row.end)),
                    ])
                } else {
                    Line::from(text(row.start, row.end))
                }
            })
            .collect()
    };
    // Keep the cursor's row in view.
    let skip = (cursor_row + 1).saturating_sub(usize::from(input.height));
    let visible: Vec<Line> = lines.drain(skip..).collect();
    frame.render_widget(Paragraph::new(visible), input);

    // The bottom line's two zones (T59.1): the input's key hints on the left,
    // the primary-action and Close buttons on the right, all through the
    // shared footer renderer. The primary button's label follows the scenario
    //.
    app.dialog_footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓←→ move", "Shift-Enter new line", "Ctrl+U clear"],
        &[("Enter", app.primary_label()), ("Esc", "Close")],
        theme,
    );
}

/// The stop dialog's rect: a small centered modal, at least 54 columns wide and
/// 9 rows tall, clamped to the screen.
pub fn stop_area(screen: Rect) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = 9.min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// The stop dialog (Esc while a build runs, T46.1): a centered " Stop build " modal
/// with the three choices as a vertical list -- the selected choice in the shared
/// selected-row style (T118.1) -- and a two-zone bottom line of hints
/// left, buttons right (T59.1). Rendered on top of everything else.
fn render_stop_dialog(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = stop_area(frame.area());
    frame.render_widget(Clear, area);
    app.stop_close.set(close_button_rect(area));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(" Stop build ").centered())
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let lines: Vec<Line> = STOP_CHOICES
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let focused = index == app.stop_selected;
            let marker = if focused { "▶ " } else { "  " };
            let mut line = Line::from(vec![
                Span::raw(marker),
                Span::raw(choice.label()),
                Span::styled(
                    format!(" -- {}", choice.detail()),
                    Style::new().fg(if focused {
                        theme.highlighted_text
                    } else {
                        theme.muted_text
                    }),
                ),
            ]);
            if focused {
                line = line.style(selected_row_style(theme));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    // The bottom line's two zones (T59.1): the selection hint on the left, the
    // Confirm and Close buttons on the right.
    app.stop_footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move"],
        &[("Enter", "Confirm"), ("Esc", "Close")],
        theme,
    );
}

/// The theme picker's rect (T43.1, T116.1): a centered modal, at least 54
/// columns wide and tall enough for its full grouped list -- the border (2),
/// the two group headers, the eleven one-line entries (13 body rows) and the
/// footer (1), so 16 -- clamped to the screen.
pub fn theme_area(screen: Rect) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = 16.min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// The theme picker's entry the pointer sits on (T43.1, T116.1): the visible
/// entry index when the position is on one of the body's visible entry rows,
/// `None` on the border, the footer, the shell behind the modal or a body row
/// past the last visible entry (a folded group's hidden area). A pure
/// hit-test like `frame_at`.
pub fn theme_row_at(position: Position, body: Rect, visible_len: usize) -> Option<usize> {
    if !body.contains(position) {
        return None;
    }
    let row = usize::from(position.y - body.y);
    (row < visible_len).then_some(row)
}

/// The theme picker (the `t` key, T43.1, T116.1): a centered " Theme " modal
/// listing the built-in themes in two collapsible groups, Dark then Light,
/// mirroring the settings overlay's grouped list -- each header carries the
/// fold glyph and toggles its group, the entries nest indented under it.
/// Every row renders in the currently active theme's classes (T115.1), with
/// a selection marker and a two-zone bottom line of hints left, buttons
/// right (T59.1). Rendered on top of everything else; the shell underneath
/// is already recoloured by the live preview, since `App::theme` resolves it.
fn render_theme_modal(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = theme_area(frame.area());
    frame.render_widget(Clear, area);
    app.theme_modal.close.set(close_button_rect(area));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(" Theme ").centered())
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    app.theme_modal.area.set(body);
    let groups = theme_modal_groups();
    let entries = app.theme_modal.entries();
    let width = groups
        .iter()
        .flat_map(|(_, names)| names.iter())
        .map(|name| name.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| theme_row_line(app, index, *entry, &groups, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
    // The bottom line's two zones (T59.1): the preview hints on the left, the
    // Save and Cancel buttons on the right. The modal is the narrowest of the
    // five, so the hint stays short enough to keep the two zones apart.
    app.theme_modal.footer.set(footer);
    render_modal_footer(
        frame,
        footer,
        &["↑↓/jk previews"],
        &[("Enter", "Save"), ("Esc", "Cancel")],
        theme,
    );
}

/// One visible entry of the picker (T43.1, T115.1, T116.1): a group header
/// line -- the marker, the fold glyph and the title, bold -- or a theme
/// entry, the name indented under its header and padded to the list's
/// longest name, rendered as a normal list row in the currently active
/// theme's classes instead of previewing the entry's own palette. The
/// selected entry carries the marker and renders in `highlighted_text`,
/// bold, through the shared selected-row style (T118.1).
fn theme_row_line(
    app: &App,
    index: usize,
    entry: Entry<&'static str>,
    groups: &[(&'static str, Vec<&'static str>)],
    width: usize,
) -> Line<'static> {
    let theme = app.theme();
    let selected = index == app.theme_modal.selected;
    let marker = if selected { "▶ " } else { "  " };
    match entry {
        Entry::Header(group) => {
            let (title, expanded) = groups
                .get(group)
                .map(|(title, _)| {
                    (
                        *title,
                        app.theme_modal
                            .expanded
                            .get(group)
                            .copied()
                            .unwrap_or(false),
                    )
                })
                .unwrap_or(("", false));
            let style = if selected {
                selected_row_style(theme)
            } else {
                base_style(theme)
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            };
            group_header_line(marker, expanded, title).style(style)
        }
        Entry::Row(name) => {
            let fg = if selected {
                theme.highlighted_text
            } else {
                theme.normal_text
            };
            let row_marker = format!("{marker}{}", " ".repeat(ENTRY_INDENT));
            let spans = vec![
                Span::styled(row_marker, Style::new().fg(fg)),
                Span::styled(format!("{name:<width$}"), Style::new().fg(fg)),
            ];
            let mut line = Line::from(spans).style(base_style(theme));
            if selected {
                line = line.style(selected_row_style(theme));
            }
            line
        }
    }
}

/// The provider's display name for the output frame title ("claude" -> "Claude").
fn provider_display_name(provider: &str) -> &str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "mistral" => "Mistral",
        other => other,
    }
}

/// The output frame title: " Builder | Claude - claude-sonnet-5.5 ", dropping the
/// model first and then the provider when `width` is too narrow; the agent type
/// always stays, so the title is never empty.
pub fn output_title(agent: &str, provider: &str, model: &str, width: usize) -> String {
    output_title_parts(agent, provider, model, None, width).0
}

/// What survives the width squeeze (T31.1): the agent type always stays; the
/// model drops first, then the provider, then the timer.
struct KeptTitle {
    provider: Option<String>,
    model: Option<String>,
    timer: Option<String>,
}

/// The output frame title's left side from its kept parts: `" {agent}"`, then
/// `" | {provider}"` and `" - {model}"` when they survived, closed by a
/// trailing space.
fn left_title(agent: &str, provider: Option<&str>, model: Option<&str>) -> String {
    let mut left = format!(" {agent}");
    if let Some(provider) = provider {
        left.push_str(" | ");
        left.push_str(provider);
    }
    if let Some(model) = model {
        left.push_str(" - ");
        left.push_str(model);
    }
    left.push(' ');
    left
}

/// The shared T31.1 truncation decision: which of the provider, model and
/// timer survive `width` columns. The candidates drop the model first, then
/// the provider; each is tried with the timer (`left + right <= width`), then
/// without it. The agent type always stays, so the title is never empty.
fn kept_title(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
) -> KeptTitle {
    let timer = timer.map(|t| format!(" {t} "));
    let mut candidates: Vec<(Option<&str>, Option<&str>)> = Vec::new();
    if !provider.is_empty() {
        if !model.is_empty() {
            candidates.push((Some(provider), Some(model)));
        }
        candidates.push((Some(provider), None));
    }
    candidates.push((None, None));
    let right_len = timer.as_ref().map_or(0, |t| t.chars().count());
    for &(provider, model) in &candidates {
        if left_title(agent, provider, model).chars().count() + right_len <= width {
            return KeptTitle {
                provider: provider.map(String::from),
                model: model.map(String::from),
                timer,
            };
        }
    }
    // The timer is the last element to disappear: without it, try the left parts
    // against the full width.
    for &(provider, model) in &candidates {
        if left_title(agent, provider, model).chars().count() <= width {
            return KeptTitle {
                provider: provider.map(String::from),
                model: model.map(String::from),
                timer: None,
            };
        }
    }
    KeptTitle {
        provider: None,
        model: None,
        timer: None,
    }
}

/// The output frame title in two parts: the agent type with provider and model
/// on the left, the running timer on the right. A narrow `width` drops the model
/// first, then the provider, and only then the timer; the agent type always
/// keeps its surrounding spaces, so the left side is never empty.
pub fn output_title_parts(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
) -> (String, String) {
    let kept = kept_title(agent, provider, model, timer, width);
    (
        left_title(agent, kept.provider.as_deref(), kept.model.as_deref()),
        kept.timer.unwrap_or_default(),
    )
}

/// The output frame title in per-segment spans (T37.1): the agent type name
/// keeps its agent colour and bold while the separator, provider, model and
/// timer carry the theme's low-emphasis `detail_colour`. Concatenated, the
/// spans are exactly [`output_title_parts`]'s strings, so the T31.1 truncation
/// behaviour is shared.
pub fn output_title_spans(
    agent: &str,
    provider: &str,
    model: &str,
    timer: Option<&str>,
    width: usize,
    agent_colour: Color,
    detail_colour: Color,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let kept = kept_title(agent, provider, model, timer, width);
    let agent_style = Style::new().fg(agent_colour).bold();
    let detail_style = Style::new().fg(detail_colour);
    // The agent span closes itself when nothing else survived the squeeze.
    let mut left = vec![Span::styled(
        if kept.provider.is_none() {
            format!(" {agent} ")
        } else {
            format!(" {agent}")
        },
        agent_style,
    )];
    if let Some(provider) = kept.provider.as_deref() {
        if let Some(model) = kept.model.as_deref() {
            left.push(Span::styled(format!(" | {provider}"), detail_style));
            left.push(Span::styled(format!(" - {model} "), detail_style));
        } else {
            left.push(Span::styled(format!(" | {provider} "), detail_style));
        }
    }
    let right = kept
        .timer
        .map(|timer| vec![Span::styled(timer, detail_style)])
        .unwrap_or_default();
    (left, right)
}

/// A session's elapsed time as `mm:ss` with zero-padded minutes.
pub fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// The output frame title's agent type: the active agent's name capitalised
/// ("planner" -> "Planner", "builder" -> "Builder"; T11.1).
pub fn agent_display(agent: &str) -> String {
    let mut chars = agent.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A session's duration in human words (T42.1): `M sec` under a minute,
/// `N min M sec` under an hour (seconds zero-padded) and `N h M min` beyond
/// (minutes-within-the-hour zero-padded).
pub fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (mins, secs) = (total / 60, total % 60);
    if total < 60 {
        format!("{secs} sec")
    } else if total < 3600 {
        format!("{mins} min {secs:02} sec")
    } else {
        format!("{} h {:02} min", total / 3600, mins % 60)
    }
}

/// The started line appended to the output pane when one agent session begins
/// (T78.1): the agent type name exactly as the finished line shows it, the
/// provider it runs on, and the configured model when one exists.
pub fn started_line(agent: &str, provider: &str, model: Option<&str>) -> String {
    match model {
        Some(model) => format!("{} started ({provider}, {model})", agent_display(agent)),
        None => format!("{} started ({provider})", agent_display(agent)),
    }
}

/// The finished line appended to the output pane when one agent session ends
/// (T42.1): the agent type name exactly as the frame title shows it, the
/// outcome's word (`finished`, `failed` or `cancelled`) and the human duration.
pub fn finished_line(agent: &str, outcome: SessionOutcome, duration: Duration) -> String {
    let word = match outcome {
        SessionOutcome::Finished => "finished",
        SessionOutcome::Failed => "failed",
        SessionOutcome::Cancelled => "cancelled",
    };
    format!(
        "{} {word} in {}",
        agent_display(agent),
        human_duration(duration)
    )
}

fn render_output(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let timer = app.session_elapsed().map(format_elapsed);
    // The agent type name keeps its fixed agent colour in every theme (T11.1,
    // T24.1, T110.1); the frame title is the one surface that keeps the
    // fixated name colour (T117.1), and the separator, provider, model and
    // timer render in the theme's low-emphasis detail colour (T37.1).
    let title_colour = Theme::agent_name_color(theme, &app.agent);
    let (left, right) = output_title_spans(
        &agent_display(&app.agent),
        provider_display_name(&app.provider),
        &app.model,
        timer.as_deref(),
        usize::from(area.width.saturating_sub(2)),
        title_colour,
        theme.muted_text,
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title_top(Line::from(left));
    // The running session timer sits right-aligned on the same title line.
    if !right.is_empty() {
        block = block.title_top(Line::from(right).right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // The focused frame reserves its bottom inner row for the key hints strip
    // (T71.1); the unfocused frame keeps the full inner area and no strip.
    let (content, hints_row) = split_hints_row(inner, app.focus == FrameFocus::Output);
    let max_scroll = render_pane(
        frame,
        theme,
        &app.output,
        app.scroll,
        "no output yet",
        content,
    );
    app.max_scroll.set(max_scroll);
    render_hints_strip(frame, hints_row, &output_hints(), theme);
}

/// Draws agent output scrolled up `scroll` visual lines from the bottom; returns the largest useful scroll.
fn render_pane(
    frame: &mut Frame,
    theme: Theme,
    pane: &Pane,
    scroll: usize,
    empty: &str,
    inner: Rect,
) -> usize {
    if pane.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                empty.to_string(),
                Style::new().fg(theme.muted_text),
            )),
            inner,
        );
        return 0;
    }

    let width = usize::from(inner.width.max(1));
    let height = usize::from(inner.height);
    let visual: Vec<Line> = pane
        .lines
        .iter()
        .flat_map(|line| visual_lines(theme, line, width))
        .collect();
    let max_scroll = visual.len().saturating_sub(height);
    let scroll = scroll.min(max_scroll);
    let end = visual.len() - scroll;
    let start = end.saturating_sub(height);
    frame.render_widget(Paragraph::new(visual[start..end].to_vec()), inner);
    max_scroll
}

/// One logical output line as visual rows: plain text hard-wraps; thinking renders
/// as markdown first, so headings, emphasis, lists and code blocks show up styled.
/// Every line, the agent's name inside it included, wears its line kind's colour;
/// the fixated per-agent name colour reaches only the output frame title (T117.1).
fn visual_lines(theme: Theme, line: &OutLine, width: usize) -> Vec<Line<'static>> {
    let style = style_of(theme, line.kind);
    if line.kind == LineKind::Thinking {
        markdown_lines(&line.text, style)
            .into_iter()
            .flat_map(|row| wrap_line(&row, width))
            .collect()
    } else {
        wrap(&line.text, width)
            .into_iter()
            .map(|piece| Line::styled(piece, style))
            .collect()
    }
}

/// Wraps a styled line at `width` columns, keeping each span's style.
fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
        .collect();
    if cells.is_empty() {
        return vec![Line::default()];
    }
    cells
        .chunks(width.max(1))
        .map(|chunk| {
            let mut spans: Vec<Span> = Vec::new();
            for (c, style) in chunk {
                match spans.last_mut() {
                    Some(last) if last.style == *style => last.content.to_mut().push(*c),
                    _ => spans.push(Span::styled(c.to_string(), *style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

fn style_of(theme: Theme, kind: LineKind) -> Style {
    // `Text` renders unstyled: no fg at all, so the line inherits the base
    // style's foreground instead of resetting it.
    if kind == LineKind::Text {
        return Style::new();
    }
    let style = Style::new().fg(Theme::line_color(theme, kind));
    if kind == LineKind::Heading {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// Hard-wraps at `width` characters; an empty line stays one empty line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// Renders the task list frame. The title reads ` Tasks | done/total - N left `
/// with only the word `Tasks` in the frame's title colour; everything after it
/// -- the pipe separator, the three counts, the slash, the dash and the word
/// `left` -- wears the theme's low-emphasis detail colour (T67.1).
fn render_tasks(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let detail = Style::new().fg(theme.muted_text);
    let done = app.tasks.iter().filter(|t| t.done).count();
    let total = app.tasks.len();
    let left = app.tasks.iter().filter(|t| !t.done).count();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::raw(" Tasks"),
            Span::styled(" | ", detail),
            Span::styled(done.to_string(), detail),
            Span::styled("/", detail),
            Span::styled(total.to_string(), detail),
            Span::styled(" - ", detail),
            Span::styled(left.to_string(), detail),
            Span::styled(" left ", detail),
        ]));
    let inner = block.inner(area);
    // The focused frame reserves its bottom inner row for the key hints strip
    // (T71.1); the unfocused frame keeps the full inner area and no strip.
    let (content, hints_row) = split_hints_row(inner, app.focus == FrameFocus::Tasks);

    // Keep the first pending task in view, a third of the way down; `task_scroll`
    // rows scrolled up from that auto view (toward earlier tasks).
    let focus = app
        .tasks
        .iter()
        .position(|t| !t.done)
        .unwrap_or(app.tasks.len());
    let auto = focus.saturating_sub(usize::from(content.height) / 3);
    app.task_max_scroll.set(auto);
    let offset = auto.saturating_sub(app.task_scroll.min(auto));
    let items: Vec<ListItem> = app
        .tasks
        .iter()
        .skip(offset)
        .map(|task| {
            let running = app.current_task.as_deref() == Some(task.id.as_str());
            let (mark, style) = match (task.done, running) {
                (true, _) => ("✔", Style::new().fg(theme.muted_text)),
                (false, true) => ("▶", Style::new().fg(theme.success).bold()),
                (false, false) => ("○", Style::new()),
            };
            let style = if app.is_new_task(&task.id) {
                Style::new()
                    .fg(theme.highlighted_text)
                    .bold()
                    .add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            ListItem::new(Line::styled(
                format!("{mark} {}  {}", task.id, task.description),
                style,
            ))
        })
        .collect();
    frame.render_widget(block, area);
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "no tasks in TASKS.md",
                Style::new().fg(theme.muted_text),
            )),
            content,
        );
    } else {
        frame.render_widget(List::new(items), content);
    }
    render_hints_strip(frame, hints_row, &task_hints(app), theme);
}

/// The merged status line (T86.1): one bottom row holding the old header's
/// chips on the left -- the status chip, then the run-mode chip when the two
/// fit -- and the status content on the right: a transient message, else the
/// key-hint chips, right-aligned. Hint chips drop as whole chip+label pairs
/// from the tail when they do not fit; a message is never dropped -- it
/// truncates at the line's right edge like a bare status bar message.
fn status_widget(app: &App, width: usize) -> Paragraph<'static> {
    let theme = app.theme();
    // A discovery round runs either on an idle engine or inside a session's empty-queue gap
    // (the phase stays Running for the whole session), so it wins over the phase chips.
    let (label, color) = match (app.stopping, app.phase) {
        (true, _) => ("STOPPING", theme.highlighted_text),
        (false, _) if app.planning => ("PLANNING", theme.chip_planning),
        (false, _) if app.discovering => ("DISCOVERING", theme.chip_discovering),
        (false, Phase::Running) => ("RUNNING", theme.success),
        (false, Phase::Startup) => ("STOPPED", theme.chip_neutral),
    };
    let chip = format!(" {label} ");
    // The run-mode chip: a quieter second chip mirroring the engine-reported readout,
    // so a change applied through the settings overlay flips it immediately.
    let mode = format!(" {} ", app.run_mode());

    // The run-mode chip drops when the two do not fit; the status chip survives.
    let fits_mode = chip.chars().count() + mode.chars().count() <= width;
    let mut spans = vec![Span::styled(
        chip,
        Style::new().fg(theme.contrast_text).bg(color).bold(),
    )];
    if fits_mode {
        spans.push(Span::styled(
            mode,
            Style::new().fg(theme.contrast_text).bg(theme.chip_neutral),
        ));
    }
    let left_width: usize = spans.iter().map(|span| span.content.chars().count()).sum();

    let right = match &app.status {
        Some(message) => {
            // Bare text: one separating space so it does not glue to the mode
            // chip, right-aligned padding when it fits -- none when it would
            // underflow, so the Paragraph truncates at the right edge.
            let pad = width.saturating_sub(left_width + 1 + message.chars().count());
            let mut spans = vec![Span::raw(" ".repeat(pad + 1))];
            // The message renders as one piece in the highlighted-text colour,
            // the agent's display name inside it included (T117.1); the
            // fixated per-agent name colour reaches only the frame title.
            spans.push(Span::styled(
                message.clone(),
                Style::new().fg(theme.highlighted_text),
            ));
            spans
        }
        None => {
            let mut keys = Vec::new();
            // The primary-action hint leads while the engine is idle: Enter
            // starts the build loop while pending tasks remain and runs a
            // discovery round once the queue is complete (T46.1).
            if app.is_idle() {
                keys.push((
                    "Enter",
                    if app.tasks.iter().any(|t| !t.done) {
                        "start build"
                    } else {
                        "run discovery"
                    },
                ));
            }
            // Esc opens the stop dialog while a build runs (T46.1); during a
            // planner or discovery run it does nothing, so no hint.
            if app.phase == Phase::Running {
                keys.push(("Esc", "stop build"));
            }
            keys.extend([
                ("?", "settings"),
                // The theme picker (T43.1) opens with `t` from any engine
                // state, so its hint sits beside the other modal key.
                ("t", "theme"),
                ("d", "detach"),
                ("q", "quit"),
            ]);
            // A pair drops as a whole from the tail when it does not fit the
            // zone left of the chips, so no half-cut chip ever shows; the kept
            // prefix is right-aligned. The chips' own padding separates them
            // from the mode chip, so no extra gap column is added.
            let zone = width.saturating_sub(left_width);
            let mut kept: Vec<(&str, &str)> = Vec::new();
            let mut used = 0;
            for (key, label) in keys {
                let pair = key.chars().count() + label.chars().count() + 4;
                if used + pair > zone {
                    break;
                }
                used += pair;
                kept.push((key, label));
            }
            let mut right = vec![Span::raw(" ".repeat(zone.saturating_sub(used)))];
            for (key, label) in kept {
                right.push(Span::styled(
                    format!(" {key} "),
                    Style::new()
                        .bold()
                        .fg(theme.contrast_text)
                        .bg(theme.chip_neutral),
                ));
                right.push(Span::styled(
                    format!(" {label} "),
                    Style::new().fg(theme.muted_text),
                ));
            }
            right
        }
    };
    spans.extend(right);
    Paragraph::new(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_core::config::{THEME_KEYS, Theme as ThemeKey};
    use patok_core::event::{Phase, Snapshot};
    use patok_core::pipeline::PipelineState;
    use patok_core::task::Task;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

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

    /// The status bar's planning message wears the highlighted-text colour in
    /// full on every built-in theme (T117.1), the agent's display name inside
    /// it included — never the fixated per-agent name colour.
    #[test]
    fn planning_status_message_wears_the_highlighted_colour_on_every_theme() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            let mut app = app_with_one_task();
            app.tui.theme = key;
            app.tui.truecolor = Some(true);
            app.planning = true;
            app.agent = "planner".into();
            let message = "Planner running...";
            app.status = Some(message.into());
            let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 1));
            status_widget(&app, 100).render(buffer.area, &mut buffer);
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
}
