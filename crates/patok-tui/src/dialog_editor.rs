//! The dialog input's multi-line text editor (finding 19, T154.2): the buffer,
//! its cursor, the Up/Down goal column and the wrap width the renderer reports,
//! split out of `App` as a self-contained type -- `App` routes keys to it and
//! owns the modal chrome (open/close, status line, submit policy) around it.
//! Pure: no I/O, so it is tested without a terminal.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::output::wrap_chunks;

/// One visual row of the dialog text: a character range of the editor's text.
/// `last` marks the final row of a logical line (the cursor may sit at its end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialogRow {
    pub start: usize,
    pub end: usize,
    pub last: bool,
}

/// The dialog input's text editor: the buffer, its cursor, the column Up/Down
/// try to return to across rows of different lengths, and the wrap width the
/// renderer reports. The buffer is kept when the dialog closes.
#[derive(Default)]
pub struct DialogEditor {
    /// The buffer; kept when the dialog is closed.
    text: String,
    /// Cursor as a character index into `text`; read it through [`DialogEditor::cursor`].
    cursor: usize,
    /// Column Up/Down try to return to across rows of different lengths.
    goal_col: Option<usize>,
    /// Wrap width of the input at the last render (0 before the first).
    width: Cell<usize>,
}

impl DialogEditor {
    /// The buffer; kept when the dialog is closed.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the whole buffer without touching the cursor, matching a raw
    /// field write: an out-of-range cursor stays raw until [`DialogEditor::cursor`]
    /// clamps it on read.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_owned();
    }

    /// The cursor as a character index into the text, clamped to its length.
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.text.chars().count())
    }

    /// Stores the cursor raw, without clamping; read it through
    /// [`DialogEditor::cursor`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    /// The render-side write of the wrap width: the renderer holds only
    /// `&App`, so the width travels through a `Cell`.
    pub fn set_width(&self, width: usize) {
        self.width.set(width);
    }

    /// Inserts `text` at the cursor, which lands after it.
    pub fn insert(&mut self, text: &str) {
        let cursor = self.cursor();
        let at = self.byte_index(cursor);
        self.text.insert_str(at, text);
        self.cursor = cursor + text.chars().count();
    }

    /// Pasted text appends at the cursor; CRLF and lone CR become LF and the
    /// Up/Down goal column is forgotten.
    pub fn insert_paste(&mut self, text: &str) {
        self.goal_col = None;
        self.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    /// Clears the buffer and homes the cursor. The Up/Down goal column is
    /// left alone, matching every call site: Ctrl+U and the submit-lifecycle
    /// clears never touch it.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Forgets the Up/Down goal column; the dialog's key path does this on
    /// every key except Up/Down themselves.
    pub fn reset_goal(&mut self) {
        self.goal_col = None;
    }

    /// The pure editing keys: character insert, Backspace/Delete, Left/Right
    /// (word motion on Ctrl or Alt), Home/End on the logical line and
    /// Up/Down across visual rows. Everything else is a no-op -- the modal
    /// chrome keys (Esc, Ctrl+C, Ctrl+S, Ctrl+U, Enter) never reach it.
    pub fn on_key(&mut self, key: KeyEvent, ctrl: bool) {
        let word = ctrl || key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char(c) if !ctrl => self.insert(c.encode_utf8(&mut [0; 4])),
            // Shift-Enter breaks the line at the cursor (T41.1): multi-line
            // requests are typed inside the input.
            KeyCode::Enter if shift => self.insert("\n"),
            KeyCode::Backspace => {
                let cursor = self.cursor();
                if cursor > 0 {
                    let start = self.byte_index(cursor - 1);
                    let end = self.byte_index(cursor);
                    self.text.replace_range(start..end, "");
                    self.cursor = cursor - 1;
                }
            }
            KeyCode::Delete => {
                let cursor = self.cursor();
                if cursor < self.text.chars().count() {
                    let start = self.byte_index(cursor);
                    let end = self.byte_index(cursor + 1);
                    self.text.replace_range(start..end, "");
                }
            }
            KeyCode::Left if word => self.cursor = self.word_left(),
            KeyCode::Right if word => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.cursor().saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor() + 1).min(self.text.chars().count()),
            KeyCode::Home => self.cursor = self.line_bounds().0,
            KeyCode::End => self.cursor = self.line_bounds().1,
            KeyCode::Up => self.move_vertically(-1),
            KeyCode::Down => self.move_vertically(1),
            _ => {}
        }
    }

    /// Visual rows of the text when wrapped at the last rendered width; width
    /// 0 (never rendered) wraps at `usize::MAX`, i.e. one row per logical line.
    /// The cutting walk itself is the shared [`wrap_chunks`]; an empty logical
    /// line still emits its own row, which the walk alone would not.
    pub fn rows(&self) -> Vec<DialogRow> {
        let width = match self.width.get() {
            0 => usize::MAX,
            w => w,
        };
        let mut rows = Vec::new();
        let mut start = 0;
        for line in self.text.split('\n') {
            let len = line.chars().count();
            if len == 0 {
                rows.push(DialogRow {
                    start,
                    end: start,
                    last: true,
                });
            } else {
                for range in wrap_chunks(len, width) {
                    rows.push(DialogRow {
                        start: start + range.start,
                        end: start + range.end,
                        last: range.end == len,
                    });
                }
            }
            start += len + 1;
        }
        rows
    }

    /// Index of the visual row holding the cursor.
    pub fn cursor_row(&self, rows: &[DialogRow]) -> usize {
        let cursor = self.cursor();
        rows.iter()
            .position(|r| cursor < r.end || (cursor == r.end && r.last))
            .unwrap_or(rows.len().saturating_sub(1))
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Character range `(start, end)` of the logical line holding the cursor.
    fn line_bounds(&self) -> (usize, usize) {
        let cursor = self.cursor();
        let (mut start, mut index) = (0, 0);
        for c in self.text.chars() {
            if index >= cursor && c == '\n' {
                break;
            }
            index += 1;
            if c == '\n' {
                start = index;
            }
        }
        (start, index)
    }

    fn word_left(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor();
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    fn move_vertically(&mut self, delta: isize) {
        let rows = self.rows();
        let current = self.cursor_row(&rows);
        let Some(target) = current.checked_add_signed(delta).and_then(|i| rows.get(i)) else {
            return;
        };
        let col = self.goal_col.unwrap_or(self.cursor() - rows[current].start);
        self.goal_col = Some(col);
        let max = if target.last {
            target.end - target.start
        } else {
            target.end - target.start - 1
        };
        self.cursor = target.start + col.min(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor_with(text: &str) -> DialogEditor {
        let mut editor = DialogEditor::default();
        editor.set_text(text);
        editor.set_cursor(text.chars().count());
        editor
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn insert_lands_the_cursor_after_the_text() {
        let mut editor = editor_with("ab");
        editor.set_cursor(1);
        editor.insert("X");
        assert_eq!((editor.text(), editor.cursor()), ("aXb", 2));
        editor.insert("YZ");
        assert_eq!((editor.text(), editor.cursor()), ("aXYZb", 4));
    }

    #[test]
    fn insert_counts_characters_not_bytes() {
        let mut editor = editor_with("日本");
        editor.set_cursor(1);
        editor.insert("語");
        assert_eq!((editor.text(), editor.cursor()), ("日語本", 2));
        editor.insert("x");
        assert_eq!((editor.text(), editor.cursor()), ("日語x本", 3));
    }

    #[test]
    fn backspace_deletes_the_character_before_the_cursor() {
        let mut editor = editor_with("héllo");
        editor.set_cursor(2);
        editor.on_key(key(KeyCode::Backspace, KeyModifiers::NONE), false);
        assert_eq!((editor.text(), editor.cursor()), ("hllo", 1));
        // Over a newline it joins the lines.
        editor.set_text("end\ntail");
        editor.set_cursor(4);
        editor.on_key(key(KeyCode::Backspace, KeyModifiers::NONE), false);
        assert_eq!((editor.text(), editor.cursor()), ("endtail", 3));
        // At position 0 it does nothing.
        editor.set_cursor(0);
        editor.on_key(key(KeyCode::Backspace, KeyModifiers::NONE), false);
        assert_eq!((editor.text(), editor.cursor()), ("endtail", 0));
    }

    #[test]
    fn delete_deletes_the_character_after_the_cursor() {
        let mut editor = editor_with("abc");
        editor.set_cursor(1);
        editor.on_key(key(KeyCode::Delete, KeyModifiers::NONE), false);
        assert_eq!((editor.text(), editor.cursor()), ("ac", 1));
        // At the end of the buffer it does nothing.
        editor.set_cursor(2);
        editor.on_key(key(KeyCode::Delete, KeyModifiers::NONE), false);
        assert_eq!(editor.text(), "ac");
    }

    #[test]
    fn shift_enter_breaks_the_line_at_the_cursor() {
        let mut editor = editor_with("ab");
        editor.on_key(key(KeyCode::Enter, KeyModifiers::SHIFT), false);
        assert_eq!((editor.text(), editor.cursor()), ("ab\n", 3));
        editor.insert("cd");
        assert_eq!(editor.text(), "ab\ncd");
    }

    #[test]
    fn ctrl_and_alt_left_right_move_by_words() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let ctrl = modifiers.contains(KeyModifiers::CONTROL);
            let mut editor = editor_with("one  two three");
            editor.on_key(key(KeyCode::Left, modifiers), ctrl);
            assert_eq!(editor.cursor(), 9);
            editor.on_key(key(KeyCode::Left, modifiers), ctrl);
            assert_eq!(editor.cursor(), 5);
            editor.on_key(key(KeyCode::Right, modifiers), ctrl);
            assert_eq!(editor.cursor(), 8);
            editor.on_key(key(KeyCode::Right, modifiers), ctrl);
            assert_eq!(editor.cursor(), 14);
        }
    }

    #[test]
    fn plain_left_right_move_by_characters() {
        let mut editor = editor_with("ab\ncd");
        editor.set_cursor(0);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 2, "the cursor moved past the newline");
        editor.on_key(key(KeyCode::Left, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 1);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Right, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 5, "no move past the end");
    }

    #[test]
    fn home_and_end_land_on_the_logical_line_bounds() {
        let mut editor = editor_with("one\ntwo\nthree");
        editor.set_cursor(9);
        editor.on_key(key(KeyCode::Home, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 8);
        editor.on_key(key(KeyCode::End, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 13);
        editor.set_cursor(0);
        editor.on_key(key(KeyCode::Home, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 0);
        editor.on_key(key(KeyCode::End, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 3);
    }

    #[test]
    fn rows_wrap_at_the_reported_width_only() {
        // Unrendered (width 0): one row per logical line.
        let editor = editor_with("one\ntwo\n");
        assert_eq!(
            editor.rows(),
            vec![
                DialogRow {
                    start: 0,
                    end: 3,
                    last: true
                },
                DialogRow {
                    start: 4,
                    end: 7,
                    last: true
                },
                DialogRow {
                    start: 8,
                    end: 8,
                    last: true
                },
            ]
        );
        // Wrapped at width 5: abcde / fghij / kl.
        let editor = editor_with("abcdefghijkl");
        editor.set_width(5);
        assert_eq!(
            editor.rows(),
            vec![
                DialogRow {
                    start: 0,
                    end: 5,
                    last: false
                },
                DialogRow {
                    start: 5,
                    end: 10,
                    last: false
                },
                DialogRow {
                    start: 10,
                    end: 12,
                    last: true
                },
            ]
        );
    }

    #[test]
    fn empty_text_has_one_empty_row() {
        let editor = DialogEditor::default();
        assert_eq!(
            editor.rows(),
            vec![DialogRow {
                start: 0,
                end: 0,
                last: true
            }]
        );
    }

    #[test]
    fn cursor_row_holds_the_cursor() {
        let mut editor = editor_with("abcdef\nab\nabcdef");
        editor.set_width(5);
        let rows = editor.rows();
        // Row ends take the cursor only on a logical line's last row.
        editor.set_cursor(5);
        assert_eq!(
            editor.cursor_row(&rows),
            1,
            "the end of a non-last row belongs to the next row"
        );
        editor.set_cursor(6);
        assert_eq!(
            editor.cursor_row(&rows),
            1,
            "the newline sits on the last row"
        );
        editor.set_cursor(9);
        assert_eq!(editor.cursor_row(&rows), 2);
        editor.set_cursor(14);
        assert_eq!(editor.cursor_row(&rows), 3);
        editor.set_cursor(16);
        assert_eq!(editor.cursor_row(&rows), 4, "end of the last row");
        // No row holds a cursor past the end of an empty row list.
        assert_eq!(editor.cursor_row(&[]), 0);
    }

    #[test]
    fn up_down_keep_the_column_and_clamp_per_row() {
        let mut editor = editor_with("abcdef\nab\nabcdef");
        editor.set_cursor(14);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 9, "clamped to the short line");
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 4, "the column is remembered");
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 4, "no row above");
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 14);
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 14, "no row below");
    }

    #[test]
    fn up_down_between_wrapped_rows() {
        let mut editor = editor_with("abcdefghijkl");
        editor.set_width(5);
        assert_eq!(editor.cursor(), 12);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 7);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 2);
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 12);
    }

    #[test]
    fn insert_paste_normalizes_newlines() {
        let mut editor = DialogEditor::default();
        editor.insert_paste("a\r\nb\rc");
        assert_eq!((editor.text(), editor.cursor()), ("a\nb\nc", 5));
    }

    #[test]
    fn reset_goal_and_paste_forget_the_column_but_typing_keeps_it() {
        // reset_goal: the App key path's call before every non-Up/Down key.
        let mut editor = editor_with("abcdef\nab\nabcdef");
        editor.set_cursor(14);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 14, "the column is remembered");
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        editor.reset_goal();
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 12, "reset_goal forgot the column");
        // A paste forgets it too.
        editor = editor_with("abcdef\nab\nabcdef");
        editor.set_cursor(14);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        editor.insert_paste("");
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 12, "the paste forgot the column");
        // Plain typing keeps it: after Up the cursor sits at 9 on the short
        // line with the column 4 remembered; typing x there and going Down
        // returns to column 4 of the last line, not to the cursor's column.
        editor = editor_with("abcdef\nab\nabcdef");
        editor.set_cursor(14);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        editor.insert("x");
        editor.on_key(key(KeyCode::Down, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 15, "typing kept the remembered column");
    }

    #[test]
    fn clear_empties_text_and_cursor_but_not_the_goal_column() {
        let mut editor = editor_with("abcdef\nab\nabcdef");
        editor.set_cursor(14);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        editor.clear();
        assert_eq!((editor.text(), editor.cursor()), ("", 0));
        editor.set_text("abcdef\nab\nabcdef");
        editor.set_cursor(9);
        editor.on_key(key(KeyCode::Up, KeyModifiers::NONE), false);
        assert_eq!(editor.cursor(), 4, "the goal column survived the clear");
    }

    #[test]
    fn set_text_leaves_the_cursor_raw_and_cursor_clamps_it() {
        let mut editor = editor_with("abc");
        editor.set_text("a");
        assert_eq!(
            editor.cursor(),
            1,
            "the raw cursor 3 clamps to the shorter text"
        );
        editor.set_cursor(99);
        assert_eq!(editor.cursor(), 1);
    }
}
