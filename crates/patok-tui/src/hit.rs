//! Pure button geometry and pointer hit-tests, moved out of the render side
//! (finding 19 in docs/private/structure-review.md): the display text, the
//! rectangles and the click tests every modal's bottom-line buttons and
//! close buttons share, the merged view's frame hit-test, the status bar's
//! chip hit-tests and the modals' row hit-tests -- all pure functions with
//! no `Frame` and no terminal, so the shell's key and mouse dispatch
//! (app.rs) and the renderers (ui) reach them through the same one
//! definition instead of importing each other.

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

#[cfg(test)]
use crate::choice::choice_row_at;
#[cfg(test)]
use crate::overlay::SETTINGS_CHOICES;
use crate::theme::Theme;
use crate::view::FrameFocus;
#[cfg(test)]
use crate::view::STOP_CHOICES;

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

/// A modal button (T44.1, extended by T59.1, split by T64.1): the bracketed
/// ` [ Key ] Label ` style -- the brackets and the key in the theme's button
/// accent colour, the label after them in the theme's regular foreground
/// colour, foreground only (no background, no bold) -- shared by every modal's
/// bottom-line buttons and close buttons, so every modal draws the same
/// buttons and they recolour with the active theme. The two spans' text joins
/// into exactly [`button_text`], whose width sizes the hit-tested rectangles.
pub(crate) fn button_line(key: &str, label: &str, theme: Theme) -> Line<'static> {
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

/// An inactive button (T5.2): the same text in the low-emphasis colour.
pub(crate) fn inactive_button_line(key: &str, label: &str, theme: Theme) -> Line<'static> {
    let style = Style::new().fg(theme.muted_text);
    let mut spans = vec![Span::styled(format!(" [ {key} ] "), style)];
    if !label.is_empty() {
        spans.push(Span::styled(format!("{label} "), style));
    }
    Line::from(spans)
}

/// A modal's top-right close button (T66.1): the bracketed ` [ x ] ` in the
/// theme's button accent colour, foreground only -- drawn through the shared
/// [`button_line`] on every modal's title row, so it recolours with the
/// active theme like every other button and no colour literal exists here.
/// A click on its rectangle ([`close_button_rect`]) runs the modal's Esc key.
pub(crate) fn close_button_line(theme: Theme) -> Line<'static> {
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

/// Whether a left click lands on a modal's close button (T66.1): the click
/// must be a press inside the button's rectangle, which the renderer recorded
/// at the last render through [`crate::ui::close_button_rect`]. The zero rect
/// a too-narrow modal records contains no real position, so the click misses
/// there naturally. Every other mouse event misses too.
pub(crate) fn close_clicked(mouse: &MouseEvent, close: Rect) -> bool {
    left_click_in(mouse, close)
}

/// Whether the event is a left-button press inside `rect`. A zero rect
/// contains no position, so it never matches.
pub(crate) fn left_click_in(mouse: &MouseEvent, rect: Rect) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return false;
    }
    rect.contains(Position::new(mouse.column, mouse.row))
}

/// The key a left click on a modal's bottom-line button stands for (T59.1):
/// the button whose rectangle (computed by [`footer_button_rects`] from the
/// same button list the renderer drew) contains the click. `None` for hint
/// clicks, clicks elsewhere and every other mouse event -- hints stay
/// non-interactive.
pub(crate) fn footer_button_code(
    mouse: &MouseEvent,
    footer: Rect,
    buttons: &[(&str, &str)],
) -> Option<KeyCode> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let position = Position::new(mouse.column, mouse.row);
    let index = footer_button_rects(footer, buttons)
        .iter()
        .position(|rect| rect.contains(position))?;
    match buttons[index].0 {
        "Enter" => Some(KeyCode::Enter),
        "Esc" => Some(KeyCode::Esc),
        _ => None,
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

/// Whether a left click lands on one of the status bar's interactive chips
/// (T133.1, T134.1): the click must be a press inside the chip's rectangle,
/// which the renderer recorded at the last render through
/// [`crate::ui::status_widget`] -- the m menu chip, the idle Enter chip and
/// the running Esc stop chip are mouse aliases of their keys. The zero rect a
/// chip-less render records contains no real position, so the click misses
/// there naturally, like [`close_clicked`] for close buttons.
/// Every other mouse event misses too.
pub(crate) fn chip_clicked(mouse: &MouseEvent, chip: Rect) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return false;
    }
    let position = Position::new(mouse.column, mouse.row);
    chip.contains(position)
}

/// The theme picker's entry the pointer sits on (T43.1, T116.1): the visible
/// entry index when the position is on one of the body's visible entry rows,
/// `None` on the border, the footer, the shell behind the modal or a body row
/// past the last visible entry (a folded group's hidden area). A pure
/// hit-test like [`frame_at`].
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
/// footer or the shell outside. A pure hit-test like [`menu_row_at`], over
/// the shared choice-dialog rows (finding 6). Test-only: the confirm dialog
/// is keyboard-driven; the unit tests pin the shared row geometry.
#[cfg(test)]
pub fn confirm_row_at(position: Position, body: Rect) -> Option<usize> {
    choice_row_at(position, body, SETTINGS_CHOICES.len())
}

/// The stop dialog's choice the pointer sits on (T148.1): the row index
/// when the position is on one of the dialog's three choice rows, `None`
/// on the dialog's blank body padding below them, the border, the title,
/// the footer or the shell outside. A pure hit-test like [`menu_row_at`],
/// over the shared choice-dialog rows (finding 6). Test-only: the stop dialog
/// is keyboard-driven; the unit tests pin the shared row geometry.
#[cfg(test)]
pub fn stop_row_at(position: Position, body: Rect) -> Option<usize> {
    choice_row_at(position, body, STOP_CHOICES.len())
}

/// The settings list's entry the pointer sits on: the visible entry index
/// when the position is on one of the list's rendered rows (offset by the
/// current scroll), `None` outside the list rect or on a viewport row past
/// the last visible entry. A pure hit-test like [`theme_row_at`].
pub fn settings_row_at(
    position: Position,
    list: Rect,
    scroll: usize,
    visible_len: usize,
) -> Option<usize> {
    if !list.contains(position) {
        return None;
    }
    let index = scroll + usize::from(position.y - list.y);
    (index < visible_len).then_some(index)
}
