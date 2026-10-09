//! The shared modal frame (finding 20 in docs/private/structure-review.md):
//! the one centred area computation and the one chrome every modal repeats --
//! the `Clear`, the bordered block, the title and the top-right close button
//! -- so a modal's renderer owns only its body and footer content. The frame
//! returns the rects the modal records for its mouse hit-testing instead of
//! writing the owner's cells, following the choice-dialog precedent.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear};

use super::common::{base_style, close_button_line, close_button_rect};
use crate::theme::Theme;

/// A modal's centred rect: the one area computation every modal's `*_area`
/// function repeated (finding 20). The caller computes and clamps the width
/// and height first, exactly as before.
pub(super) fn centered(screen: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

/// A modal title's one spelling (finding 20): left-aligned on the
/// add-task/inject-task dialog's title row, centred on every other modal's --
/// the two forms the renderers spelled separately before.
pub(super) enum ModalTitle {
    /// A left-aligned title on the top border.
    Left(&'static str),
    /// A centred title on the top border.
    Centered(&'static str),
}

/// The rects the shared frame hands back for the modal to record (finding 20):
/// the inner content rect and the close button's hit-test rectangle -- the
/// cells the modal's own mouse hit-testing reads at the next event. The
/// caller keeps the full area it passed in.
pub(super) struct ModalFrame {
    /// The modal's inner content rect, border excluded.
    pub inner: Rect,
    /// The close button's hit-test rect (T66.1); a click on it runs the
    /// modal's Esc key.
    pub close: Rect,
}

/// Renders one modal's chrome (finding 20): `Clear` over the area, then the
/// bordered block in the theme's base style with `title` and the top-right
/// close button (T66.1). Returns the frame's rects instead of writing the
/// owner's cells, so every modal keeps its own body split and recording.
pub(super) fn render_modal_frame(
    frame: &mut Frame,
    theme: Theme,
    area: Rect,
    title: ModalTitle,
) -> ModalFrame {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(base_style(theme));
    let block = match title {
        ModalTitle::Left(title) => block.title(Span::raw(title)),
        ModalTitle::Centered(title) => block.title_top(Line::from(title).centered()),
    }
    .title_top(close_button_line(theme).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    ModalFrame {
        inner,
        close: close_button_rect(area),
    }
}
