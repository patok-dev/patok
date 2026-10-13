//! The pure geometry the renderers, the dialog editor and the choice dialog
//! share (finding 19, the rest, in docs/private/structure-review-followup.md):
//! the hard-wrap boundary walk and the centred-rect computation. Imports
//! nothing from `app`, so nothing that reaches it can layer-cycle through it.

use ratatui::layout::Rect;

/// One hard-wrap implementation: the character ranges of a `len`-long
/// sequence cut at `width` columns -- the shared boundary walk behind the
/// styled [`crate::ui::output::wrap_line`], the plain
/// [`crate::ui::output::wrap`] and the dialog editor's
/// [`crate::dialog_editor::DialogEditor::rows`]. The caller resolves its own
/// zero width first: a zero step is an error, exactly like the slice `chunks`
/// it replaces.
pub(crate) fn wrap_chunks(
    len: usize,
    width: usize,
) -> impl Iterator<Item = std::ops::Range<usize>> {
    (0..len)
        .step_by(width)
        .map(move |start| start..(start + width).min(len))
}

/// A modal's centred rect: the one area computation every modal's `*_area`
/// function repeated (finding 20). The caller computes and clamps the width
/// and height first, exactly as before.
pub(crate) fn centered(screen: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}
