use std::io::Read;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

use crate::input::{parse_terminal_key_sequence, TerminalKey};

const ESC: u8 = 0x1b;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug)]
pub enum RawInputEvent {
    Key(TerminalKey),
    Paste(String),
    Mouse(MouseEvent),
    Unsupported,
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
    loop {
        let Some((event, consumed)) = extract_one_event(buffer) else {
            break;
        };
        buffer.drain(..consumed);
        let _ = tx.blocking_send(event);
    }
}

fn flush_incomplete_buffer(buffer: &mut Vec<u8>, tx: &mpsc::Sender<RawInputEvent>) {
    if buffer.is_empty() {
        return;
    }

    if buffer.starts_with(BRACKETED_PASTE_START)
        && find_subsequence(buffer, BRACKETED_PASTE_END).is_none()
    {
        tracing::trace!(len = buffer.len(), "waiting for bracketed paste terminator");
        return;
    }

    if buffer.as_slice() == [ESC] {
        let _ = tx.blocking_send(RawInputEvent::Key(TerminalKey::new(
            crossterm::event::KeyCode::Esc,
            KeyModifiers::empty(),
        )));
        buffer.clear();
        return;
    }

    if let Ok(text) = std::str::from_utf8(buffer) {
        if let Some(key) = parse_terminal_key_sequence(text) {
            let _ = tx.blocking_send(RawInputEvent::Key(key));
            buffer.clear();
            return;
        }
    }

    tracing::debug!(bytes = ?buffer, "dropping incomplete raw input buffer after timeout");
    buffer.clear();
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(_reader: &R, _timeout_ms: i32) -> Option<bool> {
    #[cfg(unix)]
    {
        let fd = _reader.as_raw_fd();
        return poll_read_ready(fd, _timeout_ms);
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

        if let Some(mouse) = parse_sgr_mouse(seq) {
            return Some((RawInputEvent::Mouse(mouse), seq_len));
        }

        if let Some(key) = parse_terminal_key_sequence(seq) {
            return Some((RawInputEvent::Key(key), seq_len));
        }

        tracing::debug!(sequence = ?seq, "dropping unsupported escape sequence");
        return Some((RawInputEvent::Unsupported, seq_len));
    }

    let ch = std::str::from_utf8(buffer).ok()?.chars().next()?;
    let consumed = ch.len_utf8();
    let seq = &buffer[..consumed];
    let text = std::str::from_utf8(seq).ok()?;
    let key = parse_terminal_key_sequence(text)?;
    Some((RawInputEvent::Key(key), consumed))
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

    if buffer.starts_with(b"\x1b]") || buffer.starts_with(b"\x1bP") || buffer.starts_with(b"\x1b_")
    {
        return find_subsequence(buffer, b"\x1b\\").map(|idx| idx + 2);
    }

    if buffer.starts_with(b"\x1bO") {
        return (buffer.len() >= 3).then_some(3);
    }

    Some(2)
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
    fn parses_legacy_up_arrow() {
        let (RawInputEvent::Key(key), consumed) = extract_one_event(b"\x1b[A").unwrap() else {
            panic!("expected key");
        };
        assert_eq!(consumed, 3);
        assert_eq!(key.code, KeyCode::Up);
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
    fn raw_input_family_matrix_is_covered() {
        let cases: &[(&[u8], KeyCode, KeyModifiers)] = &[
            (b"\x02", KeyCode::Char('b'), KeyModifiers::CONTROL),
            (b"\r", KeyCode::Enter, KeyModifiers::empty()),
            (b"\t", KeyCode::Tab, KeyModifiers::empty()),
            (b"\x7f", KeyCode::Backspace, KeyModifiers::empty()),
            (b"\x1b[A", KeyCode::Up, KeyModifiers::empty()),
            (b"\x1b[1;3A", KeyCode::Up, KeyModifiers::ALT),
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
}
