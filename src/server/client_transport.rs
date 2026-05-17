//! Blocking client socket transport for the headless server.
//!
//! This module owns the thin-client handshake, read loop, and writer loop.
//! It converts socket I/O into [`ServerEvent`] values consumed by
//! `HeadlessServer`.

use std::io::{self, Write};
use crate::platform::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::server::protocol::{
    self, ClientMessage, RenderEncoding, ServerMessage, MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

/// Minimum accepted attached client size.
///
/// Narrow observers must be allowed to drive narrow renders, otherwise the
/// server wraps pane content against a wider width and the client sees the
/// right edge clipped.
const MIN_CLIENT_COLS: u16 = 1;
const MIN_CLIENT_ROWS: u16 = 1;

/// How long to wait for a client handshake before closing the connection.
/// Set to 4 seconds (rather than 5) to guarantee the connection is closed
/// within the 5-second deadline, even with OS timer slack, thread scheduling,
/// and cleanup overhead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum input payload size (bytes) for a single `ClientMessage::Input`.
const MAX_INPUT_PAYLOAD: usize = 1024 * 1024; // 1 MB

/// Channels owned by the server side of a client writer thread.
#[derive(Clone, Debug)]
pub(crate) struct ClientWriter {
    /// Reliable control messages such as shutdown, notifications, and clipboard writes.
    pub(crate) control: std::sync::mpsc::Sender<Vec<u8>>,
    /// Droppable render messages. Capacity is one so slow clients cannot build lag.
    pub(crate) render: std::sync::mpsc::SyncSender<Vec<u8>>,
}

/// Internal event sent from client transport threads to the main event loop.
#[derive(Debug)]
pub(crate) enum ServerEvent {
    /// A new client completed the handshake.
    ClientConnected {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        render_encoding: RenderEncoding,
        writer: ClientWriter,
    },
    /// A client sent an input message.
    ClientInput { client_id: u64, data: Vec<u8> },
    /// A client requested direct attach to one terminal.
    ClientAttachTerminal {
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    },
    /// A client sent a resize message.
    ClientResize {
        client_id: u64,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    },
    /// A client detached gracefully.
    ClientDetach { client_id: u64 },
    /// A client connection was lost.
    ClientDisconnected { client_id: u64 },
    /// A client writer drained its render slot and can accept another render.
    ClientWriterDrained { client_id: u64 },
    /// Ctrl+C or external shutdown signal received.
    QuitSignal,
}

/// Clamp client-reported terminal dimensions to a minimum viable size.
pub(crate) fn clamp_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    let clamped_cols = cols.max(MIN_CLIENT_COLS);
    let clamped_rows = rows.max(MIN_CLIENT_ROWS);
    (clamped_cols, clamped_rows)
}

/// Handles the client handshake on a blocking thread.
///
/// Reads the `Hello` message, validates the version, sends `Welcome`,
/// and then enters a read loop forwarding messages to the server event channel.
pub(crate) fn handle_client_handshake(
    mut stream: UnixStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    // Reset to blocking mode — the accept loop sets nonblocking but
    // the handshake thread needs blocking I/O for read_message/write_message.
    stream.set_nonblocking(false)?;

    // Set a read timeout for the handshake.
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;

    // Read the Hello message.
    let hello: ClientMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
        Ok(msg) => msg,
        Err(protocol::FramingError::UnexpectedEof) => {
            debug!(client_id, "client disconnected before handshake");
            return Ok(());
        }
        Err(protocol::FramingError::Oversized { claimed, max }) => {
            warn!(client_id, claimed, max, "oversized handshake from client");
            return Ok(());
        }
        Err(err) => {
            debug!(client_id, err = %err, "failed to read client hello");
            return Ok(());
        }
    };

    let (client_cols, client_rows, cell_width_px, cell_height_px, render_encoding) = match hello {
        ClientMessage::Hello {
            version,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            requested_encoding,
        } => {
            // Version check.
            match protocol::check_client_version(version) {
                protocol::VersionCheck::Compatible => {}
                protocol::VersionCheck::Incompatible(reason) => {
                    // Send rejection Welcome.
                    let welcome = ServerMessage::Welcome {
                        version: PROTOCOL_VERSION,
                        encoding: RenderEncoding::SemanticFrame,
                        error: Some(reason),
                    };
                    let _ = protocol::write_message(&mut stream, &welcome);
                    return Ok(());
                }
            }

            // Clamp size.
            let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
            (
                clamped_cols,
                clamped_rows,
                cell_width_px,
                cell_height_px,
                requested_encoding,
            )
        }
        _ => {
            // First message must be Hello.
            debug!(client_id, "first message was not Hello, closing");
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::SemanticFrame,
                error: Some("expected Hello as first message".to_owned()),
            };
            let _ = protocol::write_message(&mut stream, &welcome);
            return Ok(());
        }
    };

    // Send Welcome.
    let welcome = ServerMessage::Welcome {
        version: PROTOCOL_VERSION,
        encoding: render_encoding,
        error: None,
    };
    protocol::write_message(&mut stream, &welcome).map_err(|e| io::Error::other(e.to_string()))?;

    // Clear read timeout for normal operation.
    stream.set_read_timeout(None)?;

    // Create separate channels for reliable control messages and droppable renders.
    let (control_tx, control_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
    let writer = ClientWriter {
        control: control_tx,
        render: render_tx,
    };

    // Notify the main loop about the new client.
    let _ = server_event_tx.blocking_send(ServerEvent::ClientConnected {
        client_id,
        cols: client_cols,
        rows: client_rows,
        cell_width_px,
        cell_height_px,
        render_encoding,
        writer,
    });

    // Spawn a writer thread that forwards messages from the channels to the stream.
    let write_stream = stream.try_clone()?;
    let writer_event_tx = server_event_tx.clone();
    std::thread::spawn(move || {
        client_writer_loop(
            write_stream,
            client_id,
            control_rx,
            render_rx,
            writer_event_tx,
        );
    });

    // Enter read loop — read client messages and forward to main loop.
    client_read_loop(stream, client_id, server_event_tx, should_quit)
}

/// The client writer loop — prioritizes control messages over render frames.
fn client_writer_loop(
    mut stream: UnixStream,
    client_id: u64,
    control_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    render_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    let mut control_closed = false;
    let mut render_closed = false;

    loop {
        match control_rx.try_recv() {
            Ok(data) => {
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => control_closed = true,
        }

        match render_rx.try_recv() {
            Ok(data) => {
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientWriterDrained { client_id });
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => render_closed = true,
        }

        if control_closed && render_closed {
            break;
        }

        if control_closed {
            match render_rx.recv_timeout(Duration::from_millis(5)) {
                Ok(data) => {
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientWriterDrained { client_id });
                    if !write_framed_bytes(&mut stream, &data) {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => render_closed = true,
            }
            continue;
        }

        match control_rx.recv_timeout(Duration::from_millis(5)) {
            Ok(data) => {
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => control_closed = true,
        }
    }
    debug!("client writer thread exiting");
}

fn write_framed_bytes(stream: &mut UnixStream, data: &[u8]) -> bool {
    if let Err(err) = stream.write_all(data) {
        debug!(err = %err, "client write failed, closing writer");
        return false;
    }
    if let Err(err) = stream.flush() {
        debug!(err = %err, "client flush failed, closing writer");
        return false;
    }
    true
}

/// The client read loop — reads messages from the client and forwards to the server event channel.
fn client_read_loop(
    mut stream: UnixStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    while !should_quit.load(Ordering::Acquire) {
        let msg: ClientMessage = match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
            Ok(msg) => msg,
            Err(protocol::FramingError::UnexpectedEof) => {
                // Client disconnected.
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(protocol::FramingError::Oversized { claimed, max }) => {
                warn!(
                    client_id,
                    claimed, max, "oversized message from client, closing"
                );
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
            Err(err) => {
                debug!(client_id, err = %err, "client read error, closing");
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientDisconnected { client_id });
                break;
            }
        };

        let event = match msg {
            ClientMessage::Input { data } => {
                // Validate input size.
                if data.len() > MAX_INPUT_PAYLOAD {
                    warn!(
                        client_id,
                        size = data.len(),
                        "oversized input from client, closing"
                    );
                    ServerEvent::ClientDisconnected { client_id }
                } else {
                    ServerEvent::ClientInput { client_id, data }
                }
            }
            ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            } => {
                let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
                ServerEvent::ClientResize {
                    client_id,
                    cols: clamped_cols,
                    rows: clamped_rows,
                    cell_width_px,
                    cell_height_px,
                }
            }
            ClientMessage::Detach => ServerEvent::ClientDetach { client_id },
            ClientMessage::AttachTerminal {
                terminal_id,
                takeover,
            } => ServerEvent::ClientAttachTerminal {
                client_id,
                terminal_id,
                takeover,
            },
            ClientMessage::Hello { .. } => {
                // Duplicate Hello — ignore.
                continue;
            }
        };

        if server_event_tx.blocking_send(event).is_err() {
            break; // Main loop gone.
        }
    }

    debug!(client_id, "client read thread exiting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_terminal_size_zero_zero() {
        assert_eq!(
            clamp_terminal_size(0, 0),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[test]
    fn clamp_terminal_size_one_one() {
        assert_eq!(clamp_terminal_size(1, 1), (1, 1));
    }

    #[test]
    fn clamp_terminal_size_preserves_narrow_client_size() {
        assert_eq!(clamp_terminal_size(40, 12), (40, 12));
    }

    #[test]
    fn clamp_terminal_size_valid() {
        assert_eq!(clamp_terminal_size(120, 40), (120, 40));
    }

    #[test]
    fn clamp_terminal_size_exact_minimum() {
        assert_eq!(
            clamp_terminal_size(MIN_CLIENT_COLS, MIN_CLIENT_ROWS),
            (MIN_CLIENT_COLS, MIN_CLIENT_ROWS)
        );
    }

    #[cfg(unix)]
    #[test]
    fn handshake_negotiates_terminal_ansi_encoding() {
        let (mut client_stream, server_stream) = UnixStream::pair().expect("socket pair");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let handshake_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            handle_client_handshake(server_stream, 42, &server_event_tx, &handshake_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Hello {
                version: PROTOCOL_VERSION,
                cols: 100,
                rows: 30,
                cell_width_px: 8,
                cell_height_px: 16,
                requested_encoding: RenderEncoding::TerminalAnsi,
            },
        )
        .expect("write hello");

        let welcome: ServerMessage =
            protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read welcome");
        match welcome {
            ServerMessage::Welcome {
                version,
                encoding,
                error,
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(encoding, RenderEncoding::TerminalAnsi);
                assert_eq!(error, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }

        match server_event_rx
            .blocking_recv()
            .expect("client connected event")
        {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                render_encoding,
                writer,
            } => {
                assert_eq!(client_id, 42);
                assert_eq!((cols, rows), (100, 30));
                assert_eq!((cell_width_px, cell_height_px), (8, 16));
                assert_eq!(render_encoding, RenderEncoding::TerminalAnsi);
                drop(writer);
            }
            other => panic!("expected ClientConnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("handshake thread join")
            .expect("handshake thread result");
    }

    #[cfg(unix)]
    #[test]
    fn client_read_loop_rejects_oversized_input() {
        let (mut client_stream, server_stream) = UnixStream::pair().expect("socket pair");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::Input {
                data: vec![b'x'; MAX_INPUT_PAYLOAD + 1],
            },
        )
        .expect("write oversized input");

        match server_event_rx
            .blocking_recv()
            .expect("client disconnected event")
        {
            ServerEvent::ClientDisconnected { client_id } => assert_eq!(client_id, 7),
            other => panic!("expected ClientDisconnected, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn handshake_timeout_is_within_five_second_deadline() {
        // The handshake timeout must be short enough that
        // the connection is guaranteed to close within 5 seconds even with
        // OS overhead (thread scheduling, timer slack, cleanup).
        assert!(
            HANDSHAKE_TIMEOUT < Duration::from_secs(5),
            "HANDSHAKE_TIMEOUT ({:?}) must be less than 5 seconds to guarantee \
             connection close within the 5-second deadline",
            HANDSHAKE_TIMEOUT
        );
    }
}
