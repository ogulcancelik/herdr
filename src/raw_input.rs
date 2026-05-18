use std::io::Read;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// Parse raw terminal input bytes into a list of `RawInputEvent`s.
///
/// This is used by the headless server to route client input through the
/// same parsing pipeline that the monolithic binary uses for stdin.
/// Incomplete sequences at the end of the buffer are flushed as best-effort
/// (same logic as the live input reader).
#[allow(dead_code)]
pub fn parse_raw_input_bytes(data: &[u8]) -> Vec<RawInputEvent> {
    // Delegate to the sync version which actually works.
    parse_raw_input_bytes_sync(data)
}

/// A raw input event paired with the byte range it consumed from the original buffer.
#[cfg(test)]
#[derive(Debug)]
pub struct RawInputEventWithRange {
    /// The parsed event.
    pub event: RawInputEvent,
    /// Byte offset where this event starts in the original buffer.
    pub start: usize,
    /// Number of bytes this event consumed from the original buffer.
    /// For events generated from flushed incomplete bytes, `len` may be 0
    /// (synthetic events that don't map to original bytes).
    pub len: usize,
}

/// Parse raw terminal input bytes into a list of `RawInputEventWithRange`s (synchronous version).
///
/// Unlike `parse_raw_input_bytes_sync`, this preserves the byte offset for each
/// event, allowing callers to write only the specific bytes for each event
/// instead of the entire input buffer.
#[cfg(test)]
pub fn parse_raw_input_bytes_with_ranges(data: &[u8]) -> Vec<RawInputEventWithRange> {
    let mut buffer = data.to_vec();
    let mut events = Vec::new();
    let mut offset = 0usize;

    while let Some((event, consumed)) = extract_one_event(&buffer) {
        buffer.drain(..consumed);
        events.push(RawInputEventWithRange {
            event,
            start: offset,
            len: consumed,
        });
        offset += consumed;
    }

    // Flush remaining incomplete bytes.
    if !buffer.is_empty() {
        if buffer.as_slice() == [ESC] {
            events.push(RawInputEventWithRange {
                event: RawInputEvent::Key(TerminalKey::new(
                    crossterm::event::KeyCode::Esc,
                    KeyModifiers::empty(),
                )),
                start: offset,
                len: 1,
            });
        } else if let Ok(text) = std::str::from_utf8(&buffer) {
            if let Some(key) = parse_terminal_key_sequence(text) {
                events.push(RawInputEventWithRange {
                    event: RawInputEvent::Key(key),
                    start: offset,
                    len: buffer.len(),
                });
            }
        }
    }

    events
}

/// Parse raw terminal input bytes into a list of `RawInputEvent`s (synchronous version).
///
/// Unlike `parse_raw_input_bytes`, this directly extracts events without
/// going through a channel, making it suitable for synchronous use.
pub fn parse_raw_input_bytes_sync(data: &[u8]) -> Vec<RawInputEvent> {
    let mut buffer = data.to_vec();
    let mut events = Vec::new();

    while let Some((event, consumed)) = extract_one_event(&buffer) {
        buffer.drain(..consumed);
        events.push(event);
    }

    if !buffer.is_empty() {
        if buffer.as_slice() == [ESC] {
            events.push(RawInputEvent::Key(TerminalKey::new(
                crossterm::event::KeyCode::Esc,
                KeyModifiers::empty(),
            )));
        } else if let Ok(text) = std::str::from_utf8(&buffer) {
            if let Some(key) = parse_terminal_key_sequence(text) {
                events.push(RawInputEvent::Key(key));
            }
        }
    }

    events
}

#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

use crate::input::{parse_terminal_key_sequence, TerminalKey};
use crate::terminal_theme::{parse_default_color_response, DefaultColorKind, RgbColor};

const ESC: u8 = 0x1b;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug)]
pub enum RawInputEvent {
    Key(TerminalKey),
    Paste(String),
    Mouse(MouseEvent),
    OuterFocusGained,
    OuterFocusLost,
    HostDefaultColor {
        kind: DefaultColorKind,
        color: RgbColor,
    },
    Unsupported,
}

pub(crate) fn events_require_host_surface_redraw(events: &[RawInputEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, RawInputEvent::OuterFocusGained))
}

pub fn spawn_input_reader() -> mpsc::Receiver<RawInputEvent> {
    let (tx, rx) = mpsc::channel(256);

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut scratch = [0u8; 1024];
        let mut buffer = Vec::<u8>::new();

        loop {
            match reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => {
                    buffer.extend_from_slice(&scratch[..n]);
                    drain_buffer(&mut buffer, &tx);

                    if !buffer.is_empty() && stdin_read_ready(&reader, 10) == Some(false) {
                        flush_incomplete_buffer(&mut buffer, &tx);
                    }
                }
                Err(_) => break,
            }
        }
    });

    rx
}

fn drain_buffer(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>) {
    for bytes in drain_complete_input_bytes(buffer) {
        let Some((event, _consumed)) = extract_one_event(&bytes) else {
            continue;
        };
        tracing::debug!(raw_bytes = ?bytes, event = ?event, "raw input event parsed");
        let _ = tx.blocking_send(event);
    }
}

pub(crate) fn drain_complete_input_bytes(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();

    while let Some((_event, consumed)) = extract_one_event(buffer) {
        chunks.push(buffer[..consumed].to_vec());
        buffer.drain(..consumed);
    }

    chunks
}

fn flush_incomplete_buffer(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>) {
    if let Some(bytes) = flush_incomplete_input_bytes(buffer) {
        if bytes.as_slice() == [ESC] {
            let _ = tx.blocking_send(RawInputEvent::Key(TerminalKey::new(
                crossterm::event::KeyCode::Esc,
                KeyModifiers::empty(),
            )));
            return;
        }

        let Some((event, _consumed)) = extract_one_event(&bytes) else {
            return;
        };
        let _ = tx.blocking_send(event);
    }
}

pub(crate) fn flush_incomplete_input_bytes(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buffer.is_empty() {
        return None;
    }

    if buffer.starts_with(BRACKETED_PASTE_START)
        && find_subsequence(buffer, BRACKETED_PASTE_END).is_none()
    {
        tracing::trace!(len = buffer.len(), "waiting for bracketed paste terminator");
        return None;
    }

    if buffer.as_slice() == [ESC] {
        tracing::warn!(
            bytes = ?buffer,
            "flushing lone escape after input timeout; if this follows an alt chord or focus switch it may reach the pane as plain esc"
        );
        return Some(std::mem::take(buffer));
    }

    if let Ok(text) = std::str::from_utf8(buffer) {
        if parse_terminal_key_sequence(text).is_some() {
            return Some(std::mem::take(buffer));
        }
    }

    if starts_with_incomplete_utf8_char(buffer) {
        tracing::trace!(bytes = ?buffer, "waiting for UTF-8 continuation bytes");
        return None;
    }

    tracing::debug!(bytes = ?buffer, "dropping incomplete raw input buffer after timeout");
    buffer.clear();
    None
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    #[cfg(unix)]
    {
        let fd = _reader.as_raw_fd();
        poll_read_ready(fd, _timeout_ms)
    }
}

#[cfg(not(unix))]
fn stdin_read_ready<R>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    None
}

#[cfg(unix)]
fn poll_read_ready(fd: i32, timeout_ms: i32) -> Option<bool> {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }

    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: usize, timeout: i32) -> i32;
    }

    const POLLIN: i16 = 0x0001;

    let mut pfd = PollFd {
        fd,
        events: POLLIN,
        revents: 0,
    };

    let result = unsafe { poll(&mut pfd as *mut PollFd, 1, timeout_ms) };
    if result < 0 {
        None
    } else {
        Some(result > 0)
    }
}

fn extract_one_event(buffer: &[u8]) -> Option<(RawInputEvent, usize)> {
    if buffer.is_empty() {
        return None;
    }

    if buffer.starts_with(BRACKETED_PASTE_START) {
        let end = find_subsequence(buffer, BRACKETED_PASTE_END)?;
        let content = std::str::from_utf8(&buffer[BRACKETED_PASTE_START.len()..end]).ok()?;
        return Some((
            RawInputEvent::Paste(content.to_string()),
            end + BRACKETED_PASTE_END.len(),
        ));
    }

    if buffer[0] == ESC {
        let seq_len = complete_escape_sequence_len(buffer)?;
        let seq = std::str::from_utf8(&buffer[..seq_len]).ok()?;

        if let Some((kind, color)) = parse_default_color_response(seq) {
            return Some((RawInputEvent::HostDefaultColor { kind, color }, seq_len));
        }

        match seq {
            "\x1b[I" => return Some((RawInputEvent::OuterFocusGained, seq_len)),
            "\x1b[O" => return Some((RawInputEvent::OuterFocusLost, seq_len)),
            _ => {}
        }

        if let Some(mouse) = parse_sgr_mouse(seq) {
            return Some((RawInputEvent::Mouse(mouse), seq_len));
        }

        if let Some(key) = parse_terminal_key_sequence(seq) {
            return Some((RawInputEvent::Key(key), seq_len));
        }

        tracing::debug!(sequence = ?seq, "dropping unsupported escape sequence");
        return Some((RawInputEvent::Unsupported, seq_len));
    }

    let consumed = first_complete_utf8_char_len(buffer)?;
    let text = std::str::from_utf8(&buffer[..consumed]).ok()?;
    let key = parse_terminal_key_sequence(text)?;
    Some((RawInputEvent::Key(key), consumed))
}

fn first_complete_utf8_char_len(buffer: &[u8]) -> Option<usize> {
    let width = utf8_char_width(*buffer.first()?)?;

    if buffer.len() < width {
        return None;
    }

    std::str::from_utf8(&buffer[..width]).ok()?;
    Some(width)
}

fn starts_with_incomplete_utf8_char(buffer: &[u8]) -> bool {
    match std::str::from_utf8(buffer) {
        Ok(_) => false,
        Err(err) => err.valid_up_to() == 0 && err.error_len().is_none(),
    }
}

fn utf8_char_width(first: u8) -> Option<usize> {
    if first < 0x80 {
        Some(1)
    } else if first & 0b1110_0000 == 0b1100_0000 {
        Some(2)
    } else if first & 0b1111_0000 == 0b1110_0000 {
        Some(3)
    } else if first & 0b1111_1000 == 0b1111_0000 {
        Some(4)
    } else {
        None
    }
}

fn complete_escape_sequence_len(buffer: &[u8]) -> Option<usize> {
    if buffer.len() == 1 {
        return None;
    }

    if buffer.starts_with(b"\x1b\x1b") {
        return complete_escape_sequence_len(&buffer[1..]).map(|len| len + 1);
    }

    if buffer.starts_with(b"\x1b[") {
        if buffer.starts_with(b"\x1b[<") {
            return find_csi_final(buffer, b"Mm");
        }
        return find_csi_final(
            buffer,
            b"@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~",
        );
    }

    if buffer.starts_with(b"\x1b]") {
        return find_osc_terminator(buffer);
    }

    if buffer.starts_with(b"\x1bP") || buffer.starts_with(b"\x1b_") {
        return find_subsequence(buffer, b"\x1b\\").map(|idx| idx + 2);
    }

    if buffer.starts_with(b"\x1bO") {
        return (buffer.len() >= 3).then_some(3);
    }

    Some(2)
}

fn find_osc_terminator(buffer: &[u8]) -> Option<usize> {
    find_subsequence(buffer, b"\x1b\\")
        .map(|idx| idx + 2)
        .or_else(|| {
            buffer
                .iter()
                .position(|byte| *byte == b'\x07')
                .map(|idx| idx + 1)
        })
}

fn find_csi_final(buffer: &[u8], finals: &[u8]) -> Option<usize> {
    for (idx, byte) in buffer.iter().enumerate().skip(2) {
        if finals.contains(byte) {
            return Some(idx + 1);
        }
    }
    None
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_sgr_mouse(sequence: &str) -> Option<MouseEvent> {
    let body = sequence.strip_prefix("\x1b[<")?;
    let final_char = body.chars().last()?;
    if final_char != 'M' && final_char != 'm' {
        return None;
    }

    let payload = &body[..body.len() - 1];
    let mut parts = payload.split(';');
    let cb = parts.next()?.parse::<u8>().ok()?;
    let column = parts.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let row = parts.next()?.parse::<u16>().ok()?.checked_sub(1)?;
    let (kind, modifiers) = parse_mouse_cb(cb)?;

    let kind = if final_char == 'm' {
        match kind {
            MouseEventKind::Down(button) => MouseEventKind::Up(button),
            other => other,
        }
    } else {
        kind
    };

    Some(MouseEvent {
        kind,
        column,
        row,
        modifiers,
    })
}

fn parse_mouse_cb(cb: u8) -> Option<(MouseEventKind, KeyModifiers)> {
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    let dragging = cb & 0b0010_0000 == 0b0010_0000;

    let kind = match (button_number, dragging) {
        (0, false) => MouseEventKind::Down(MouseButton::Left),
        (1, false) => MouseEventKind::Down(MouseButton::Middle),
        (2, false) => MouseEventKind::Down(MouseButton::Right),
        (0, true) => MouseEventKind::Drag(MouseButton::Left),
        (1, true) => MouseEventKind::Drag(MouseButton::Middle),
        (2, true) => MouseEventKind::Drag(MouseButton::Right),
        (3, false) => MouseEventKind::Up(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseEventKind::Moved,
        (4, false) => MouseEventKind::ScrollUp,
        (5, false) => MouseEventKind::ScrollDown,
        (6, false) => MouseEventKind::ScrollLeft,
        (7, false) => MouseEventKind::ScrollRight,
        _ => return None,
    };

    let mut modifiers = KeyModifiers::empty();
    if cb & 0b0000_0100 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if cb & 0b0000_1000 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if cb & 0b0001_0000 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }

    Some((kind, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind};

    fn assert_raw_key(event: RawInputEvent, code: KeyCode, modifiers: KeyModifiers) {
        let RawInputEvent::Key(key) = event else {
            panic!("expected key");
        };
        assert_eq!(key.code, code);
        assert_eq!(key.modifiers, modifiers);
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        let hex = hex.trim();
        assert_eq!(hex.len() % 2, 0, "hex string must have even length");
        (0..hex.len())
            .step_by(2)
            .map(|idx| u8::from_str_radix(&hex[idx..idx + 2], 16).unwrap())
            .collect()
    }

    fn parse_fixture_key_code(value: &str) -> KeyCode {
        match value {
            "enter" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "esc" => KeyCode::Esc,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "insert" => KeyCode::Insert,
            "delete" => KeyCode::Delete,
            value if value.starts_with("char:") => {
                KeyCode::Char(value.trim_start_matches("char:").chars().next().unwrap())
            }
            other => panic!("unsupported fixture key code: {other}"),
        }
    }

    fn parse_fixture_modifiers(value: &str) -> KeyModifiers {
        if value == "-" || value.is_empty() {
            return KeyModifiers::empty();
        }

        let mut modifiers = KeyModifiers::empty();
        for part in value.split('+') {
            match part {
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "alt" => modifiers |= KeyModifiers::ALT,
                "control" => modifiers |= KeyModifiers::CONTROL,
                "super" => modifiers |= KeyModifiers::SUPER,
                "hyper" => modifiers |= KeyModifiers::HYPER,
                "meta" => modifiers |= KeyModifiers::META,
                other => panic!("unsupported fixture modifier: {other}"),
            }
        }
        modifiers
    }

    fn collect_events(rx: &mut mpsc::Receiver<RawInputEvent>) -> Vec<RawInputEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn drain_chunk(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>, chunk: &[u8]) {
        buffer.extend_from_slice(chunk);
        drain_buffer(buffer, tx);
    }

    #[test]
    fn parses_kitty_shift_letter_release() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[108:76;2:3u").unwrap()
        else {
            panic!("expected key");
        };
        assert_eq!(consumed, 13);
        assert_eq!(key.code, KeyCode::Char('l'));
        assert_eq!(key.modifiers, KeyModifiers::SHIFT);
        assert_eq!(key.kind, KeyEventKind::Release);
        assert_eq!(key.shifted_codepoint, Some('L' as u32));
    }

    #[test]
    fn parses_bracketed_paste() {
        let (RawInputEvent::Paste(text), consumed) =
            extract_one_event(b"\x1b[200~hello\x1b[201~rest").unwrap()
        else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello");
        assert_eq!(consumed, 17);
    }

    #[test]
    fn parses_sgr_mouse() {
        let (RawInputEvent::Mouse(mouse), consumed) = extract_one_event(b"\x1b[<0;20;10M").unwrap()
        else {
            panic!("expected mouse");
        };
        assert_eq!(consumed, 11);
        assert_eq!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(mouse.column, 19);
        assert_eq!(mouse.row, 9);
    }

    #[test]
    fn parses_host_default_color_response_with_st() {
        let (RawInputEvent::HostDefaultColor { kind, color }, consumed) =
            extract_one_event(b"\x1b]10;rgb:cccc/dddd/eeee\x1b\\").unwrap()
        else {
            panic!("expected host color response");
        };
        assert_eq!(consumed, 25);
        assert_eq!(kind, DefaultColorKind::Foreground);
        assert_eq!(
            color,
            RgbColor {
                r: 0xcc,
                g: 0xdd,
                b: 0xee
            }
        );
    }

    #[test]
    fn parses_host_default_color_response_with_bel() {
        let (RawInputEvent::HostDefaultColor { kind, color }, consumed) =
            extract_one_event(b"\x1b]11;#112233\x07").unwrap()
        else {
            panic!("expected host color response");
        };
        assert_eq!(consumed, 13);
        assert_eq!(kind, DefaultColorKind::Background);
        assert_eq!(
            color,
            RgbColor {
                r: 0x11,
                g: 0x22,
                b: 0x33
            }
        );
    }

    #[test]
    fn parses_legacy_up_arrow() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[A").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 3);
        assert_eq!(key.code, KeyCode::Up);
    }

    #[test]
    fn parses_outer_focus_events() {
        let (event, consumed) = extract_one_event(b"\x1b[I").unwrap();
        assert_eq!(consumed, 3);
        assert!(matches!(event, RawInputEvent::OuterFocusGained));

        let (event, consumed) = extract_one_event(b"\x1b[O").unwrap();
        assert_eq!(consumed, 3);
        assert!(matches!(event, RawInputEvent::OuterFocusLost));
    }

    #[test]
    fn outer_focus_gained_requests_host_surface_redraw() {
        let events = parse_raw_input_bytes_sync(b"\x1b[I");
        assert!(events_require_host_surface_redraw(&events));

        let events = parse_raw_input_bytes_sync(b"\x1b[O");
        assert!(!events_require_host_surface_redraw(&events));
    }

    #[test]
    fn parses_xterm_alt_up_arrow() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[1;3A").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 6);
        assert_eq!(key.code, KeyCode::Up);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_legacy_alt_backspace() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b\x7f").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 2);
        assert_eq!(key.code, KeyCode::Backspace);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_kitty_alt_backspace() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[127;3u").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::Backspace);
        assert_eq!(key.modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_enhanced_pageup_press() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[5;1:1~").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::PageUp);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, KeyEventKind::Press);
    }

    #[test]
    fn parses_enhanced_pagedown_release() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[6;1:3~").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 8);
        assert_eq!(key.code, KeyCode::PageDown);
        assert_eq!(key.modifiers, KeyModifiers::empty());
        assert_eq!(key.kind, KeyEventKind::Release);
    }

    #[test]
    fn raw_input_family_matrix_is_covered() {
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            (b"\x02", KeyCode::Char('b'), KeyModifiers::CONTROL),
            (b"\r", KeyCode::Enter, KeyModifiers::empty()),
            (b"\t", KeyCode::Tab, KeyModifiers::empty()),
            (b"\x7f", KeyCode::Backspace, KeyModifiers::empty()),
            (b"\x1b[A", KeyCode::Up, KeyModifiers::empty()),
            (b"\x1b[1;3A", KeyCode::Up, KeyModifiers::ALT),
            (b"\x1b\x7f", KeyCode::Backspace, KeyModifiers::ALT),
            (b"\x1b[127;3u", KeyCode::Backspace, KeyModifiers::ALT),
            (b"\x1b[57420;1u", KeyCode::Down, KeyModifiers::empty()),
            (b"\x1b[57423;1u", KeyCode::Home, KeyModifiers::empty()),
            (b"\x1b[49:33;2:1u", KeyCode::Char('1'), KeyModifiers::SHIFT),
        ];

        for (bytes, code, modifiers) in cases {
            let (event, consumed) = extract_one_event(bytes).unwrap();
            assert_eq!(consumed, bytes.len());
            assert_raw_key(event, *code, *modifiers);
        }
    }

    #[test]
    fn flushes_lone_escape_after_timeout() {
        let (tx, mut rx) = mpsc::channel(4);
        let mut buffer = vec![ESC];
        flush_incomplete_buffer(&mut buffer, &tx);
        assert!(buffer.is_empty());
        let event = rx.try_recv().unwrap();
        let RawInputEvent::Key(key) = event else {
            panic!("expected key");
        };
        assert_eq!(key.code, KeyCode::Esc);
    }

    #[test]
    fn parses_raw_ctrl_b() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x02").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 1);
        assert_eq!(key.code, KeyCode::Char('b'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn parses_raw_lf_as_ctrl_j() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\n").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 1);
        assert_eq!(key.code, KeyCode::Char('j'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    fn assert_fixture_extracts_whole_events(corpus: &str, macos_layout: bool) {
        for line in corpus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut columns: Vec<_> = line.split('\t').collect();
            if columns.len() == 5 {
                columns.push("");
            }

            if macos_layout {
                if columns.len() == 6 {
                    columns.push("");
                }
                assert_eq!(
                    columns.len(),
                    7,
                    "macOS fixture row must have 7 columns: {line}"
                );
                if columns[2].is_empty() {
                    continue;
                }
                let bytes = decode_hex(columns[2]);
                let (event, consumed) = extract_one_event(&bytes).unwrap();
                assert_eq!(
                    consumed,
                    bytes.len(),
                    "fixture should extract a whole event: {line}"
                );
                assert_raw_key(
                    event,
                    parse_fixture_key_code(columns[3]),
                    parse_fixture_modifiers(columns[4]),
                );
            } else {
                if columns.len() == 5 {
                    columns.push("");
                }
                let (bytes_hex, code, modifiers) = match columns.len() {
                    6 => {
                        if columns[1].chars().all(|ch| ch.is_ascii_hexdigit()) {
                            (columns[1], columns[2], columns[3])
                        } else {
                            (columns[2], columns[3], columns[4])
                        }
                    }
                    7 => (columns[2], columns[3], columns[4]),
                    _ => panic!("fixture row must have 6 or 7 columns: {line}"),
                };
                assert!(
                    bytes_hex.chars().all(|ch| ch.is_ascii_hexdigit()),
                    "non-hex fixture bytes: {bytes_hex} in {line}"
                );
                let bytes = decode_hex(bytes_hex);
                let (event, consumed) = extract_one_event(&bytes).unwrap();
                assert_eq!(
                    consumed,
                    bytes.len(),
                    "fixture should extract a whole event: {line}"
                );
                assert_raw_key(
                    event,
                    parse_fixture_key_code(code),
                    parse_fixture_modifiers(modifiers),
                );
            }
        }
    }

    #[test]
    fn raw_input_corpus_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/keyboard_protocol_corpus.tsv");
        assert_fixture_extracts_whole_events(corpus, false);
    }

    #[test]
    fn raw_input_macos_terminal_variants_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/macos_terminal_variants.tsv");
        assert_fixture_extracts_whole_events(corpus, true);
    }

    #[test]
    fn raw_input_linux_terminal_variants_fixture_extracts_whole_events() {
        let corpus = include_str!("../tests/fixtures/linux_terminal_variants.tsv");
        assert_fixture_extracts_whole_events(corpus, false);
    }

    #[test]
    fn chunked_legacy_arrow_waits_for_completion() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"[A");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Up,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn lone_escape_is_buffered_until_timeout_flush() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        flush_incomplete_buffer(&mut buffer, &tx);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Esc,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn escape_followed_by_arrow_before_flush_does_not_emit_escape() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"[B");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Down,
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn escape_followed_by_alt_char_before_flush_becomes_alt_key() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b");
        assert_eq!(buffer, b"\x1b");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"b");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('b'),
            KeyModifiers::ALT,
        );
    }

    #[test]
    fn chunked_kitty_sequence_waits_for_completion() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[49:33;2:");
        assert_eq!(buffer, b"\x1b[49:33;2:");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"1u");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('1'),
            KeyModifiers::SHIFT,
        );
    }

    #[test]
    fn chunked_bracketed_paste_waits_for_terminator() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[200~hello");
        assert_eq!(buffer, b"\x1b[200~hello");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"\x1b[201~");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        let RawInputEvent::Paste(text) = &events[0] else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello");
    }

    #[test]
    fn incomplete_bracketed_paste_is_not_flushed_on_timeout() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, b"\x1b[200~hello\nworld");
        assert_eq!(buffer, b"\x1b[200~hello\nworld");
        assert!(collect_events(&mut rx).is_empty());

        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, b"\x1b[200~hello\nworld");
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, b"\x1b[201~");
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        let RawInputEvent::Paste(text) = &events[0] else {
            panic!("expected paste");
        };
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn complete_utf8_char_before_incomplete_char_is_drained() {
        let mut buffer = "你".as_bytes().to_vec();
        buffer.push("好".as_bytes()[0]);

        let chunks = drain_complete_input_bytes(&mut buffer);

        assert_eq!(chunks, vec!["你".as_bytes().to_vec()]);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);
    }

    #[test]
    fn incomplete_utf8_prefix_is_not_flushed_on_timeout() {
        let mut buffer = vec!["好".as_bytes()[0]];

        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);
    }

    #[test]
    fn invalid_utf8_lead_byte_is_flushed_instead_of_buffered_forever() {
        let mut buffer = vec![0xC0];

        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert!(buffer.is_empty());
    }

    #[test]
    fn complete_utf8_char_before_incomplete_char_survives_timeout_and_next_chunk() {
        let mut buffer = "你".as_bytes().to_vec();
        buffer.push("好".as_bytes()[0]);

        let chunks = drain_complete_input_bytes(&mut buffer);
        assert_eq!(chunks, vec!["你".as_bytes().to_vec()]);
        assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
        assert_eq!(buffer, vec!["好".as_bytes()[0]]);

        buffer.extend_from_slice(&"好".as_bytes()[1..]);
        let chunks = drain_complete_input_bytes(&mut buffer);
        assert_eq!(chunks, vec!["好".as_bytes().to_vec()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn chunked_utf8_waits_for_continuation_byte() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();

        drain_chunk(&mut buffer, &tx, "é".as_bytes().get(..1).unwrap());
        assert_eq!(buffer, vec![0xC3]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, "é".as_bytes().get(1..).unwrap());
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('é'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn chunked_cjk_utf8_waits_for_all_continuation_bytes() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();
        let bytes = "好".as_bytes();

        drain_chunk(&mut buffer, &tx, &bytes[..1]);
        assert_eq!(buffer, bytes[..1]);
        assert!(collect_events(&mut rx).is_empty());
        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, bytes[..1]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, &bytes[1..2]);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());
        flush_incomplete_buffer(&mut buffer, &tx);
        assert_eq!(buffer, bytes[..2]);
        assert!(collect_events(&mut rx).is_empty());

        drain_chunk(&mut buffer, &tx, &bytes[2..]);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('好'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn chunked_four_byte_utf8_waits_for_all_continuation_bytes() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut buffer = Vec::new();
        let bytes = "🙂".as_bytes();

        for split in 1..bytes.len() {
            drain_chunk(&mut buffer, &tx, &bytes[split - 1..split]);
            assert_eq!(buffer, bytes[..split]);
            assert!(collect_events(&mut rx).is_empty());
            flush_incomplete_buffer(&mut buffer, &tx);
            assert_eq!(buffer, bytes[..split]);
            assert!(collect_events(&mut rx).is_empty());
        }

        drain_chunk(&mut buffer, &tx, &bytes[bytes.len() - 1..]);
        assert!(buffer.is_empty());
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_raw_key(
            events.into_iter().next().unwrap(),
            KeyCode::Char('🙂'),
            KeyModifiers::empty(),
        );
    }

    #[test]
    fn long_multilingual_voice_like_burst_drains_without_truncation() {
        let text = "你好，今天我们测试一段比较长的语音输入。こんにちは。안녕하세요.🙂".repeat(128);
        assert!(
            text.len() > 4096,
            "test input should exceed the client read buffer"
        );
        let mut buffer = text.as_bytes().to_vec();

        let chunks = drain_complete_input_bytes(&mut buffer);
        let rebuilt: Vec<u8> = chunks.into_iter().flatten().collect();

        assert!(buffer.is_empty());
        assert_eq!(rebuilt, text.as_bytes());
    }

    #[test]
    fn long_multilingual_burst_survives_one_byte_chunks_and_timeouts() {
        let text = "中文かなカナ한글🙂，。".repeat(64);
        let mut buffer = Vec::new();
        let mut rebuilt = Vec::new();

        for byte in text.as_bytes() {
            buffer.push(*byte);
            for chunk in drain_complete_input_bytes(&mut buffer) {
                rebuilt.extend(chunk);
            }
            if !buffer.is_empty() {
                assert_eq!(flush_incomplete_input_bytes(&mut buffer), None);
            }
        }

        for chunk in drain_complete_input_bytes(&mut buffer) {
            rebuilt.extend(chunk);
        }

        assert!(buffer.is_empty());
        assert_eq!(rebuilt, text.as_bytes());
    }

    #[test]
    fn parse_with_ranges_tracks_byte_offsets() {
        use super::parse_raw_input_bytes_with_ranges;

        // Input: Up arrow (3 bytes) + 'a' (1 byte) + Down arrow (3 bytes)
        let input = b"\x1b[Aa\x1b[B".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 3, "should parse three events");

        // Up arrow: \x1b[A at offset 0, length 3
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 3);
        assert!(matches!(
            &ranges[0].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Up
        ));

        // 'a' at offset 3, length 1
        assert_eq!(ranges[1].start, 3);
        assert_eq!(ranges[1].len, 1);
        assert!(matches!(
            &ranges[1].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Char('a')
        ));

        // Down arrow: \x1b[B at offset 4, length 3
        assert_eq!(ranges[2].start, 4);
        assert_eq!(ranges[2].len, 3);
        assert!(matches!(
            &ranges[2].event,
            RawInputEvent::Key(k) if k.code == KeyCode::Down
        ));

        // Verify the raw bytes for each event slice correctly.
        assert_eq!(
            &input[ranges[0].start..ranges[0].start + ranges[0].len],
            b"\x1b[A"
        );
        assert_eq!(
            &input[ranges[1].start..ranges[1].start + ranges[1].len],
            b"a"
        );
        assert_eq!(
            &input[ranges[2].start..ranges[2].start + ranges[2].len],
            b"\x1b[B"
        );
    }

    #[test]
    fn parse_with_ranges_handles_single_event() {
        use super::parse_raw_input_bytes_with_ranges;

        let input = b"a".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, 1);
    }

    #[test]
    fn parse_with_ranges_handles_mouse_event() {
        use super::parse_raw_input_bytes_with_ranges;

        let input = b"\x1b[<0;20;10M".to_vec();
        let ranges = parse_raw_input_bytes_with_ranges(&input);

        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].len, input.len());
        assert!(matches!(&ranges[0].event, RawInputEvent::Mouse(_)));
    }
}
