//! The shared three-choice dialog component (finding 6 in
//! docs/private/structure-review.md): one generic choice list behind both the
//! stop dialog (T46.1, T148.1) and the settings overlay's unsaved-changes
//! dialog (T146.1) -- the area, the row hit-test, the Up/Down/Enter/Esc key
//! handling, the mouse hover and click behaviour and the renderer live here
//! once; the two dialogs instantiate them with their own titles, choices,
//! footer Esc labels and dispatch bodies. The owner keeps its state flat on
//! the paths the tests read (`stop_selected`, `confirm_selected`), so the
//! functions operate on that field instead of owning it.

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::{close_clicked, footer_button_code, render_modal_footer, selected_row_style};
use super::modal::{ModalTitle, render_modal_frame};
use crate::theme::Theme;

/// One labelled choice row of a three-choice dialog (finding 6): the label
/// rendered at the row's start and the one-line detail rendered after the
/// ` -- ` separator -- the same shape the stop dialog's `StopChoice` and the
/// unsaved-changes dialog's `SettingsChoice` already expose as inherent
/// methods, so the trait only forwards to them.
pub(crate) trait Choice {
    /// The row's label, rendered before the separator.
    fn label(&self) -> &'static str;
    /// The row's one-line explanation, rendered after the separator.
    fn detail(&self) -> &'static str;
}

/// The dialog's rect: a small centered modal, at least 54 columns wide and
/// 9 rows tall, clamped to the screen.
pub(super) fn choice_area(screen: Rect) -> Rect {
    let width = (screen.width * 3 / 5).max(54).min(screen.width);
    let height = 9.min(screen.height);
    super::modal::centered(screen, width, height)
}

/// The choice row the pointer sits on (T146.1, T148.1): the row index when
/// the position is on one of the dialog's choice rows, `None` on the
/// dialog's blank body padding below them, the border, the title, the
/// footer or the shell outside. A pure hit-test like
/// [`super::theme_row_at`].
pub(super) fn choice_row_at(position: Position, body: Rect, len: usize) -> Option<usize> {
    if !body.contains(position) {
        return None;
    }
    let row = usize::from(position.y - body.y);
    (row < len).then_some(row)
}

/// The rects the renderer hands back for the owner to record: the close
/// button, the choice rows' body and the bottom line -- the cells the
/// dialog's mouse hit-testing reads at the next event.
pub(super) struct ChoiceRects {
    /// The modal's close button rect (T66.1); a click on it runs Esc.
    pub close: Rect,
    /// The choice rows' body rect (T146.1, T148.1); the rows' mouse
    /// hit-testing reads it.
    pub body: Rect,
    /// The bottom line rect (T59.1); its buttons' mouse hit-testing reads it.
    pub footer: Rect,
}

/// One shared key step of a three-choice dialog (finding 6): Up -- and `k`
/// only when `jk` -- moves the selection up, Down -- and `j` only when `jk`
/// -- moves it down (both clamped into the `len` rows, the selection never
/// leaving the list), Enter asks the owner to confirm the choice at the
/// (clamped) selection, Esc asks it to close with no effect, and everything
/// else is swallowed. The stop dialog passes no `jk` aliases; the
/// unsaved-changes dialog passes them (the theme picker's convention).
/// Matches the raw `KeyCode` with no modifier checks, so Ctrl+J keeps
/// moving the selection exactly as it does today.
pub(crate) enum ChoiceKey {
    /// Esc: the dialog closes with no effect.
    Close,
    /// Enter: confirm the choice at the clamped selection index.
    Enter(usize),
    /// Everything else, including the selection moves: swallowed.
    Swallowed,
}

/// The shared key handling over the owner's selection field; see
/// [`ChoiceKey`].
pub(crate) fn choice_key(code: KeyCode, jk: bool, len: usize, selected: &mut usize) -> ChoiceKey {
    match code {
        KeyCode::Esc => ChoiceKey::Close,
        KeyCode::Up => {
            *selected = selected.saturating_sub(1);
            ChoiceKey::Swallowed
        }
        KeyCode::Char('k') if jk => {
            *selected = selected.saturating_sub(1);
            ChoiceKey::Swallowed
        }
        KeyCode::Down => {
            *selected = (*selected + 1).min(len - 1);
            ChoiceKey::Swallowed
        }
        KeyCode::Char('j') if jk => {
            *selected = (*selected + 1).min(len - 1);
            ChoiceKey::Swallowed
        }
        KeyCode::Enter => ChoiceKey::Enter((*selected).min(len - 1)),
        _ => ChoiceKey::Swallowed,
    }
}

/// The shared mouse step of a three-choice dialog (finding 6): a left click
/// on the close button or on one of the footer's buttons stands for that
/// button's key, which the owner runs through its own key path; a pointer
/// move over a choice row asks it to move the selection highlight there; a
/// left click inside a choice row asks it to select that row and then run
/// its Enter -- a click and Enter on a row are indistinguishable
/// (T146.1, T148.1). A move or click that misses every row -- the blank
/// body padding below the choices, the border, the title, the footer's
/// non-button columns -- or lands outside the dialog does nothing.
pub(crate) enum ChoiceMouse {
    /// The close button or a footer button: run this key through the
    /// dialog's key path.
    Key(KeyCode),
    /// A pointer move over a choice row: move the selection highlight there.
    Hover(usize),
    /// A left click on a choice row: select it, then run its Enter.
    Click(usize),
    /// Everything else: nothing happens.
    None,
}

/// The shared mouse handling; see [`ChoiceMouse`]. The checks keep today's
/// order: the close button first, then the footer's buttons, then the body
/// rows -- the footer sits outside the body, so the order cannot overlap.
pub(crate) fn choice_mouse(
    mouse: &MouseEvent,
    close: Rect,
    footer: Rect,
    esc_label: &'static str,
    body: Rect,
    len: usize,
) -> ChoiceMouse {
    if close_clicked(mouse, close) {
        return ChoiceMouse::Key(KeyCode::Esc);
    }
    if let Some(code) =
        footer_button_code(mouse, footer, &[("Enter", "Confirm"), ("Esc", esc_label)])
    {
        return ChoiceMouse::Key(code);
    }
    let position = Position::new(mouse.column, mouse.row);
    let row = choice_row_at(position, body, len);
    match mouse.kind {
        // The move moves only the highlight, never the dispatch: a move that
        // misses every row leaves the selection alone.
        MouseEventKind::Moved => row.map_or(ChoiceMouse::None, ChoiceMouse::Hover),
        // The click is the row's Enter key verbatim: it moves the selection
        // to the row, then runs the Enter dispatch, so a click and Enter on
        // the same row are indistinguishable.
        MouseEventKind::Down(MouseButton::Left) => {
            row.map_or(ChoiceMouse::None, ChoiceMouse::Click)
        }
        _ => ChoiceMouse::None,
    }
}

/// The shared renderer of a three-choice dialog (T46.1, T146.1, T148.1): a
/// centered bordered modal titled `title` with the close button top right,
/// the choices as a vertical list -- the selected choice in the shared
/// selected-row style (T118.1), its detail in the highlighted-text colour,
/// the others muted -- and a two-zone bottom line of the selection hint
/// left, the Confirm and `esc_label` buttons right (T59.1). Rendered on top
/// of everything else by the caller. Returns the rects the owner records
/// for its mouse hit-testing instead of writing the owner's cells, so the
/// component stays generic over the state.
pub(super) fn render_choice_dialog<T: Choice>(
    frame: &mut Frame,
    theme: Theme,
    title: &'static str,
    choices: &[T],
    selected: usize,
    esc_label: &'static str,
) -> ChoiceRects {
    let area = choice_area(frame.area());
    let chrome = render_modal_frame(frame, theme, area, ModalTitle::Centered(title));
    let inner = chrome.inner;
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let lines: Vec<Line> = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let focused = index == selected;
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
    render_modal_footer(
        frame,
        footer,
        &["↑↓ move"],
        &[("Enter", "Confirm"), ("Esc", esc_label)],
        theme,
    );
    ChoiceRects {
        close: chrome.close,
        body,
        footer,
    }
}
