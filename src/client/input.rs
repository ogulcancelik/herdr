//! Stdin input reading for the thin client.
//!
//! On Unix, reads stdin bytes and forwards framed input to the main event loop.
//! The server handles semantic parsing. On Windows, crossterm may surface
//! terminal control strings as character key events, so the reader re-frames
//! those control bytes before forwarding semantic client input events.
//!
//! This is simpler and more reliable because:
//! - The server has the same input parsing code
//! - We avoid duplicating parsing logic in the client
//! - Host terminal control replies can be buffered or discarded before they leak

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::time::Duration;
use tokio::sync::mpsc;

use super::ClientLoopEvent;

#[cfg(any(windows, test))]
mod windows_vti;

// ---------------------------------------------------------------------------
// Stdin reader thread
// ---------------------------------------------------------------------------

/// Reads raw bytes from stdin and sends them to the main event loop.
///
/// This runs on a dedicated thread because stdin reading is blocking.
/// The main loop receives the raw bytes and forwards them as
/// `ClientMessage::Input` to the server.
pub fn stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
) {
    #[cfg(windows)]
    {
        let _ = (host_color_query_sent, host_mouse_capture_active);
        windows_stdin_reader_loop(event_tx, should_quit);
    }

    #[cfg(unix)]
    unix_stdin_reader_loop(
        event_tx,
        should_quit,
        host_color_query_sent,
        host_mouse_capture_active,
    );
}

#[cfg(unix)]
fn unix_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    host_color_query_sent: bool,
    host_mouse_capture_active: Arc<AtomicBool>,
) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut scratch = [0u8; 4096];
    let mut framer = crate::raw_input::RawInputByteFramer::default();
    if host_color_query_sent {
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();
    }

    while !should_quit.load(Ordering::Acquire) {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                for data in framer.push(&scratch[..n]) {
                    if event_tx
                        .blocking_send(ClientLoopEvent::StdinInput(data))
                        .is_err()
                    {
                        return;
                    }
                }

                let timeout_ms = idle_flush_timeout_ms(
                    &framer,
                    host_mouse_capture_active.load(Ordering::Acquire),
                );
                if stdin_read_ready(&reader, timeout_ms) == Some(false) {
                    let had_pending = framer.has_pending_input();
                    let chunks = framer.flush_timeout();
                    let held_escape = had_pending && chunks.is_empty();
                    for data in chunks {
                        if event_tx
                            .blocking_send(ClientLoopEvent::StdinInput(data))
                            .is_err()
                        {
                            return;
                        }
                    }
                    if held_escape
                        && stdin_read_ready(
                            &reader,
                            crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS,
                        ) == Some(false)
                    {
                        for data in framer.flush_timeout() {
                            if event_tx
                                .blocking_send(ClientLoopEvent::StdinInput(data))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
        }
    }
}

#[cfg(unix)]
fn idle_flush_timeout_ms(
    framer: &crate::raw_input::RawInputByteFramer,
    host_mouse_capture_active: bool,
) -> i32 {
    if host_mouse_capture_active && framer.has_pending_lone_escape() {
        crate::raw_input::MOUSE_ACTIVE_LONE_ESCAPE_FLUSH_TIMEOUT_MS
    } else {
        crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS
    }
}

#[cfg(windows)]
fn windows_stdin_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    if !super::windows_vti_input_backend_enabled() {
        windows_crossterm_reader_loop(event_tx, should_quit);
    } else {
        match windows_vti::console_input_handle() {
            Ok(handle) if windows_vti::virtual_terminal_input_enabled(handle) => {
                windows_vti::raw_console_reader_loop(handle, event_tx, should_quit);
            }
            _ => windows_crossterm_reader_loop(event_tx, should_quit),
        }
    }
}

#[cfg(windows)]
fn windows_crossterm_reader_loop(
    event_tx: mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    let mut framer = crate::raw_input::RawInputFramer::default();
    let mut raw_sequence_pending = false;

    while !should_quit.load(Ordering::Acquire) {
        match crossterm::event::poll(Duration::from_millis(10)) {
            Ok(true) => {}
            Ok(false) => {
                if raw_sequence_pending {
                    tracing::debug!("windows input raw sequence timed out; flushing");
                    if !send_windows_raw_events(framer.flush_timeout(), &event_tx) {
                        return;
                    }
                    raw_sequence_pending = false;
                }
                continue;
            }
            Err(_) => break,
        }

        let event = match crossterm::event::read() {
            Ok(event) => event,
            Err(_) => break,
        };

        if let Some(bytes) = windows_key_raw_bytes(&event, raw_sequence_pending) {
            tracing::debug!(
                bytes = ?bytes,
                pending_before = raw_sequence_pending,
                "windows input routed through raw framer"
            );
            let events = framer.push(&bytes);
            raw_sequence_pending = events.is_empty();
            if !send_windows_raw_events(events, &event_tx) {
                return;
            }
            continue;
        }

        if raw_sequence_pending {
            tracing::debug!("windows input raw sequence interrupted by semantic event; flushing");
            if !send_windows_raw_events(framer.flush_timeout(), &event_tx) {
                return;
            }
            raw_sequence_pending = false;
        }

        if windows_event_is_control_key(&event) {
            tracing::debug!(event = ?event, "windows control key forwarded as semantic input");
        }

        let Some(event) = crate::protocol::ClientInputEvent::from_crossterm(event) else {
            continue;
        };
        if event_tx
            .blocking_send(ClientLoopEvent::StdinEvents(vec![event]))
            .is_err()
        {
            return;
        }
    }

    if raw_sequence_pending {
        let _ = send_windows_raw_events(framer.flush_timeout(), &event_tx);
    }
}

#[cfg(windows)]
fn windows_event_is_control_key(event: &crossterm::event::Event) -> bool {
    use crossterm::event::{Event, KeyModifiers};

    matches!(
        event,
        Event::Key(key)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || matches!(key.code, crossterm::event::KeyCode::Char(ch) if ch.is_control())
    )
}

#[cfg(any(windows, test))]
fn windows_key_raw_bytes(
    event: &crossterm::event::Event,
    raw_sequence_pending: bool,
) -> Option<Vec<u8>> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

    let Event::Key(key) = event else {
        return None;
    };
    if key.kind == KeyEventKind::Release {
        return None;
    }

    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => Some(vec![0x1b]),
        KeyCode::Char('[') if !raw_sequence_pending && key.modifiers == KeyModifiers::CONTROL => {
            Some(vec![0x1b])
        }
        KeyCode::Char(ch)
            if !raw_sequence_pending
                && matches!(ch, 'i' | 'I')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let mut buf = [0; 4];
            Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        KeyCode::Char(ch) if raw_sequence_pending || ch.is_control() => {
            let mut bytes = Vec::new();
            if key.modifiers.contains(KeyModifiers::ALT) {
                bytes.push(0x1b);
            }
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            Some(bytes)
        }
        _ => None,
    }
}

#[cfg(windows)]
fn send_windows_raw_events(
    events: Vec<crate::raw_input::RawInputEvent>,
    event_tx: &mpsc::Sender<ClientLoopEvent>,
) -> bool {
    let raw_event_count = events.len();
    let events = events
        .into_iter()
        .filter_map(windows_client_input_event_from_raw)
        .collect::<Vec<_>>();
    if events.is_empty() {
        return true;
    }

    tracing::debug!(
        raw_event_count,
        forwarded_event_count = events.len(),
        "windows raw-framed input events forwarded"
    );
    event_tx
        .blocking_send(ClientLoopEvent::StdinEvents(events))
        .is_ok()
}

#[cfg(any(windows, test))]
fn windows_client_input_event_from_raw(
    event: crate::raw_input::RawInputEvent,
) -> Option<crate::protocol::ClientInputEvent> {
    match event {
        crate::raw_input::RawInputEvent::Key(key) => Some(crate::protocol::ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::from_crossterm(key.code)?,
            modifiers: key.modifiers.bits(),
            kind: crate::protocol::ClientKeyKind::from_crossterm(key.kind),
        }),
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            Some(crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::from_crossterm(mouse.kind)?,
                column: mouse.column,
                row: mouse.row,
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Paste(text) => {
            Some(crate::protocol::ClientInputEvent::Paste { text })
        }
        crate::raw_input::RawInputEvent::OuterFocusGained => {
            Some(crate::protocol::ClientInputEvent::FocusGained)
        }
        crate::raw_input::RawInputEvent::OuterFocusLost => {
            Some(crate::protocol::ClientInputEvent::FocusLost)
        }
        crate::raw_input::RawInputEvent::HostDefaultColor { .. }
        | crate::raw_input::RawInputEvent::HostColorSchemeChanged(_)
        | crate::raw_input::RawInputEvent::Unsupported => None,
    }
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    poll_read_ready(reader.as_raw_fd(), timeout_ms)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    // The stdin reader thread is hard to unit test since it reads from actual stdin.
    // Integration tests will verify the full client→server input flow.
    // Here we test the event type construction.

    use super::*;

    #[cfg(unix)]
    #[test]
    fn stdin_input_event_carries_raw_bytes() {
        let data = vec![0x1b, b'[', b'A']; // Up arrow escape sequence
        let event = ClientLoopEvent::StdinInput(data.clone());
        match event {
            ClientLoopEvent::StdinInput(d) => assert_eq!(d, data),
            _ => panic!("expected StdinInput event"),
        }
    }

    #[test]
    fn raw_input_idle_flush_timeout_keeps_escape_responsive() {
        let timeout_ms = std::hint::black_box(crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS);
        assert!(timeout_ms <= 20);
    }

    #[test]
    fn mouse_active_lone_escape_gets_longer_reassembly_window() {
        let mut framer = crate::raw_input::RawInputByteFramer::default();
        assert!(framer.push(b"\x1b").is_empty());

        assert_eq!(
            idle_flush_timeout_ms(&framer, false),
            crate::raw_input::RAW_INPUT_IDLE_FLUSH_TIMEOUT_MS
        );
        assert_eq!(
            idle_flush_timeout_ms(&framer, true),
            crate::raw_input::MOUSE_ACTIVE_LONE_ESCAPE_FLUSH_TIMEOUT_MS
        );
        let mouse_timeout_ms =
            std::hint::black_box(crate::raw_input::MOUSE_ACTIVE_LONE_ESCAPE_FLUSH_TIMEOUT_MS);
        assert!(mouse_timeout_ms > 100);
    }
}

#[cfg(test)]
mod windows_tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn windows_control_chars_are_reframed_as_raw_bytes() {
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
        assert_eq!(
            windows_key_raw_bytes(&escape, false).as_deref(),
            Some(b"\x1b".as_slice())
        );

        let enter = Event::Key(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::empty()));
        assert_eq!(
            windows_key_raw_bytes(&enter, false).as_deref(),
            Some(b"\r".as_slice())
        );

        let printable = Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
        assert_eq!(windows_key_raw_bytes(&printable, false), None);

        let pending_arrow_tail =
            Event::Key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::empty()));
        assert_eq!(
            windows_key_raw_bytes(&pending_arrow_tail, true).as_deref(),
            Some(b"[".as_slice())
        );
    }

    #[test]
    fn windows_ctrl_bracket_starts_raw_escape_sequence() {
        let ctrl_bracket = Event::Key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::CONTROL));
        assert_eq!(
            windows_key_raw_bytes(&ctrl_bracket, false).as_deref(),
            Some(b"\x1b".as_slice())
        );

        let mut framer = crate::raw_input::RawInputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        let events = framer.push(b"[<35;48;26M");
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw mouse converts");
        assert!(matches!(
            event,
            crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::Moved,
                column: 47,
                row: 25,
                modifiers: _,
            }
        ));
    }

    #[test]
    fn windows_ctrl_shift_bracket_stays_semantic() {
        let ctrl_shift_bracket = Event::Key(KeyEvent::new(
            KeyCode::Char('['),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(windows_key_raw_bytes(&ctrl_shift_bracket, false), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_ctrl_d_semantic_event_encodes_to_eot() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(windows_key_raw_bytes(&event, false), None);

        let event =
            crate::protocol::ClientInputEvent::from_crossterm(event).expect("ctrl-d converts");
        let raw = event.to_raw_input_event();
        let crate::raw_input::RawInputEvent::Key(key) = raw else {
            panic!("expected key");
        };
        assert_eq!(key.code, KeyCode::Char('d'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
        assert_eq!(
            crate::input::encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy),
            b"\x04"
        );
    }

    #[test]
    fn windows_pasted_printable_ctrl_i_routes_as_literal_i() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL));
        assert_eq!(
            windows_key_raw_bytes(&event, false).as_deref(),
            Some(b"i".as_slice())
        );

        let event = Event::Key(KeyEvent::new(
            KeyCode::Char('I'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(
            windows_key_raw_bytes(&event, false).as_deref(),
            Some(b"I".as_slice())
        );
    }

    #[test]
    fn windows_eot_control_char_normalizes_to_ctrl_d() {
        let event = Event::Key(KeyEvent::new(KeyCode::Char('\u{4}'), KeyModifiers::empty()));
        let bytes = windows_key_raw_bytes(&event, false).expect("eot routes through raw framer");
        assert_eq!(bytes, b"\x04");

        let mut framer = crate::raw_input::RawInputFramer::default();
        let events = framer.push(&bytes);
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw eot converts");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('d'),
                modifiers: KeyModifiers::CONTROL.bits(),
                kind: crate::protocol::ClientKeyKind::Press,
            }
        );
    }

    #[test]
    fn windows_pending_escape_sequence_converts_to_semantic_arrow() {
        let mut framer = crate::raw_input::RawInputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        assert!(framer.push(b"[").is_empty());
        let events = framer.push(b"A");
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw arrow converts");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Up,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }
        );
    }

    #[test]
    fn windows_bare_escape_flushes_to_semantic_escape() {
        let mut framer = crate::raw_input::RawInputFramer::default();
        assert!(framer.push(b"\x1b").is_empty());
        let events = framer.flush_timeout();
        assert_eq!(events.len(), 1);

        let event = windows_client_input_event_from_raw(events.into_iter().next().unwrap())
            .expect("raw escape converts");
        assert_eq!(
            event,
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }
        );
    }
}
