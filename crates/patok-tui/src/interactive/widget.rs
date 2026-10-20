//! Paints a vt100 screen into a ratatui buffer: the in-repo widget SPEC
//! allows in place of tui-term.

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::Widget,
};

/// The child's screen as a widget. The cursor is painted into the buffer as a
/// reversed cell, so the widget works with only a `Buffer`.
pub(crate) struct ScreenView<'a> {
    screen: &'a vt100::Screen,
    cursor: bool,
}

impl<'a> ScreenView<'a> {
    pub(crate) fn new(screen: &'a vt100::Screen) -> Self {
        Self {
            screen,
            cursor: true,
        }
    }

    pub(crate) fn cursor(self, show: bool) -> Self {
        Self {
            cursor: show,
            ..self
        }
    }
}

impl Widget for ScreenView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (rows, cols) = self.screen.size();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &mut buf[(area.x + x, area.y + y)];
                // Cells past the screen's size must not keep stale content.
                let Some(vc) = (y < rows && x < cols)
                    .then(|| self.screen.cell(y, x))
                    .flatten()
                else {
                    cell.reset();
                    continue;
                };
                let style = style_of(vc);
                if vc.is_wide_continuation() {
                    // Same shape `Buffer::set_stringn` leaves behind; the
                    // diff skips it, the style keeps the background whole.
                    cell.reset();
                    cell.set_style(style);
                } else if vc.is_wide() && (x + 1 >= area.width || x + 1 >= cols) {
                    // The glyph would overflow the area.
                    cell.set_symbol(" ").set_style(style);
                } else if vc.has_contents() {
                    cell.set_symbol(vc.contents()).set_style(style);
                } else {
                    // Empty cells still carry a background colour.
                    cell.set_symbol(" ").set_style(style);
                }
            }
        }

        if self.cursor
            && let Some(mut p) = cursor_position(self.screen, area)
        {
            // On a wide glyph's second half, highlight the whole glyph.
            let on_continuation = self
                .screen
                .cell(p.y - area.y, p.x - area.x)
                .is_some_and(|c| c.is_wide_continuation());
            if on_continuation && p.x > area.x {
                p.x -= 1;
            }
            let cell = &mut buf[p];
            // Toggle, so an inverse cell under the cursor still shows it.
            if cell.modifier.contains(Modifier::REVERSED) {
                cell.modifier.remove(Modifier::REVERSED);
            } else {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

/// Where the child's cursor sits in `area`, or `None` when it is hidden,
/// scrolled back, or outside `area`. For `Frame::set_cursor_position`.
pub(crate) fn cursor_position(screen: &vt100::Screen, area: Rect) -> Option<Position> {
    // In scrollback the live cursor's position has no meaning.
    if screen.hide_cursor() || screen.scrollback() > 0 {
        return None;
    }
    let (r, c) = screen.cursor_position();
    if r >= area.height || c >= area.width {
        return None;
    }
    Some(Position::new(area.x + c, area.y + r))
}

fn style_of(cell: &vt100::Cell) -> Style {
    // Both colours are always set, so the widget overwrites whatever theme
    // styling the buffer held before.
    let mut modifier = Modifier::empty();
    for (on, flag) in [
        (cell.bold(), Modifier::BOLD),
        (cell.dim(), Modifier::DIM),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        // The host does the swap: it cannot be done here for `Default`.
        (cell.inverse(), Modifier::REVERSED),
    ] {
        if on {
            modifier |= flag;
        }
    }
    Style::default()
        .fg(color(cell.fgcolor()))
        .bg(color(cell.bgcolor()))
        .add_modifier(modifier)
}

/// `Default` maps to `Reset`: the child's default is the host terminal's,
/// matching the dark answer `replies.rs` gives to OSC 10/11. 0-15 stay
/// `Indexed` so the host palette decides them.
fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(bytes: &[u8], rows: u16, cols: u16, area: Rect, w: u16, h: u16) -> Buffer {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(bytes);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        ScreenView::new(parser.screen()).render(area, &mut buf);
        buf
    }

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn styles(buf: &Buffer) -> String {
        let spell = |colour: Color| match colour {
            Color::Reset => "-".to_string(),
            Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
            Color::Indexed(i) => format!("i{i}"),
            other => format!("{other:?}"),
        };
        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                if c.fg != Color::Reset || c.bg != Color::Reset || !c.modifier.is_empty() {
                    lines.push(format!(
                        "{x},{y} {:?} fg={} bg={} {:?}",
                        c.symbol(),
                        spell(c.fg),
                        spell(c.bg),
                        c.modifier
                    ));
                }
            }
        }
        lines.join("\n")
    }

    fn snap(buf: &Buffer) -> String {
        format!("{}\n---\n{}", text(buf), styles(buf))
    }

    fn full(rows: u16, cols: u16) -> Rect {
        Rect::new(0, 0, cols, rows)
    }

    #[test]
    fn plain_text_and_line_wrap() {
        let buf = render(b"hello\r\nworld", 4, 10, full(4, 10), 10, 4);
        insta::assert_snapshot!(snap(&buf));
    }

    #[test]
    fn sgr_colors_indexed_rgb_and_default() {
        let bytes = b"\x1b[31mR\x1b[42mG\x1b[38;5;200mI\x1b[38;2;1;2;3mT\x1b[0mN";
        let buf = render(bytes, 1, 8, full(1, 8), 8, 1);
        insta::assert_snapshot!(snap(&buf));
    }

    #[test]
    fn sgr_attributes() {
        let bytes = b"\x1b[1mB\x1b[0m\x1b[2mD\x1b[0m\x1b[3mI\x1b[0m\x1b[4mU\x1b[0m\x1b[7mV\x1b[0m";
        let buf = render(bytes, 1, 8, full(1, 8), 8, 1);
        insta::assert_snapshot!(snap(&buf));
    }

    #[test]
    fn wide_chars() {
        let buf = render("a中b".as_bytes(), 1, 6, full(1, 6), 6, 1);
        assert_eq!(buf[(1, 0)].symbol(), "中");
        assert_eq!(buf[(2, 0)].symbol(), " ");
        assert_eq!(buf[(3, 0)].symbol(), "b");
        insta::assert_snapshot!(snap(&buf));
    }

    #[test]
    fn wide_char_clipped_at_area_edge() {
        // The wide char lands on the area's last column.
        let buf = render("ab中".as_bytes(), 1, 6, Rect::new(0, 0, 3, 1), 6, 1);
        assert_eq!(buf[(2, 0)].symbol(), " ");
        assert_eq!(buf[(3, 0)].symbol(), " ");
        insta::assert_snapshot!(snap(&buf));
    }

    #[test]
    fn renders_at_area_offset_and_clears_outside_screen() {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process(b"abcd\r\nefgh");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        for y in 0..5 {
            for x in 0..10 {
                buf[(x, y)].set_symbol("x");
            }
        }
        ScreenView::new(parser.screen())
            .cursor(false)
            .render(Rect::new(2, 1, 6, 3), &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "x");
        assert_eq!(buf[(2, 0)].symbol(), "x");
        assert_eq!(buf[(8, 1)].symbol(), "x");
        assert_eq!(buf[(2, 1)].symbol(), "a");
        insta::assert_snapshot!(snap(&buf));
    }

    fn reversed(buf: &Buffer) -> Vec<(u16, u16)> {
        let mut v = Vec::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].modifier.contains(Modifier::REVERSED) {
                    v.push((x, y));
                }
            }
        }
        v
    }

    #[test]
    fn cursor_hidden_by_dectcem() {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process(b"ab\x1b[?25l");
        let mut buf = Buffer::empty(full(2, 4));
        ScreenView::new(parser.screen()).render(full(2, 4), &mut buf);
        assert!(reversed(&buf).is_empty());
        assert_eq!(cursor_position(parser.screen(), full(2, 4)), None);
    }

    #[test]
    fn cursor_on_inverse_cell_clears_reversed() {
        // Writing the inverse char leaves the cursor one to the right;
        // move back onto it.
        let buf = render(b"\x1b[7mA\x1b[0m\x1b[D", 1, 4, full(1, 4), 4, 1);
        assert!(!buf[(0, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn cursor_paints_reversed_and_can_be_disabled() {
        let mut parser = vt100::Parser::new(1, 4, 0);
        parser.process(b"ab");
        let mut buf = Buffer::empty(full(1, 4));
        ScreenView::new(parser.screen()).render(full(1, 4), &mut buf);
        assert_eq!(reversed(&buf), vec![(2, 0)]);
        let mut buf = Buffer::empty(full(1, 4));
        ScreenView::new(parser.screen())
            .cursor(false)
            .render(full(1, 4), &mut buf);
        assert!(reversed(&buf).is_empty());
    }

    #[test]
    fn cursor_on_wide_continuation_highlights_glyph() {
        // Cursor addressed to column 2 (1-based 2) = the continuation half.
        let buf = render("中\x1b[1;2H".as_bytes(), 1, 4, full(1, 4), 4, 1);
        assert_eq!(reversed(&buf), vec![(0, 0)]);
    }

    #[test]
    fn cursor_position_offsets_and_bounds() {
        let mut parser = vt100::Parser::new(3, 5, 0);
        parser.process(b"a\r\nbc");
        let area = Rect::new(4, 2, 5, 3);
        assert_eq!(
            cursor_position(parser.screen(), area),
            Some(Position::new(6, 3))
        );
        assert_eq!(
            cursor_position(parser.screen(), Rect::new(0, 0, 2, 3)),
            None
        );
        assert_eq!(
            cursor_position(parser.screen(), Rect::new(0, 0, 5, 1)),
            None
        );
    }

    #[test]
    fn cursor_hidden_in_scrollback() {
        let mut parser = vt100::Parser::new(2, 5, 10);
        parser.process(b"1\r\n2\r\n3\r\n4");
        parser.screen_mut().set_scrollback(1);
        assert_eq!(cursor_position(parser.screen(), full(2, 5)), None);
        let mut buf = Buffer::empty(full(2, 5));
        ScreenView::new(parser.screen()).render(full(2, 5), &mut buf);
        assert_eq!(text(&buf), "2\n3");
        assert!(reversed(&buf).is_empty());
    }
}
