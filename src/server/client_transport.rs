//! Blocking client socket transport for the headless server.
//!
//! This module owns the thin-client handshake, read loop, and writer loop.
//! It converts socket I/O into [`ServerEvent`] values consumed by
//! `HeadlessServer`.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SendError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ipc::LocalStream;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientInputEvent, ClientKeybindings,
    ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage, MAX_CLIPBOARD_IMAGE_PAYLOAD,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
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
/// Maximum structured input events accepted in one client message.
const MAX_INPUT_EVENT_BATCH: usize = 4096;

/// Channels owned by the server side of a client writer thread.
#[derive(Clone, Debug)]
pub(crate) struct ClientWriter {
    /// Reliable control messages such as shutdown, notifications, and clipboard writes.
    pub(crate) control: ClientControlWriter,
    /// Droppable render messages. Capacity is one so slow clients cannot build lag.
    pub(crate) render: ClientRenderWriter,
}

#[cfg(test)]
impl ClientWriter {
    pub(crate) fn test_channel(
        control: std::sync::mpsc::Sender<Vec<u8>>,
        render: std::sync::mpsc::SyncSender<Vec<u8>>,
    ) -> Self {
        Self {
            control: ClientControlWriter {
                target: ClientControlTarget::Channel(control),
            },
            render: ClientRenderWriter {
                target: ClientRenderTarget::Channel(render),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClientControlWriter {
    target: ClientControlTarget,
}

#[derive(Debug)]
enum ClientControlTarget {
    Queue(Arc<ClientWriterQueue>),
    #[cfg(test)]
    Channel(std::sync::mpsc::Sender<Vec<u8>>),
}

#[derive(Debug)]
pub(crate) struct ClientRenderWriter {
    target: ClientRenderTarget,
}

#[derive(Debug)]
enum ClientRenderTarget {
    Queue(Arc<ClientWriterQueue>),
    #[cfg(test)]
    Channel(std::sync::mpsc::SyncSender<Vec<u8>>),
}

impl Clone for ClientControlWriter {
    fn clone(&self) -> Self {
        match &self.target {
            ClientControlTarget::Queue(queue) => {
                queue.add_sender();
                Self {
                    target: ClientControlTarget::Queue(queue.clone()),
                }
            }
            #[cfg(test)]
            ClientControlTarget::Channel(sender) => Self {
                target: ClientControlTarget::Channel(sender.clone()),
            },
        }
    }
}

impl Drop for ClientControlWriter {
    fn drop(&mut self) {
        match &self.target {
            ClientControlTarget::Queue(queue) => queue.remove_sender(),
            #[cfg(test)]
            ClientControlTarget::Channel(_) => {}
        }
    }
}

impl ClientControlWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            target: ClientControlTarget::Queue(queue),
        }
    }

    pub(crate) fn send(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        match &self.target {
            ClientControlTarget::Queue(queue) => queue.send_control(data),
            #[cfg(test)]
            ClientControlTarget::Channel(sender) => sender.send(data),
        }
    }
}

impl Clone for ClientRenderWriter {
    fn clone(&self) -> Self {
        match &self.target {
            ClientRenderTarget::Queue(queue) => {
                queue.add_sender();
                Self {
                    target: ClientRenderTarget::Queue(queue.clone()),
                }
            }
            #[cfg(test)]
            ClientRenderTarget::Channel(sender) => Self {
                target: ClientRenderTarget::Channel(sender.clone()),
            },
        }
    }
}

impl Drop for ClientRenderWriter {
    fn drop(&mut self) {
        match &self.target {
            ClientRenderTarget::Queue(queue) => queue.remove_sender(),
            #[cfg(test)]
            ClientRenderTarget::Channel(_) => {}
        }
    }
}

impl ClientRenderWriter {
    fn queue(queue: Arc<ClientWriterQueue>) -> Self {
        queue.add_sender();
        Self {
            target: ClientRenderTarget::Queue(queue),
        }
    }

    pub(crate) fn try_send(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        match &self.target {
            ClientRenderTarget::Queue(queue) => queue.try_send_render(data),
            #[cfg(test)]
            ClientRenderTarget::Channel(sender) => sender.try_send(data),
        }
    }
}

#[derive(Debug)]
struct ClientWriterQueue {
    state: Mutex<ClientWriterQueueState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct ClientWriterQueueState {
    control: VecDeque<Vec<u8>>,
    render: Option<Vec<u8>>,
    senders: usize,
    writer_alive: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum ClientWriteItem {
    Control(Vec<u8>),
    Render(Vec<u8>),
}

impl ClientWriterQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClientWriterQueueState {
                writer_alive: true,
                ..ClientWriterQueueState::default()
            }),
            ready: Condvar::new(),
        })
    }

    fn add_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_add(1);
    }

    fn remove_sender(&self) {
        let mut state = self.lock_state();
        state.senders = state.senders.saturating_sub(1);
        self.ready.notify_one();
    }

    fn send_control(&self, data: Vec<u8>) -> Result<(), SendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(SendError(data));
        }
        state.control.push_back(data);
        self.ready.notify_one();
        Ok(())
    }

    fn try_send_render(&self, data: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        let mut state = self.lock_state();
        if !state.writer_alive {
            return Err(TrySendError::Disconnected(data));
        }
        if state.render.is_some() {
            return Err(TrySendError::Full(data));
        }
        state.render = Some(data);
        self.ready.notify_one();
        Ok(())
    }

    fn recv(&self) -> Option<ClientWriteItem> {
        let mut state = self.lock_state();
        loop {
            if let Some(data) = state.control.pop_front() {
                return Some(ClientWriteItem::Control(data));
            }
            if let Some(data) = state.render.take() {
                self.ready.notify_one();
                return Some(ClientWriteItem::Render(data));
            }
            if state.senders == 0 {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn close_writer(&self) {
        let mut state = self.lock_state();
        state.writer_alive = false;
        self.ready.notify_all();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ClientWriterQueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
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
        keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
        direct_attach_requested: bool,
        writer: ClientWriter,
    },
    /// A client sent an input message.
    ClientInput { client_id: u64, data: Vec<u8> },
    /// A client sent structured input events.
    ClientInputEvents {
        client_id: u64,
        events: Vec<crate::protocol::ClientInputEvent>,
    },
    /// A client sent local clipboard image bytes to paste into a remote pane.
    ClientClipboardImage {
        client_id: u64,
        extension: String,
        data: Vec<u8>,
    },
    /// A client requested direct attach to one terminal.
    ClientAttachTerminal {
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    },
    /// A client requested read-only observation of one terminal.
    ClientObserveTerminal { client_id: u64, target: String },
    /// A client requested writable control of one terminal.
    ClientControlTerminal {
        client_id: u64,
        target: String,
        takeover: bool,
    },
    /// A direct terminal attach client requested scrollback movement.
    ClientAttachScroll {
        client_id: u64,
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
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

fn parse_client_keybindings(
    keybindings: ClientKeybindings,
) -> Result<Option<Box<crate::config::LiveKeybindConfig>>, String> {
    match keybindings {
        ClientKeybindings::Server => Ok(None),
        ClientKeybindings::Local { keys_toml } => {
            let mut config = toml::from_str::<crate::config::Config>(&keys_toml)
                .map_err(|err| format!("invalid client keybindings: {err}"))?;
            config.keys.command.clear();
            Ok(Some(Box::new(crate::config::LiveKeybindConfig {
                prefix: config.prefix_key(),
                keybinds: config.keybinds(),
            })))
        }
    }
}

fn input_events_within_limits(events: &[ClientInputEvent]) -> bool {
    if events.len() > MAX_INPUT_EVENT_BATCH {
        return false;
    }

    let mut paste_bytes = 0usize;
    for event in events {
        if let ClientInputEvent::Paste { text } = event {
            paste_bytes = paste_bytes.saturating_add(text.len());
            if paste_bytes > MAX_INPUT_PAYLOAD {
                return false;
            }
        }
    }

    true
}

#[cfg(windows)]
fn set_client_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    context: &'static str,
    client_id: u64,
) -> io::Result<()> {
    match stream.set_recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            debug!(client_id, err = %err, context, "client socket receive timeout unavailable");
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[cfg(not(windows))]
fn set_client_recv_timeout(
    stream: &LocalStream,
    timeout: Option<Duration>,
    _context: &'static str,
    _client_id: u64,
) -> io::Result<()> {
    stream.set_recv_timeout(timeout)
}

/// Handles the client handshake on a blocking thread.
///
/// Reads the `Hello` message, validates the version, sends `Welcome`,
/// and then enters a read loop forwarding messages to the server event channel.
pub(crate) fn handle_client_handshake(
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    // Reset to blocking mode — the accept loop sets nonblocking but
    // the handshake thread needs blocking I/O for read_message/write_message.
    stream.set_nonblocking(false)?;

    set_client_recv_timeout(
        &stream,
        Some(HANDSHAKE_TIMEOUT),
        "client handshake read timeout unavailable",
        client_id,
    )?;

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

    let (
        client_cols,
        client_rows,
        cell_width_px,
        cell_height_px,
        render_encoding,
        keybindings,
        direct_attach_requested,
    ) = match hello {
        ClientMessage::Hello {
            version,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            requested_encoding,
            keybindings,
            launch_mode,
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

            let keybindings = match parse_client_keybindings(keybindings) {
                Ok(keybindings) => keybindings,
                Err(error) => {
                    let welcome = ServerMessage::Welcome {
                        version: PROTOCOL_VERSION,
                        encoding: RenderEncoding::SemanticFrame,
                        error: Some(error),
                    };
                    let _ = protocol::write_message(&mut stream, &welcome);
                    return Ok(());
                }
            };

            // Clamp size.
            let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
            (
                clamped_cols,
                clamped_rows,
                cell_width_px,
                cell_height_px,
                requested_encoding,
                keybindings,
                launch_mode == ClientLaunchMode::TerminalAttach,
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

    set_client_recv_timeout(
        &stream,
        None,
        "failed to clear client handshake read timeout",
        client_id,
    )?;

    // Create separate channels for reliable control messages and droppable renders.
    let writer_queue = ClientWriterQueue::new();
    let writer = ClientWriter {
        control: ClientControlWriter::queue(writer_queue.clone()),
        render: ClientRenderWriter::queue(writer_queue.clone()),
    };

    // Spawn a writer thread that forwards messages from the channels to the stream.
    let write_stream = stream.try_clone()?;
    let writer_event_tx = server_event_tx.clone();
    std::thread::spawn(move || {
        client_writer_loop(write_stream, client_id, writer_queue, writer_event_tx);
    });

    // Notify the main loop about the new client.
    let _ = server_event_tx.blocking_send(ServerEvent::ClientConnected {
        client_id,
        cols: client_cols,
        rows: client_rows,
        cell_width_px,
        cell_height_px,
        render_encoding,
        keybindings,
        direct_attach_requested,
        writer,
    });

    // Enter read loop — read client messages and forward to main loop.
    client_read_loop(stream, client_id, server_event_tx, should_quit)
}

/// The client writer loop — prioritizes control messages over render frames.
fn client_writer_loop(
    mut stream: LocalStream,
    client_id: u64,
    writer_queue: Arc<ClientWriterQueue>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    while let Some(item) = writer_queue.recv() {
        match item {
            ClientWriteItem::Control(data) => {
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
            }
            ClientWriteItem::Render(data) => {
                let _ =
                    server_event_tx.blocking_send(ServerEvent::ClientWriterDrained { client_id });
                if !write_framed_bytes(&mut stream, &data) {
                    break;
                }
            }
        }
    }
    writer_queue.close_writer();
    debug!("client writer thread exiting");
}

fn write_framed_bytes(stream: &mut LocalStream, data: &[u8]) -> bool {
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
    mut stream: LocalStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    should_quit: &Arc<AtomicBool>,
) -> io::Result<()> {
    while !should_quit.load(Ordering::Acquire) {
        let msg: ClientMessage = match protocol::read_message(&mut stream, MAX_GRAPHICS_FRAME_SIZE)
        {
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
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                } else {
                    ServerEvent::ClientInput { client_id, data }
                }
            }
            ClientMessage::InputEvents { events } => {
                if !input_events_within_limits(&events) {
                    warn!(
                        client_id,
                        count = events.len(),
                        "oversized input events from client, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                } else {
                    ServerEvent::ClientInputEvents { client_id, events }
                }
            }
            ClientMessage::ObserveTerminal { target } => {
                ServerEvent::ClientObserveTerminal { client_id, target }
            }
            ClientMessage::ControlTerminal { target, takeover } => {
                ServerEvent::ClientControlTerminal {
                    client_id,
                    target,
                    takeover,
                }
            }
            ClientMessage::ClipboardImage { extension, data } => {
                if data.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                    warn!(
                        client_id,
                        size = data.len(),
                        "oversized clipboard image from client, closing"
                    );
                    let _ = server_event_tx
                        .blocking_send(ServerEvent::ClientDisconnected { client_id });
                    break;
                } else {
                    ServerEvent::ClientClipboardImage {
                        client_id,
                        extension,
                        data,
                    }
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
            ClientMessage::AttachScroll {
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            } => ServerEvent::ClientAttachScroll {
                client_id,
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
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
    use interprocess::local_socket::traits::Listener as _;
    use std::path::PathBuf;

    struct TestSocketPath(PathBuf);

    impl Drop for TestSocketPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn unique_test_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let filename = format!("h{}-{nanos}.sock", std::process::id());
        #[cfg(unix)]
        {
            let _ = name;
            PathBuf::from("/tmp").join(filename)
        }
        #[cfg(windows)]
        {
            std::env::temp_dir().join(format!("herdr-{name}-{filename}"))
        }
    }

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, TestSocketPath) {
        let path = unique_test_path(name);
        let _ = std::fs::remove_file(&path);
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, TestSocketPath(path))
    }

    fn test_queue_writer() -> (ClientWriter, Arc<ClientWriterQueue>) {
        let queue = ClientWriterQueue::new();
        (
            ClientWriter {
                control: ClientControlWriter::queue(queue.clone()),
                render: ClientRenderWriter::queue(queue.clone()),
            },
            queue,
        )
    }

    fn frame_server_message(message: &ServerMessage) -> Vec<u8> {
        let mut bytes = Vec::new();
        protocol::write_message(&mut bytes, message).expect("frame server message");
        bytes
    }

    #[test]
    fn client_writer_queue_keeps_render_slot_bounded() {
        let (writer, _queue) = test_queue_writer();
        let first = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("first".into()),
        });
        let second = frame_server_message(&ServerMessage::WindowTitle {
            title: Some("second".into()),
        });

        writer.render.try_send(first).expect("first render fits");
        assert!(matches!(
            writer.render.try_send(second),
            Err(TrySendError::Full(_))
        ));
    }

    #[test]
    fn client_writer_prioritizes_control_and_reports_render_drain() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-writer-priority");
        let (writer, queue) = test_queue_writer();
        writer
            .render
            .try_send(frame_server_message(&ServerMessage::WindowTitle {
                title: Some("render".into()),
            }))
            .expect("queue render");
        writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("queue control");

        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let handle = std::thread::spawn(move || {
            client_writer_loop(server_stream, 9, queue, server_event_tx);
        });

        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read control") {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected control message first, got {other:?}"),
        }
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE).expect("read render") {
            ServerMessage::WindowTitle { title } => assert_eq!(title.as_deref(), Some("render")),
            other => panic!("expected render message second, got {other:?}"),
        }
        match server_event_rx
            .blocking_recv()
            .expect("writer drained render slot")
        {
            ServerEvent::ClientWriterDrained { client_id } => assert_eq!(client_id, 9),
            other => panic!("expected writer drained event, got {other:?}"),
        }

        drop(writer);
        handle.join().expect("writer exits after senders drop");
    }

    #[test]
    fn client_writer_exits_when_all_writer_handles_drop() {
        let (_client_stream, server_stream, _path) = local_stream_pair("client-writer-drop");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 11, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits without polling after senders drop");
    }

    #[test]
    fn client_writer_clone_keeps_loop_alive_until_final_drop() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-writer-clone-drop");
        let (writer, queue) = test_queue_writer();
        let cloned_writer = writer.clone();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 12, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(writer);
        cloned_writer
            .control
            .send(frame_server_message(&ServerMessage::ReloadSoundConfig))
            .expect("cloned writer still sends after original drops");
        match protocol::read_message(&mut client_stream, MAX_FRAME_SIZE)
            .expect("read control from cloned writer")
        {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected cloned control message, got {other:?}"),
        }
        assert!(
            done_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "writer exited while cloned handles were still alive"
        );

        drop(cloned_writer);
        done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("writer exits after final cloned writer drops");
    }

    #[test]
    fn client_writer_closes_queue_after_socket_write_failure() {
        let (client_stream, server_stream, _path) =
            local_stream_pair("client-writer-socket-failure");
        #[cfg(not(windows))]
        server_stream
            .set_send_timeout(Some(Duration::from_millis(100)))
            .expect("set test send timeout");
        let (writer, queue) = test_queue_writer();
        let (server_event_tx, _server_event_rx) = mpsc::channel(4);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            client_writer_loop(server_stream, 13, queue, server_event_tx);
            let _ = done_tx.send(());
        });

        drop(client_stream);
        writer
            .control
            .send(vec![b'x'; 1024 * 1024])
            .expect("message is accepted before the writer observes socket failure");
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer exits after socket write failure");

        assert!(matches!(writer.control.send(vec![b'y']), Err(SendError(_))));
        assert!(matches!(
            writer.render.try_send(vec![b'z']),
            Err(TrySendError::Disconnected(_))
        ));
    }

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

    #[test]
    fn parse_client_keybindings_accepts_local_profile() {
        let keybindings = parse_client_keybindings(ClientKeybindings::Local {
            keys_toml: r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"

[[keys.command]]
key = "prefix+g"
command = "lazygit"
"#
            .to_owned(),
        })
        .expect("valid client keybindings")
        .expect("local profile");

        assert_eq!(keybindings.prefix.0, crossterm::event::KeyCode::Char('a'));
        assert!(keybindings
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+t"));
        assert!(keybindings.keybinds.custom_commands.is_empty());
    }

    #[test]
    fn parse_client_keybindings_tolerates_disabled_bindings() {
        let keybindings = parse_client_keybindings(ClientKeybindings::Local {
            keys_toml: r#"
[keys]
new_tab = "ctrl+notakey"
"#
            .to_owned(),
        })
        .expect("diagnostic-only client keybindings should be accepted")
        .expect("local profile");

        assert!(keybindings.keybinds.new_tab.bindings.is_empty());
        assert!(keybindings
            .keybinds
            .next_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
    }

    #[test]
    fn handshake_negotiates_terminal_ansi_encoding() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-handshake-ansi");
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
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::App,
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
                keybindings,
                direct_attach_requested,
                writer,
            } => {
                assert_eq!(client_id, 42);
                assert_eq!((cols, rows), (100, 30));
                assert_eq!((cell_width_px, cell_height_px), (8, 16));
                assert_eq!(render_encoding, RenderEncoding::TerminalAnsi);
                assert!(keybindings.is_none());
                assert!(!direct_attach_requested);
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

    #[test]
    fn handshake_marks_terminal_attach_launch_mode() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-handshake-terminal-attach");
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
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::TerminalAttach,
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
                direct_attach_requested,
                writer,
                ..
            } => {
                assert!(direct_attach_requested);
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

    #[test]
    fn client_read_loop_rejects_oversized_input() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-oversized");
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
    fn client_read_loop_forwards_input_events() {
        let (mut client_stream, server_stream, _path) = local_stream_pair("client-read-events");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });
        let events = vec![
            ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            },
            ClientInputEvent::FocusGained,
        ];

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: events.clone(),
            },
        )
        .expect("write input events");

        match server_event_rx
            .blocking_recv()
            .expect("client input events event")
        {
            ServerEvent::ClientInputEvents {
                client_id,
                events: actual,
            } => {
                assert_eq!(client_id, 7);
                assert_eq!(actual, events);
            }
            other => panic!("expected ClientInputEvents, got {other:?}"),
        }

        drop(client_stream);
        should_quit.store(true, Ordering::Release);
        handle
            .join()
            .expect("read thread join")
            .expect("read thread result");
    }

    #[test]
    fn client_read_loop_rejects_oversized_input_event_batch() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-oversized-events");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: vec![ClientInputEvent::FocusGained; MAX_INPUT_EVENT_BATCH + 1],
            },
        )
        .expect("write oversized input events");

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
    fn client_read_loop_rejects_oversized_input_event_paste() {
        let (mut client_stream, server_stream, _path) =
            local_stream_pair("client-read-oversized-paste");
        let (server_event_tx, mut server_event_rx) = mpsc::channel(4);
        let should_quit = Arc::new(AtomicBool::new(false));
        let read_quit = should_quit.clone();
        let handle = std::thread::spawn(move || {
            client_read_loop(server_stream, 7, &server_event_tx, &read_quit)
        });

        protocol::write_message(
            &mut client_stream,
            &ClientMessage::InputEvents {
                events: vec![ClientInputEvent::Paste {
                    text: "x".repeat(MAX_INPUT_PAYLOAD + 1),
                }],
            },
        )
        .expect("write oversized paste event");

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
