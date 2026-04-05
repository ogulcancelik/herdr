use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};

use super::{KeyboardProtocol, MouseProtocolEncoding, TerminalKey};

const KITTY_FLAG_REPORT_EVENT_TYPES: u16 = 0b0000_0010;
const KITTY_FLAG_REPORT_ALTERNATE_KEYS: u16 = 0b0000_0100;

/// Encode a key event for a PTY child using the pane's negotiated keyboard protocol.
#[allow(dead_code)] // exercised in input unit tests; production uses PaneRuntime helpers
pub fn encode_key(key: KeyEvent, protocol: KeyboardProtocol) -> Vec<u8> {
    encode_terminal_key(key.into(), protocol)
}

pub fn encode_terminal_key(key: TerminalKey, protocol: KeyboardProtocol) -> Vec<u8> {
    if let Some(bytes) = encode_text_input(&key) {
        return bytes;
    }

    if let KeyboardProtocol::Kitty { flags } = protocol {
        if let Some(bytes) = try_encode_csi_u(&key, flags) {
            return bytes;
        }
    }
    encode_legacy(key.as_key_event())
}

#[allow(dead_code)] // exercised in input unit tests; production uses PaneRuntime helpers
pub fn encode_cursor_key(code: KeyCode, application_cursor: bool) -> Vec<u8> {
    match (code, application_cursor) {
        (KeyCode::Up, true) => b"\x1bOA".to_vec(),
        (KeyCode::Down, true) => b"\x1bOB".to_vec(),
        (KeyCode::Right, true) => b"\x1bOC".to_vec(),
        (KeyCode::Left, true) => b"\x1bOD".to_vec(),
        (KeyCode::Up, false) => b"\x1b[A".to_vec(),
        (KeyCode::Down, false) => b"\x1b[B".to_vec(),
        (KeyCode::Right, false) => b"\x1b[C".to_vec(),
        (KeyCode::Left, false) => b"\x1b[D".to_vec(),
        _ => encode_legacy(KeyEvent::new(code, KeyModifiers::empty())),
    }
}

pub fn encode_mouse_scroll(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let button = match kind {
        MouseEventKind::ScrollUp => 64u16,
        MouseEventKind::ScrollDown => 65u16,
        MouseEventKind::ScrollLeft => 66u16,
        MouseEventKind::ScrollRight => 67u16,
        _ => return None,
    };
    encode_mouse_cb(button, false, column, row, modifiers, encoding)
}

pub fn encode_mouse_button(
    kind: MouseEventKind,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let (button, release) = match kind {
        MouseEventKind::Down(MouseButton::Left) => (0u16, false),
        MouseEventKind::Down(MouseButton::Middle) => (1u16, false),
        MouseEventKind::Down(MouseButton::Right) => (2u16, false),
        MouseEventKind::Up(MouseButton::Left) => (0u16, true),
        MouseEventKind::Up(MouseButton::Middle) => (1u16, true),
        MouseEventKind::Up(MouseButton::Right) => (2u16, true),
        MouseEventKind::Drag(MouseButton::Left) => (32u16, false),
        MouseEventKind::Drag(MouseButton::Middle) => (33u16, false),
        MouseEventKind::Drag(MouseButton::Right) => (34u16, false),
        _ => return None,
    };
    encode_mouse_cb(button, release, column, row, modifiers, encoding)
}

fn encode_mouse_cb(
    base_button: u16,
    release: bool,
    column: u16,
    row: u16,
    modifiers: KeyModifiers,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let mut cb = match (encoding, release) {
        (MouseProtocolEncoding::Sgr, true) => base_button,
        (_, true) => 3,
        (_, false) => base_button,
    };
    if modifiers.contains(KeyModifiers::SHIFT) {
        cb += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        cb += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        cb += 16;
    }

    let column = column as u32 + 1;
    let row = row as u32 + 1;

    match encoding {
        MouseProtocolEncoding::Sgr => Some(
            format!(
                "\x1b[<{cb};{column};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        MouseProtocolEncoding::Default => {
            let cb = u8::try_from(cb + 32).ok()?;
            let column = u8::try_from(column + 32).ok()?;
            let row = u8::try_from(row + 32).ok()?;
            Some(vec![0x1b, b'[', b'M', cb, column, row])
        }
        MouseProtocolEncoding::Utf8 => {
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(b"\x1b[M");
            push_mouse_codepoint(&mut bytes, cb as u32 + 32)?;
            push_mouse_codepoint(&mut bytes, column + 32)?;
            push_mouse_codepoint(&mut bytes, row + 32)?;
            Some(bytes)
        }
    }
}

fn push_mouse_codepoint(bytes: &mut Vec<u8>, value: u32) -> Option<()> {
    let ch = char::from_u32(value)?;
    let mut buf = [0u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    Some(())
}

/// CSI u encoding: \e[{codepoint};{modifiers}u
/// Used when the child has pushed Kitty keyboard enhancement.
/// Returns None if the key doesn't need CSI u (unmodified basic keys).
fn try_encode_csi_u(key: &TerminalKey, flags: u16) -> Option<Vec<u8>> {
    let mods = key.modifiers;

    // Unmodified keys use legacy encoding (more compatible)
    if mods.is_empty() {
        return None;
    }

    // Plain Ctrl+letter is well-represented in legacy (bytes 1-26)
    if mods == KeyModifiers::CONTROL {
        if let KeyCode::Char(c) = key.code {
            if c.is_ascii_alphabetic() {
                return None; // let legacy handle it
            }
        }
    }

    // Special keys (arrows, F-keys, etc.) have well-established legacy
    // xterm modified formats (\x1b[1;3A for Alt+Up, etc.) that are universally
    // understood. Even Ghostty sends these in legacy format with kitty mode on.
    // Only use CSI u for character keys and keys without legacy representations.
    match key.code {
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Insert
        | KeyCode::Delete
        | KeyCode::F(_) => {
            return None; // let legacy handle these
        }
        _ => {}
    }

    let (codepoint, alternate_shifted) = match key.code {
        KeyCode::Char(c) => {
            let base = canonical_kitty_char(c, mods);
            let shifted = alternate_shifted_codepoint(key, flags);
            (base as u32, shifted)
        }
        KeyCode::Enter => (13, None),
        KeyCode::Tab => (9, None),
        KeyCode::Backspace => (127, None),
        KeyCode::Esc => (27, None),
        _ => return None, // fall back to legacy for unhandled keys
    };

    let modifier = kitty_modifier(mods);
    let event_suffix = kitty_event_suffix(key, flags);

    let sequence = match (alternate_shifted, event_suffix) {
        (Some(shifted), Some(event)) => format!("\x1b[{codepoint}:{shifted};{modifier}:{event}u"),
        (Some(shifted), None) => format!("\x1b[{codepoint}:{shifted};{modifier}u"),
        (None, Some(event)) => format!("\x1b[{codepoint};{modifier}:{event}u"),
        (None, None) => format!("\x1b[{codepoint};{modifier}u"),
    };

    Some(sequence.into_bytes())
}

/// Legacy terminal encoding (standard escape sequences).
fn encode_legacy(key: KeyEvent) -> Vec<u8> {
    let mods = key.modifiers;

    // Modified special keys (arrows, home, end, etc.) use xterm format:
    //   \x1b[1;{modifier}A  for arrows/home/end
    //   \x1b[{n};{modifier}~ for insert/delete/pgup/pgdn
    // The ESC-prefix hack doesn't work for these since they're already escape sequences.
    if !mods.is_empty() {
        if let Some(bytes) = encode_modified_special(key.code, mods) {
            return bytes;
        }
    }

    // Alt modifier on character keys: prefix with ESC
    if mods.contains(KeyModifiers::ALT) {
        let inner = KeyEvent::new(key.code, mods.difference(KeyModifiers::ALT));
        let mut bytes = vec![0x1b];
        bytes.extend(encode_legacy_inner(inner));
        return bytes;
    }
    encode_legacy_inner(key)
}

/// xterm-style encoding for modified special keys.
/// Modifier value: 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
fn encode_modified_special(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
    let modifier = xterm_modifier(mods);
    if modifier <= 1 {
        return None; // no modifiers to encode
    }

    match code {
        // CSI 1;{mod}{letter} format
        KeyCode::Up => Some(format!("\x1b[1;{modifier}A").into_bytes()),
        KeyCode::Down => Some(format!("\x1b[1;{modifier}B").into_bytes()),
        KeyCode::Right => Some(format!("\x1b[1;{modifier}C").into_bytes()),
        KeyCode::Left => Some(format!("\x1b[1;{modifier}D").into_bytes()),
        KeyCode::Home => Some(format!("\x1b[1;{modifier}H").into_bytes()),
        KeyCode::End => Some(format!("\x1b[1;{modifier}F").into_bytes()),
        // CSI {n};{mod}~ format
        KeyCode::Insert => Some(format!("\x1b[2;{modifier}~").into_bytes()),
        KeyCode::Delete => Some(format!("\x1b[3;{modifier}~").into_bytes()),
        KeyCode::PageUp => Some(format!("\x1b[5;{modifier}~").into_bytes()),
        KeyCode::PageDown => Some(format!("\x1b[6;{modifier}~").into_bytes()),
        // F1-F4: CSI 1;{mod}{P-S}
        KeyCode::F(1) => Some(format!("\x1b[1;{modifier}P").into_bytes()),
        KeyCode::F(2) => Some(format!("\x1b[1;{modifier}Q").into_bytes()),
        KeyCode::F(3) => Some(format!("\x1b[1;{modifier}R").into_bytes()),
        KeyCode::F(4) => Some(format!("\x1b[1;{modifier}S").into_bytes()),
        // F5-F12: CSI {n};{mod}~
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => unreachable!(),
            };
            Some(format!("\x1b[{code};{modifier}~").into_bytes())
        }
        _ => None,
    }
}

/// xterm modifier encoding: 1 + shift(1) + alt(2) + ctrl(4)
/// Used for legacy modified special keys (arrows, function keys, etc.)
fn xterm_modifier(mods: KeyModifiers) -> u32 {
    let mut m = 1u32;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    m
}

/// Kitty protocol modifier encoding: 1 + shift(1) + alt(2) + ctrl(4) + super(8) + hyper(16) + meta(32)
/// Superset of xterm — adds Super/Hyper/Meta bits.
fn kitty_modifier(mods: KeyModifiers) -> u32 {
    let mut m = xterm_modifier(mods);
    if mods.contains(KeyModifiers::SUPER) {
        m += 8;
    }
    if mods.contains(KeyModifiers::HYPER) {
        m += 16;
    }
    if mods.contains(KeyModifiers::META) {
        m += 32;
    }
    m
}

fn encode_text_input(key: &TerminalKey) -> Option<Vec<u8>> {
    if key.modifiers != KeyModifiers::SHIFT {
        return None;
    }

    let ch = match key.code {
        KeyCode::Char(ch) => shifted_text_char(key, ch)?,
        _ => return None,
    };

    match key.kind {
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat => {
            let mut buf = [0u8; 4];
            Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        crossterm::event::KeyEventKind::Release => Some(Vec::new()),
    }
}

fn shifted_text_char(key: &TerminalKey, ch: char) -> Option<char> {
    if let Some(shifted) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(shifted);
    }

    if ch.is_ascii_uppercase() {
        return Some(ch);
    }

    if ch.is_ascii_lowercase() {
        return Some(ch.to_ascii_uppercase());
    }

    None
}

fn canonical_kitty_char(ch: char, mods: KeyModifiers) -> char {
    if mods.contains(KeyModifiers::SHIFT) && ch.is_ascii_uppercase() {
        ch.to_ascii_lowercase()
    } else {
        ch
    }
}

fn alternate_shifted_codepoint(key: &TerminalKey, flags: u16) -> Option<u32> {
    if flags & KITTY_FLAG_REPORT_ALTERNATE_KEYS == 0 {
        return None;
    }

    if let Some(shifted) = key.shifted_codepoint {
        return Some(shifted);
    }

    match key.code {
        KeyCode::Char(ch)
            if key.modifiers.contains(KeyModifiers::SHIFT) && ch.is_ascii_uppercase() =>
        {
            Some(ch as u32)
        }
        _ => None,
    }
}

fn kitty_event_suffix(key: &TerminalKey, flags: u16) -> Option<u8> {
    if flags & KITTY_FLAG_REPORT_EVENT_TYPES == 0 {
        return None;
    }

    Some(match key.kind {
        crossterm::event::KeyEventKind::Press => 1,
        crossterm::event::KeyEventKind::Repeat => 2,
        crossterm::event::KeyEventKind::Release => 3,
    })
}

fn encode_legacy_inner(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(ch) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let upper = ch.to_ascii_uppercase();
                match upper {
                    'A'..='Z' => vec![upper as u8 - 64],
                    ' ' | '@' | '2' => vec![0],
                    '[' | '3' => vec![27],
                    '\\' | '4' => vec![28],
                    ']' | '5' => vec![29],
                    '^' | '6' => vec![30],
                    '_' | '7' | '-' => vec![31],
                    _ => vec![ch as u8],
                }
            } else {
                let mut buf = [0u8; 4];
                ch.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![127],
        KeyCode::Tab => vec![9],
        KeyCode::BackTab => vec![27, 91, 90],
        KeyCode::Esc => vec![27],
        KeyCode::Left => vec![27, 91, 68],
        KeyCode::Right => vec![27, 91, 67],
        KeyCode::Up => vec![27, 91, 65],
        KeyCode::Down => vec![27, 91, 66],
        KeyCode::Home => vec![27, 91, 72],
        KeyCode::End => vec![27, 91, 70],
        KeyCode::PageUp => vec![27, 91, 53, 126],
        KeyCode::PageDown => vec![27, 91, 54, 126],
        KeyCode::Delete => vec![27, 91, 51, 126],
        KeyCode::Insert => vec![27, 91, 50, 126],
        KeyCode::F(n) => encode_f_key(n),
        _ => vec![],
    }
}

fn encode_f_key(n: u8) -> Vec<u8> {
    match n {
        1 => vec![27, 79, 80],
        2 => vec![27, 79, 81],
        3 => vec![27, 79, 82],
        4 => vec![27, 79, 83],
        5 => vec![27, 91, 49, 53, 126],
        6 => vec![27, 91, 49, 55, 126],
        7 => vec![27, 91, 49, 56, 126],
        8 => vec![27, 91, 49, 57, 126],
        9 => vec![27, 91, 50, 48, 126],
        10 => vec![27, 91, 50, 49, 126],
        11 => vec![27, 91, 50, 51, 126],
        12 => vec![27, 91, 50, 52, 126],
        _ => vec![],
    }
}
