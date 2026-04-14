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

#[cfg(unix)]
fn stdin_read_ready<R: AsRawFd>(reader: &R, timeout_ms: i32) -> Option<bool> {
    poll_read_ready(reader.as_raw_fd(), timeout_ms)
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
