//! The shared three-choice dialog's renderer (finding 6 in
//! docs/private/structure-review.md): the one `Frame`-touching half of the
//! generic choice list behind both the stop dialog (T46.1, T148.1) and the
//! settings overlay's unsaved-changes dialog (T146.1); the pure core -- the
//! area, the row hit-test, the key handling and the mouse behaviour the
//! shell's dispatch reads -- lives in [`crate::choice`] so app.rs reaches it
//! without importing the render side.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::common::{render_modal_footer, selected_row_style};
use super::modal::{ModalTitle, render_modal_frame};
use crate::choice::{Choice, ChoiceRects, choice_area};
use crate::theme::Theme;

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
