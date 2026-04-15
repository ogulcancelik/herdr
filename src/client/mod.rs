//! Thin client mode — connects to the server's client socket.
//!
//! The client:
//! - Connects to `herdr-client.sock`, sends Hello with terminal size and protocol version
//! - Sets up the real terminal (raw mode, mouse capture, keyboard enhancements)
//! - Receives Frame messages and blits them to the terminal (diff against last frame)
//! - Reads stdin events (keystrokes, mouse, paste) and sends them as ClientMessage::Input
//! - Detects terminal resize and sends ClientMessage::Resize
//! - Restores terminal on exit (normal or error)
//! - Handles ServerShutdown gracefully (clean exit, informative message to stderr)
//! - Handles server unreachable (clear error screen, not blank/hang)
//! - Forwards OSC 52 clipboard writes from server to its own stdout
//! - Displays sound/toast notifications forwarded from server

mod blit;
mod input;

use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use tracing::{debug, info, warn};

use crate::server::headless::client_socket_path;
use crate::server::protocol::{
    self, ClientMessage, FrameData, ServerMessage, MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

/// State tracking for the thin client.
struct ClientState {
    /// The last frame we rendered, used for diff-based blitting.
    last_frame: Option<FrameData>,
    /// The terminal size we reported to the server in our last Hello/Resize.
    reported_size: (u16, u16),
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "failed to connect to server: {err}")?;
                let path = client_socket_path();
                write!(
                    f,
                    "\nIs herdr server running? Start it with `herdr server`."
                )?;
                write!(f, "\nSocket path: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(f, "server rejected handshake (version {version}): {error}")
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        write!(f, "detached from server")?;
                        write!(f, "\nRun `herdr` to reattach")?;
                    }
                    _ => {
                        write!(f, "server shut down")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                write!(f, "lost connection to server: {err}")
            }
            ClientError::Protocol(err) => {
                write!(f, "protocol error: {err}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        ClientError::Protocol(err)
    }
}

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
fn setup_terminal() -> io::Result<TerminalGuard> {
    ratatui::init();
    execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    )?;

    // tmux doesn't understand kitty keyboard protocol push.
    // Enable modifyOtherKeys mode 2 for tmux.
    let in_tmux = std::env::var("TMUX").is_ok();
    if in_tmux {
        io::stdout().write_all(b"\x1b[>4;2m")?;
        io::stdout().flush()?;
    }

    Ok(TerminalGuard { in_tmux })
}

/// Guard that restores the terminal when dropped.
struct TerminalGuard {
    in_tmux: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Reset modifyOtherKeys if we enabled it.
        if self.in_tmux {
            let _ = io::stdout().write_all(b"\x1b[>4;0m");
            let _ = io::stdout().flush();
        }

        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        ratatui::restore();
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// Performs the client→server handshake.
///
/// Sends Hello with the terminal size and protocol version, reads the Welcome
/// response. Returns Ok(()) on success, or an error if the server rejects us.
fn do_handshake(stream: &mut UnixStream, cols: u16, rows: u16) -> Result<(), ClientError> {
    // Send Hello.
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols,
        rows,
    };
    protocol::write_message(stream, &hello).map_err(|e| {
        ClientError::ConnectionFailed(io::Error::new(io::ErrorKind::Other, e.to_string()))
    })?;

    // Read Welcome.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(ClientError::ConnectionFailed)?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    stream
        .set_read_timeout(None)
        .map_err(ClientError::ConnectionFailed)?;

    match welcome {
        ServerMessage::Welcome { version, error } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, "handshake succeeded");
            Ok(())
        }
        _ => Err(ClientError::Protocol(protocol::FramingError::Io(
            io::Error::new(io::ErrorKind::InvalidData, "expected Welcome message"),
        ))),
    }
}

// ---------------------------------------------------------------------------
// Client event loop
// ---------------------------------------------------------------------------

/// Internal events for the client event loop.
enum ClientLoopEvent {
    /// Raw input bytes from stdin.
    StdinInput(Vec<u8>),
    /// Terminal resize detected.
    Resize(u16, u16),
    /// Server message received.
    ServerMessage(ServerMessage),
    /// Server reader thread exited (connection lost).
    ServerDisconnected,
    /// Timer tick.
    Timer,
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    init_logging();

    let socket_path = client_socket_path();
    info!(path = %socket_path.display(), "connecting to server");

    // Try to connect to the server.
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(err) => {
            // Server unreachable — show clear error and exit.
            let client_err = ClientError::ConnectionFailed(err);
            eprintln!("herdr: {client_err}");
            std::process::exit(1);
        }
    };

    // Get the terminal size before handshake (before raw mode).
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // Perform handshake while the stream is still in blocking mode.
    if let Err(err) = do_handshake(&mut stream, cols, rows) {
        eprintln!("herdr: {err}");
        std::process::exit(1);
    }

    // Now set up the terminal (raw mode, mouse, keyboard enhancements).
    // This must happen AFTER the handshake succeeds, so we don't leave
    // the terminal in raw mode if the server rejects us.
    let _guard = setup_terminal().map_err(|err| {
        eprintln!("herdr: failed to set up terminal: {err}");
        err
    })?;

    // Install a panic hook to restore the terminal on panic (same as monolithic).
    let in_tmux = std::env::var("TMUX").is_ok();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if in_tmux {
            let _ = io::stdout().write_all(b"\x1b[>4;0m");
        }
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        ratatui::restore();
        original_hook(info);
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let should_quit = Arc::new(AtomicBool::new(false));

    // Install Ctrl+C handler.
    let quit_flag = should_quit.clone();
    let _ = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    });

    let result = rt.block_on(async { run_client_loop(stream, cols, rows, should_quit).await });

    // Restore the terminal before printing any final status message.
    drop(_guard);

    if let Err(err) = result {
        eprintln!("herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        info!("herdr client exiting");

        if matches!(
            err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached"
        ) {
            return Ok(());
        }

        std::process::exit(1);
    }

    rt.shutdown_timeout(Duration::from_millis(100));
    info!("herdr client exiting");
    Ok(())
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    stream: UnixStream,
    cols: u16,
    rows: u16,
    should_quit: Arc<AtomicBool>,
) -> Result<(), ClientError> {
    let mut state = ClientState {
        last_frame: None,
        reported_size: (cols, rows),
    };

    // Channel for events from the stdin, resize, and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);

    // Spawn the stdin reader thread.
    let stdin_quit = should_quit.clone();
    let stdin_tx = event_tx.clone();
    std::thread::spawn(move || {
        input::stdin_reader_loop(stdin_tx, &stdin_quit);
    });

    query_host_terminal_theme();

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    std::thread::spawn(move || {
        resize_poll_loop(resize_tx, cols, rows, &resize_quit);
    });

    // Spawn the server reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    let server_read_quit = should_quit.clone();
    let server_read_tx = event_tx.clone();
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    std::thread::spawn(move || {
        server_reader_thread(read_stream, server_read_tx, &server_read_quit);
    });

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut write_stream = stream;
    write_stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // Main event loop.
    while !should_quit.load(Ordering::Acquire) {
        let event = tokio::select! {
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            _ = tokio::time::sleep(Duration::from_millis(100)) => ClientLoopEvent::Timer,
        };

        match event {
            ClientLoopEvent::StdinInput(data) => {
                let msg = ClientMessage::Input { data };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::Resize(new_cols, new_rows) => {
                state.reported_size = (new_cols, new_rows);
                let msg = ClientMessage::Resize {
                    cols: new_cols,
                    rows: new_rows,
                };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::ServerMessage(msg) => match msg {
                ServerMessage::Frame(frame_data) => {
                    blit::blit_frame(&frame_data, state.last_frame.as_ref());
                    state.last_frame = Some(frame_data);
                    let _ = io::stdout().flush();
                }
                ServerMessage::ServerShutdown { reason } => {
                    return Err(ClientError::ServerShutdown { reason });
                }
                ServerMessage::Notify { .. } => {}
                ServerMessage::Clipboard { data } => {
                    forward_clipboard(&data);
                    let _ = io::stdout().flush();
                }
                ServerMessage::Welcome { .. } => {
                    debug!("received unexpected Welcome in main loop");
                }
            },
            ClientLoopEvent::ServerDisconnected => {
                return Err(ClientError::ConnectionLost(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "server closed connection",
                )));
            }
            ClientLoopEvent::Timer => {
                // Check if we should quit.
            }
        }
    }

    // Clean exit (Ctrl+C). Send Detach before closing.
    let detach = ClientMessage::Detach;
    let _ = write_to_server(&mut write_stream, &detach);
    let _ = io::stdout().flush();

    Ok(())
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    mut stream: UnixStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, MAX_FRAME_SIZE) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage(msg))
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // Should not happen with blocking mode, but handle gracefully
                // in case the stream was set nonblocking by another clone.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write helper
// ---------------------------------------------------------------------------

/// Writes a message to the server stream (blocking).
fn write_to_server(stream: &mut UnixStream, msg: &ClientMessage) -> io::Result<()> {
    protocol::write_message(stream, msg)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
}

// ---------------------------------------------------------------------------
// Clipboard forwarding
// ---------------------------------------------------------------------------

/// Decode a clipboard payload forwarded by the server.
fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Forwards a clipboard write from the server to the local client clipboard.
fn forward_clipboard(data: &str) {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return;
    };

    crate::selection::write_osc52_bytes(&bytes);
}

// ---------------------------------------------------------------------------
// Resize polling
// ---------------------------------------------------------------------------

/// Polls the terminal size and sends resize events when it changes.
fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    should_quit: &Arc<AtomicBool>,
) {
    let mut last_size = (initial_cols, initial_rows);
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let new_size = crossterm::terminal::size().unwrap_or(last_size);
        if new_size != last_size {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(new_size.0, new_size.1))
                .is_err()
            {
                break; // Main loop gone.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Initialize logging for the client process.
fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

fn write_host_terminal_theme_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

fn init_logging() {
    use std::fs::{self, OpenOptions};
    use tracing_subscriber::EnvFilter;

    let log_dir = crate::config::config_dir();
    let _ = fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("herdr-client.log");

    // Rotate: truncate if over 5MB.
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > 5 * 1024 * 1024 {
            let _ = fs::remove_file(&log_path);
        }
    }

    let file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(f) => f,
        Err(_) => return,
    };

    let filter =
        EnvFilter::try_from_env("HERDR_LOG").unwrap_or_else(|_| EnvFilter::new("herdr=info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .with_target(false)
        .try_init();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_host_terminal_theme_query_emits_osc_queries() {
        let mut output = Vec::new();
        write_host_terminal_theme_query(&mut output).unwrap();
        assert_eq!(
            output,
            crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes()
        );
    }

    #[test]
    fn client_error_display_connection_failed() {
        let err = ClientError::ConnectionFailed(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("failed to connect to server"),
            "should mention connection failure: {msg}"
        );
        assert!(
            msg.contains("herdr server"),
            "should suggest starting server: {msg}"
        );
    }

    #[test]
    fn client_error_display_handshake_rejected() {
        let err = ClientError::HandshakeRejected {
            version: 1,
            error: "incompatible".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("rejected handshake"),
            "should mention rejection: {msg}"
        );
        assert!(msg.contains("incompatible"), "should include error: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown() {
        let err = ClientError::ServerShutdown {
            reason: Some("maintenance".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
        assert!(msg.contains("maintenance"), "should include reason: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown_no_reason() {
        let err = ClientError::ServerShutdown { reason: None };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
    }

    #[test]
    fn client_error_display_connection_lost() {
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to server"),
            "should mention lost connection: {msg}"
        );
    }

    #[test]
    fn decode_clipboard_payload_decodes_base64() {
        assert_eq!(decode_clipboard_payload("dGVzdA=="), Some(b"test".to_vec()));
    }

    #[test]
    fn decode_clipboard_payload_rejects_invalid_base64() {
        assert_eq!(decode_clipboard_payload("not-base64!!!"), None);
    }

    #[test]
    fn forward_clipboard_uses_local_clipboard_path() {
        unsafe {
            std::env::set_var("SSH_CONNECTION", "1 2 3 4");
        }
        forward_clipboard("dGVzdA==");
        unsafe {
            std::env::remove_var("SSH_CONNECTION");
        }
    }
}
