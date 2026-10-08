//! The add-task / inject-task dialog, the stop dialog and the theme picker.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::common::{
    ENTRY_INDENT, base_style, close_button_line, close_button_rect, group_header_line,
    render_modal_footer, selected_row_style,
};
use crate::app::{App, DialogKind, STOP_CHOICES, menu_entries};
use crate::overlay::{Entry, SETTINGS_CHOICES};
use crate::theme::theme_modal_groups;
use patok_core::event::Phase;

/// The placeholder hint rendered in the empty input area of the add-task dialog
/// (T41.1): a low-emphasis watermark that reads as a hint, not typed content.
/// It disappears with the first typed or pasted character and never enters
/// `dialog_text` or a submitted request.
pub const DIALOG_WATERMARK: &str = "Press Enter to run Discovery agent";

/// The inject-task modal's empty-input watermark (T76.1), in the place of the
/// add-task dialog's: it says what the confirm does with the typed text, so
/// the normalized append is never a surprise.
pub const INJECT_WATERMARK: &str = "Type the task; it lands as an unchecked task line";

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

pub(super) fn render_dialog(frame: &mut Frame, app: &App) {
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
    // stay in place while it moves. The cursor cell wears the highlighted-text
    // foreground (T138.1), the colour selected rows and the scrollbar thumb
    // already share.
    // REVERSED keeps the cell a visible block: a bare foreground on a space
    // would draw nothing.
    let cursor_style = Style::new()
        .fg(theme.highlighted_text)
        .add_modifier(Modifier::REVERSED);
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
pub(super) fn render_stop_dialog(frame: &mut Frame, app: &App) {
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
    app.stop_body.set(body);
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

/// The m menu's rect: a small centered modal, at least 54 columns wide and
/// tall enough for its rows -- the border (2), the entry rows and the footer
/// (1) -- clamped to the screen; the stop entry adds one row while a
/// build runs.
pub fn menu_area(screen: Rect, running: bool) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = (menu_entries(running).len() + 3).min(usize::from(screen.height)) as u16;
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// The m menu (the `m` key): a centered " Menu " modal listing the status
/// bar's former secondary hints as rows -- the selected choice in the shared
/// selected-row style (T118.1) -- and a two-zone bottom line of hints left,
/// buttons right (T59.1), styled like the stop dialog. Each entry runs the
/// action its direct key binding triggers; the stop row renders only
/// while a build runs. Each rendered row is clickable: a left click moves
/// the selection to the row and runs it (T137.1), so the body rect is
/// recorded for the app's mouse hit-testing. Rendered on top of everything
/// else.
pub(super) fn render_menu(frame: &mut Frame, app: &App) {
    let theme = app.theme();
    let running = app.phase == Phase::Running;
    let area = menu_area(frame.area(), running);
    frame.render_widget(Clear, area);
    app.menu_close.set(close_button_rect(area));
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme))
        .title_top(Line::from(" Menu ").centered())
        .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    app.menu_body.set(body);
    let lines: Vec<Line> = menu_entries(running)
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let focused = index == app.menu_selected;
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
    app.menu_footer.set(footer);
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

/// The m menu's entry the pointer sits on (T137.1): the entry index when
/// the position is on one of the body's rendered rows, `None` on the border,
/// the title, the footer or the shell behind the modal. A pure hit-test
/// like [`theme_row_at`].
pub fn menu_row_at(position: Position, body: Rect, visible_len: usize) -> Option<usize> {
    if !body.contains(position) {
        return None;
    }
    let row = usize::from(position.y - body.y);
    (row < visible_len).then_some(row)
}

/// The unsaved-changes dialog's choice the pointer sits on (T146.1): the row
/// index when the position is on one of the dialog's three choice rows, `None`
/// on the dialog's blank body padding below them, the border, the title, the
/// footer or the shell outside. A pure hit-test like [`menu_row_at`].
pub fn confirm_row_at(position: Position, body: Rect) -> Option<usize> {
    if !body.contains(position) {
        return None;
    }
    let row = usize::from(position.y - body.y);
    (row < SETTINGS_CHOICES.len()).then_some(row)
}

/// The stop dialog's choice the pointer sits on (T148.1): the row index
/// when the position is on one of the dialog's three choice rows, `None`
/// on the dialog's blank body padding below them, the border, the title,
/// the footer or the shell outside. A pure hit-test like [`menu_row_at`].
pub fn stop_row_at(position: Position, body: Rect) -> Option<usize> {
    if !body.contains(position) {
        return None;
    }
    let row = usize::from(position.y - body.y);
    (row < STOP_CHOICES.len()).then_some(row)
}

/// The theme picker (the `t` key, T43.1, T116.1): a centered " Theme " modal
/// listing the built-in themes in two collapsible groups, Dark then Light,
/// mirroring the settings overlay's grouped list -- each header carries the
/// fold glyph and toggles its group, the entries nest indented under it.
/// Every row renders in the currently active theme's classes (T115.1), with
/// a selection marker and a two-zone bottom line of hints left, buttons
/// right (T59.1). Rendered on top of everything else; the shell underneath
/// is already recoloured by the live preview, since `App::theme` resolves it.
pub(super) fn render_theme_modal(frame: &mut Frame, app: &App) {
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
pub(super) fn theme_row_line(
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
