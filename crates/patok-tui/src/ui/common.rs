//! Building blocks shared by every pane and modal: theme styles, buttons, key
//! hints, modal footers, group headers and the scrollbar. The pure button
//! geometry and click tests they share with the shell's mouse dispatch live in
//! [`crate::hit`].

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::hit::{button_line, footer_button_rects};
use crate::theme::Theme;

/// Entry rows indent this many spaces past the focus marker, nesting them under
/// their section header; headers keep the bare marker margin.
pub(super) const ENTRY_INDENT: usize = 4;

/// The theme's base style: the shell's background and foreground. Painting it
/// over an area recolours that area (and everything unstyled rendered into it)
/// for the built-in themes, and is a no-op for the default one.
pub(super) fn base_style(theme: Theme) -> Style {
    Style::new().fg(theme.foreground).bg(theme.background)
}

/// The selected row's style shared by every selection-bearing modal (T118.1):
/// the highlighted-text foreground on the normal modal background, bold --
/// only the foreground switches, the background never does.
pub(super) fn selected_row_style(theme: Theme) -> Style {
    base_style(theme)
        .fg(theme.highlighted_text)
        .add_modifier(Modifier::BOLD)
}

/// A modal bottom line's key-hint zone (T59.1): the hints joined with ` · `,
/// plain text without brackets, in the theme's low-emphasis footer colour, so
/// no hint ever carries the button accent.
pub(super) fn hint_line(hints: &[&str], colour: Color) -> Line<'static> {
    Line::styled(format!(" {}", hints.join(" · ")), Style::new().fg(colour))
}

/// The longest prefix of width-costed items that fits a budget: `base`
/// columns count first, then each item's cost plus `gap` columns between
/// items; the first cost that overflows and every one after it are
/// dropped. Returns how many leading costs were kept and the columns they
/// used, base and gaps included -- the shared tail-drop of the hint strip
/// ([`fitted_hints`]), the status line's buttons and the output frame
/// title's parts.
pub(super) fn fitted_prefix<I>(costs: I, base: usize, gap: usize, budget: usize) -> (usize, usize)
where
    I: IntoIterator<Item = usize>,
{
    let mut used = base;
    let mut kept = 0;
    for cost in costs {
        let next = used + cost + usize::from(kept > 0) * gap;
        if next > budget {
            break;
        }
        used = next;
        kept += 1;
    }
    (kept, used)
}

/// The hints that fit a frame's reserved bottom row (T71.1): the leading hints
/// while the strip the shared hint formatting builds -- one leading space and
/// ` · ` between hints -- stays within `width` columns; the first hint that
/// does not fit and every one after it are dropped, so a narrow frame sheds
/// trailing hints instead of overflowing its borders. Nothing fits when even
/// the first hint does not. The fit itself is the shared [`fitted_prefix`]
/// tail-drop with the strip's own leading space and ` · ` gap.
pub fn fitted_hints<'a>(hints: &[&'a str], width: usize) -> Vec<&'a str> {
    let costs = hints.iter().map(|hint| hint.chars().count());
    let (kept, _) = fitted_prefix(costs, 1, 3, width);
    hints[..kept].to_vec()
}

/// The columns a collapsed frame's line spends outside its title: the leading
/// rule dash and the trailing `─ [ ↓ ] ─` button.
const COLLAPSED_CHROME: usize = 10;

/// The width a collapsed frame's title spans may use on a line `width`
/// columns wide.
pub(super) fn collapsed_title_width(width: u16) -> usize {
    usize::from(width).saturating_sub(COLLAPSED_CHROME)
}

/// A frame collapsed to its single line: a rule with the title's `left` spans,
/// the `right` spans (the output frame's timer) at the end of the rule, then
/// the `[ arrow ]` button that expands it -- `─ Title ──── 00:00 ─ [ ↓ ] ─`.
/// The title spans must fit [`collapsed_title_width`]; a click anywhere on the
/// line focuses the frame like any frame click, so the button needs no rect of
/// its own.
pub(super) fn render_collapsed(
    frame: &mut Frame,
    area: Rect,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    arrow: &str,
    theme: Theme,
) {
    let used: usize = left
        .iter()
        .chain(&right)
        .map(|span| span.content.chars().count())
        .sum();
    let fill = collapsed_title_width(area.width).saturating_sub(used);
    let mut spans = vec![Span::raw("─")];
    spans.extend(left);
    spans.push(Span::raw("─".repeat(fill)));
    spans.extend(right);
    spans.push(Span::raw("─ "));
    spans.push(Span::styled(
        format!("[ {arrow} ]"),
        Style::new().fg(theme.highlighted_text),
    ));
    spans.push(Span::raw(" ─"));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The focused frame's inner split (T71.1): the bottom inner row reserved for
/// the frame's key hints strip and the rest for its content -- or the whole
/// inner area with no hints row when the frame is unfocused, so the strip is
/// completely absent and the content keeps every row. A frame too short to
/// spare a row (a one-row interior) also renders no strip, so nothing ever
/// overflows or squeezes the content to nothing on a tiny terminal.
pub(super) fn split_hints_row(inner: Rect, focused: bool) -> (Rect, Option<Rect>) {
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
pub(super) fn render_hints_strip(
    frame: &mut Frame,
    hints_row: Option<Rect>,
    hints: &[&str],
    theme: Theme,
) {
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
/// A modal's bottom line (T59.1): one line in two zones -- the key hints that
/// act on the modal's content left-aligned in a left-hand zone, the
/// modal-level action buttons right-aligned in a fixed right-hand zone, the
/// two separated by at least two columns. Hints render without brackets in
/// the theme's low-emphasis footer colour; buttons render through the shared
/// [`button_line`] with their bracketed key in the theme's button accent and
/// their label in the theme's foreground (T64.1). Nothing else on the line.
pub(super) fn render_modal_footer(
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

/// One group header line: the focus marker, the fold glyph (▾ expanded, ▸
/// folded) and the title, bold -- shared by the settings overlay's sections
/// and the theme picker's Dark/Light groups (T116.1), so the fold glyph
/// logic exists in one place.
pub(super) fn group_header_line(marker: &str, expanded: bool, title: &str) -> Line<'static> {
    Line::styled(
        format!("{marker}{} {title}", if expanded { '▾' } else { '▸' }),
        Style::new().add_modifier(Modifier::BOLD),
    )
}

/// A hand-rendered scrollbar on the body's right edge when the rows overflow.
/// The thumb wears the theme's `highlighted_text` class and the rail its
/// `scrollbar_rail` class (T121.1).
pub(super) fn render_scrollbar(
    frame: &mut Frame,
    theme: Theme,
    body: Rect,
    start: usize,
    total: usize,
) {
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
