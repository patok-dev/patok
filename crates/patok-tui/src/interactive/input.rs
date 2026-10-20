//! Encodes crossterm key, paste and mouse events into the bytes a PTY child
//! expects on stdin. Pure functions, no I/O: the caller writes the result to
//! the `PtySession`.
//!
//! The child always gets legacy xterm encoding. The kitty keyboard query
//! (`CSI ? u`) is left unanswered (see `replies.rs`), so a child never opts in
//! to kitty or modifyOtherKeys reporting.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;

/// The child's input modes, read from the emulator before each event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputModes {
    pub(crate) application_cursor: bool,
    pub(crate) application_keypad: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) mouse_mode: vt100::MouseProtocolMode,
    pub(crate) mouse_encoding: vt100::MouseProtocolEncoding,
}

impl InputModes {
    pub(crate) fn from_screen(screen: &vt100::Screen) -> Self {
        Self {
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_mode: screen.mouse_protocol_mode(),
            mouse_encoding: screen.mouse_protocol_encoding(),
        }
    }
}

/// What to do with a key event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyInput {
    /// The exit chord: close the terminal modal.
    Exit,
    /// Write these bytes to the child.
    Forward(Vec<u8>),
    /// Nothing to send.
    Ignore,
}

const ESC: u8 = 0x1b;

/// The xterm modifier parameter, or `None` when no modifier is held.
fn csi_mod(mods: KeyModifiers) -> Option<u8> {
    let m = 1
        + u8::from(mods.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(mods.contains(KeyModifiers::ALT))
        + 4 * u8::from(mods.contains(KeyModifiers::CONTROL));
    (m > 1).then_some(m)
}

/// The C0 byte for `Ctrl+c`, or `None` when `c` has no control form.
fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' | 'A'..='Z' => Some(c as u8 & 0x1f),
        '@' | ' ' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Prefixes `bytes` with ESC when Alt is held.
fn alt_prefix(alt: bool, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if alt {
        out.push(ESC);
    }
    out.extend_from_slice(bytes);
    out
}

/// `ESC [ <letter>`, `ESC O <letter>` or `ESC [ 1 ; m <letter>`.
fn ss3_or_csi(letter: u8, mods: Option<u8>, application: bool) -> Vec<u8> {
    match mods {
        Some(m) => format!("\x1b[1;{m}{}", letter as char).into_bytes(),
        None if application => vec![ESC, b'O', letter],
        None => vec![ESC, b'[', letter],
    }
}

/// `ESC [ n ~` or `ESC [ n ; m ~`.
fn tilde(n: u8, mods: Option<u8>) -> Vec<u8> {
    match mods {
        Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
        None => format!("\x1b[{n}~").into_bytes(),
    }
}

/// F1-F4: `ESC O x`, or `ESC [ 1 ; m x` with modifiers.
fn ss3_fkey(letter: u8, mods: Option<u8>) -> Vec<u8> {
    ss3_or_csi(letter, mods, true)
}

fn keypad_app_byte(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(b'p' + (c as u8 - b'0')),
        '.' => Some(b'n'),
        '+' => Some(b'k'),
        '-' => Some(b'm'),
        '*' => Some(b'j'),
        '/' => Some(b'o'),
        _ => None,
    }
}

pub(crate) fn encode_key(key: &KeyEvent, modes: &InputModes) -> KeyInput {
    if key.kind == KeyEventKind::Release {
        return KeyInput::Ignore;
    }
    let mods = key.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);

    // `Ctrl+]` is the exit chord. It reaches us two ways: as `Char(']')` +
    // CONTROL from kitty or modifyOtherKeys terminals, and as the legacy byte
    // 0x1D, which crossterm reports as `Char('5')` + CONTROL.
    if ctrl && matches!(key.code, KeyCode::Char(']' | '5')) {
        return KeyInput::Exit;
    }

    let m = csi_mod(mods);

    // Keypad keys only differ from ordinary ones when crossterm can tell them
    // apart, which needs kitty's KEYPAD state flag.
    if modes.application_keypad && key.state.contains(KeyEventState::KEYPAD) {
        let byte = match key.code {
            KeyCode::Char(c) => keypad_app_byte(c),
            KeyCode::Enter => Some(b'M'),
            _ => None,
        };
        if let Some(b) = byte {
            return KeyInput::Forward(vec![ESC, b'O', b]);
        }
    }

    let bytes = match key.code {
        KeyCode::Up => ss3_or_csi(b'A', m, modes.application_cursor),
        KeyCode::Down => ss3_or_csi(b'B', m, modes.application_cursor),
        KeyCode::Right => ss3_or_csi(b'C', m, modes.application_cursor),
        KeyCode::Left => ss3_or_csi(b'D', m, modes.application_cursor),
        KeyCode::Home => ss3_or_csi(b'H', m, modes.application_cursor),
        KeyCode::End => ss3_or_csi(b'F', m, modes.application_cursor),
        KeyCode::Insert => tilde(2, m),
        KeyCode::Delete => tilde(3, m),
        KeyCode::PageUp => tilde(5, m),
        KeyCode::PageDown => tilde(6, m),
        KeyCode::F(n @ 1..=4) => ss3_fkey(b'O' + n, m),
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            tilde(code, m)
        }
        // Shift+Enter stays a plain CR: the child never asked for kitty or
        // modifyOtherKeys, so it could not tell the difference anyway.
        KeyCode::Enter => alt_prefix(alt, b"\r"),
        KeyCode::Tab if shift => vec![ESC, b'[', b'Z'],
        KeyCode::Tab => alt_prefix(alt, b"\t"),
        KeyCode::BackTab => vec![ESC, b'[', b'Z'],
        KeyCode::Backspace if ctrl => alt_prefix(alt, &[0x08]),
        KeyCode::Backspace => alt_prefix(alt, &[0x7f]),
        // Esc always goes to the child: TUI agents (Codex, Claude Code) use it
        // to interrupt or cancel. Closing the modal is bound to `Ctrl+]`
        // instead, which a child almost never needs, so Esc must not be
        // swallowed here.
        KeyCode::Esc => alt_prefix(alt, &[ESC]),
        KeyCode::Char(c) => {
            // Kitty reports Shift+a as `Char('a')` + SHIFT (we don't request
            // alternate keys), so uppercase letters here. Shifted non-letters
            // can't be recovered without the keyboard layout and pass through.
            let c = if shift && c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c
            };
            let mut buf = [0u8; 4];
            let plain = c.encode_utf8(&mut buf).as_bytes();
            match ctrl.then(|| ctrl_byte(c)).flatten() {
                Some(b) => alt_prefix(alt, &[b]),
                None => alt_prefix(alt, plain),
            }
        }
        _ => return KeyInput::Ignore,
    };
    KeyInput::Forward(bytes)
}

pub(crate) fn encode_paste(text: &str, modes: &InputModes) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    if !modes.bracketed_paste {
        return normalised.into_bytes();
    }
    // Strip embedded end markers so pasted text can't close the bracket early.
    let safe = normalised.replace("\x1b[201~", "");
    format!("\x1b[200~{safe}\x1b[201~").into_bytes()
}

fn button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

/// Encodes a mouse event for the child, with coordinates relative to `area`.
///
/// `None` means the event is not forwarded: tracking is off, the point is
/// outside `area`, the mode doesn't report this event, or it can't be
/// encoded. On a wheel event the caller scrolls local scrollback instead.
pub(crate) fn encode_mouse(event: &MouseEvent, area: Rect, modes: &InputModes) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as Mode;

    let mode = modes.mouse_mode;
    if mode == Mode::None {
        return None;
    }
    if event.column < area.x
        || event.row < area.y
        || event.column >= area.x.saturating_add(area.width)
        || event.row >= area.y.saturating_add(area.height)
    {
        return None;
    }
    let x = event.column - area.x + 1;
    let y = event.row - area.y + 1;

    let sgr = modes.mouse_encoding == vt100::MouseProtocolEncoding::Sgr;
    let mut release = false;
    let mut code = match event.kind {
        MouseEventKind::Down(b) => button_code(b),
        MouseEventKind::Up(b) => {
            if mode == Mode::Press {
                return None;
            }
            release = true;
            if sgr { button_code(b) } else { 3 }
        }
        MouseEventKind::Drag(b) => {
            if !matches!(mode, Mode::ButtonMotion | Mode::AnyMotion) {
                return None;
            }
            32 + button_code(b)
        }
        MouseEventKind::Moved => {
            if mode != Mode::AnyMotion {
                return None;
            }
            35
        }
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
    };
    // Press-only mode never reports modifiers.
    if mode != Mode::Press {
        let mods = event.modifiers;
        code += 4 * u16::from(mods.contains(KeyModifiers::SHIFT))
            + 8 * u16::from(mods.contains(KeyModifiers::ALT))
            + 16 * u16::from(mods.contains(KeyModifiers::CONTROL));
    }

    match modes.mouse_encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
        }
        vt100::MouseProtocolEncoding::Default => {
            let mut out = vec![ESC, b'[', b'M'];
            for v in [code, x, y] {
                out.push(u8::try_from(32 + v).ok()?);
            }
            Some(out)
        }
        vt100::MouseProtocolEncoding::Utf8 => {
            let mut out = vec![ESC, b'[', b'M'];
            for v in [code, x, y] {
                let ch = char::from_u32(u32::from(32 + v)).filter(|_| 32 + v <= 2047)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vt100::{MouseProtocolEncoding as Enc, MouseProtocolMode as Mode};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn modes() -> InputModes {
        InputModes::default()
    }

    fn fwd(code: KeyCode, mods: KeyModifiers) -> KeyInput {
        encode_key(&key(code, mods), &modes())
    }

    fn bytes(b: &[u8]) -> KeyInput {
        KeyInput::Forward(b.to_vec())
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

    #[test]
    fn exit_chord_both_forms() {
        assert_eq!(fwd(KeyCode::Char(']'), CTRL), KeyInput::Exit);
        assert_eq!(fwd(KeyCode::Char('5'), CTRL), KeyInput::Exit);
        assert_eq!(fwd(KeyCode::Char(']'), NONE), bytes(b"]"));
        assert_eq!(fwd(KeyCode::Char('5'), NONE), bytes(b"5"));
    }

    #[test]
    fn release_is_ignored() {
        let mut k = key(KeyCode::Char('a'), NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(encode_key(&k, &modes()), KeyInput::Ignore);
        k.kind = KeyEventKind::Repeat;
        assert_eq!(encode_key(&k, &modes()), bytes(b"a"));
    }

    #[test]
    fn esc_and_alt() {
        assert_eq!(fwd(KeyCode::Esc, NONE), bytes(b"\x1b"));
        assert_eq!(fwd(KeyCode::Esc, ALT), bytes(b"\x1b\x1b"));
        assert_eq!(fwd(KeyCode::Char('x'), ALT), bytes(b"\x1bx"));
    }

    #[test]
    fn cursor_keys() {
        let app = InputModes {
            application_cursor: true,
            ..modes()
        };
        let up = key(KeyCode::Up, NONE);
        assert_eq!(encode_key(&up, &modes()), bytes(b"\x1b[A"));
        assert_eq!(encode_key(&up, &app), bytes(b"\x1bOA"));
        assert_eq!(fwd(KeyCode::Down, NONE), bytes(b"\x1b[B"));
        assert_eq!(fwd(KeyCode::Right, NONE), bytes(b"\x1b[C"));
        assert_eq!(fwd(KeyCode::Left, NONE), bytes(b"\x1b[D"));
        assert_eq!(fwd(KeyCode::End, NONE), bytes(b"\x1b[F"));
        assert_eq!(fwd(KeyCode::Up, CTRL), bytes(b"\x1b[1;5A"));
        assert_eq!(fwd(KeyCode::Home, SHIFT), bytes(b"\x1b[1;2H"));
        assert_eq!(
            encode_key(&key(KeyCode::Up, CTRL), &app),
            bytes(b"\x1b[1;5A")
        );
    }

    #[test]
    fn tilde_keys() {
        assert_eq!(fwd(KeyCode::Insert, NONE), bytes(b"\x1b[2~"));
        assert_eq!(fwd(KeyCode::Delete, NONE), bytes(b"\x1b[3~"));
        assert_eq!(fwd(KeyCode::PageUp, NONE), bytes(b"\x1b[5~"));
        assert_eq!(fwd(KeyCode::PageDown, SHIFT), bytes(b"\x1b[6;2~"));
    }

    #[test]
    fn function_keys() {
        assert_eq!(fwd(KeyCode::F(1), NONE), bytes(b"\x1bOP"));
        assert_eq!(fwd(KeyCode::F(4), NONE), bytes(b"\x1bOS"));
        assert_eq!(fwd(KeyCode::F(5), NONE), bytes(b"\x1b[15~"));
        assert_eq!(fwd(KeyCode::F(12), NONE), bytes(b"\x1b[24~"));
        assert_eq!(fwd(KeyCode::F(1), CTRL), bytes(b"\x1b[1;5P"));
        assert_eq!(fwd(KeyCode::F(13), NONE), KeyInput::Ignore);
    }

    #[test]
    fn control_and_text_chars() {
        assert_eq!(fwd(KeyCode::Char('a'), CTRL), bytes(&[0x01]));
        assert_eq!(fwd(KeyCode::Char('c'), CTRL), bytes(&[0x03]));
        assert_eq!(fwd(KeyCode::Char(' '), CTRL), bytes(&[0x00]));
        assert_eq!(fwd(KeyCode::Char('['), CTRL), bytes(&[0x1b]));
        assert_eq!(fwd(KeyCode::Char('é'), CTRL), bytes("é".as_bytes()));
        assert_eq!(fwd(KeyCode::Char('a'), CTRL | ALT), bytes(b"\x1b\x01"));
        assert_eq!(fwd(KeyCode::Char('a'), SHIFT), bytes(b"A"));
        assert_eq!(fwd(KeyCode::Char('é'), NONE), bytes("é".as_bytes()));
    }

    #[test]
    fn enter_tab_backspace() {
        assert_eq!(fwd(KeyCode::Enter, NONE), bytes(b"\r"));
        assert_eq!(fwd(KeyCode::Enter, SHIFT), bytes(b"\r"));
        assert_eq!(fwd(KeyCode::Tab, NONE), bytes(b"\t"));
        assert_eq!(fwd(KeyCode::BackTab, SHIFT), bytes(b"\x1b[Z"));
        assert_eq!(fwd(KeyCode::Tab, SHIFT), bytes(b"\x1b[Z"));
        assert_eq!(fwd(KeyCode::Backspace, NONE), bytes(&[0x7f]));
        assert_eq!(fwd(KeyCode::Backspace, CTRL), bytes(&[0x08]));
    }

    #[test]
    fn keypad() {
        let mut k = key(KeyCode::Char('5'), NONE);
        k.state = KeyEventState::KEYPAD;
        let app = InputModes {
            application_keypad: true,
            ..modes()
        };
        assert_eq!(encode_key(&k, &app), bytes(b"\x1bOu"));
        assert_eq!(encode_key(&k, &modes()), bytes(b"5"));
        let mut enter = key(KeyCode::Enter, NONE);
        enter.state = KeyEventState::KEYPAD;
        assert_eq!(encode_key(&enter, &app), bytes(b"\x1bOM"));
        let mut other = key(KeyCode::Char('x'), NONE);
        other.state = KeyEventState::KEYPAD;
        assert_eq!(encode_key(&other, &app), bytes(b"x"));
        let mut plus = key(KeyCode::Char('+'), NONE);
        plus.state = KeyEventState::KEYPAD;
        assert_eq!(encode_key(&plus, &app), bytes(b"\x1bOk"));
    }

    #[test]
    fn unhandled_codes_are_ignored() {
        assert_eq!(fwd(KeyCode::CapsLock, NONE), KeyInput::Ignore);
        assert_eq!(fwd(KeyCode::Null, NONE), KeyInput::Ignore);
    }

    #[test]
    fn paste() {
        let bracketed = InputModes {
            bracketed_paste: true,
            ..modes()
        };
        assert_eq!(encode_paste("a\r\nb\nc", &modes()), b"a\rb\rc");
        assert_eq!(
            encode_paste("a\nb", &bracketed),
            b"\x1b[200~a\rb\x1b[201~".to_vec()
        );
        assert_eq!(
            encode_paste("a\x1b[201~b", &bracketed),
            b"\x1b[200~ab\x1b[201~".to_vec()
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: mods,
        }
    }

    fn mouse_modes(mode: Mode, encoding: Enc) -> InputModes {
        InputModes {
            mouse_mode: mode,
            mouse_encoding: encoding,
            ..modes()
        }
    }

    const AREA: Rect = Rect {
        x: 2,
        y: 1,
        width: 10,
        height: 5,
    };

    #[test]
    fn mouse_off_and_outside() {
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 3, 2, NONE);
        let wheel = mouse(MouseEventKind::ScrollUp, 3, 2, NONE);
        assert_eq!(encode_mouse(&down, AREA, &modes()), None);
        assert_eq!(encode_mouse(&wheel, AREA, &modes()), None);
        let m = mouse_modes(Mode::PressRelease, Enc::Sgr);
        for (c, r) in [(1, 2), (3, 0), (12, 2), (3, 6)] {
            let e = mouse(MouseEventKind::Down(MouseButton::Left), c, r, NONE);
            assert_eq!(encode_mouse(&e, AREA, &m), None);
        }
    }

    #[test]
    fn mouse_sgr() {
        let m = mouse_modes(Mode::PressRelease, Enc::Sgr);
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 3, 2, NONE);
        let up = mouse(MouseEventKind::Up(MouseButton::Right), 3, 2, NONE);
        let wheel = mouse(MouseEventKind::ScrollDown, 2, 1, NONE);
        let ctrl = mouse(MouseEventKind::Down(MouseButton::Middle), 3, 2, CTRL);
        assert_eq!(
            encode_mouse(&down, AREA, &m),
            Some(b"\x1b[<0;2;2M".to_vec())
        );
        assert_eq!(encode_mouse(&up, AREA, &m), Some(b"\x1b[<2;2;2m".to_vec()));
        assert_eq!(
            encode_mouse(&wheel, AREA, &m),
            Some(b"\x1b[<65;1;1M".to_vec())
        );
        assert_eq!(
            encode_mouse(&ctrl, AREA, &m),
            Some(b"\x1b[<17;2;2M".to_vec())
        );
        let shift_alt = mouse(MouseEventKind::ScrollUp, 2, 1, SHIFT | ALT);
        assert_eq!(
            encode_mouse(&shift_alt, AREA, &m),
            Some(b"\x1b[<76;1;1M".to_vec())
        );
    }

    #[test]
    fn mouse_default_and_utf8() {
        let m = mouse_modes(Mode::PressRelease, Enc::Default);
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 3, 2, NONE);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 3, 2, NONE);
        assert_eq!(
            encode_mouse(&down, AREA, &m),
            Some(vec![0x1b, b'[', b'M', 32, 34, 34])
        );
        assert_eq!(
            encode_mouse(&up, AREA, &m),
            Some(vec![0x1b, b'[', b'M', 35, 34, 34])
        );
        let far = Rect::new(0, 0, 400, 5);
        let e = mouse(MouseEventKind::Down(MouseButton::Left), 300, 0, NONE);
        assert_eq!(encode_mouse(&e, far, &m), None);
        let u = mouse_modes(Mode::PressRelease, Enc::Utf8);
        assert_eq!(
            encode_mouse(&e, far, &u),
            Some("\x1b[M \u{14d}!".as_bytes().to_vec())
        );
        let huge = Rect::new(0, 0, 3000, 5);
        let e = mouse(MouseEventKind::Down(MouseButton::Left), 2500, 0, NONE);
        assert_eq!(encode_mouse(&e, huge, &u), None);
    }

    #[test]
    fn mouse_mode_filters() {
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 3, 2, NONE);
        let moved = mouse(MouseEventKind::Moved, 3, 2, NONE);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 3, 2, NONE);
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 3, 2, CTRL);
        let pr = mouse_modes(Mode::PressRelease, Enc::Sgr);
        let bm = mouse_modes(Mode::ButtonMotion, Enc::Sgr);
        let am = mouse_modes(Mode::AnyMotion, Enc::Sgr);
        let press = mouse_modes(Mode::Press, Enc::Sgr);
        assert_eq!(encode_mouse(&drag, AREA, &pr), None);
        assert_eq!(
            encode_mouse(&drag, AREA, &bm),
            Some(b"\x1b[<32;2;2M".to_vec())
        );
        assert_eq!(encode_mouse(&moved, AREA, &bm), None);
        assert_eq!(
            encode_mouse(&moved, AREA, &am),
            Some(b"\x1b[<35;2;2M".to_vec())
        );
        assert_eq!(encode_mouse(&up, AREA, &press), None);
        assert_eq!(
            encode_mouse(&down, AREA, &press),
            Some(b"\x1b[<0;2;2M".to_vec())
        );
    }

    #[test]
    fn modes_from_screen() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"\x1b[?1h\x1b=\x1b[?2004h\x1b[?1002h\x1b[?1006h");
        assert_eq!(
            InputModes::from_screen(parser.screen()),
            InputModes {
                application_cursor: true,
                application_keypad: true,
                bracketed_paste: true,
                mouse_mode: Mode::ButtonMotion,
                mouse_encoding: Enc::Sgr,
            }
        );
    }
}
