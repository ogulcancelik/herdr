//! Stdin input reading for the thin client.
//!
//! Reads raw bytes from stdin and forwards them to the main event loop.
//! Unlike the monolithic herdr, the thin client does NOT parse the input
//! into key/mouse/paste events — it just sends the raw bytes to the server
//! as `ClientMessage::Input`. The server handles parsing.
//!
//! This is simpler and more reliable because:
//! - The server has the same input parsing code
//! - We avoid duplicating parsing logic in the client
//! - Raw forwarding preserves all escape sequences faithfully

#[cfg(not(windows))]
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

use super::ClientLoopEvent;

// ---------------------------------------------------------------------------
// Stdin reader thread
// ---------------------------------------------------------------------------

/// Reads raw bytes from stdin and sends them to the main event loop.
///
/// This runs on a dedicated thread because stdin reading is blocking.
/// The main loop receives the raw bytes and forwards them as
/// `ClientMessage::Input` to the server.
#[cfg(not(windows))]
pub fn stdin_reader_loop(event_tx: mpsc::Sender<ClientLoopEvent>, should_quit: &Arc<AtomicBool>) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut scratch = [0u8; 4096];
    let mut buffer = Vec::new();

    while !should_quit.load(Ordering::Acquire) {
        match reader.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&scratch[..n]);

                for data in crate::raw_input::drain_complete_input_bytes(&mut buffer) {
                    if event_tx
                        .blocking_send(ClientLoopEvent::StdinInput(data))
                        .is_err()
                    {
                        return;
                    }
                }

                if !buffer.is_empty() && stdin_read_ready(&reader, 10) == Some(false) {
                    if let Some(data) = crate::raw_input::flush_incomplete_input_bytes(&mut buffer)
                    {
                        if event_tx
                            .blocking_send(ClientLoopEvent::StdinInput(data))
                            .is_err()
                        {
                            return;
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

/// Windows console doesn't deliver key/mouse events as byte sequences over
/// stdin — they come as `INPUT_RECORD` structs and need `ReadConsoleInput`.
/// crossterm wraps that as `event::read()` returning parsed [`Event`]s, so
/// we read those, re-encode each event into the same VT escape sequence a
/// Unix terminal would have produced, and feed it through the existing
/// byte-oriented client-to-server pipeline.
#[cfg(windows)]
pub fn stdin_reader_loop(event_tx: mpsc::Sender<ClientLoopEvent>, should_quit: &Arc<AtomicBool>) {
    use crossterm::event::{self, Event, KeyEventKind, MouseEventKind};

    use crate::input::{
        encode_mouse_button, encode_mouse_scroll, encode_terminal_key, KeyboardProtocol,
        MouseProtocolEncoding, TerminalKey,
    };

    fn encode_into(batch: &mut Vec<u8>, event: Event) {
        match event {
            Event::Key(key) => {
                if matches!(key.kind, KeyEventKind::Release) {
                    return;
                }
                batch
                    .extend_from_slice(&encode_terminal_key(TerminalKey::from(key), KeyboardProtocol::Legacy));
            }
            Event::Mouse(mouse) => {
                let encoded = match mouse.kind {
                    MouseEventKind::Down(_)
                    | MouseEventKind::Up(_)
                    | MouseEventKind::Drag(_) => encode_mouse_button(
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                        mouse.modifiers,
                        MouseProtocolEncoding::Sgr,
                    ),
                    MouseEventKind::ScrollUp
                    | MouseEventKind::ScrollDown
                    | MouseEventKind::ScrollLeft
                    | MouseEventKind::ScrollRight => encode_mouse_scroll(
                        mouse.kind,
                        mouse.column,
                        mouse.row,
                        mouse.modifiers,
                        MouseProtocolEncoding::Sgr,
                    ),
                    MouseEventKind::Moved => None,
                };
                if let Some(bytes) = encoded {
                    batch.extend_from_slice(&bytes);
                }
            }
            Event::Paste(text) => {
                batch.extend_from_slice(b"\x1b[200~");
                batch.extend_from_slice(text.as_bytes());
                batch.extend_from_slice(b"\x1b[201~");
            }
            Event::FocusGained => batch.extend_from_slice(b"\x1b[I"),
            Event::FocusLost => batch.extend_from_slice(b"\x1b[O"),
            Event::Resize(_, _) => {}
        }
    }

    let mut batch: Vec<u8> = Vec::with_capacity(128);

    while !should_quit.load(Ordering::Acquire) {
        // Block directly on the console event queue. crossterm wakes us
        // immediately when a key/mouse/paste arrives, so there's no poll
        // interval rounding the per-keystroke latency up.
        let event = match event::read() {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!(?err, "crossterm event::read failed; stopping reader");
                return;
            }
        };

        if should_quit.load(Ordering::Acquire) {
            return;
        }

        // Drain any already-queued events into the same batch so a burst
        // of keystrokes round-trips as one ClientMessage::Input instead of
        // N separate ones.
        batch.clear();
        encode_into(&mut batch, event);
        while let Ok(true) = event::poll(std::time::Duration::from_secs(0)) {
            match event::read() {
                Ok(next) => encode_into(&mut batch, next),
                Err(_) => break,
            }
        }

        if batch.is_empty() {
            continue;
        }
        if event_tx
            .blocking_send(ClientLoopEvent::StdinInput(std::mem::take(&mut batch)))
            .is_err()
        {
            return;
        }
    }
}

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    poll_read_ready(reader.as_raw_fd(), timeout_ms)
}

#[cfg(not(unix))]
#[allow(dead_code)]
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // The stdin reader thread is hard to unit test since it reads from actual stdin.
    // Integration tests will verify the full client→server input flow.
    // Here we test the event type construction.

    use super::*;

    #[test]
    fn stdin_input_event_carries_raw_bytes() {
        let data = vec![0x1b, b'[', b'A']; // Up arrow escape sequence
        let event = ClientLoopEvent::StdinInput(data.clone());
        match event {
            ClientLoopEvent::StdinInput(d) => assert_eq!(d, data),
            _ => panic!("expected StdinInput event"),
        }
    }
}
