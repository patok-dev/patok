//! Answers to terminal queries, which vt100 parses but never answers.

use std::io::Write;

/// Reported for `OSC 10 ; ?`. The widget draws default colours as the host
/// terminal's default, which patok cannot know (the theme uses
/// `Color::Reset`). Dark is reported because it is the most common, and an
/// answer is required either way.
const DEFAULT_FG: (u8, u8, u8) = (0xe5, 0xe5, 0xe5);
/// Reported for `OSC 11 ; ?`; see [`DEFAULT_FG`].
const DEFAULT_BG: (u8, u8, u8) = (0, 0, 0);

/// Queued answers to the child's terminal queries. Programs such as
/// Claude Code and Codex send DA/DSR/OSC queries at startup and block until
/// a terminal answers; vt100 only parses them, so without these replies the
/// child hangs.
#[derive(Default)]
pub(super) struct Replies {
    pending: Vec<u8>,
}

impl Replies {
    /// Takes the replies queued since the last call.
    pub(super) fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    fn color(&mut self, code: u8, (r, g, b): (u8, u8, u8)) {
        // ST-terminated because the callback is not told whether the query
        // used BEL; ST is accepted by all common query parsers.
        let _ = write!(
            self.pending,
            "\x1b]{code};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\"
        );
    }
}

impl vt100::Callbacks for Replies {
    // The kitty keyboard query `CSI ? u` is deliberately unanswered: the child
    // receives legacy key encoding, and Codex's crossterm probe falls back on
    // the DA1 answer.
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i2.is_some() {
            return;
        }
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let (row, col) = screen.cursor_position();
        let (row, col) = (u32::from(row) + 1, u32::from(col) + 1);
        let _ = match (i1, c, first) {
            // DA1: VT100 with AVO, the common minimal answer.
            (None, 'c', 0) => self.pending.write_all(b"\x1b[?1;2c"),
            // DA2.
            (Some(b'>'), 'c', 0) => self.pending.write_all(b"\x1b[>0;0;0c"),
            // DSR: terminal OK.
            (None, 'n', 5) => self.pending.write_all(b"\x1b[0n"),
            // CPR and DECXCPR.
            (None, 'n', 6) => write!(self.pending, "\x1b[{row};{col}R"),
            (Some(b'?'), 'n', 6) => write!(self.pending, "\x1b[?{row};{col}R"),
            _ => Ok(()),
        };
    }

    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"10", b"?"] => self.color(10, DEFAULT_FG),
            [b"11", b"?"] => self.color(11, DEFAULT_BG),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(query: &str) -> Vec<u8> {
        let mut p = vt100::Parser::new_with_callbacks(24, 80, 0, Replies::default());
        p.process(query.as_bytes());
        p.callbacks_mut().take()
    }

    #[test]
    fn da1() {
        assert_eq!(answer("\x1b[c"), b"\x1b[?1;2c");
        assert_eq!(answer("\x1b[0c"), b"\x1b[?1;2c");
    }

    #[test]
    fn da2() {
        assert_eq!(answer("\x1b[>c"), b"\x1b[>0;0;0c");
    }

    #[test]
    fn dsr_status() {
        assert_eq!(answer("\x1b[5n"), b"\x1b[0n");
    }

    #[test]
    fn cursor_position() {
        assert_eq!(answer("\x1b[3;5H\x1b[6n"), b"\x1b[3;5R");
        assert_eq!(answer("\x1b[3;5H\x1b[?6n"), b"\x1b[?3;5R");
    }

    #[test]
    fn osc_colors_with_either_terminator() {
        for term in ["\x07", "\x1b\\"] {
            assert_eq!(
                answer(&format!("\x1b]10;?{term}")),
                b"\x1b]10;rgb:e5e5/e5e5/e5e5\x1b\\"
            );
            assert_eq!(
                answer(&format!("\x1b]11;?{term}")),
                b"\x1b]11;rgb:0000/0000/0000\x1b\\"
            );
        }
    }

    #[test]
    fn take_empties_the_queue() {
        let mut p = vt100::Parser::new_with_callbacks(24, 80, 0, Replies::default());
        p.process(b"\x1b[c");
        assert!(!p.callbacks_mut().take().is_empty());
        assert!(p.callbacks_mut().take().is_empty());
    }

    #[test]
    fn unrelated_input_is_not_answered() {
        assert!(answer("\x1b[?2004h").is_empty());
        assert!(answer("hello\r\n").is_empty());
        assert!(answer("\x1b]0;title\x07").is_empty());
    }
}
