//! Headless server mode — runs the herdr event loop without a real terminal.
//!
//! The server:
//! - Does not enter raw mode or read stdin
//! - Creates and listens on both `herdr.sock` (existing JSON API) and
//!   `herdr-client.sock` (new binary protocol)
//! - Initializes AppState and all PTYs from session restore or fresh state
//! - Runs the main event loop (drain events, drain API requests, scheduled tasks)
//! - Renders to a virtual ratatui Buffer in memory
//! - Accepts client connections on the client socket
//! - Streams frames to connected clients after each render
//! - Routes client input events through the existing input pipeline
//! - Continues running after client disconnect
//! - Handles stale socket cleanup, explicit server stop, minimum terminal size,
//!   and pane spawn failure during restore

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use base64::Engine;

use crate::api;
use crate::app;
use crate::app::state::AppState;
use crate::app::Mode;
use crate::config;
use crate::detect::AgentState;
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::server::protocol::{
    self, ClientMessage, CursorState, FrameData, ServerMessage, MAX_FRAME_SIZE, PROTOCOL_VERSION,
};

// ---------------------------------------------------------------------------
// Loop event enum for the headless server event loop
// ---------------------------------------------------------------------------

/// Events that the headless server event loop can process.
enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(api::ApiRequestMessage),
    ServerEvent(ServerEvent),
    RenderRequested,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default shared runtime size (columns, rows) when no clients are attached.
const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;

/// Minimum accepted attached client size.
///
/// Narrow observers must be allowed to drive narrow renders, otherwise the
/// server wraps pane content against a wider width and the client sees the
/// right edge clipped.
const MIN_CLIENT_COLS: u16 = 1;
const MIN_CLIENT_ROWS: u16 = 1;

/// Legacy environment variable for overriding the client socket path.
///
/// Contractual override behavior for auto-detect uses `HERDR_SOCKET_PATH`.
/// This variable is kept as a fallback for callers that explicitly need a
/// client-only override when `HERDR_SOCKET_PATH` is not set.
const CLIENT_SOCKET_PATH_ENV_VAR: &str = "HERDR_CLIENT_SOCKET_PATH";

/// Socket permission mode (owner read/write only).
const SOCKET_PERMISSION_MODE: u32 = 0o600;

/// Timeout for in-flight API requests during shutdown.
#[allow(dead_code)]
const SHUTDOWN_API_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a client handshake before closing the connection.
/// Set to 4 seconds (rather than 5) to guarantee the connection is closed
/// within the 5-second deadline, even with
/// OS timer slack, thread scheduling, and cleanup overhead.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// Maximum input payload size (bytes) for a single `ClientMessage::Input`.
const MAX_INPUT_PAYLOAD: usize = 1024 * 1024; // 1 MB

/// How often the idle headless loop wakes to poll the std UnixListener for new
/// client connections.
///
/// The listener is non-blocking and not integrated into `tokio::select!`, so
/// a low-frequency wake is required to notice new thin-client attaches while
/// otherwise idle. Keep this much slower than the old resize-poll cadence to
/// avoid reintroducing the idle CPU spin.
const CLIENT_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

fn should_forward_toast_to_clients(delivery: config::ToastDelivery) -> bool {
    matches!(delivery, config::ToastDelivery::Terminal)
}

fn toast_event_text(kind: app::state::ToastKind) -> &'static str {
    match kind {
        app::state::ToastKind::NeedsAttention => "needs attention",
        app::state::ToastKind::Finished => "finished",
        app::state::ToastKind::UpdateInstalled => "updated",
    }
}

fn toast_message_from_state_change(
    state: &AppState,
    pane_id: PaneId,
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<String> {
    let kind = app::actions::notification_toast_for_state_change(
        suppress_active_tab_notifications,
        prev_state,
        new_state,
    )?;

    state
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(ws_idx, ws)| {
            ws.tabs.iter().find_map(|tab| {
                let pane = tab.panes.get(&pane_id)?;
                let agent_label = pane.effective_agent_label()?;
                Some(format!(
                    "{} {}: {}",
                    agent_label,
                    toast_event_text(kind),
                    app::actions::notification_context(ws, ws_idx, pane_id)
                ))
            })
        })
}

// ---------------------------------------------------------------------------
// Socket path helpers
// ---------------------------------------------------------------------------

/// Returns the path for the client protocol socket.
///
/// Contract-aligned override behavior:
/// 1. If CLI `--session <name>` is active, use that session's client socket.
/// 2. If `HERDR_SOCKET_PATH` is set, derive the client socket path from it by
///    inserting `-client` before `.sock` (e.g. `herdr.sock` -> `herdr-client.sock`).
///    This keeps JSON API and client socket overrides consistent.
/// 3. Otherwise, honor `HERDR_CLIENT_SOCKET_PATH` (legacy/testing fallback).
/// 4. Otherwise, use the active session data directory.
pub fn client_socket_path() -> PathBuf {
    if crate::session::explicit_session_requested() {
        return crate::session::client_socket_path_for(crate::session::active_name().as_deref());
    }
    client_socket_path_from_overrides(
        std::env::var(api::SOCKET_PATH_ENV_VAR).ok().as_deref(),
        std::env::var(CLIENT_SOCKET_PATH_ENV_VAR).ok().as_deref(),
    )
}

fn client_socket_path_from_overrides(
    api_socket_override: Option<&str>,
    client_socket_override: Option<&str>,
) -> PathBuf {
    if let Some(api_socket_override) = api_socket_override {
        return derive_client_socket_from_api_socket(Path::new(api_socket_override));
    }

    if let Some(client_socket_override) = client_socket_override {
        return PathBuf::from(client_socket_override);
    }

    crate::session::client_socket_path_for(crate::session::active_name().as_deref())
}

fn derive_client_socket_from_api_socket(api_socket_path: &Path) -> PathBuf {
    let stem = api_socket_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("herdr");
    let parent = api_socket_path.parent().unwrap_or_else(|| Path::new(""));

    if api_socket_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "sock")
    {
        return parent.join(format!("{stem}-client.sock"));
    }

    parent.join(format!("{stem}-client.sock"))
}

/// Clamp client-reported terminal dimensions to a minimum viable size.
pub fn clamp_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    let clamped_cols = cols.max(MIN_CLIENT_COLS);
    let clamped_rows = rows.max(MIN_CLIENT_ROWS);
    (clamped_cols, clamped_rows)
}

// ---------------------------------------------------------------------------
// Connected client state
// ---------------------------------------------------------------------------

/// A connected client tracked by the server.
struct ClientConnection {
    /// The client's terminal size (after clamping).
    terminal_size: (u16, u16),
    /// Last known host terminal default colors for this client.
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    /// Last reported focus state for this client's outer terminal.
    outer_terminal_focus: Option<bool>,
    /// Monotonic activity stamp used to choose the fallback foreground client.
    last_activity: u64,
    /// Last frame sent to this client. Used to skip identical frame sends.
    last_frame: Option<FrameData>,
    /// Channel for sending framed ServerMessage data to the client writer thread.
    writer: Option<std::sync::mpsc::Sender<Vec<u8>>>,
}

// ---------------------------------------------------------------------------
// Stale socket cleanup
// ---------------------------------------------------------------------------

/// Prepares a socket path for binding: creates parent directories,
/// removes stale socket files (where no server is listening), and
/// returns an error if a live server is already bound.
fn prepare_socket_path(path: &Path) -> io::Result<()> {
    crate::ipc::prepare_socket_path(path, |path| {
        format!(
            "herdr server is already running (socket busy at {})",
            path.display()
        )
    })
}

/// Restricts socket file permissions to owner-only (0o600).
fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    crate::ipc::restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
}

// ---------------------------------------------------------------------------
// Virtual rendering
// ---------------------------------------------------------------------------

struct CursorTrackingBackend {
    inner: TestBackend,
    rendered_cursor: Option<Position>,
}

impl CursorTrackingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            rendered_cursor: None,
        }
    }

    fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.inner.buffer()
    }

    fn rendered_cursor(&self) -> Option<CursorState> {
        self.rendered_cursor.map(|pos| CursorState {
            x: pos.x,
            y: pos.y,
            visible: true,
        })
    }
}

impl Backend for CursorTrackingBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()?;
        self.rendered_cursor = None;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.rendered_cursor = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Renders the AppState to an in-memory ratatui Buffer.
///
/// This produces the same output as the monolithic binary's terminal draw,
/// but writes to a `Buffer` instead of stdout. Cursor visibility is captured
/// from explicit frame cursor intent rather than incidental backend state.
fn render_virtual(
    app_state: &mut AppState,
    area: Rect,
    resize_panes: bool,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    if resize_panes {
        crate::ui::compute_view(app_state, area);
    } else {
        crate::ui::compute_view_without_resizing_panes(app_state, area);
    }

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            crate::ui::render(app_state, frame);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor =
        focused_terminal_cursor(app_state).or_else(|| terminal.backend().rendered_cursor());

    (buffer, cursor)
}

fn focused_terminal_cursor(app_state: &AppState) -> Option<CursorState> {
    if app_state.mode != Mode::Terminal {
        return None;
    }

    let ws_idx = app_state.active?;
    let ws = app_state.workspaces.get(ws_idx)?;
    let info = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)?;
    let rt = ws.runtimes.get(&info.id)?;
    let cursor = rt.cursor_state(info.inner_rect, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible,
    })
}

// ---------------------------------------------------------------------------
// Headless server
// ---------------------------------------------------------------------------

/// The headless server — runs the herdr event loop without a real terminal.
pub struct HeadlessServer {
    app: app::App,
    client_listener: UnixListener,
    client_socket_path: PathBuf,
    clients: HashMap<u64, ClientConnection>,
    next_client_id: u64,
    /// The client currently driving the shared pane runtime size and theme.
    foreground_client_id: Option<u64>,
    /// Monotonic activity counter used to pick the most recently active client.
    next_activity_stamp: u64,
    /// Shared pane runtime size derived from the foreground client,
    /// or MIN_COLS × MIN_ROWS when no clients are connected.
    effective_size: (u16, u16),
    /// Flag set when shutdown is initiated.
    shutting_down: bool,
    /// Flag set by Ctrl+C or `server stop` signal.
    should_quit: Arc<AtomicBool>,
    /// Channel for receiving server events from client connection threads.
    server_event_rx: mpsc::Receiver<ServerEvent>,
    /// Sender for server events (cloned for each client thread).
    server_event_tx: mpsc::Sender<ServerEvent>,
}

impl HeadlessServer {
    /// Creates and starts the headless server.
    ///
    /// This:
    /// 1. Prepares the client socket path (cleans up stale sockets)
    /// 2. Binds the client socket listener
    /// 3. Returns the server ready to run
    pub fn new(app: app::App) -> io::Result<Self> {
        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;

        let listener = UnixListener::bind(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        info!(path = %client_path.display(), "client protocol socket listening");

        // Set non-blocking on the listener so we can poll it from the event loop.
        listener.set_nonblocking(true)?;

        let should_quit = Arc::new(AtomicBool::new(false));

        // Channel for server events from client threads.
        let (server_event_tx, server_event_rx) = mpsc::channel(64);

        Ok(Self {
            app,
            client_listener: listener,
            client_socket_path: client_path,
            clients: HashMap::new(),
            next_client_id: 1,
            foreground_client_id: None,
            next_activity_stamp: 1,
            effective_size: (MIN_COLS, MIN_ROWS),
            shutting_down: false,
            should_quit,
            server_event_rx,
            server_event_tx,
        })
    }

    /// Runs the headless server event loop until shutdown.
    ///
    /// This is the server's main loop — analogous to `App::run()` but without
    /// a real terminal. It:
    /// - Drains internal events (pane death, state changes)
    /// - Drains API requests (from the JSON socket)
    /// - Accepts new client connections
    /// - Reads client messages and routes input
    /// - Handles scheduled tasks (resize poll, animation, session save, etc.)
    /// - Renders virtually and streams frames to clients
    pub async fn run(&mut self) -> io::Result<()> {
        crate::logging::startup("server");

        // Register SIGINT handler for graceful shutdown.
        let should_quit = self.should_quit.clone();
        let quit_notify = self.server_event_tx.clone();
        ctrlc_handler(should_quit, quit_notify);

        // No input_rx needed — server doesn't read stdin.
        // We use None for input_rx so the event loop doesn't try to read from stdin.
        self.app.input_rx = None;

        let mut needs_render = true;

        loop {
            // If shutdown has been initiated, complete it and exit.
            if self.shutting_down {
                self.complete_shutdown()?;
                break;
            }

            // Check if we should start shutting down.
            if self.app.state.should_quit || self.should_quit.load(Ordering::Acquire) {
                self.initiate_shutdown();
                continue;
            }

            // 1. Check render_dirty flag from PTY reader tasks.
            if self.app.render_dirty.load(Ordering::Acquire) {
                needs_render = true;
            }

            // 2. Drain internal events.
            if self.drain_internal_events_with_forwarding() {
                needs_render = true;
            }

            // 3. Drain API requests.
            if self.drain_api_requests_with_shutdown_check() {
                needs_render = true;
            }

            self.app.sync_focus_events();
            self.app.sync_session_save_schedule();

            // 4. Accept new client connections.
            self.accept_client_connections()?;

            // 5. Drain server events from client threads.
            if self.drain_server_events() {
                needs_render = true;
            }

            // 6. Handle scheduled tasks.
            let now = Instant::now();
            if self.handle_scheduled_tasks_headless(now) {
                needs_render = true;
            }

            // Handle deferred requests.
            if self.app.state.request_complete_onboarding {
                self.app.state.request_complete_onboarding = false;
                self.app.open_settings_from_onboarding();
                needs_render = true;
            }

            if self.app.state.request_new_workspace {
                self.app.state.request_new_workspace = false;
                self.app.create_workspace();
                needs_render = true;
            }

            if self.app.state.request_new_tab {
                self.app.state.request_new_tab = false;
                self.app.create_tab();
                needs_render = true;
            }

            if self.app.state.request_reload_config {
                self.app.state.request_reload_config = false;
                self.app.reload_config();
                needs_render = true;
            }

            self.drain_client_sound_config_reload_request();

            self.app.sync_headless_animation_timer(now);

            // 7. Render virtually and stream frames.
            if needs_render && self.app.can_render_now(now) {
                self.app.render_dirty.swap(false, Ordering::AcqRel);
                self.render_and_stream();
                self.app.last_render_at = Some(now);
                needs_render = false;
                continue;
            }

            // 8. Wait for next event.
            let next_deadline = self
                .app
                .next_headless_loop_deadline(now, needs_render)
                .map(|deadline| deadline.min(now + CLIENT_ACCEPT_POLL_INTERVAL))
                .or(Some(now + CLIENT_ACCEPT_POLL_INTERVAL));
            let event = {
                tokio::select! {
                    maybe_api = self.app.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(msg),
                        None => LoopEvent::Timer,
                    },
                    maybe_ev = self.app.event_rx.recv() => match maybe_ev {
                        Some(ev) => LoopEvent::Internal(ev),
                        None => LoopEvent::Timer,
                    },
                    maybe_server_ev = self.server_event_rx.recv() => match maybe_server_ev {
                        Some(ev) => LoopEvent::ServerEvent(ev),
                        None => LoopEvent::Timer,
                    },
                    _ = sleep_until_or_pending(next_deadline) => LoopEvent::Timer,
                    _ = self.app.render_notify.notified() => LoopEvent::RenderRequested,
                }
            };

            match event {
                LoopEvent::Timer => {}
                LoopEvent::Internal(ev) => {
                    if self.handle_internal_event_with_forwarding(ev) {
                        needs_render = true;
                    }
                }
                LoopEvent::Api(msg) => {
                    if self.handle_api_request_with_shutdown_check(msg) {
                        needs_render = true;
                    }
                }
                LoopEvent::ServerEvent(ev) => {
                    if self.handle_server_event(ev) {
                        needs_render = true;
                    }
                }
                LoopEvent::RenderRequested => {
                    if self.app.render_dirty.load(Ordering::Acquire) {
                        needs_render = true;
                    }
                }
            }
        }

        // Save session on exit.
        if !self.app.no_session {
            self.app.save_session_now();
        }

        info!("headless server exiting");
        Ok(())
    }

    fn allocate_activity_stamp(&mut self) -> u64 {
        let stamp = self.next_activity_stamp;
        self.next_activity_stamp = self.next_activity_stamp.saturating_add(1);
        stamp
    }

    fn resize_shared_runtime_to_effective_size(&mut self) {
        if self.foreground_client_id.is_none() {
            return;
        }
        let (cols, rows) = self.effective_size;
        let area = Rect::new(0, 0, cols, rows);
        crate::ui::compute_view(&mut self.app.state, area);

        // Shared runtime size changes affect pane wrapping and foreground-driven
        // rendering semantics. Force one fresh frame to every remaining client
        // even if the next rendered buffer compares equal to its cached frame.
        for client in self.clients.values_mut() {
            client.last_frame = None;
        }
    }

    fn sync_foreground_client_state(&mut self) {
        let Some(client_id) = self.foreground_client_id else {
            self.effective_size = (MIN_COLS, MIN_ROWS);
            self.app.state.outer_terminal_focus = None;
            return;
        };
        let Some(client) = self.clients.get(&client_id) else {
            self.foreground_client_id = None;
            self.effective_size = (MIN_COLS, MIN_ROWS);
            self.app.state.outer_terminal_focus = None;
            return;
        };

        self.effective_size = client.terminal_size;
        self.app.state.outer_terminal_focus = client.outer_terminal_focus;
        if client.outer_terminal_focus == Some(true) {
            self.app.state.mark_active_tab_seen();
        }
        if !client.host_terminal_theme.is_empty() {
            self.app.set_host_terminal_theme(client.host_terminal_theme);
        }
    }

    fn foreground_client_outer_focus(&self) -> Option<bool> {
        let client_id = self.foreground_client_id?;
        self.clients.get(&client_id)?.outer_terminal_focus
    }

    fn active_tab_suppresses_notifications(&self, is_active_tab: bool) -> bool {
        crate::app::actions::active_tab_suppresses_notifications(
            is_active_tab,
            self.foreground_client_outer_focus(),
        )
    }

    fn promote_client_to_foreground(&mut self, client_id: u64) -> bool {
        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        client.last_activity = stamp;

        let changed = self.foreground_client_id != Some(client_id);
        self.foreground_client_id = Some(client_id);
        self.sync_foreground_client_state();
        changed
    }

    fn promote_latest_remaining_client(&mut self) -> bool {
        let next_foreground = self
            .clients
            .iter()
            .max_by_key(|(_, client)| client.last_activity)
            .map(|(&client_id, _)| client_id);
        let changed = next_foreground != self.foreground_client_id;
        self.foreground_client_id = next_foreground;
        self.sync_foreground_client_state();
        changed
    }

    fn remove_client(&mut self, client_id: u64) -> bool {
        let was_foreground = self.foreground_client_id == Some(client_id);
        self.clients.remove(&client_id);
        if was_foreground {
            self.promote_latest_remaining_client()
        } else {
            false
        }
    }

    fn update_client_host_theme_from_events(
        &mut self,
        client_id: u64,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };

        let mut next_theme = client.host_terminal_theme;
        for event in events {
            if let crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } = event {
                next_theme = next_theme.with_color(*kind, *color);
            }
        }

        if next_theme == client.host_terminal_theme {
            return false;
        }

        client.host_terminal_theme = next_theme;
        if self.foreground_client_id == Some(client_id) {
            self.app.set_host_terminal_theme(next_theme)
        } else {
            false
        }
    }

    fn update_client_outer_focus_from_events(
        &mut self,
        client_id: u64,
        events: &[crate::raw_input::RawInputEvent],
    ) {
        let next_focus = events
            .iter()
            .filter_map(|event| match event {
                crate::raw_input::RawInputEvent::OuterFocusGained => Some(true),
                crate::raw_input::RawInputEvent::OuterFocusLost => Some(false),
                _ => None,
            })
            .last();

        let Some(next_focus) = next_focus else {
            return;
        };
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        client.outer_terminal_focus = Some(next_focus);
        if self.foreground_client_id == Some(client_id) {
            self.app.state.outer_terminal_focus = Some(next_focus);
        }
    }

    fn events_include_interaction(events: &[crate::raw_input::RawInputEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                crate::raw_input::RawInputEvent::Key(_)
                    | crate::raw_input::RawInputEvent::Mouse(_)
                    | crate::raw_input::RawInputEvent::Paste(_)
                    | crate::raw_input::RawInputEvent::OuterFocusGained
            )
        })
    }

    /// Accepts pending client connections from the non-blocking listener.
    fn accept_client_connections(&mut self) -> io::Result<()> {
        loop {
            match self.client_listener.accept() {
                Ok((stream, _addr)) => {
                    let client_id = self.next_client_id;
                    self.next_client_id += 1;

                    if let Err(err) = stream.set_nonblocking(true) {
                        warn!(err = %err, "failed to set client stream nonblocking");
                        continue;
                    }

                    // Spawn a thread for the handshake and read loop.
                    let should_quit = self.should_quit.clone();
                    let server_event_tx = self.server_event_tx.clone();
                    std::thread::spawn(move || {
                        if let Err(err) = handle_client_handshake(
                            stream,
                            client_id,
                            &server_event_tx,
                            &should_quit,
                        ) {
                            debug!(client_id, err = %err, "client handshake failed");
                        }
                    });
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    // No more pending connections.
                    break;
                }
                Err(err) => {
                    error!(err = %err, "client listener accept failed");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Drains server events from the dedicated channel.
    ///
    /// Returns true if any input was processed (requiring a re-render).
    fn drain_server_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.server_event_rx.try_recv() {
            changed |= self.handle_server_event(ev);
        }
        changed
    }

    /// Handles a single internal event with forwarding logic for clipboard,
    /// sound, and toast notifications to connected clients.
    ///
    /// ALL internal events MUST be routed through this method to ensure
    /// clipboard/notify forwarding is never bypassed. Do not call
    /// `self.app.handle_internal_event()` directly for any internal event
    /// in the headless server — use this method instead.
    ///
    /// Returns true if the event changed visual state (requiring a re-render).
    fn handle_internal_event_with_forwarding(&mut self, ev: AppEvent) -> bool {
        match &ev {
            AppEvent::ClipboardWrite { content } => {
                // Clipboard writes are client-local side effects. Forward them only to
                // the foreground client instead of broadcasting to every attached client.
                if let Some(client_id) = self.foreground_client_id {
                    let data = base64::engine::general_purpose::STANDARD.encode(content.as_slice());
                    self.send_to_client(client_id, ServerMessage::Clipboard { data });
                }
                // ClipboardWrite doesn't change visual state — no render needed.
                false
            }
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
            } => {
                // Capture toast before handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = *agent;
                let state_val = *state;

                // Find the previous state of this pane before the event
                // is processed. We need this to determine if a sound
                // notification would be triggered.
                let prev_state = self
                    .app
                    .state
                    .workspaces
                    .iter()
                    .find_map(|ws| {
                        ws.tabs
                            .iter()
                            .find_map(|tab| tab.panes.get(&pane_id_val).map(|p| p.state))
                    })
                    .unwrap_or(crate::detect::AgentState::Unknown);

                // Handle the state change (updates pane state, sets toast on AppState).
                // Headless mode disables local sound playback separately from the
                // sound policy so reloads can keep server-side notification policy live.
                self.sync_foreground_client_state();
                self.app.handle_internal_event(ev);

                // Forward sound notification to clients when server-side sound policy allows it.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                if self.app.state.sound.allows(agent_val) {
                    if let Some(sound) = crate::app::actions::notification_sound_for_state_change(
                        suppress_active_tab_notifications,
                        prev_state,
                        state_val,
                    ) {
                        let msg = match sound {
                            crate::sound::Sound::Done => "agent done",
                            crate::sound::Sound::Request => "agent attention",
                        };
                        self.send_to_foreground_client(ServerMessage::Notify {
                            kind: protocol::NotifyKind::Sound,
                            message: msg.to_owned(),
                        });
                    }
                }

                let toast_msg =
                    if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
                        if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                            self.app
                                .state
                                .toast
                                .as_ref()
                                .map(|toast| format!("{}: {}", toast.title, toast.context))
                        } else {
                            toast_message_from_state_change(
                                &self.app.state,
                                pane_id_val,
                                suppress_active_tab_notifications,
                                prev_state,
                                state_val,
                            )
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_to_foreground_client(ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: msg,
                    });
                }

                true
            }
            AppEvent::HookStateReported {
                pane_id,
                agent_label,
                state,
                ..
            } => {
                // The hook authority may not change the effective pane state if
                // the detected agent doesn't match. We forward based on the
                // hook-reported state transition regardless.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = crate::detect::parse_agent_label(agent_label);
                let hook_state_val = *state;

                // Capture the previous hook authority state for this pane.
                // If no hook authority exists, use the effective state.
                let prev_hook_state = self
                    .app
                    .state
                    .workspaces
                    .iter()
                    .find_map(|ws| {
                        ws.tabs.iter().find_map(|tab| {
                            tab.panes.get(&pane_id_val).map(|p| {
                                p.hook_authority
                                    .as_ref()
                                    .map(|h| h.state)
                                    .unwrap_or(p.state)
                            })
                        })
                    })
                    .unwrap_or(crate::detect::AgentState::Unknown);

                self.sync_foreground_client_state();
                self.app.handle_internal_event(ev);

                // Forward sound notification based on hook state transition when
                // server-side sound policy allows it. This ensures API-reported state
                // changes (pane.report_agent) produce notifications even before
                // fallback detection confirms.
                let is_active_tab = self
                    .app
                    .state
                    .active
                    .and_then(|ws_idx| self.app.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| {
                        ws.find_tab_index_for_pane(pane_id_val)
                            .is_some_and(|tab_idx| ws.active_tab_index() == tab_idx)
                    });

                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                if self.app.state.sound.allows(agent_val) {
                    if let Some(sound) = crate::app::actions::notification_sound_for_state_change(
                        suppress_active_tab_notifications,
                        prev_hook_state,
                        hook_state_val,
                    ) {
                        let msg = match sound {
                            crate::sound::Sound::Done => "agent done",
                            crate::sound::Sound::Request => "agent attention",
                        };
                        self.send_to_foreground_client(ServerMessage::Notify {
                            kind: protocol::NotifyKind::Sound,
                            message: msg.to_owned(),
                        });
                    }
                }

                let toast_msg =
                    if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
                        if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                            self.app
                                .state
                                .toast
                                .as_ref()
                                .map(|toast| format!("{}: {}", toast.title, toast.context))
                        } else {
                            toast_message_from_state_change(
                                &self.app.state,
                                pane_id_val,
                                suppress_active_tab_notifications,
                                prev_hook_state,
                                hook_state_val,
                            )
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_to_foreground_client(ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: msg,
                    });
                }

                true
            }
            AppEvent::UpdateReady { version } => {
                let toast_before = self.app.state.toast.clone();
                let version = version.clone();

                self.app.handle_internal_event(ev);

                let toast_msg =
                    if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
                        if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                            self.app
                                .state
                                .toast
                                .as_ref()
                                .map(|toast| format!("{}: {}", toast.title, toast.context))
                        } else {
                            Some(format!(
                                "v{version} available: detach, then run `herdr update`"
                            ))
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_to_foreground_client(ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: msg,
                    });
                }

                true
            }
            _ => {
                self.app.handle_internal_event(ev);
                true
            }
        }
    }

    /// Drains internal events, forwarding clipboard, sound, and toast
    /// notifications to connected clients instead of processing them locally.
    ///
    /// In the monolithic mode:
    /// - `ClipboardWrite` events are written to stdout via `write_osc52_bytes`.
    /// - Sound notifications are played locally via `sound::play`.
    /// - Toast notifications are set on AppState and rendered into the frame.
    ///
    /// In the headless server, there is no stdout terminal or audio subsystem,
    /// so we:
    /// - Forward `ClipboardWrite` as `ServerMessage::Clipboard` to the
    ///   foreground client only.
    /// - Detect when a sound would be played and forward as
    ///   `ServerMessage::Notify { kind: Sound }` to the foreground client.
    /// - Detect when a toast is set on AppState and forward as
    ///   `ServerMessage::Notify { kind: Toast }` to the foreground client.
    fn drain_internal_events_with_forwarding(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.app.event_rx.try_recv() {
            changed |= self.handle_internal_event_with_forwarding(ev);
        }
        changed
    }

    fn drain_client_sound_config_reload_request(&mut self) {
        if !self.app.state.request_client_sound_config_reload {
            return;
        }
        self.app.state.request_client_sound_config_reload = false;
        self.send_to_all_clients(ServerMessage::ReloadSoundConfig);
    }

    /// Encodes a server message into a length-prefixed frame.
    fn frame_server_message(msg: &ServerMessage) -> Result<Vec<u8>, protocol::FramingError> {
        let mut framed = Vec::new();
        protocol::write_message(&mut framed, msg)?;
        Ok(framed)
    }

    /// Sends a message to all connected clients.
    /// Broken connections are tracked and cleaned up.
    fn send_to_all_clients(&mut self, msg: ServerMessage) {
        let serialized = match Self::frame_server_message(&msg) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(err = %err, "failed to serialize message for clients");
                return;
            }
        };

        let mut broken_clients: Vec<u64> = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if let Some(writer) = &client.writer {
                if writer.send(serialized.clone()).is_err() {
                    debug!(client_id, "client writer channel closed during broadcast");
                    broken_clients.push(client_id);
                }
            }
        }

        // Remove broken clients.
        for client_id in broken_clients {
            let foreground_changed = self.remove_client(client_id);
            if foreground_changed {
                self.resize_shared_runtime_to_effective_size();
            }
        }
    }

    /// Sends a client-local side effect to the foreground client only.
    fn send_to_foreground_client(&mut self, msg: ServerMessage) -> bool {
        let Some(client_id) = self.foreground_client_id else {
            return false;
        };
        self.send_to_client(client_id, msg)
    }

    /// Sends a message to a specific client. Returns false if the client
    /// was not found or the send failed (client removed).
    fn send_to_client(&mut self, client_id: u64, msg: ServerMessage) -> bool {
        let serialized = match Self::frame_server_message(&msg) {
            Ok(framed) => framed,
            Err(err) => {
                warn!(client_id, err = %err, "failed to serialize message for client");
                return false;
            }
        };

        if let Some(client) = self.clients.get(&client_id) {
            if let Some(writer) = &client.writer {
                if writer.send(serialized).is_err() {
                    debug!(
                        client_id,
                        "client writer channel closed during targeted send"
                    );
                    let foreground_changed = self.remove_client(client_id);
                    if foreground_changed {
                        self.resize_shared_runtime_to_effective_size();
                    }
                    return false;
                }
            }
            true
        } else {
            false
        }
    }

    /// Handles a server event. Returns true if the event requires a re-render.
    fn handle_server_event(&mut self, ev: ServerEvent) -> bool {
        match ev {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                writer,
            } => {
                info!(client_id, cols, rows, "client connected");
                let last_activity = self.allocate_activity_stamp();
                self.clients.insert(
                    client_id,
                    ClientConnection {
                        terminal_size: (cols, rows),
                        host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                        outer_terminal_focus: None,
                        last_activity,
                        last_frame: None,
                        writer: Some(writer),
                    },
                );
                self.foreground_client_id = Some(client_id);
                self.sync_foreground_client_state();
                self.resize_shared_runtime_to_effective_size();
                true
            }
            ServerEvent::ClientInput { client_id, data } => {
                debug!(client_id, len = data.len(), "client input received");
                if let Some(client) = self.clients.get_mut(&client_id) {
                    // Ensure the next render after client input is delivered even if the
                    // current frame buffer still compares equal. Input can change cursor or
                    // PTY state asynchronously, and thin clients should not stall waiting for
                    // a post-input frame behind identical-frame dedupe.
                    client.last_frame = None;
                }
                let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                self.update_client_outer_focus_from_events(client_id, &events);
                let interaction = Self::events_include_interaction(&events);
                let foreground_changed = if interaction {
                    self.promote_client_to_foreground(client_id)
                } else {
                    false
                };
                if foreground_changed {
                    self.resize_shared_runtime_to_effective_size();
                }
                let theme_changed = self.update_client_host_theme_from_events(client_id, &events);
                self.app
                    .route_client_events(events, self.foreground_client_id == Some(client_id));

                // Check if the detach keybind was triggered during input processing.
                if self.app.state.detach_requested {
                    self.app.state.detach_requested = false;
                    info!(client_id, "client detach requested via keybind");

                    // Send a ServerShutdown with "detached" reason to this client
                    // so it exits cleanly (not with a connection-lost error).
                    // The client will close its connection after receiving this,
                    // which triggers a ClientDisconnected event that removes it.
                    self.send_to_client(
                        client_id,
                        ServerMessage::ServerShutdown {
                            reason: Some("detached".to_owned()),
                        },
                    );

                    // Don't remove the client here — let the client disconnect
                    // naturally after receiving the ServerShutdown. The client's
                    // read loop will see EOF and the server will get a
                    // ClientDisconnected event which handles cleanup.
                    //
                    // However, we do need to stop sending frames to this client
                    // since it's detaching. Drop the writer channel so no more
                    // frames are queued for this client.
                    if let Some(client) = self.clients.get_mut(&client_id) {
                        client.writer = None;
                    }

                    // No re-render needed for remaining clients.
                    false
                } else {
                    foreground_changed || theme_changed || interaction
                }
            }
            ServerEvent::ClientResize {
                client_id,
                cols,
                rows,
            } => {
                info!(client_id, cols, rows, "client resize");
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.terminal_size = (cols, rows);
                }
                self.promote_client_to_foreground(client_id);
                self.resize_shared_runtime_to_effective_size();
                true
            }
            ServerEvent::ClientDetach { client_id } => {
                info!(client_id, "client detached");
                let foreground_changed = self.remove_client(client_id);
                if foreground_changed {
                    self.resize_shared_runtime_to_effective_size();
                }
                true
            }
            ServerEvent::ClientDisconnected { client_id } => {
                info!(client_id, "client disconnected");
                let foreground_changed = self.remove_client(client_id);
                if foreground_changed {
                    self.resize_shared_runtime_to_effective_size();
                }
                true
            }
            ServerEvent::QuitSignal => {
                // The quit check at the top of the loop handles this.
                // No render needed — the next iteration will initiate shutdown.
                false
            }
        }
    }

    /// Drains API requests with shutdown awareness.
    ///
    /// During shutdown, remaining requests get a `server_unavailable` error.
    fn drain_api_requests_with_shutdown_check(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.app.api_rx.try_recv() {
            changed |= self.handle_api_request_with_shutdown_check(msg);
        }
        changed
    }

    /// Handles a single API request with shutdown awareness.
    ///
    /// Also forwards any toast/sound notifications that result from the API
    /// request to connected clients. API methods like `pane.report_agent`
    /// trigger internal events that may set toast state or would normally
    /// play sounds — in headless mode we forward these to clients instead.
    fn handle_api_request_with_shutdown_check(&mut self, msg: api::ApiRequestMessage) -> bool {
        if self.shutting_down {
            // During shutdown, respond with server_unavailable.
            let response = serde_json::to_string(&api::schema::ErrorResponse {
                id: msg.request.id,
                error: api::schema::ErrorBody {
                    code: "server_unavailable".into(),
                    message: "server is shutting down".into(),
                },
            })
            .unwrap_or_else(|_| {
                r#"{"id":"","error":{"code":"server_unavailable","message":"server is shutting down"}}"#
                    .to_string()
            });
            let _ = msg.respond_to.send(response);
            return false;
        }

        let changed = api::request_changes_ui(&msg.request);

        // Capture toast and pane agent states before the API call so we can
        // forward any resulting notifications to connected clients.
        // API requests like pane.report_agent trigger handle_internal_event
        // internally, which bypasses drain_internal_events_with_forwarding.
        // Headless mode disables local sound playback, so sound notifications
        // need to be forwarded to clients here; toasts may be set but not forwarded.
        //
        // Note: pane.report_agent sets hook_authority on the pane, but the
        // effective state may not change until the fallback detector confirms
        // the agent (detected_agent must match). So we capture both the
        // effective state AND the hook authority state for comparison.
        let toast_before = self.app.state.toast.clone();
        let pane_states_before: Vec<(
            usize,
            crate::layout::PaneId,
            crate::detect::AgentState,
            Option<crate::detect::AgentState>,
        )> = self
            .app
            .state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.panes.iter().map(move |(&pane_id, pane)| {
                        (
                            ws_idx,
                            pane_id,
                            pane.state,
                            pane.hook_authority.as_ref().map(|h| h.state),
                        )
                    })
                })
            })
            .collect();

        self.sync_foreground_client_state();
        let response = self.app.handle_api_request(msg.request);
        let _ = msg.respond_to.send(response);

        // Forward new toast state only when terminal delivery is selected.
        // Herdr delivery renders the toast in-frame and must not ask clients to
        // show a terminal/desktop notification.
        let toast_after = self.app.state.toast.clone();
        let forwarded_toast_from_state =
            if should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                && toast_after.is_some()
                && toast_after != toast_before
            {
                if let Some(toast) = &toast_after {
                    let msg_text = format!("{}: {}", toast.title, toast.context);
                    debug!(msg = %msg_text, "forwarding toast notification from API request");
                    self.send_to_foreground_client(ServerMessage::Notify {
                        kind: protocol::NotifyKind::Toast,
                        message: msg_text,
                    });
                    true
                } else {
                    false
                }
            } else {
                false
            };

        // Forward sound notifications for any pane state changes that occurred
        // during the API request. Compare before/after pane states (including
        // hook authority state) to find transitions that would trigger a sound.
        //
        // pane.report_agent sets hook_authority on the pane, but the effective
        // state may not change until the fallback detector confirms the agent
        // (detected_agent must match hook_authority.agent). We check BOTH the
        // effective state AND the hook authority state so that API-reported
        // state changes trigger notifications even before fallback confirmation.
        for (ws_idx, pane_id, prev_effective_state, prev_hook_state) in &pane_states_before {
            let pane_after = self
                .app
                .state
                .workspaces
                .get(*ws_idx)
                .and_then(|ws| ws.tabs.iter().find_map(|tab| tab.panes.get(pane_id)));

            let Some(pane_after) = pane_after else {
                continue;
            };

            let new_effective_state = pane_after.state;
            let new_hook_state = pane_after.hook_authority.as_ref().map(|h| h.state);

            // Check effective state change first.
            let effective_changed = new_effective_state != *prev_effective_state;
            // Check hook authority state change — this catches API-reported
            // state changes that haven't been confirmed by fallback detection.
            let hook_changed = new_hook_state != *prev_hook_state;

            if effective_changed || hook_changed {
                // Use the hook state if available (it reflects the API-reported
                // state), otherwise use the effective state.
                let prev_state = prev_hook_state.unwrap_or(*prev_effective_state);
                let new_state = new_hook_state.unwrap_or(new_effective_state);

                // Skip if the derived transition is a no-op.
                if prev_state == new_state {
                    continue;
                }

                let is_active_tab = self.app.state.pane_is_in_active_tab(*ws_idx, *pane_id);
                let suppress_active_tab_notifications =
                    self.active_tab_suppresses_notifications(is_active_tab);

                // Get the known agent for sound settings. Unknown custom labels
                // fall back to None so clients use the generic sound behavior.
                let agent = pane_after.effective_known_agent();

                debug!(
                    ws_idx,
                    pane_id = pane_id.raw(),
                    prev_state = ?prev_state,
                    new_state = ?new_state,
                    agent = ?agent,
                    effective_changed,
                    hook_changed,
                    "pane state changed during API request, checking sound notification"
                );

                if !forwarded_toast_from_state
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if let Some(kind) = crate::app::actions::notification_toast_for_state_change(
                        suppress_active_tab_notifications,
                        prev_state,
                        new_state,
                    ) {
                        if let Some(agent_label) = pane_after.effective_agent_label() {
                            let event_text = match kind {
                                crate::app::state::ToastKind::NeedsAttention => "needs attention",
                                crate::app::state::ToastKind::Finished => "finished",
                                crate::app::state::ToastKind::UpdateInstalled => "updated",
                            };
                            let msg_text = format!(
                                "{} {}: {}",
                                agent_label,
                                event_text,
                                crate::app::actions::notification_context(
                                    &self.app.state.workspaces[*ws_idx],
                                    *ws_idx,
                                    *pane_id,
                                )
                            );
                            self.send_to_foreground_client(ServerMessage::Notify {
                                kind: protocol::NotifyKind::Toast,
                                message: msg_text,
                            });
                        }
                    }
                }

                // Forward sound notification when server-side sound policy allows it.
                // Clients still decide locally whether they can execute the side effect.
                if self.app.state.sound.allows(agent) {
                    if let Some(sound) = crate::app::actions::notification_sound_for_state_change(
                        suppress_active_tab_notifications,
                        prev_state,
                        new_state,
                    ) {
                        let msg_text = match sound {
                            crate::sound::Sound::Done => "agent done",
                            crate::sound::Sound::Request => "agent attention",
                        };
                        debug!(sound = ?sound, "forwarding sound notification from API request");
                        self.send_to_foreground_client(ServerMessage::Notify {
                            kind: protocol::NotifyKind::Sound,
                            message: msg_text.to_owned(),
                        });
                    }
                }
            }
        }

        changed
    }

    /// Renders the current state to client-sized virtual buffers and streams
    /// frames to all connected clients.
    fn render_and_stream(&mut self) {
        let foreground_client_id = self.foreground_client_id;
        let mut render_targets: Vec<(u64, (u16, u16), bool)> = self
            .clients
            .iter()
            .filter(|(_, client)| client.writer.is_some())
            .map(|(&client_id, client)| {
                (
                    client_id,
                    client.terminal_size,
                    foreground_client_id == Some(client_id),
                )
            })
            .collect();

        render_targets.sort_by_key(|(client_id, _, is_foreground)| (*is_foreground, *client_id));

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            let _ = render_virtual(&mut self.app.state, area, resize_panes);
            debug!(
                cols,
                rows, resize_panes, "rendered virtual frame with no attached clients"
            );
            return;
        }

        let mut broken_clients: Vec<u64> = Vec::new();
        for (client_id, (cols, rows), is_foreground) in render_targets {
            let area = Rect::new(0, 0, cols, rows);
            let (buffer, cursor) = render_virtual(&mut self.app.state, area, is_foreground);
            let frame = FrameData::from_ratatui_buffer(&buffer, cursor);

            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                continue;
            };

            if client.last_frame.as_ref() == Some(&frame) {
                continue;
            }

            let message = ServerMessage::Frame(frame.clone());
            let serialized = match Self::frame_server_message(&message) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame for client");
                    broken_clients.push(client_id);
                    continue;
                }
            };

            if writer.send(serialized).is_err() {
                debug!(client_id, "client writer channel closed, marking as broken");
                broken_clients.push(client_id);
                continue;
            }

            client.last_frame = Some(frame);
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                let foreground_changed = self.remove_client(client_id);
                if foreground_changed {
                    self.resize_shared_runtime_to_effective_size();
                }
            }
        }

        let (cols, rows) = self.effective_size;
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }

    /// Handle scheduled tasks for the headless server.
    ///
    /// Similar to `App::handle_scheduled_tasks` but without resize polling
    /// (the server doesn't have a terminal to resize).
    fn handle_scheduled_tasks_headless(&mut self, now: Instant) -> bool {
        let mut changed = false;

        self.app.sync_headless_animation_timer(now);

        // No resize polling needed — server has no terminal.
        // Client resize messages drive size changes instead.

        if self
            .app
            .config_diagnostic_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.config_diagnostic_deadline = None;
            self.app.state.config_diagnostic = None;
            changed = true;
        }

        if self
            .app
            .toast_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.toast_deadline = None;
            self.app.state.toast = None;
            changed = true;
        }

        if self
            .app
            .next_animation_tick
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.state.spinner_tick = self
                .app
                .state
                .spinner_tick
                .wrapping_add(app::HEADLESS_ANIMATION_TICK_STEP);
            self.app.next_animation_tick = Some(now + app::HEADLESS_ANIMATION_INTERVAL);
            changed = true;
        }

        if self
            .app
            .git_refresh_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            for ws in &mut self.app.state.workspaces {
                ws.refresh_git_ahead_behind();
            }
            self.app.last_git_remote_status_refresh = now;
            changed = true;
        }

        if self
            .app
            .next_auto_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.run_auto_update_check();
        }

        if self
            .app
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.save_session_now();
        }

        self.app.sync_headless_animation_timer(now);
        changed
    }

    /// Initiates graceful shutdown.
    fn initiate_shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        info!("server shutdown initiated");
        self.shutting_down = true;

        // Send ServerShutdown to all connected clients.
        let shutdown_msg = ServerMessage::ServerShutdown {
            reason: Some("server is shutting down".to_owned()),
        };
        self.send_to_all_clients(shutdown_msg);

        // Give client writer threads a moment to flush the shutdown message.
        // A short sleep ensures the message is written to the socket before
        // we close the connections.
        std::thread::sleep(Duration::from_millis(50));

        // Signal the main loop to exit.
        self.should_quit.store(true, Ordering::Release);
        self.app.state.should_quit = true;
    }

    /// Completes the shutdown sequence: send ServerShutdown to clients,
    /// close client connections, remove socket files, and clean up.
    fn complete_shutdown(&mut self) -> io::Result<()> {
        info!("completing server shutdown");

        // Send ServerShutdown to all remaining clients.
        if !self.clients.is_empty() {
            let shutdown_msg = ServerMessage::ServerShutdown {
                reason: Some("server is shutting down".to_owned()),
            };
            self.send_to_all_clients(shutdown_msg);

            // Give writer threads a moment to flush before closing.
            std::thread::sleep(Duration::from_millis(50));
        }

        // Drain remaining API requests with server_unavailable.
        self.drain_api_requests_with_shutdown_check();

        // Close all client connections.
        self.clients.clear();

        // Remove socket files.
        self.cleanup_sockets()?;

        Ok(())
    }

    /// Removes socket files created by the server.
    fn cleanup_sockets(&self) -> io::Result<()> {
        if let Err(err) = fs::remove_file(&self.client_socket_path) {
            if err.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %self.client_socket_path.display(),
                    err = %err,
                    "failed to remove client socket on shutdown"
                );
            }
        }
        Ok(())
    }
}

impl Drop for HeadlessServer {
    fn drop(&mut self) {
        let _ = self.cleanup_sockets();
    }
}

// ---------------------------------------------------------------------------
// Client handshake
// ---------------------------------------------------------------------------

/// Internal event sent from the handshake thread to the main event loop.
#[derive(Debug)]
pub enum ServerEvent {
    /// A new client completed the handshake.
    ClientConnected {
        client_id: u64,
        cols: u16,
        rows: u16,
        writer: std::sync::mpsc::Sender<Vec<u8>>,
    },
    /// A client sent an input message.
    ClientInput { client_id: u64, data: Vec<u8> },
    /// A client sent a resize message.
    ClientResize {
        client_id: u64,
        cols: u16,
        rows: u16,
    },
    /// A client detached gracefully.
    ClientDetach { client_id: u64 },
    /// A client connection was lost.
    ClientDisconnected { client_id: u64 },
    /// Ctrl+C or external shutdown signal received.
    QuitSignal,
}

/// Handles the client handshake on a blocking thread.
///
/// Reads the `Hello` message, validates the version, sends `Welcome`,
/// and then enters a read loop forwarding messages to the server event channel.
fn handle_client_handshake(
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

    let (client_cols, client_rows) = match hello {
        ClientMessage::Hello {
            version,
            cols,
            rows,
        } => {
            // Version check.
            match protocol::check_client_version(version) {
                protocol::VersionCheck::Compatible => {}
                protocol::VersionCheck::Incompatible(reason) => {
                    // Send rejection Welcome.
                    let welcome = ServerMessage::Welcome {
                        version: PROTOCOL_VERSION,
                        error: Some(reason),
                    };
                    let _ = protocol::write_message(&mut stream, &welcome);
                    return Ok(());
                }
            }

            // Clamp size.
            let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
            (clamped_cols, clamped_rows)
        }
        _ => {
            // First message must be Hello.
            debug!(client_id, "first message was not Hello, closing");
            let welcome = ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                error: Some("expected Hello as first message".to_owned()),
            };
            let _ = protocol::write_message(&mut stream, &welcome);
            return Ok(());
        }
    };

    // Send Welcome.
    let welcome = ServerMessage::Welcome {
        version: PROTOCOL_VERSION,
        error: None,
    };
    protocol::write_message(&mut stream, &welcome)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    // Clear read timeout for normal operation.
    stream.set_read_timeout(None)?;

    // Create a channel for writing frames back to the client.
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // Notify the main loop about the new client.
    let _ = server_event_tx.blocking_send(ServerEvent::ClientConnected {
        client_id,
        cols: client_cols,
        rows: client_rows,
        writer: frame_tx,
    });

    // Spawn a writer thread that forwards frames from the channel to the stream.
    let write_stream = stream.try_clone()?;
    let write_quit = should_quit.clone();
    std::thread::spawn(move || {
        client_writer_loop(write_stream, frame_rx, &write_quit);
    });

    // Enter read loop — read client messages and forward to main loop.
    client_read_loop(stream, client_id, server_event_tx, should_quit)
}

/// The client writer loop — sends frames from the channel to the client stream.
fn client_writer_loop(
    mut stream: UnixStream,
    frame_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    should_quit: &Arc<AtomicBool>,
) {
    while !should_quit.load(Ordering::Acquire) {
        match frame_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(data) => {
                if let Err(err) = stream.write_all(&data) {
                    debug!(err = %err, "client write failed, closing writer");
                    break;
                }
                if let Err(err) = stream.flush() {
                    debug!(err = %err, "client flush failed, closing writer");
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    debug!("client writer thread exiting");
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
            ClientMessage::Resize { cols, rows } => {
                let (clamped_cols, clamped_rows) = clamp_terminal_size(cols, rows);
                ServerEvent::ClientResize {
                    client_id,
                    cols: clamped_cols,
                    rows: clamped_rows,
                }
            }
            ClientMessage::Detach => ServerEvent::ClientDetach { client_id },
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Installs a Ctrl+C handler that sets the should_quit flag and wakes up
/// the event loop by sending a QuitSignal on the server event channel.
fn ctrlc_handler(should_quit: Arc<AtomicBool>, server_event_tx: mpsc::Sender<ServerEvent>) {
    let _ = ctrlc::set_handler(move || {
        should_quit.store(true, Ordering::Release);
        // Wake up the event loop so the quit flag is checked promptly.
        let _ = server_event_tx.try_send(ServerEvent::QuitSignal);
    });
}

/// Sleep until a deadline, or return pending if none.
async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending().await,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the headless server. This is the entry point called from main.rs.
pub fn run_server() -> io::Result<()> {
    init_logging();

    let loaded_config = config::Config::load();
    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();

    // Start the JSON API socket server.
    let _api_server = match api::start_server(api_tx, event_hub.clone()) {
        Ok(server) => server,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("error: herdr server is already running");
            eprintln!("api socket: {}", api::socket_path().display());
            std::process::exit(1);
        }
        Err(err) => return Err(err),
    };

    let no_session = false; // Server always does session persistence.

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let result = rt.block_on(async {
        // Create the App (with AppState, event channels, etc.).
        let mut app = app::App::new(
            &loaded_config.config,
            no_session,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            None, // startup_release_notes
            api_rx,
            event_hub,
        );

        // The server runs headless — disable local notification side effects.
        // Sound and terminal notifications are forwarded to connected clients
        // as ServerMessage::Notify instead of emitted by the server process.
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;

        // Create the headless server.
        let mut server = match HeadlessServer::new(app) {
            Ok(server) => server,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                eprintln!("error: herdr server is already running");
                eprintln!("client socket: {}", client_socket_path().display());
                std::process::exit(1);
            }
            Err(err) => return Err(err),
        };

        info!(
            api_socket = %api::socket_path().display(),
            client_socket = %client_socket_path().display(),
            "herdr server started"
        );

        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

/// Initialize logging for the server process.
fn init_logging() {
    crate::logging::init_file_logging("herdr-server.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::server::protocol::CursorState;

    fn test_headless_server() -> HeadlessServer {
        let config = crate::config::Config::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app =
            crate::app::App::new(&config, true, None, None, api_rx, api::EventHub::default());
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;

        let dir = std::env::temp_dir().join(format!(
            "hh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("client.sock");
        let _ = fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).expect("bind test listener");
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let (server_event_tx, server_event_rx) = mpsc::channel(64);

        HeadlessServer {
            app,
            client_listener: listener,
            client_socket_path: socket_path,
            clients: HashMap::new(),
            next_client_id: 1,
            foreground_client_id: None,
            next_activity_stamp: 1,
            effective_size: (MIN_COLS, MIN_ROWS),
            shutting_down: false,
            should_quit: Arc::new(AtomicBool::new(false)),
            server_event_rx,
            server_event_tx,
        }
    }

    fn read_server_message(bytes: Vec<u8>) -> ServerMessage {
        let mut cursor = std::io::Cursor::new(bytes);
        protocol::read_message(&mut cursor, MAX_FRAME_SIZE).expect("decode server message")
    }

    fn read_server_frame(bytes: Vec<u8>) -> FrameData {
        match read_server_message(bytes) {
            ServerMessage::Frame(frame) => frame,
            other => panic!("expected frame, got {other:?}"),
        }
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
    fn client_socket_path_derived_from_api_socket_override() {
        let path = client_socket_path_from_overrides(Some("/tmp/test-herdr.sock"), None);
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_api_override_takes_precedence_over_legacy_client_override() {
        let path = client_socket_path_from_overrides(
            Some("/tmp/test-herdr.sock"),
            Some("/tmp/legacy-client.sock"),
        );
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_respects_legacy_client_override_without_api_override() {
        let path = client_socket_path_from_overrides(None, Some("/tmp/test-herdr-client.sock"));
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_defaults_to_config_dir() {
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let path = client_socket_path_from_overrides(None, None);
        assert_eq!(path, config::config_dir().join("herdr-client.sock"));
    }

    #[test]
    fn derive_client_socket_from_api_socket_without_sock_extension() {
        let derived = derive_client_socket_from_api_socket(Path::new("/tmp/custom-api"));
        assert_eq!(derived, PathBuf::from("/tmp/custom-api-client.sock"));
    }

    #[test]
    fn prepare_socket_path_removes_stale_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("stale.sock");

        // Create a socket file that nobody is listening on.
        {
            let _listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        }
        // The listener scope ended, so the socket is now stale.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // prepare_socket_path should remove it without error.
        let result = prepare_socket_path(&socket_path);
        assert!(result.is_ok(), "should remove stale socket: {result:?}");

        // Socket file should be gone.
        assert!(!socket_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_socket_path_rejects_live_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("live.sock");

        // Bind a live listener.
        let _listener = UnixListener::bind(&socket_path).expect("bind");

        let result = prepare_socket_path(&socket_path);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AddrInUse);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtual_render_produces_nonempty_buffer() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (buffer, _cursor) = render_virtual(&mut state, area, true);
        assert_eq!(buffer.area.width, 80);
        assert_eq!(buffer.area.height, 24);
    }

    #[test]
    fn virtual_render_without_frame_cursor_keeps_cursor_hidden() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) = render_virtual(&mut state, area, true);

        assert_eq!(cursor, None);
    }

    #[tokio::test]
    async fn virtual_render_preserves_explicit_frame_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::pane::PaneRuntime::test_with_screen_bytes(20, 5, b"left"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) = render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x + 4,
                y: pane.inner_rect.y,
                visible: true,
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_preserves_hidden_focused_pane_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::pane::PaneRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) = render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x + 4,
                y: pane.inner_rect.y,
                visible: false,
            })
        );
    }

    #[test]
    fn latest_active_client_drives_shared_size_theme_and_fallback() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (160, 45),
                host_terminal_theme: crate::terminal_theme::TerminalTheme {
                    foreground: Some(crate::terminal_theme::RgbColor {
                        r: 0xaa,
                        g: 0xbb,
                        b: 0xcc,
                    }),
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 0x11,
                        g: 0x22,
                        b: 0x33,
                    }),
                },
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: None,
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme {
                    foreground: Some(crate::terminal_theme::RgbColor {
                        r: 0x10,
                        g: 0x20,
                        b: 0x30,
                    }),
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 0xdd,
                        g: 0xee,
                        b: 0xff,
                    }),
                },
                outer_terminal_focus: None,
                last_activity: 2,
                last_frame: None,
                writer: None,
            },
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.effective_size, (160, 45));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&1].host_terminal_theme
        );

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.effective_size, (80, 24));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&2].host_terminal_theme
        );

        assert!(server.remove_client(2));
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.effective_size, (160, 45));
        assert_eq!(
            server.app.state.host_terminal_theme,
            server.clients[&1].host_terminal_theme
        );
    }

    #[test]
    fn focus_lost_updates_client_without_promoting_foreground() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: None,
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: Some(true),
                last_activity: 2,
                last_frame: None,
                writer: None,
            },
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.foreground_client_id, Some(2));
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(false));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[test]
    fn focus_gained_promotes_client_to_foreground() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: None,
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: Some(true),
                last_activity: 2,
                last_frame: None,
                writer: None,
            },
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });

        assert!(changed);
        assert_eq!(server.foreground_client_id, Some(1));
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[test]
    fn foreground_client_focus_event_updates_app_focus_state() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: Some(true),
                last_activity: 1,
                last_frame: None,
                writer: None,
            },
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[O".to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(false));
        assert_eq!(server.app.state.outer_terminal_focus, Some(false));
    }

    #[test]
    fn render_and_stream_uses_each_client_terminal_size() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (desktop_tx, desktop_rx) = std::sync::mpsc::channel();
        let (phone_tx, phone_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(desktop_tx),
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 2,
                last_frame: None,
                writer: Some(phone_tx),
            },
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        server.render_and_stream();

        let desktop_frame = read_server_frame(desktop_rx.recv().expect("desktop frame"));
        let phone_frame = read_server_frame(phone_rx.recv().expect("phone frame"));

        assert_eq!((desktop_frame.width, desktop_frame.height), (120, 40));
        assert_eq!((phone_frame.width, phone_frame.height), (80, 24));
    }

    #[test]
    fn render_and_stream_skips_identical_frame_sends() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (client_tx, client_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(client_tx),
            },
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        server.render_and_stream();
        let first = client_rx.recv_timeout(Duration::from_millis(100));
        assert!(first.is_ok(), "expected first frame to be sent");

        server.render_and_stream();
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "identical frame should not be sent twice"
        );
    }

    #[test]
    fn client_sound_reload_request_refreshes_attached_clients() {
        let mut server = test_headless_server();
        let (client_tx, client_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(client_tx),
            },
        );
        server.app.state.request_client_sound_config_reload = true;

        server.drain_client_sound_config_reload_request();

        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("client sound reload message"),
        ) {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected ReloadSoundConfig, got {other:?}"),
        }
        assert!(!server.app.state.request_client_sound_config_reload);
    }

    #[test]
    fn clipboard_write_targets_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_rx) = std::sync::mpsc::channel();
        let (foreground_tx, foreground_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(background_tx),
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 2,
                last_frame: None,
                writer: Some(foreground_tx),
            },
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(!changed);
        match read_server_message(
            foreground_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground clipboard message"),
        ) {
            ServerMessage::Clipboard { data } => assert_eq!(data, "dGVzdA=="),
            other => panic!("expected clipboard message, got {other:?}"),
        }
        assert!(
            background_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive clipboard writes"
        );
    }

    #[test]
    fn client_local_notifications_target_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_rx) = std::sync::mpsc::channel();
        let (foreground_tx, foreground_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (120, 40),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(background_tx),
            },
        );
        server.clients.insert(
            2,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 2,
                last_frame: None,
                writer: Some(foreground_tx),
            },
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.send_to_foreground_client(ServerMessage::Notify {
            kind: protocol::NotifyKind::Toast,
            message: "pi finished: workspace 1".to_string(),
        }));

        match read_server_message(
            foreground_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground toast message"),
        ) {
            ServerMessage::Notify { kind, message } => {
                assert_eq!(kind, protocol::NotifyKind::Toast);
                assert_eq!(message, "pi finished: workspace 1");
            }
            other => panic!("expected toast notify, got {other:?}"),
        }
        assert!(
            background_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive client-local notifications"
        );
    }

    #[test]
    fn herdr_toast_delivery_keeps_toast_in_frame_without_client_notify() {
        let mut server = test_headless_server();
        let (client_tx, client_rx) = std::sync::mpsc::channel();

        server.clients.insert(
            1,
            ClientConnection {
                terminal_size: (80, 24),
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                outer_terminal_focus: None,
                last_activity: 1,
                last_frame: None,
                writer: Some(client_tx),
            },
        );
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
            version: "9.9.9".to_string(),
        });

        assert!(changed);
        assert!(server.app.state.toast.is_some());
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "herdr delivery should render in-frame instead of forwarding a terminal notification"
        );
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

    /// Verify that no direct calls to `self.app.handle_internal_event`
    /// exist outside of `handle_internal_event_with_forwarding` in this
    /// module. This ensures the forwarding bypass cannot be reintroduced.
    ///
    /// The search pattern looks for `handle_internal_event` calls that
    /// are NOT inside the `handle_internal_event_with_forwarding` method.
    #[test]
    fn no_handle_internal_event_bypass_in_module() {
        let source = include_str!("headless.rs");

        // Find all lines containing handle_internal_event
        let mut bypass_lines: Vec<String> = Vec::new();
        let mut inside_forwarding_method = false;
        let mut forwarding_method_brace_depth = 0u32;

        for (i, line) in source.lines().enumerate() {
            let line_num = i + 1;

            // Track when we're inside handle_internal_event_with_forwarding
            if line.contains("fn handle_internal_event_with_forwarding") {
                inside_forwarding_method = true;
                forwarding_method_brace_depth = 0;
            }

            if inside_forwarding_method {
                // Count braces to track when we exit the method
                for ch in line.chars() {
                    match ch {
                        '{' => forwarding_method_brace_depth += 1,
                        '}' => {
                            forwarding_method_brace_depth =
                                forwarding_method_brace_depth.saturating_sub(1);
                            if forwarding_method_brace_depth == 0 {
                                inside_forwarding_method = false;
                            }
                        }
                        _ => {}
                    }
                }
            } else if line.contains("self.app.handle_internal_event(")
                && !line.trim().starts_with("///")
                && !line.contains("contains(")
            {
                // Direct call to handle_internal_event outside the forwarding method
                bypass_lines.push(format!("line {}: {}", line_num, line.trim()));
            }
        }

        assert!(
            bypass_lines.is_empty(),
            "Found direct calls to self.app.handle_internal_event outside \
             handle_internal_event_with_forwarding (bypass risk):\n  {}",
            bypass_lines.join("\n  ")
        );
    }
}
