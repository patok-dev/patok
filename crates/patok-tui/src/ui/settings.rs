//! The settings overlay, its help box and the unsaved-changes dialog.

use patok_core::config::SettingValue;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::common::{
    ENTRY_INDENT, base_style, group_header_line, render_modal_footer, render_scrollbar,
    selected_row_style,
};
use super::modal::{ModalTitle, render_modal_frame};
use crate::app::{App, CONFIRM_ESC_LABEL, OVERLAY_BUTTONS};
use crate::overlay::{Entry, FieldKind, Row, SECTIONS, SETTINGS_CHOICES, StatusLevel};
use crate::theme::Theme;

/// The settings overlay rect: Large --
/// 90% of the width and 80% of the height, at least 80x24, clamped to the screen and
/// centered.
pub fn settings_area(screen: Rect) -> Rect {
    let width = (screen.width * 9 / 10).max(80).min(screen.width);
    let height = (screen.height * 4 / 5).max(24).min(screen.height);
    super::modal::centered(screen, width, height)
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
pub(super) fn render_settings_overlay(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let area = settings_area(frame.area());
    let chrome = render_modal_frame(
        frame,
        theme,
        area,
        ModalTitle::Centered(" Settings -- Patok "),
    );
    app.overlay.close.set(chrome.close);
    let inner = chrome.inner;
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
    // The offset follows the cursor from the key path (`move_focus`) and the
    // wheel moves it freely (`scroll_by`); the render only records the list
    // rect and clamps the offset into the scrollable range, so folds and
    // resizes self-heal without pulling the wheel's free offset back into
    // the cursor's margin band.
    let height = usize::from(list.height.max(1));
    let start = app
        .overlay
        .scroll
        .get()
        .min(lines.len().saturating_sub(height));
    app.overlay.list.set(list);
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
                Style::new().fg(theme.highlighted_text),
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
        &OVERLAY_BUTTONS,
        theme,
    );
}

/// The settings overlay's help box (T85.1): a bordered box titled with the
/// focused entry's label -- the section's title on a header -- holding the
/// focused entry's description from the owning schema's registry, wrapped to
/// the box's width in the theme's help colour. No focused entry (unreachable
/// while every section renders its header) renders the empty box.
pub(super) fn render_settings_help(frame: &mut Frame, help: Rect, app: &App, theme: Theme) {
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
/// wide and 9 rows tall, clamped to the screen -- the shared choice-dialog
/// rect (finding 6).
pub fn settings_confirm_area(screen: Rect) -> Rect {
    crate::choice::choice_area(screen)
}

/// The unsaved-changes dialog (Esc or q on a dirty overlay): a centered
/// " Unsaved changes " modal with the three choices as a vertical list --
/// the selected choice in the shared selected-row style (T118.1) -- and a
/// two-zone bottom line of hints left, buttons right (T59.1), styled like
/// the stop dialog. Rendered on top of everything else. The shared
/// choice-dialog renderer draws it (finding 6); this records the rects its
/// mouse hit-testing reads.
pub(super) fn render_settings_confirm(frame: &mut Frame, app: &App) {
    let rects = super::choice::render_choice_dialog(
        frame,
        app.theme(),
        " Unsaved changes ",
        &SETTINGS_CHOICES,
        app.overlay.confirm_selected,
        CONFIRM_ESC_LABEL,
    );
    app.overlay.confirm_close.set(rects.close);
    app.overlay.confirm_body.set(rects.body);
    app.overlay.confirm_footer.set(rects.footer);
}

/// One overlay row as a rendered line: section headers carry the collapse marker,
/// booleans a checkbox, enums their current choice in cycle markers, numbers and text
/// their value (or the live editor buffer while it is open), read-only rows a muted
/// report. Entry rows render indented past the focus marker so they nest under their
/// section header; headers keep the bare marker margin. The
/// focused row carries a cursor marker and renders in the shared selected-row
/// style (T118.1): the highlighted-text foreground on the normal modal
/// background, bold.
pub(super) fn settings_row_line(
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
                        // The block cursor at the end of the buffer, wearing
                        // the highlighted-text foreground (T138.1), the colour
                        // selected rows and the scrollbar thumb already share.
                        spans.push(Span::styled(
                            " ",
                            Style::new()
                                .fg(theme.highlighted_text)
                                .add_modifier(Modifier::REVERSED),
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
pub(super) fn setting_display_value(app: &App, key: &str) -> Option<SettingValue> {
    let row = SECTIONS
        .iter()
        .flat_map(|section| section.rows.iter())
        .find(|row| row.key == key)?;
    app.setting_value(Entry::Row(row))
}

/// An enum row's current choice, defaulting to the cycle's first value.
pub(super) fn enum_display(app: &App, key: &str, values: &[&str]) -> String {
    let current =
        setting_display_value(app, key).and_then(|value| value.as_str().map(str::to_string));
    match current {
        Some(choice) if values.contains(&choice.as_str()) => choice,
        _ => values.first().copied().unwrap_or_default().to_string(),
    }
}
