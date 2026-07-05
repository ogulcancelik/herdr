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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyModifiers, MouseEventKind};
use interprocess::local_socket::traits::Listener as _;
#[cfg(windows)]
use interprocess::local_socket::traits::Stream as _;
#[cfg(unix)]
use interprocess::local_socket::ListenerNonblockingMode;
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use base64::Engine;
use bytes::Bytes;

use crate::api;
use crate::app;
use crate::config;
use crate::events::AppEvent;
use crate::ipc::{
    bind_local_listener, remove_socket_file_if_owned, socket_file_identity, LocalListener,
    SocketFileIdentity,
};
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, FrameData, ServerMessage, MAX_FRAME_SIZE,
    MAX_GRAPHICS_FRAME_SIZE,
};
#[cfg(unix)]
use crate::server::client_accept::{
    accept_pending_client_connections, reject_pending_client_connections,
};
use crate::server::client_transport::ServerEvent;
use crate::server::clients::{
    events_include_interaction, latest_app_client, render_targets, terminal_stream_client_ids,
    ClientConnection, ClientConnectionMode,
};
use crate::server::keybindings::{app_keybindings, apply_keybindings};
use crate::server::notifications::{
    should_forward_toast_to_clients, toast_message_from_state_change, toast_notify_kind,
};
use crate::server::socket_paths::{
    client_socket_path, prepare_socket_path, restrict_socket_permissions,
};
use crate::server::terminal_attach::paste_payload_for_runtime;

#[cfg(test)]
use crate::protocol::RenderEncoding;
#[cfg(test)]
use crate::server::client_transport::ClientWriter;
#[cfg(test)]
use std::fs;

fn sound_notify_message(sound: crate::sound::Sound) -> &'static str {
    match sound {
        crate::sound::Sound::Done => "agent done",
        crate::sound::Sound::Request => "agent attention",
    }
}

fn notification_show_response_shown(response: &str) -> bool {
    let Ok(response) = serde_json::from_str::<api::schema::SuccessResponse>(response) else {
        return false;
    };
    matches!(
        response.result,
        api::schema::ResponseResult::NotificationShow { shown: true, .. }
    )
}

fn non_empty_body(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

// ---------------------------------------------------------------------------
// Loop event enum for the headless server event loop
// ---------------------------------------------------------------------------

/// Events that the headless server event loop can process.
enum LoopEvent {
    Timer,
    Internal(AppEvent),
    Api(Box<api::ApiRequestMessage>),
    ServerEvent(ServerEvent),
    RenderRequested,
}

fn rect_fits_frame(rect: Rect, frame: &FrameData) -> bool {
    rect.x.saturating_add(rect.width) <= frame.width
        && rect.y.saturating_add(rect.height) <= frame.height
}

fn apply_terminal_dirty_patch(
    frame: &mut FrameData,
    area: Rect,
    patch: crate::pane::TerminalDirtyPatch,
) -> bool {
    if !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    for (local_y, row_cells) in patch.rows {
        if local_y >= area.height || row_cells.len() != usize::from(area.width) {
            return false;
        }
        let frame_y = area.y + local_y;
        let start = usize::from(frame_y) * width + usize::from(area.x);
        let end = start + usize::from(area.width);
        if end > frame.cells.len() {
            return false;
        }
        frame.cells[start..end].clone_from_slice(&row_cells);
    }
    true
}

fn dirty_patch_intersects_hyperlinks(
    frame: &FrameData,
    area: Rect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> bool {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    for (local_y, _) in &patch.rows {
        if *local_y >= area.height {
            return true;
        }
        let frame_y = area.y + *local_y;
        let start = usize::from(frame_y) * width + usize::from(area.x);
        let end = start + usize::from(area.width);
        if end > frame.cells.len() {
            return true;
        }
        if frame.cells[start..end]
            .iter()
            .any(|cell| cell.hyperlink.is_some())
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default shared runtime size (columns, rows) when no clients are attached.
const MIN_COLS: u16 = 80;
const MIN_ROWS: u16 = 24;

/// Timeout for in-flight API requests during shutdown.
#[allow(dead_code)]
const SHUTDOWN_API_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the idle headless loop wakes to poll the local listener for new
/// client connections.
///
/// The listener is non-blocking and not integrated into `tokio::select!`, so
/// a low-frequency wake is required to notice new thin-client attaches while
/// otherwise idle. Keep this much slower than the old resize-poll cadence to
/// avoid reintroducing the idle CPU spin.
const CLIENT_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Headless server
// ---------------------------------------------------------------------------

/// The headless server — runs the herdr event loop without a real terminal.
pub struct HeadlessServer {
    app: app::App,
    #[cfg(unix)]
    api_tx: Option<api::ApiRequestSender>,
    #[cfg(unix)]
    api_server: Option<api::ServerHandle>,
    #[cfg(unix)]
    client_listener: LocalListener,
    client_socket_path: PathBuf,
    client_socket_identity: SocketFileIdentity,
    clients: HashMap<u64, ClientConnection>,
    #[cfg(unix)]
    next_client_id: u64,
    /// The client currently driving the shared pane runtime size, theme, and input keybindings.
    foreground_client_id: Option<u64>,
    /// Server-owned keybindings, restored when foreground clients use server mode.
    server_keybindings: crate::config::LiveKeybindConfig,
    /// Full server config warning shown to clients that use server keybindings.
    server_config_diagnostic: Option<String>,
    /// Server config warning with keybinding diagnostics removed for local-keybinding clients.
    server_config_diagnostic_without_keybindings: Option<String>,
    /// Writable direct attach owner per terminal id string.
    terminal_attach_owners: HashMap<String, u64>,
    /// Monotonic activity counter used to pick the most recently active client.
    next_activity_stamp: u64,
    /// Shared pane runtime size derived from the foreground client,
    /// or MIN_COLS × MIN_ROWS when no clients are connected.
    effective_size: (u16, u16),
    /// Flag set when shutdown is initiated.
    shutting_down: bool,
    /// Flag set while exporting live PTYs to a replacement server.
    handoff_in_progress: bool,
    /// Imported panes get one app-safe resize nudge after the first client attaches.
    #[cfg(unix)]
    pending_handoff_repaint_nudge: bool,
    /// Flag set by Ctrl+C or `server stop` signal.
    should_quit: Arc<AtomicBool>,
    /// Channel for receiving server events from client connection threads.
    server_event_rx: mpsc::Receiver<ServerEvent>,
    /// Sender for server events (cloned for each client thread).
    server_event_tx: mpsc::Sender<ServerEvent>,
    /// Pure host-link state machine (Task 3): attach/detach and the
    /// Connecting/Connected/Reconnecting/Offline lifecycle. The single
    /// mutation point for it is `handle_host_event`/the `host.*` API
    /// handlers below, mirroring how `self.app.state` is only mutated from
    /// one place.
    #[cfg(unix)]
    host_links: crate::server::host_link::HostLinkRegistry,
    /// Bijection between adopted remote panes and locally-allocated
    /// `PaneId`s (Task 6), reconciled from every `HostEvent::Snapshot`.
    #[cfg(unix)]
    remote_panes: crate::server::remote_pane::RemotePaneRegistry,
    /// The `TerminalId` of the host-tagged `TerminalState` (in
    /// `self.app.state.terminals`) for each adopted remote pane, keyed by the
    /// same `RemotePaneKey` `remote_panes` bijects to a local `PaneId` (Task
    /// 9b). A parallel map -- rather than folding the `TerminalId` into
    /// `RemotePaneRegistry` itself -- keeps that registry a pure
    /// `RemotePaneKey`<->`PaneId` bijection, reusable/testable on its own;
    /// this mirrors how `host_links` (pure link state machine) and
    /// `AppState.host_links` (display projection) already stay two synced
    /// structures rather than one merged type. Maintained ONLY from
    /// `handle_host_event`/its helpers (the single mutation point), in
    /// lockstep with `remote_panes`, `self.app.state.terminals`, and
    /// `self.app.state.remote_pane_display`: a key has an entry here if and
    /// only if it is adopted in `remote_panes`, and the named `TerminalId`
    /// names a host-tagged terminal there. The remote's raw `AgentStatus`
    /// is not stashed here -- it is projected straight into
    /// `remote_pane_display` (via `sync_remote_pane_display`) the moment it
    /// arrives, so nothing on the server side needs to hold it.
    #[cfg(unix)]
    remote_pane_terminals:
        HashMap<crate::server::remote_pane::RemotePaneKey, crate::terminal::TerminalId>,
    /// The remote's OWN terminal id (`RemotePaneInfo::remote_terminal_id`)
    /// for each adopted remote pane, keyed by the same `RemotePaneKey` as
    /// `remote_pane_terminals` (Task 9b, HALF 2). Needed to open
    /// `RemotePaneAttach::attach` against the right remote terminal channel
    /// -- distinct from the LOCAL `TerminalId` `remote_pane_terminals` maps
    /// to, which only identifies this side's host-tagged `TerminalState`.
    /// A second parallel map rather than widening `remote_pane_terminals`'s
    /// value, for the same reason that map stays a thin
    /// `RemotePaneKey`->`TerminalId` association (see its doc comment and
    /// the commit-2 history of trying and reverting a wrapper struct there).
    /// Maintained at the same single mutation point
    /// (`seed_remote_pane_terminal`/`remove_remote_pane_terminal`); the
    /// bijection assert below covers it.
    #[cfg(unix)]
    remote_pane_remote_terminal_ids: HashMap<crate::server::remote_pane::RemotePaneKey, String>,
    /// The `TerminalId` of the remote pane currently focused for live
    /// viewing (Task 9b, HALF 2, VIEW-ONLY: routing local input to it is a
    /// follow-up commit). `None` when no remote pane is focused. Setting
    /// this to `Some` is what `render_and_stream`'s App branch reads to
    /// substitute the focused runtime for the workspace panes, and is also
    /// `focus_remote_pane`'s trigger for the off-loop attach. Mutated only
    /// by `focus_remote_pane`/`blur_focused_remote_pane` and the
    /// `RemotePaneAttachFailed`/`HostEvent::AttachFailed` handlers (all on
    /// the main loop thread).
    #[cfg(unix)]
    focused_remote_pane: Option<crate::terminal::TerminalId>,
    /// The live attach handle for `focused_remote_pane`, once
    /// `RemotePaneAttachEstablished` registers it (`None` while attaching,
    /// unfocused, or torn down). Owns the ssh terminal channel + reader
    /// thread; `blur_focused_remote_pane`/the attach-failed handlers detach
    /// it (graceful `Detach` write, then `Drop`'s force-close + reader-join)
    /// so nothing leaks across a focus change. Not read for input/resize
    /// yet -- routing local input to a focused remote pane is a follow-up
    /// commit; this commit is VIEW-ONLY (typing does not reach the remote
    /// pane), so `send_input`/`resize` stay unreachable from production for
    /// now.
    #[cfg(unix)]
    focused_remote_pane_attach: Option<crate::server::remote_pane::RemotePaneAttach>,
    /// Monotonic counter bumped every time `focus_remote_pane` actually
    /// starts a NEW off-loop attach (not the same-terminal early return).
    /// Stamped onto that attempt's success handback
    /// (`ServerEvent::RemotePaneAttachEstablished::focus_epoch`) so
    /// `handle_remote_pane_attach_established` can reject a stale handback
    /// from an attach attempt superseded by a NEWER attempt on the SAME
    /// `terminal_id` (rapid refocus A -> B -> A) -- `terminal_id` equality
    /// alone (`still_focused`) cannot tell those apart once focus can
    /// return to the terminal a stale attach was already in flight for.
    #[cfg(unix)]
    remote_pane_focus_epoch: u64,
    /// Per-host background-thread handle plus the reusable transport
    /// materials, keyed by the same `HostLinkId` `host_links` uses.
    #[cfg(unix)]
    host_link_runtimes: HashMap<crate::server::host_link::HostLinkId, HostLinkRuntime>,
    /// Monotonic per-attach generation. Bumped on every `host.attach` and
    /// stamped onto that link's event loop (and reused across its reconnect
    /// respawns) so the consumer can drop events from a superseded loop
    /// after a detach+reattach of the same alias.
    #[cfg(unix)]
    next_host_generation: u64,
    /// Cloned into each spawned per-host event loop; forwarded into
    /// `server_event_tx` by one long-lived bridge thread (see `new()`) so
    /// `HostEvent`s reach the main loop through the same async channel
    /// everything else already drains.
    #[cfg(unix)]
    host_event_tx: std::sync::mpsc::Sender<crate::server::remote_pane::HostEventEnvelope>,
    /// Test-only escape hatch: when set, `host.attach` builds its transport
    /// from this closure instead of `SshTransport`, so the host-link
    /// lifecycle can be exercised against an in-process fake/real API
    /// server (`UnixSocketTransport`) without a real `ssh` binary or
    /// network target, and with no config-file I/O. Always `None` in
    /// production.
    #[cfg(unix)]
    host_transport_override_for_test: Option<std::sync::Arc<HostTransportFactory>>,
}

/// How `sync_remote_pane_display` should treat a remote pane's
/// `custom_status` (Task 9b). A Snapshot carries a fresh presentation, so it
/// `Set`s; a bare `StatusChanged` carries no presentation, so it `Keep`s the
/// pane's last-known value rather than clobbering it to `None`.
#[cfg(unix)]
enum CustomStatusUpdate {
    Set(Option<String>),
    Keep,
}

/// Per-host runtime state kept alive by `HeadlessServer` for as long as a
/// host link is attached (or being reconnected).
#[cfg(unix)]
struct HostLinkRuntime {
    /// This loop generation; carried through reconnect respawns and matched
    /// against inbound `HostEventEnvelope`s to drop superseded events.
    generation: u64,
    event_loop: crate::server::remote_pane::HostEventLoopHandle,
    /// Reusable transport materials (holding the `ManagedSshConfig` guard in
    /// production). Built once at attach time and moved forward across every
    /// reconnect respawn, so reconnecting never re-reads config or
    /// re-writes the ssh config.
    transport: HostTransportHandle,
}

/// Everything needed to build a fresh `LinkTransport` for one host link with
/// NO disk I/O -- the config read + managed-ssh-config write happen exactly
/// once, when the handle is first built (`build_host_transport_handle`).
/// `open()` is cheap and infallible and can run once per reconnect attempt.
#[cfg(unix)]
enum HostTransportHandle {
    /// Production: hold the managed ssh config guard alive and rebuild a
    /// cheap `SshTransport` per connection from the cached paths.
    Ssh {
        target: String,
        session_name: String,
        ssh_options: Option<crate::remote::ManagedSshOptions>,
        // Kept alive for the link's whole lifetime; see
        // `host_transport::SshTransport`'s doc comment for why the guard
        // must outlive the transport. `None` when the config opted out of
        // (or failed to write) a managed ssh config.
        _ssh_guard: Option<crate::remote::ManagedSshConfig>,
    },
    /// Test: a factory that builds a fresh in-process transport with no I/O.
    Test {
        host: String,
        factory: std::sync::Arc<HostTransportFactory>,
    },
}

#[cfg(unix)]
impl HostTransportHandle {
    fn open(&self) -> Box<dyn crate::server::host_transport::LinkTransport> {
        match self {
            HostTransportHandle::Ssh {
                target,
                session_name,
                ssh_options,
                ..
            } => Box::new(crate::server::host_transport::SshTransport {
                target: target.clone(),
                remote_herdr: crate::remote::default_installed_remote_herdr(),
                session_name: session_name.clone(),
                ssh_options: ssh_options.clone(),
            }),
            HostTransportHandle::Test { host, factory } => {
                let build: &HostTransportFactory = factory.as_ref();
                build(host)
            }
        }
    }
}

#[cfg(unix)]
type HostTransportFactory =
    dyn Fn(&str) -> Box<dyn crate::server::host_transport::LinkTransport> + Send + Sync;

/// `host.list`/`host.attach`'s `HostInfo.state` snake_case label -- the ONE
/// place `server::host_link::LinkState` is converted to a wire string.
/// `Reconnecting`'s `attempt` is deliberately not mirrored here either
/// (matches the sidebar's `HostLinkDisplayState` choice, see its doc
/// comment).
#[cfg(unix)]
fn link_state_label(state: crate::server::host_link::LinkState) -> &'static str {
    use crate::server::host_link::LinkState;
    match state {
        LinkState::Connecting => "connecting",
        LinkState::Connected => "connected",
        LinkState::Reconnecting { .. } => "reconnecting",
        LinkState::Offline => "offline",
    }
}

/// The ONE place `server::host_link::LinkState` is converted to
/// `app::state::HostLinkDisplayState` for the sidebar (Task 7).
#[cfg(unix)]
fn host_link_display_state(
    state: crate::server::host_link::LinkState,
) -> crate::app::state::HostLinkDisplayState {
    use crate::app::state::HostLinkDisplayState;
    use crate::server::host_link::LinkState;
    match state {
        LinkState::Connecting => HostLinkDisplayState::Connecting,
        LinkState::Connected => HostLinkDisplayState::Connected,
        LinkState::Reconnecting { .. } => HostLinkDisplayState::Reconnecting,
        LinkState::Offline => HostLinkDisplayState::Offline,
    }
}

/// The ONE place `api::schema::AgentStatus` is inverted back to a
/// `crate::detect::AgentState` for a host-tagged `TerminalState` (Task 9b).
/// The exact inverse of `pane_agent_status` (`src/app/api_helpers.rs`):
/// `Done` and `Idle` both forward-map from `AgentState::Idle`, differing
/// only in the local `PaneState.seen` bit (`Done` == idle-and-unseen).
/// Host-tagged terminals adopted from a remote host never have a
/// `PaneState` -- Option B deliberately keeps them out of workspaces -- so
/// there is nowhere to carry that `seen` bit locally; both invert to
/// `AgentState::Idle` and the `Done`/`Idle` distinction is lost across this
/// hop. Working/Blocked/Unknown fold both `seen` values to the same status
/// forward, so inverting them is unambiguous.
#[cfg(unix)]
fn remote_agent_status_to_terminal_state(
    status: api::schema::AgentStatus,
) -> crate::detect::AgentState {
    use crate::detect::AgentState;
    use api::schema::AgentStatus;
    match status {
        AgentStatus::Done | AgentStatus::Idle => AgentState::Idle,
        AgentStatus::Working => AgentState::Working,
        AgentStatus::Blocked => AgentState::Blocked,
        AgentStatus::Unknown => AgentState::Unknown,
    }
}

#[cfg(unix)]
fn host_success_response(id: String, result: api::schema::ResponseResult) -> String {
    serde_json::to_string(&api::schema::SuccessResponse { id, result })
        .unwrap_or_else(|_| "{}".to_string())
}

fn host_error_response(id: String, code: &str, message: String) -> String {
    serde_json::to_string(&api::schema::ErrorResponse {
        id,
        error: api::schema::ErrorBody {
            code: code.into(),
            message,
        },
    })
    .unwrap_or_else(|_| "{}".to_string())
}

#[cfg(not(unix))]
fn unsupported_host_command_response(id: String) -> String {
    host_error_response(
        id,
        "unsupported_platform",
        "host commands are only supported on Unix".to_string(),
    )
}

fn apply_terminal_attach_scroll(
    runtime: &crate::terminal::TerminalRuntime,
    source: AttachScrollSource,
    direction: AttachScrollDirection,
    lines: u16,
    column: Option<u16>,
    row: Option<u16>,
    modifiers: u8,
) -> Result<(), String> {
    let wheel_kind = match direction {
        AttachScrollDirection::Up => MouseEventKind::ScrollUp,
        AttachScrollDirection::Down => MouseEventKind::ScrollDown,
    };
    if let AttachScrollSource::PageKey { input } = source {
        let host_scroll = runtime.input_state().is_some_and(|input_state| {
            !input_state.alternate_screen && !input_state.mouse_reporting_enabled()
        });
        if host_scroll {
            match direction {
                AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
                AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
            }
            return Ok(());
        }
        return apply_terminal_attach_input(runtime, input);
    }

    match runtime.wheel_routing() {
        Some(crate::pane::WheelRouting::MouseReport) => {
            runtime.scroll_reset();
            let column = column.unwrap_or(0);
            let row = row.unwrap_or(0);
            let Some(bytes) = runtime.encode_mouse_wheel(
                wheel_kind,
                column,
                row,
                KeyModifiers::from_bits_truncate(modifiers),
            ) else {
                return Err(format!(
                    "failed to encode terminal attach mouse wheel event: {wheel_kind:?}"
                ));
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach mouse wheel input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::AlternateScroll) => {
            runtime.scroll_reset();
            let Some(bytes) = runtime.encode_alternate_scroll(wheel_kind) else {
                return Ok(());
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach alternate scroll input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::HostScroll) | None => match direction {
            AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
            AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
        },
    }
    Ok(())
}

fn apply_terminal_attach_input(
    runtime: &crate::terminal::TerminalRuntime,
    data: Vec<u8>,
) -> Result<(), String> {
    runtime.scroll_reset();
    runtime
        .try_send_bytes(Bytes::from(data))
        .map_err(|err| format!("terminal attach input failed: {err}"))
}

/// Spawns the one long-lived bridge thread that turns `HostEvent`s from any
/// number of per-host event-loop threads (each holding a clone of the
/// returned sender) into `ServerEvent::HostEvent`s on the main loop's async
/// channel. `spawn_host_event_loop`'s sender is a plain thread-blocking
/// `std::sync::mpsc::Sender` (Task 6, unchanged), so this thread is the
/// seam that lets `tokio::select!`/`drain_server_events` pick host events up
/// without polling. Exits once every std sender (this bridge's clone plus
/// every per-host loop's clone) has dropped.
#[cfg(unix)]
fn spawn_host_event_bridge(
    server_event_tx: mpsc::Sender<ServerEvent>,
) -> std::sync::mpsc::Sender<crate::server::remote_pane::HostEventEnvelope> {
    let (std_tx, std_rx) =
        std::sync::mpsc::channel::<crate::server::remote_pane::HostEventEnvelope>();
    std::thread::spawn(move || {
        while let Ok(envelope) = std_rx.recv() {
            if server_event_tx
                .blocking_send(ServerEvent::HostEvent(envelope))
                .is_err()
            {
                break;
            }
        }
    });
    std_tx
}

#[cfg(windows)]
fn spawn_windows_client_accept_thread(
    listener: LocalListener,
    should_quit: Arc<AtomicBool>,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    std::thread::spawn(move || {
        let mut next_client_id = 1_u64;
        while !should_quit.load(Ordering::Acquire) {
            let stream = match listener.accept() {
                Ok(stream) => stream,
                Err(err) => {
                    if should_quit.load(Ordering::Acquire) {
                        break;
                    }
                    error!(err = %err, "client listener accept failed");
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };

            let client_id = next_client_id;
            next_client_id = next_client_id.saturating_add(1);

            if let Err(err) = stream.set_nonblocking(true) {
                warn!(err = %err, "failed to set client stream nonblocking");
                continue;
            }

            let should_quit = should_quit.clone();
            let server_event_tx = server_event_tx.clone();
            std::thread::spawn(move || {
                if let Err(err) = crate::server::client_transport::handle_client_handshake(
                    stream,
                    client_id,
                    &server_event_tx,
                    &should_quit,
                ) {
                    debug!(client_id, err = %err, "client handshake failed");
                }
            });
        }
    });
}

impl HeadlessServer {
    /// Creates and starts the headless server.
    ///
    /// This:
    /// 1. Prepares the client socket path (cleans up stale sockets)
    /// 2. Binds the client socket listener
    /// 3. Returns the server ready to run
    pub fn new(
        app: app::App,
        config_diagnostics: &[String],
        api_tx: Option<api::ApiRequestSender>,
        api_server: Option<api::ServerHandle>,
    ) -> io::Result<Self> {
        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;

        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        info!(path = %client_path.display(), "client protocol socket listening");

        // Set non-blocking on Unix so we can poll it from the event loop.
        #[cfg(unix)]
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        let should_quit = Arc::new(AtomicBool::new(false));

        // Channel for server events from client threads.
        let (server_event_tx, server_event_rx) = mpsc::channel(64);
        #[cfg(windows)]
        spawn_windows_client_accept_thread(listener, should_quit.clone(), server_event_tx.clone());

        let server_keybindings = app_keybindings(&app);
        let (server_config_diagnostic, server_config_diagnostic_without_keybindings) =
            server_config_diagnostic_summaries(config_diagnostics);
        #[cfg(not(unix))]
        let _ = (&api_tx, &api_server);
        #[cfg(unix)]
        let host_event_tx = spawn_host_event_bridge(server_event_tx.clone());

        Ok(Self {
            app,
            #[cfg(unix)]
            api_tx,
            #[cfg(unix)]
            api_server,
            #[cfg(unix)]
            client_listener: listener,
            client_socket_path: client_path,
            client_socket_identity,
            clients: HashMap::new(),
            #[cfg(unix)]
            next_client_id: 1,
            foreground_client_id: None,
            server_keybindings,
            server_config_diagnostic,
            server_config_diagnostic_without_keybindings,
            terminal_attach_owners: HashMap::new(),
            next_activity_stamp: 1,
            effective_size: (MIN_COLS, MIN_ROWS),
            shutting_down: false,
            handoff_in_progress: false,
            #[cfg(unix)]
            pending_handoff_repaint_nudge: false,
            should_quit,
            server_event_rx,
            server_event_tx,
            #[cfg(unix)]
            host_links: crate::server::host_link::HostLinkRegistry::default(),
            #[cfg(unix)]
            remote_panes: crate::server::remote_pane::RemotePaneRegistry::default(),
            #[cfg(unix)]
            remote_pane_terminals: HashMap::new(),
            #[cfg(unix)]
            remote_pane_remote_terminal_ids: HashMap::new(),
            #[cfg(unix)]
            focused_remote_pane: None,
            #[cfg(unix)]
            focused_remote_pane_attach: None,
            #[cfg(unix)]
            remote_pane_focus_epoch: 0,
            #[cfg(unix)]
            host_link_runtimes: HashMap::new(),
            #[cfg(unix)]
            next_host_generation: 1,
            #[cfg(unix)]
            host_event_tx,
            #[cfg(unix)]
            host_transport_override_for_test: None,
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
        let mut needs_full_render = true;

        loop {
            crate::render_prof::event("loop.tick");
            crate::render_prof::flush_if_due();

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
                crate::render_prof::event("render.request.pty_dirty");
            }

            // 2. Drain a bounded internal-event batch. API handlers perform an
            // exhaustive forwarding-aware drain before reading pane/runtime state.
            if self.drain_internal_events_with_forwarding() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.internal_events");
            }

            // 3. Drain API requests.
            if self.drain_api_requests_with_shutdown_check() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.api_requests");
            }

            self.app.sync_focus_events();
            self.app.sync_session_save_schedule();

            // 4. Accept new client connections.
            self.accept_client_connections()?;

            // 5. Drain server events from client threads.
            if self.drain_server_events() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.server_events");
            }

            // 6. Handle scheduled tasks.
            let now = Instant::now();
            if self.handle_scheduled_tasks_headless(now, needs_render) {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.scheduled_tasks");
            }

            if self.handle_deferred_requests_headless() {
                needs_render = true;
                needs_full_render = true;
            }

            if latest_app_client(&self.clients).is_some() && self.app.ensure_default_workspace() {
                needs_render = true;
                needs_full_render = true;
                crate::render_prof::event("full_render_cause.default_workspace");
            }

            self.drain_client_config_reload_request();
            self.stream_host_mouse_capture_mode();

            self.app.sync_headless_animation_timer(now);

            // 7. Render virtually and stream frames.
            if needs_render && self.app.can_render_now(now) {
                crate::render_prof::event("render.attempt");
                let pty_dirty = self.app.render_dirty.swap(false, Ordering::AcqRel);
                if pty_dirty {
                    crate::render_prof::event("render.attempt.pty_dirty");
                }
                if needs_full_render {
                    crate::render_prof::event("retained_gate.needs_full_render");
                } else if !pty_dirty {
                    crate::render_prof::event("retained_gate.not_pty_dirty");
                }
                let rendered_retained =
                    pty_dirty && !needs_full_render && self.render_retained_pty_update_and_stream();
                if !rendered_retained {
                    crate::render_prof::event("full_render.invoke");
                    self.render_and_stream();
                }
                self.app.last_render_at = Some(now);
                needs_render = false;
                needs_full_render = false;
                continue;
            }

            // 8. Wait for next event.
            let next_deadline = self
                .app
                .next_headless_loop_deadline_with_git_refresh(
                    now,
                    needs_render,
                    self.has_app_client(),
                )
                .map(|deadline| deadline.min(now + CLIENT_ACCEPT_POLL_INTERVAL))
                .or(Some(now + CLIENT_ACCEPT_POLL_INTERVAL));
            let event = {
                tokio::select! {
                    maybe_api = self.app.api_rx.recv() => match maybe_api {
                        Some(msg) => LoopEvent::Api(Box::new(msg)),
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
                        needs_full_render = true;
                    }
                }
                LoopEvent::Api(msg) => {
                    if self.handle_api_request_with_shutdown_check(*msg) {
                        needs_render = true;
                        needs_full_render = true;
                    }
                }
                LoopEvent::ServerEvent(ev) => {
                    if self.handle_server_event(ev) {
                        needs_render = true;
                        needs_full_render = true;
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

    fn handle_deferred_requests_headless(&mut self) -> bool {
        let mut needs_render = false;

        if self.app.state.request_complete_onboarding {
            self.app.state.request_complete_onboarding = false;
            self.app.open_settings_from_onboarding();
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_onboarding");
        }

        if self.app.state.request_new_workspace {
            self.app.state.request_new_workspace = false;
            let response = self.dispatch_headless_runtime_mutation(
                "headless.workspace.create",
                crate::api::schema::Method::WorkspaceCreate(
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: None,
                        focus: true,
                        label: None,
                        env: Default::default(),
                    },
                ),
            );
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create workspace"
                );
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_new_workspace");
        }

        if self.app.state.request_new_tab {
            self.app.state.request_new_tab = false;
            let label = self.app.state.requested_new_tab_name.take();
            let response = self.dispatch_headless_runtime_mutation(
                "headless.tab.create",
                crate::api::schema::Method::TabCreate(crate::api::schema::TabCreateParams {
                    workspace_id: None,
                    cwd: None,
                    focus: true,
                    label,
                    env: Default::default(),
                }),
            );
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create tab"
                );
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_new_tab");
        }

        if let Some(ws_idx) = self.app.state.request_new_linked_worktree.take() {
            self.app.open_new_linked_worktree_dialog(ws_idx);
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }

        if let Some(ws_idx) = self.app.state.request_open_existing_worktree.take() {
            self.app.open_existing_worktree_dialog(ws_idx);
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }

        if let Some(cwd) = self.app.state.request_new_workspace_cwd.take() {
            let response = self.dispatch_headless_runtime_mutation(
                "headless.workspace.create_cwd",
                crate::api::schema::Method::WorkspaceCreate(
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: Some(cwd.display().to_string()),
                        focus: true,
                        label: None,
                        env: Default::default(),
                    },
                ),
            );
            if let Err(error) = response {
                error!(
                    code = %error.code,
                    message = %error.message,
                    "failed to create workspace at requested cwd"
                );
                self.app.state.mode = app::Mode::Navigate;
            }
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_workspace_cwd");
        }

        if let Some(ws_idx) = self.app.state.request_remove_linked_worktree.take() {
            self.app.open_remove_linked_worktree_confirmation(ws_idx);
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_dialog");
        }

        if self.app.state.request_submit_worktree_create {
            self.app.state.request_submit_worktree_create = false;
            self.app.submit_worktree_create_via_api();
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }

        if self.app.state.request_submit_worktree_open {
            self.app.state.request_submit_worktree_open = false;
            self.app.submit_worktree_open_via_api();
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }

        if self.app.state.request_submit_worktree_remove {
            self.app.state.request_submit_worktree_remove = false;
            self.app.submit_worktree_remove_via_api();
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_worktree_submit");
        }

        if self.app.state.request_reload_config {
            self.app.state.request_reload_config = false;
            self.reload_server_config(true);
            needs_render = true;
            crate::render_prof::event("full_render_cause.config_reload");
        }

        // Central per-iteration drain of a remote-pane focus/blur request set
        // by ANY focus path (client input, JSON API pane.focus, plugin
        // action, agent-notification focus). Draining here rather than in a
        // per-path handler is what guarantees a `Blur` set via the API path
        // (which never runs `handle_client_input_events`) still tears down a
        // focused remote pane's attach + runtime. See
        // `apply_requested_remote_pane_focus`.
        if self.apply_requested_remote_pane_focus() {
            needs_render = true;
            crate::render_prof::event("full_render_cause.deferred_remote_pane_focus");
        }

        needs_render
    }

    fn dispatch_headless_runtime_mutation(
        &mut self,
        id: &'static str,
        method: api::schema::Method,
    ) -> Result<(), api::schema::ErrorBody> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        self.handle_api_request_with_shutdown_check_inner(
            api::ApiRequestMessage {
                request: api::schema::Request {
                    id: id.to_string(),
                    method,
                },
                respond_to,
            },
            true,
        );
        match response_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(response) => serde_json::from_str::<api::schema::ErrorResponse>(&response)
                .map(|response| Err(response.error))
                .unwrap_or(Ok(())),
            Err(err) => Err(api::schema::ErrorBody {
                code: "internal_error".into(),
                message: format!("headless runtime mutation response failed: {err}"),
            }),
        }
    }

    fn allocate_activity_stamp(&mut self) -> u64 {
        let stamp = self.next_activity_stamp;
        self.next_activity_stamp = self.next_activity_stamp.saturating_add(1);
        stamp
    }

    fn resize_shared_runtime_to_effective_size(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(true);
    }

    fn resize_shared_runtime_to_effective_size_before_input(&mut self) {
        self.resize_shared_runtime_to_effective_size_with_pending_agent_resumes(false);
    }

    fn resize_shared_runtime_to_effective_size_with_pending_agent_resumes(
        &mut self,
        start_pending_agent_resumes: bool,
    ) {
        if self.foreground_client_id.is_none() {
            return;
        }
        let Some(client_id) = self.foreground_client_id else {
            return;
        };
        let Some(client) = self.clients.get(&client_id) else {
            return;
        };
        let (cols, rows) = self.effective_size;
        let area = Rect::new(0, 0, cols, rows);
        if self.app.state.kitty_graphics_enabled && client.cell_size.is_known() {
            crate::ui::compute_view_with_cell_size(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
                client.cell_size,
            );
        } else {
            crate::ui::compute_view_with_runtime_registry(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
            );
        }

        // Shared runtime size changes affect pane wrapping and foreground-driven
        // rendering semantics. Force one fresh frame to every remaining client
        // even if the next rendered buffer compares equal to its cached frame.
        for client in self.clients.values_mut() {
            client.request_full_redraw();
        }
        if !start_pending_agent_resumes {
            self.app.pending_agent_resume_deadline = None;
            return;
        }
        let now = Instant::now();
        self.app.sync_pending_agent_resume_deadline(now);
        if self
            .app
            .start_pending_agent_resumes(self.app.pending_agent_resume_due(now))
        {
            for client in self.clients.values_mut() {
                client.request_full_redraw();
            }
        }
    }

    fn sync_foreground_client_state(&mut self) {
        let Some(client_id) = self.foreground_client_id else {
            self.effective_size = (MIN_COLS, MIN_ROWS);
            self.app.state.outer_terminal_focus = None;
            let server_keybindings = self.server_keybindings.clone();
            apply_keybindings(&mut self.app, &server_keybindings);
            self.sync_visible_server_config_diagnostic(false);
            return;
        };
        let Some(client) = self.clients.get(&client_id) else {
            self.foreground_client_id = None;
            self.effective_size = (MIN_COLS, MIN_ROWS);
            self.app.state.outer_terminal_focus = None;
            let server_keybindings = self.server_keybindings.clone();
            apply_keybindings(&mut self.app, &server_keybindings);
            self.sync_visible_server_config_diagnostic(false);
            return;
        };

        let terminal_size = client.terminal_size;
        let outer_terminal_focus = client.outer_terminal_focus;
        let host_terminal_theme = client.host_terminal_theme;
        let host_terminal_appearance = client.host_terminal_appearance;
        let host_terminal_appearance_explicit = client.host_terminal_appearance_explicit;
        let uses_local_keybindings = client.keybindings.is_some();
        let keybindings = client
            .keybindings
            .as_deref()
            .unwrap_or(&self.server_keybindings)
            .clone();

        self.effective_size = terminal_size;
        self.app.state.outer_terminal_focus = outer_terminal_focus;
        apply_keybindings(&mut self.app, &keybindings);
        self.sync_visible_server_config_diagnostic(uses_local_keybindings);
        if outer_terminal_focus == Some(true) {
            self.app.state.mark_active_tab_seen();
        }
        self.app.set_host_terminal_appearance_state(
            host_terminal_appearance,
            host_terminal_appearance_explicit,
        );
        self.app.set_host_terminal_theme(host_terminal_theme);
    }

    #[cfg(unix)]
    fn perform_live_handoff(
        &mut self,
        params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        info!("starting live handoff");
        let import_exe = params.import_exe.as_deref().map(std::path::PathBuf::from);
        let socket_path = crate::server::handoff::handoff_socket_path();
        let token = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let listener = match crate::server::handoff::bind_listener(&socket_path) {
            Ok(listener) => listener,
            Err(err) => {
                self.handoff_in_progress = false;
                return Err(err);
            }
        };

        let mut pane_by_terminal = HashMap::new();
        for ws in &self.app.state.workspaces {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    pane_by_terminal.insert(pane.attached_terminal_id.clone(), pane_id.raw());
                }
            }
        }
        if pane_by_terminal.len() > crate::server::handoff::MAX_FDS_PER_HANDOFF {
            let _ = std::fs::remove_file(&socket_path);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "live handoff supports at most {} panes in one update; close panes or restart herdr normally",
                    crate::server::handoff::MAX_FDS_PER_HANDOFF
                ),
            ));
        }

        self.handoff_in_progress = true;
        self.disconnect_all_clients_for_handoff();
        let _ = reject_pending_client_connections(&self.client_listener);

        let mut paused_terminal_ids = Vec::new();
        for terminal_id in pane_by_terminal.keys() {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                if let Err(err) = runtime.pause_handoff_reader(Duration::from_secs(2)) {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(err);
                }
                paused_terminal_ids.push(terminal_id.clone());
            }
        }

        let snapshot = crate::persist::capture(
            &self.app.state.workspaces,
            &self.app.state.terminals,
            &self.app.terminal_runtimes,
            self.app.state.active,
            self.app.state.selected,
            self.app.state.sidebar_width,
            self.app.state.sidebar_section_split,
            self.app.state.collapsed_space_keys.clone(),
            // Captured from the authoritative `HostLinkRegistry` (not the
            // Task 7 display projection) since it is in scope here. The
            // handoff-import server re-attaches these on the receiving side
            // (`run_handoff_import_server`), mirroring how `run_server`
            // re-attaches from a session.json restore, so a live update no
            // longer silently drops remote host links.
            crate::persist::HostSnapshot::from_host_ids(
                self.host_links.iter().map(|link| link.id.0.clone()),
            ),
        );

        let mut handoff_entries = Vec::new();
        for (terminal_id, runtime) in self.app.terminal_runtimes.iter() {
            let Some(pane_id) = pane_by_terminal.get(terminal_id).copied() else {
                continue;
            };
            let mut handoff_runtime = runtime.handoff_runtime_state(pane_id);
            let has_agent_session = self
                .app
                .state
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| terminal.persisted_agent_session.is_some());
            if !has_agent_session {
                handoff_runtime.initial_history_ansi = runtime.handoff_history_ansi();
            }
            handoff_entries.push((terminal_id.clone(), handoff_runtime));
        }

        let panes = handoff_entries
            .iter()
            .map(|(_, runtime)| runtime.clone())
            .collect();
        let manifest = crate::server::handoff::manifest_for(
            snapshot,
            panes,
            params.expected_protocol,
            params.expected_version,
        );
        let mut import_child = match crate::server::handoff::spawn_handoff_import(
            import_exe.as_deref(),
            &socket_path,
            &token,
        ) {
            Ok(child) => child,
            Err(err) => {
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };
        let child_pid = import_child.id();
        info!(pid = child_pid, socket = %socket_path.display(), "spawned handoff import server");

        let mut fds = Vec::new();
        let duplicate_result = (|| {
            for (terminal_id, _) in &handoff_entries {
                let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) else {
                    continue;
                };
                fds.push(runtime.duplicate_handoff_fd()?);
            }
            Ok::<(), io::Error>(())
        })();
        if let Err(err) = duplicate_result {
            for fd in fds {
                let _ = unsafe { libc::close(fd) };
            }
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
            return Err(err);
        }

        let mut stream = match crate::server::handoff::accept_and_validate_on(
            listener,
            &socket_path,
            &token,
            &manifest,
        ) {
            Ok(stream) => stream,
            Err(err) => {
                for fd in fds {
                    let _ = unsafe { libc::close(fd) };
                }
                crate::server::handoff::cleanup_failed_import_child(&mut import_child);
                self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                return Err(err);
            }
        };

        let send_result = crate::server::handoff::send_fds_and_wait_restored(&mut stream, &fds);
        for fd in fds {
            let _ = unsafe { libc::close(fd) };
        }
        if let Err(err) = send_result {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
            return Err(err);
        }

        if let Some(api_server) = &self.api_server {
            let _ = api_server.remove_socket_file_if_owned();
        } else {
            let _ = std::fs::remove_file(crate::api::socket_path());
        }
        let _ = remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity);
        if let Err(err) = crate::server::handoff::wait_ready(&mut stream) {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            match self.wait_then_restore_public_sockets_after_failed_handoff() {
                Ok(()) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                }
                Err(restore_err) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(io::Error::other(format!(
                        "handoff replacement server did not become ready: {err}; old server could not restore public sockets: {restore_err}"
                    )));
                }
            }
            return Err(io::Error::other(format!(
                "handoff replacement server did not become ready: {err}"
            )));
        }
        if let Err(err) = crate::server::handoff::report_committed(&mut stream) {
            crate::server::handoff::cleanup_failed_import_child(&mut import_child);
            match self.wait_then_restore_public_sockets_after_failed_handoff() {
                Ok(()) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                }
                Err(restore_err) => {
                    self.rollback_handoff_before_commit(&socket_path, &paused_terminal_ids);
                    return Err(io::Error::other(format!(
                        "handoff replacement server was ready, but commit failed: {err}; old server could not restore public sockets: {restore_err}"
                    )));
                }
            }
            return Err(err);
        }

        for (terminal_id, runtime) in self.app.terminal_runtimes.drain_for_handoff() {
            if !pane_by_terminal.contains_key(&terminal_id) {
                continue;
            }
            debug!(terminal = %terminal_id, "preserving pane runtime for handoff");
            runtime.preserve_for_handoff();
        }
        crate::server::handoff::wait_owned_ack(&mut stream);

        self.shutting_down = true;
        self.app.state.should_quit = true;
        self.app.no_session = true;
        info!("live handoff completed; old server exiting");
        Ok(())
    }

    #[cfg(not(unix))]
    fn perform_live_handoff(
        &mut self,
        _params: crate::api::schema::ServerLiveHandoffParams,
    ) -> io::Result<()> {
        Err(io::Error::other("live handoff is only supported on Unix"))
    }

    // -----------------------------------------------------------------
    // host.* API handlers + HostEvent consumer (Task 9)
    //
    // `host_links` (Task 3), `remote_panes` (Task 6), and
    // `AppState.host_links` (Task 7) are ALL mutated only from the methods
    // below -- this is the single mutation point the multi-host plan calls
    // for, mirroring how `self.app.state` elsewhere is only ever touched
    // from the main loop. `handle_host_event` is reached exclusively via
    // `ServerEvent::HostEvent`, which itself is only produced by the one
    // bridge thread spawned in `new()`; nothing else sends it.
    // -----------------------------------------------------------------

    /// Core host-attach lifecycle: validate the target, register it in
    /// `host_links`, allocate a fresh generation, build the transport
    /// materials once, and spawn the per-host poll loop. Shared by
    /// `handle_host_attach_api` (`host.attach`) and `restore_attached_hosts`
    /// (Task 10 session restore at server startup) so there is exactly one
    /// place that drives this lifecycle -- restore is just a fresh attach
    /// per persisted host, with no special-casing for an unreachable host
    /// (it degrades through the normal Connecting -> Reconnecting -> Offline
    /// path like any other attach).
    #[cfg(unix)]
    fn attach_host(
        &mut self,
        host: String,
    ) -> Result<api::schema::HostInfo, (&'static str, String)> {
        if let Err(err) = crate::remote::validate_remote_target(&host) {
            return Err(("invalid_request", err));
        }
        let link_id = crate::server::host_link::HostLinkId(host.clone());
        if self.host_links.attach(link_id.clone()).is_err() {
            return Err((
                "already_attached",
                format!("host '{host}' is already attached"),
            ));
        }
        let generation = self.alloc_host_generation();
        debug!(host = %host, generation, "attaching host link");
        // Build the transport materials ONCE here (config read + managed
        // ssh-config write), then reuse them across every reconnect respawn
        // of this link -- reconnecting must not stall the main loop on
        // per-attempt disk I/O.
        let transport = self.build_host_transport_handle(&host);
        self.sync_host_link_display(&link_id);
        self.spawn_host_link_runtime(link_id.clone(), generation, transport);
        Ok(self.host_info_for(&link_id))
    }

    #[cfg(unix)]
    fn handle_host_attach_api(
        &mut self,
        id: String,
        params: api::schema::HostAttachParams,
    ) -> String {
        match self.attach_host(params.host) {
            Ok(info) => {
                host_success_response(id, api::schema::ResponseResult::HostAttached { host: info })
            }
            Err((code, message)) => host_error_response(id, code, message),
        }
    }

    /// Re-attaches every host persisted in the restored session snapshot
    /// (Task 10), reusing `attach_host` so restore never duplicates the
    /// `host.attach` lifecycle. Called once from `run_server` right after
    /// the `HeadlessServer` (and its `host_event_tx` bridge) is constructed.
    /// An attach failure here (e.g. a host removed from ssh config since the
    /// session was saved) is logged and skipped -- it does not stop the
    /// server from starting, matching how host.attach failures never take
    /// down the whole process.
    #[cfg(unix)]
    fn restore_attached_hosts(&mut self, hosts: Vec<crate::persist::HostSnapshot>) {
        for host in hosts {
            if let Err((code, message)) = self.attach_host(host.host.clone()) {
                warn!(
                    host = %host.host,
                    code,
                    message = %message,
                    "failed to reattach persisted host at restore"
                );
            }
        }
    }

    /// Non-unix builds have no host-link machinery at all (see
    /// `HostLinkRegistry`'s module doc); a persisted host list is simply
    /// dropped with a warning instead of silently vanishing.
    #[cfg(not(unix))]
    fn restore_attached_hosts(&mut self, hosts: Vec<crate::persist::HostSnapshot>) {
        if !hosts.is_empty() {
            warn!(
                count = hosts.len(),
                "host links are only supported on Unix; not reattaching persisted hosts"
            );
        }
    }

    /// Allocates the next per-attach generation (see
    /// [`HeadlessServer::next_host_generation`]).
    #[cfg(unix)]
    fn alloc_host_generation(&mut self) -> u64 {
        let generation = self.next_host_generation;
        self.next_host_generation = self.next_host_generation.saturating_add(1);
        generation
    }

    /// The current loop generation of an attached host, for the
    /// generation-drop test to craft a stale-generation envelope.
    #[cfg(all(test, unix))]
    fn host_generation_for_test(&self, host: &str) -> Option<u64> {
        self.host_link_runtimes
            .get(&crate::server::host_link::HostLinkId(host.to_string()))
            .map(|runtime| runtime.generation)
    }

    #[cfg(not(unix))]
    fn handle_host_attach_api(
        &mut self,
        id: String,
        _params: api::schema::HostAttachParams,
    ) -> String {
        unsupported_host_command_response(id)
    }

    #[cfg(unix)]
    fn handle_host_list_api(&self, id: String) -> String {
        let remote_panes = &self.remote_panes;
        let hosts: Vec<api::schema::HostInfo> = self
            .host_links
            .iter()
            .map(|link| api::schema::HostInfo {
                host: link.id.0.clone(),
                state: link_state_label(link.state).to_string(),
                pane_count: remote_panes.panes_for_host(&link.id).count() as u32,
            })
            .collect();
        host_success_response(id, api::schema::ResponseResult::HostList { hosts })
    }

    #[cfg(not(unix))]
    fn handle_host_list_api(&self, id: String) -> String {
        unsupported_host_command_response(id)
    }

    #[cfg(unix)]
    fn handle_host_detach_api(
        &mut self,
        id: String,
        params: api::schema::HostDetachParams,
    ) -> String {
        let host = params.host;
        let link_id = crate::server::host_link::HostLinkId(host.clone());
        if !self.host_links.detach(&link_id) {
            return host_error_response(id, "not_found", format!("host '{host}' is not attached"));
        }
        debug!(host = %host, "detaching host link");
        self.stop_host_link_runtime(&link_id);
        // Capture the keys before releasing: `release_host` drops the
        // registry's own record of them, and `remote_pane_terminals` is
        // keyed by `RemotePaneKey`, not by the released `PaneId`s it
        // returns.
        let released_keys: Vec<crate::server::remote_pane::RemotePaneKey> = self
            .remote_panes
            .panes_for_host(&link_id)
            .map(|(key, _)| key.clone())
            .collect();
        self.remote_panes.release_host(&link_id);
        for key in released_keys {
            self.remove_remote_pane_terminal(&key);
        }
        self.app
            .state
            .host_links
            .remove(&crate::terminal::TerminalHostTag::new(host.clone()));
        host_success_response(
            id,
            api::schema::ResponseResult::HostDetached {
                host,
                detached: true,
            },
        )
    }

    #[cfg(not(unix))]
    fn handle_host_detach_api(
        &mut self,
        id: String,
        _params: api::schema::HostDetachParams,
    ) -> String {
        unsupported_host_command_response(id)
    }

    /// `host.list`'s `HostInfo` for one just-attached (or already-attached)
    /// link: reads back through the same registries `handle_host_list_api`
    /// does, so `host.attach`'s response and a subsequent `host.list` never
    /// disagree.
    #[cfg(unix)]
    fn host_info_for(&self, id: &crate::server::host_link::HostLinkId) -> api::schema::HostInfo {
        api::schema::HostInfo {
            host: id.0.clone(),
            state: self
                .host_links
                .state(id)
                .map(link_state_label)
                .unwrap_or("offline")
                .to_string(),
            pane_count: self.remote_panes.panes_for_host(id).count() as u32,
        }
    }

    /// Builds the reusable transport materials for one host link, doing the
    /// config read + managed-ssh-config write exactly once. Production yields
    /// an `Ssh` handle that owns the `ManagedSshConfig` guard; a test
    /// override yields a `Test` handle wrapping the in-process factory. The
    /// returned handle's `open()` is cheap + infallible and is called once
    /// per reconnect attempt WITHOUT repeating this I/O.
    #[cfg(unix)]
    fn build_host_transport_handle(&self, host: &str) -> HostTransportHandle {
        if let Some(factory) = &self.host_transport_override_for_test {
            return HostTransportHandle::Test {
                host: host.to_string(),
                factory: std::sync::Arc::clone(factory),
            };
        }
        let manage_ssh_config = crate::config::Config::load()
            .config
            .remote
            .manage_ssh_config;
        let ssh_guard = manage_ssh_config
            .then(|| crate::remote::write_managed_ssh_config().ok())
            .flatten();
        let ssh_options = ssh_guard.as_ref().map(|guard| guard.options().clone());
        let session_name = crate::session::active_name()
            .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
        HostTransportHandle::Ssh {
            target: host.to_string(),
            session_name,
            ssh_options,
            _ssh_guard: ssh_guard,
        }
    }

    /// Opens a fresh transport from the (already-built, no-I/O) handle and
    /// spawns a new per-host event loop stamped with `generation`, recording
    /// its handle in `host_link_runtimes`. Called both from `host.attach`
    /// and, for a reconnect attempt, from `handle_host_link_down` (which
    /// moves the same handle + generation forward).
    #[cfg(unix)]
    fn spawn_host_link_runtime(
        &mut self,
        host: crate::server::host_link::HostLinkId,
        generation: u64,
        transport: HostTransportHandle,
    ) {
        let event_loop = crate::server::remote_pane::spawn_host_event_loop(
            host.clone(),
            transport.open(),
            generation,
            self.host_event_tx.clone(),
        );
        self.host_link_runtimes.insert(
            host,
            HostLinkRuntime {
                generation,
                event_loop,
                transport,
            },
        );
    }

    /// Stops and joins the per-host loop thread (if any) and drops its
    /// transport materials (and ssh guard). Used by `host.detach`;
    /// `handle_host_link_down` does its own remove+join since a `LinkDown`
    /// loop has already exited.
    #[cfg(unix)]
    fn stop_host_link_runtime(&mut self, host: &crate::server::host_link::HostLinkId) {
        if let Some(runtime) = self.host_link_runtimes.remove(host) {
            runtime.event_loop.stop();
            runtime.event_loop.join();
        }
    }

    /// Stops and joins every per-host loop thread on shutdown, for symmetry
    /// with the careful stop+join `host.detach` does per link (so the
    /// per-host threads and their ssh children don't outlive the server as
    /// detached threads relying on process exit).
    #[cfg(unix)]
    fn stop_all_host_link_runtimes(&mut self) {
        for (_, runtime) in self.host_link_runtimes.drain() {
            runtime.event_loop.stop();
            runtime.event_loop.join();
        }
    }

    /// The single `HostEvent` consumer. Reached only via
    /// `ServerEvent::HostEvent`, itself only produced by the bridge thread
    /// in `new()` -- see the module doc above `handle_host_attach_api`.
    #[cfg(unix)]
    fn handle_host_event(&mut self, envelope: crate::server::remote_pane::HostEventEnvelope) {
        use crate::server::remote_pane::HostEvent;

        // Generation guard: drop any event whose stamped generation no
        // longer matches the emitting host's current loop generation. This
        // closes the detach+reattach race -- a `Snapshot` the *old* loop
        // sent before `stop()` was observed can still be queued when the
        // same alias is re-attached; without this it would reconcile
        // against the new generation's registry (and, once frame streaming
        // lands, route stale pane ids to the wrong terminal after a remote
        // restart re-mints ids from zero). Defense in depth with the
        // `state()`-none check in the `Snapshot` arm below.
        let host = envelope.event.host().clone();
        match self.host_link_runtimes.get(&host) {
            Some(runtime) if runtime.generation == envelope.generation => {}
            _ => {
                debug!(
                    host = %host.0,
                    generation = envelope.generation,
                    "dropping host event from a superseded link generation"
                );
                return;
            }
        }

        match envelope.event {
            HostEvent::Snapshot { host, panes } => {
                // `fetch_snapshot` (remote_pane.rs) can return a snapshot
                // just before `stop()` is observed -- `events_tx.send` isn't
                // itself gated on the stop flag -- so a `host.detach` (with
                // no re-attach) can race a just-completed round trip. The
                // generation guard above catches the detach+reattach case;
                // this catches the plain detach case (no runtime, so no
                // generation to match -- already returned above -- but keep
                // this as a belt-and-braces check in case the runtime is
                // present while the link state was cleared).
                if self.host_links.state(&host).is_none() {
                    return;
                }
                // Authoritative reconciliation (mandatory consumer contract
                // from the Task 6/7 reviews): adopt panes new to the
                // registry, release registered panes missing from this
                // snapshot, and re-seed link state from EVERY snapshot, not
                // just the first.
                self.host_links.on_connected(&host);
                self.sync_host_link_display(&host);
                self.reconcile_remote_pane_snapshot(&host, panes);
            }
            HostEvent::StatusChanged {
                host,
                remote_pane_id,
                status,
            } => {
                // Look the pane up via `remote_pane_terminals` -- never
                // adopt on a status event, only Snapshot reconciliation
                // adopts. Unknown/unmapped key (e.g. a status update racing
                // a release): no-op, idempotent and tolerant.
                let key = crate::server::remote_pane::RemotePaneKey {
                    host,
                    remote_pane_id,
                };
                if let Some(terminal_id) = self.remote_pane_terminals.get(&key).cloned() {
                    if let Some(terminal) = self.app.state.terminals.get_mut(&terminal_id) {
                        terminal.state = remote_agent_status_to_terminal_state(status);
                    }
                    // Status-only update: refresh `seen`, keep the last
                    // snapshot's `custom_status` (the wire event carries none).
                    self.sync_remote_pane_display(&terminal_id, status, CustomStatusUpdate::Keep);
                }
            }
            HostEvent::PaneClosed {
                host,
                remote_pane_id,
            } => {
                // Idempotent + unknown-tolerant (mandatory consumer
                // contract): `release` is a no-op for an already-released or
                // never-adopted key.
                let key = crate::server::remote_pane::RemotePaneKey {
                    host,
                    remote_pane_id,
                };
                self.remote_panes.release(&key);
                self.remove_remote_pane_terminal(&key);
            }
            HostEvent::LinkDown { host } => {
                self.handle_host_link_down(host);
            }
            HostEvent::TerminalBytes {
                host: _,
                local_pane,
                width,
                height,
                bytes,
            } => {
                self.feed_remote_pane_terminal_bytes(local_pane, width, height, &bytes);
            }
            HostEvent::AttachFailed {
                host,
                local_pane,
                reason,
            } => {
                self.handle_remote_pane_attach_failed_event(host, local_pane, reason);
            }
        }
    }

    /// Reconciles `RemotePaneRegistry` against one authoritative snapshot:
    /// adopts panes new to it, releases registered panes for `host` that are
    /// absent from it. This is the sole close-path after a remote
    /// SetupRefresh and the sole old-id retirement path after a
    /// cross-workspace `pane.move` on the remote (mandatory consumer
    /// contract from the Task 6/7 reviews).
    #[cfg(unix)]
    fn reconcile_remote_pane_snapshot(
        &mut self,
        host: &crate::server::host_link::HostLinkId,
        panes: Vec<crate::server::remote_pane::RemotePaneInfo>,
    ) {
        let incoming: std::collections::HashSet<&str> = panes
            .iter()
            .map(|pane| pane.remote_pane_id.as_str())
            .collect();
        let stale: Vec<crate::server::remote_pane::RemotePaneKey> = self
            .remote_panes
            .panes_for_host(host)
            .filter(|(key, _)| !incoming.contains(key.remote_pane_id.as_str()))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.remote_panes.release(&key);
            self.remove_remote_pane_terminal(&key);
        }
        for pane in panes {
            let key = crate::server::remote_pane::RemotePaneKey {
                host: host.clone(),
                remote_pane_id: pane.remote_pane_id.clone(),
            };
            self.remote_panes
                .adopt(key.clone(), crate::layout::PaneId::alloc);
            // Re-seed status/label from EVERY snapshot, not just on first
            // adoption -- the authoritative reconciliation contract applies
            // to a pane's presentation the same way it does to the adopted
            // set itself.
            self.seed_remote_pane_terminal(key, pane);
        }
    }

    /// Creates (on first adoption) or re-seeds (on every later snapshot)
    /// the host-tagged `TerminalState` for one adopted remote pane. `key`
    /// must already be adopted in `remote_panes`; this maintains the
    /// `TerminalId` half of the bijection (`remote_pane_terminals` +
    /// `self.app.state.terminals`), the remote's own terminal id
    /// (`remote_pane_remote_terminal_ids`, needed by `focus_remote_pane` to
    /// open `RemotePaneAttach::attach` against the right remote terminal),
    /// and, at the same mutation point, the sidebar's
    /// `AppState::remote_pane_display` projection.
    #[cfg(unix)]
    fn seed_remote_pane_terminal(
        &mut self,
        key: crate::server::remote_pane::RemotePaneKey,
        pane: crate::server::remote_pane::RemotePaneInfo,
    ) {
        let host_tag = crate::terminal::TerminalHostTag::new(key.host.0.clone());
        self.remote_pane_remote_terminal_ids
            .insert(key.clone(), pane.remote_terminal_id.clone());
        let terminal_id = self
            .remote_pane_terminals
            .entry(key)
            .or_insert_with(crate::terminal::TerminalId::alloc)
            .clone();

        let terminal = self
            .app
            .state
            .terminals
            .entry(terminal_id.clone())
            .or_insert_with(|| {
                crate::terminal::TerminalState::new(terminal_id.clone(), PathBuf::new())
            });
        terminal.host = Some(host_tag);
        terminal.state = remote_agent_status_to_terminal_state(pane.agent_status);
        // Seeded from whichever field a local agent pane would populate
        // first: `pane_details` (src/workspace/aggregate.rs) checks
        // `terminal.agent_name` before falling back to
        // `effective_agent_label()` (hook/detection authority, which an
        // adopted remote pane never has locally). Unlike a local pane,
        // `agent_name` being `None` here does NOT hide the entry -- the
        // sidebar gives host-tagged terminals their own inclusion rule (a
        // placeholder label) instead of reusing `pane_details`' drop-if-
        // no-label filter, since a remote agent that hasn't been identified
        // yet must not vanish. Prefer the raw agent identity (`agent`, the
        // remote's own `effective_agent_label`) over the presentation
        // override (`display_agent`) over the manual pane label (`label`),
        // so a real agent identity wins when more than one is present.
        match pane.agent.or(pane.display_agent).or(pane.label) {
            Some(label) => terminal.set_agent_name(label),
            None => terminal.clear_agent_name(),
        }

        // A Snapshot carries a fresh presentation, so overwrite custom_status.
        self.sync_remote_pane_display(
            &terminal_id,
            pane.agent_status,
            CustomStatusUpdate::Set(pane.custom_status),
        );
    }

    /// The single place a remote pane's status is projected into
    /// `AppState::remote_pane_display` for the sidebar (Task 9b). Called from
    /// `seed_remote_pane_terminal` (every Snapshot re-seed) and the
    /// `StatusChanged` arm of `handle_host_event`. `seen` is always recomputed
    /// from the incoming raw `status` (the one bit `TerminalState::state`
    /// cannot carry -- `Done` vs `Idle`). `custom_status` is only carried by a
    /// Snapshot, so a bare `StatusChanged` passes [`CustomStatusUpdate::Keep`]
    /// to preserve the pane's last-known value rather than clobbering it to
    /// `None`.
    #[cfg(unix)]
    fn sync_remote_pane_display(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        status: api::schema::AgentStatus,
        custom_status: CustomStatusUpdate,
    ) {
        let seen = status != api::schema::AgentStatus::Done;
        let entry = self
            .app
            .state
            .remote_pane_display
            .entry(terminal_id.clone())
            .or_insert_with(|| crate::app::state::RemotePaneDisplay {
                seen,
                custom_status: None,
            });
        entry.seen = seen;
        if let CustomStatusUpdate::Set(custom_status) = custom_status {
            entry.custom_status = custom_status;
        }
    }

    /// Removes the host-tagged `TerminalState` (if any) tracked for a
    /// released/absent remote pane, keeping `remote_pane_terminals`,
    /// `remote_pane_remote_terminal_ids`, `self.app.state.terminals`, and
    /// `self.app.state.remote_pane_display` in lockstep with
    /// `remote_panes`. Idempotent: a key with no tracked terminal is a
    /// no-op. If the removed terminal happens to be the currently focused
    /// remote pane (e.g. it closed on the remote, or a `Snapshot`
    /// reconciliation retired it, while focused), blurs it first so the
    /// live attach + remote-fed runtime don't outlive the pane they were
    /// for -- this is a normal pane-closed path, not the link-down/input
    /// coordination deferred to a follow-up commit.
    #[cfg(unix)]
    fn remove_remote_pane_terminal(&mut self, key: &crate::server::remote_pane::RemotePaneKey) {
        self.remote_pane_remote_terminal_ids.remove(key);
        if let Some(terminal_id) = self.remote_pane_terminals.remove(key) {
            if self.focused_remote_pane.as_ref() == Some(&terminal_id) {
                self.blur_focused_remote_pane();
            }
            self.app.state.terminals.remove(&terminal_id);
            self.app.state.remote_pane_display.remove(&terminal_id);
        }
    }

    /// Reverse lookup: the `RemotePaneKey` for an already-adopted
    /// host-tagged terminal, by scanning `remote_pane_terminals`. `focus_
    /// remote_pane` is the only production caller; the map is small (one
    /// entry per adopted remote pane) so a linear scan is fine.
    #[cfg(unix)]
    fn remote_pane_key_for_terminal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
    ) -> Option<crate::server::remote_pane::RemotePaneKey> {
        self.remote_pane_terminals
            .iter()
            .find(|(_, tid)| *tid == terminal_id)
            .map(|(key, _)| key.clone())
    }

    /// `HostEvent::TerminalBytes` handler: sizes and feeds the focused
    /// remote pane's runtime. A workspace-less runtime is never in
    /// `pane_infos`, so nothing else resizes it -- this handler is
    /// deliberately the only place that does. `width`/`height` are the
    /// remote's actual post-clamp frame dimensions (see the `HostEvent::
    /// TerminalBytes` doc comment), which is why the local grid is sized
    /// FROM them rather than from whatever this side originally requested.
    /// `TerminalRuntime::resize` no-ops when the size is unchanged, so
    /// calling it every frame is cheap. Guard: no runtime registered (not
    /// attached, or attaching but not established yet) -> drop, documented,
    /// no-op.
    #[cfg(unix)]
    fn feed_remote_pane_terminal_bytes(
        &mut self,
        local_pane: crate::layout::PaneId,
        width: u16,
        height: u16,
        bytes: &[u8],
    ) {
        let Some(key) = self.remote_panes.key_for(local_pane).cloned() else {
            return;
        };
        let Some(terminal_id) = self.remote_pane_terminals.get(&key).cloned() else {
            return;
        };
        let Some(runtime) = self.app.terminal_runtimes.get(&terminal_id) else {
            return;
        };
        runtime.resize(height, width, 0, 0);
        runtime.feed_remote_bytes(bytes);
    }

    /// The runtime to render in place of the workspace panes when a remote
    /// pane is focused (Task 9b, HALF 2, VIEW-ONLY). `None` whenever there
    /// is nothing to substitute: unfocused, or focused but still attaching
    /// (no `RemotePaneAttachEstablished` yet) -- in that window the
    /// workspace panes keep rendering normally, which is the honest state
    /// (the remote grid genuinely isn't available yet). Always `None` on
    /// non-Unix (multi-host is unix-only).
    #[cfg(unix)]
    fn focused_remote_pane_render_target(&self) -> Option<&crate::terminal::TerminalRuntime> {
        let terminal_id = self.focused_remote_pane.as_ref()?;
        self.app.terminal_runtimes.get(terminal_id)
    }

    #[cfg(not(unix))]
    fn focused_remote_pane_render_target(&self) -> Option<&crate::terminal::TerminalRuntime> {
        None
    }

    /// Focuses one remote pane for live viewing (Task 9b, HALF 2,
    /// VIEW-ONLY: typing does not reach the remote pane yet -- that is a
    /// follow-up commit). Tears down any previously-focused remote pane
    /// first (`blur_focused_remote_pane`), so a rapid refocus never leaves
    /// two live attaches/runtimes around.
    ///
    /// Reached from production via `AppState::requested_remote_pane_focus`:
    /// a sidebar click or keyboard nav resolving an `AgentFocusTarget::
    /// Remote` sets that field, and `apply_requested_remote_pane_focus`
    /// (drained once per main-loop iteration from
    /// `handle_deferred_requests_headless`) calls this. Also exercised
    /// directly by this module's own `host_lifecycle` tests (a genuine
    /// focus -> attach -> render -> blur round trip, including against a
    /// real second server for the underlying attach machinery).
    ///
    /// Looks up the adopted pane's host/local-`PaneId`/remote-terminal-id,
    /// then spawns a detached thread that performs the (up to 60s) blocking
    /// ssh handshake via `RemotePaneAttach::attach` OFF the main loop --
    /// `attach()` itself must never run on the thread driving the event
    /// loop. The established attach (or failure) is handed back via
    /// `ServerEvent::RemotePaneAttachEstablished`/
    /// `RemotePaneAttachFailed`, whose handlers are the only points that
    /// mutate `AppState`/`terminal_runtimes` for this (single mutation
    /// point discipline, same as every other host-event path). The sink
    /// handed to `attach()` is stamped with the host link's CURRENT
    /// generation, so the reader thread's `TerminalBytes`/`AttachFailed`
    /// events reach `handle_host_event`'s generation guard correctly
    /// stamped instead of being silently dropped. `remote_pane_focus_epoch`
    /// is bumped here (NOT on the same-terminal early return above) and
    /// captured into the handback so a stale same-terminal handback from a
    /// superseded attach attempt (rapid refocus A -> B -> A) can be told
    /// apart from the current one -- see the field's doc comment.
    #[cfg(unix)]
    fn focus_remote_pane(&mut self, terminal_id: crate::terminal::TerminalId) {
        if self.focused_remote_pane.as_ref() == Some(&terminal_id) {
            return;
        }
        self.blur_focused_remote_pane();

        let Some(key) = self.remote_pane_key_for_terminal(&terminal_id) else {
            debug!(terminal = %terminal_id, "focus_remote_pane: not an adopted remote pane terminal");
            return;
        };
        let Some(local_pane) = self.remote_panes.local_for(&key) else {
            debug!(terminal = %terminal_id, "focus_remote_pane: remote pane key has no adopted local pane");
            return;
        };
        let Some(remote_terminal_id) = self.remote_pane_remote_terminal_ids.get(&key).cloned()
        else {
            debug!(terminal = %terminal_id, "focus_remote_pane: no remote terminal id recorded for this pane");
            return;
        };
        let Some(runtime) = self.host_link_runtimes.get(&key.host) else {
            debug!(host = %key.host.0, "focus_remote_pane: host link has no active runtime");
            return;
        };
        let generation = runtime.generation;
        let transport = runtime.transport.open();
        let sink =
            crate::server::remote_pane::HostEventSink::new(generation, self.host_event_tx.clone());
        let server_event_tx = self.server_event_tx.clone();
        let host = key.host.clone();
        let (cols, rows) = self.effective_size;

        self.remote_pane_focus_epoch = self.remote_pane_focus_epoch.wrapping_add(1);
        let focus_epoch = self.remote_pane_focus_epoch;

        debug!(
            host = %host.0,
            terminal = %terminal_id,
            pane = local_pane.raw(),
            focus_epoch,
            "focusing remote pane; attaching off the main loop"
        );
        self.focused_remote_pane = Some(terminal_id.clone());

        std::thread::spawn(move || {
            match crate::server::remote_pane::RemotePaneAttach::attach(
                host.clone(),
                local_pane,
                remote_terminal_id,
                cols,
                rows,
                transport.as_ref(),
                sink,
            ) {
                Ok(attach) => {
                    let _ =
                        server_event_tx.blocking_send(ServerEvent::RemotePaneAttachEstablished {
                            host,
                            terminal_id,
                            local_pane,
                            generation,
                            focus_epoch,
                            attach,
                        });
                }
                Err(err) => {
                    let _ = server_event_tx.blocking_send(ServerEvent::RemotePaneAttachFailed {
                        host,
                        terminal_id,
                        reason: err.to_string(),
                    });
                }
            }
        });
    }

    /// Handles the off-loop attach thread's success handback (see
    /// `focus_remote_pane`). Guards against four races before registering
    /// anything, gracefully detaching (and dropping) the just-established
    /// `attach` instead in every case: (1) focus moved on to something else
    /// (or was cleared) while the blocking ssh handshake was in flight; (2)
    /// the host link's generation no longer matches (it was detached or
    /// reconnected meanwhile) -- registering would keep a live ssh channel
    /// whose `TerminalBytes` are stamped with a now-superseded generation
    /// and so would be silently dropped by `handle_host_event` forever; (3)
    /// the pane itself was released while attaching; (4) this attempt's
    /// `focus_epoch` is no longer the current one -- a newer focus attempt
    /// on the SAME `terminal_id` already superseded it (rapid refocus A ->
    /// B -> A), which (1)'s `terminal_id` equality alone cannot detect since
    /// focus legitimately returned to this terminal. Otherwise registers
    /// `TerminalRuntime::spawn_remote_fed` for `terminal_id` -- the ONLY
    /// place a remote-fed runtime is inserted (memory budget: only a
    /// focused pane keeps a live grid + ssh channel) -- and stores `attach`
    /// so a future input commit has somewhere to reach it (view-only today:
    /// nothing calls `attach.send_input`/`attach.resize` yet).
    #[cfg(unix)]
    fn handle_remote_pane_attach_established(
        &mut self,
        host: crate::server::host_link::HostLinkId,
        terminal_id: crate::terminal::TerminalId,
        local_pane: crate::layout::PaneId,
        generation: u64,
        focus_epoch: u64,
        attach: crate::server::remote_pane::RemotePaneAttach,
    ) {
        let still_focused = self.focused_remote_pane.as_ref() == Some(&terminal_id);
        let generation_current = self
            .host_link_runtimes
            .get(&host)
            .is_some_and(|runtime| runtime.generation == generation);
        let pane_still_adopted = self.app.state.terminals.contains_key(&terminal_id);
        let epoch_current = self.remote_pane_focus_epoch == focus_epoch;

        if !still_focused || !generation_current || !pane_still_adopted || !epoch_current {
            debug!(
                host = %host.0,
                terminal = %terminal_id,
                still_focused,
                generation_current,
                pane_still_adopted,
                epoch_current,
                "dropping a superseded remote pane attach"
            );
            attach.detach();
            return;
        }

        let (cols, rows) = self.effective_size;
        match crate::terminal::TerminalRuntime::spawn_remote_fed(
            local_pane,
            rows,
            cols,
            self.app.state.pane_scrollback_limit_bytes,
            self.app.state.host_terminal_theme,
            self.app.render_notify.clone(),
            self.app.render_dirty.clone(),
        ) {
            Ok(runtime) => {
                self.app
                    .terminal_runtimes
                    .insert(terminal_id.clone(), runtime);
                self.focused_remote_pane_attach = Some(attach);
                debug!(
                    host = %host.0,
                    terminal = %terminal_id,
                    "remote pane attach established; runtime registered"
                );
            }
            Err(err) => {
                warn!(
                    host = %host.0,
                    terminal = %terminal_id,
                    err = %err,
                    "failed to construct remote-fed runtime for focused pane"
                );
                attach.detach();
                if self.focused_remote_pane.as_ref() == Some(&terminal_id) {
                    self.focused_remote_pane = None;
                }
            }
        }
    }

    /// Handles the off-loop attach thread's failure handback (`attach()`
    /// itself returned `Err`, e.g. ssh unreachable or the handshake was
    /// refused/timed out -- see `focus_remote_pane`). Distinct from
    /// `HostEvent::AttachFailed` (handled below), which fires when
    /// `attach()` succeeded at write time but the remote refused the
    /// terminal id via a pre-frame `ServerShutdown`. Only clears focus if
    /// `terminal_id` is still the focused pane -- if focus already moved on
    /// while this attach attempt was failing, there is nothing to clear.
    #[cfg(unix)]
    fn handle_remote_pane_attach_failed(
        &mut self,
        host: crate::server::host_link::HostLinkId,
        terminal_id: crate::terminal::TerminalId,
        reason: String,
    ) {
        if self.focused_remote_pane.as_ref() != Some(&terminal_id) {
            return;
        }
        warn!(host = %host.0, terminal = %terminal_id, reason = %reason, "remote pane attach failed");
        self.focused_remote_pane = None;
        self.send_notify_to_foreground_client(
            protocol::NotifyKind::Toast,
            format!("could not attach to remote pane on {}", host.0),
            Some(reason),
        );
    }

    /// Handles a mid-attach refusal: `attach()` succeeded at write time (so
    /// `RemotePaneAttachEstablished` already registered a runtime + stored
    /// `attach`), but the reader saw a `ServerShutdown` before any
    /// `Terminal` frame -- the remote refused this `terminal_id` (unknown,
    /// closed, or already owned). Clears focus, tears down the runtime +
    /// attach, and notifies the foreground client. Reached only through
    /// `handle_host_event`'s generation guard, so `local_pane` is
    /// necessarily current. Idempotent/tolerant: if the pane was already
    /// released (or was never the focused one), this is a no-op -- the next
    /// `Snapshot` reconciliation is the authoritative cleanup path either
    /// way.
    #[cfg(unix)]
    fn handle_remote_pane_attach_failed_event(
        &mut self,
        host: crate::server::host_link::HostLinkId,
        local_pane: crate::layout::PaneId,
        reason: Option<String>,
    ) {
        let Some(key) = self.remote_panes.key_for(local_pane).cloned() else {
            return;
        };
        let Some(terminal_id) = self.remote_pane_terminals.get(&key).cloned() else {
            return;
        };
        if self.focused_remote_pane.as_ref() != Some(&terminal_id) {
            return;
        }

        let reason_text = reason.unwrap_or_else(|| "unknown reason".to_string());
        warn!(
            host = %host.0,
            terminal = %terminal_id,
            reason = %reason_text,
            "remote refused terminal attach for the focused pane"
        );
        self.focused_remote_pane = None;
        if let Some(attach) = self.focused_remote_pane_attach.take() {
            // The reader that produced this event has already ended, so
            // this drop's reader-thread join is immediate.
            attach.detach();
        }
        if let Some(runtime) = self.app.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
        self.send_notify_to_foreground_client(
            protocol::NotifyKind::Toast,
            format!("remote pane attach refused on {}", host.0),
            Some(reason_text),
        );
    }

    /// Clears `focused_remote_pane`, gracefully detaches any live
    /// `RemotePaneAttach` (best-effort `Detach` write, then `Drop`'s
    /// force-close + reader-thread join -- see `RemotePaneAttach::detach`'s
    /// doc comment), and removes the remote-fed runtime from
    /// `terminal_runtimes` so nothing leaks across a focus change (memory
    /// budget: only the focused pane keeps a live runtime + ssh channel).
    /// Idempotent: a no-op when nothing is focused. This is the "basic
    /// blur/teardown" this commit guarantees; full link-down teardown
    /// coordination is a follow-up commit.
    #[cfg(unix)]
    fn blur_focused_remote_pane(&mut self) {
        let Some(terminal_id) = self.focused_remote_pane.take() else {
            return;
        };
        if let Some(attach) = self.focused_remote_pane_attach.take() {
            attach.detach();
        }
        if let Some(runtime) = self.app.terminal_runtimes.remove(&terminal_id) {
            runtime.shutdown();
        }
        debug!(terminal = %terminal_id, "blurred focused remote pane");
    }

    /// Test-only bijection check for the Task 9b invariant: every
    /// host-tagged `TerminalState` corresponds to a live `remote_panes`
    /// entry, and vice versa, and `AppState::remote_pane_display` tracks
    /// exactly the same set of terminals as `remote_pane_terminals`.
    /// `AppState::assert_invariants_for_test` already checks the shape of
    /// `terminal.host` (non-empty tag) and that every `remote_pane_display`
    /// key names a host-tagged terminal, but it cannot see
    /// `RemotePaneRegistry` -- `app/` sits below `server/` in the
    /// dependency graph, so `AppState` must not import
    /// `crate::server::remote_pane` -- so the full cross-check lives here,
    /// where `remote_panes`, `remote_pane_terminals`, and
    /// `self.app.state.terminals` are all visible together.
    #[cfg(all(test, unix))]
    fn assert_remote_pane_terminal_bijection_for_test(&self) {
        use std::collections::HashSet;

        self.app.state.assert_invariants_for_test();

        // remote_pane_terminals -> remote_panes + app.state.terminals +
        // remote_pane_display: every tracked key is still adopted, its
        // TerminalId names a host-tagged terminal carrying that key's host,
        // and the sidebar's display projection has a matching entry.
        for (key, terminal_id) in &self.remote_pane_terminals {
            assert!(
                self.remote_panes.local_for(key).is_some(),
                "remote_pane_terminals tracks {key:?} -> {terminal_id}, but it is not adopted in remote_panes"
            );
            let terminal = self
                .app
                .state
                .terminals
                .get(terminal_id)
                .unwrap_or_else(|| {
                    panic!("remote_pane_terminals names missing terminal {terminal_id} for {key:?}")
                });
            assert_eq!(
                terminal.host.as_ref().map(|tag| tag.as_str()),
                Some(key.host.0.as_str()),
                "terminal {terminal_id} host tag does not match its RemotePaneKey's host"
            );
            assert!(
                self.app.state.remote_pane_display.contains_key(terminal_id),
                "remote_pane_terminals tracks {key:?} -> {terminal_id}, but remote_pane_display has no entry for it"
            );
        }

        // remote_panes -> remote_pane_terminals: every adopted pane, across
        // every currently-registered host link, has a tracked terminal.
        for link in self.host_links.iter() {
            for (key, _local) in self.remote_panes.panes_for_host(&link.id) {
                assert!(
                    self.remote_pane_terminals.contains_key(key),
                    "adopted remote pane {key:?} has no tracked TerminalId"
                );
            }
        }

        // app.state.terminals -> remote_pane_terminals: no ghost host-tagged
        // terminal outlives its registry entry.
        let tracked: HashSet<&crate::terminal::TerminalId> =
            self.remote_pane_terminals.values().collect();
        for terminal in self.app.state.terminals.values() {
            if terminal.host.is_some() {
                assert!(
                    tracked.contains(&terminal.id),
                    "terminal {} is host-tagged but not tracked in remote_pane_terminals",
                    terminal.id
                );
            }
        }

        // app.state.remote_pane_display -> remote_pane_terminals: no ghost
        // display entry outlives its terminal record either.
        for terminal_id in self.app.state.remote_pane_display.keys() {
            assert!(
                tracked.contains(terminal_id),
                "remote_pane_display tracks {terminal_id}, but it has no remote_pane_terminals record"
            );
        }

        // remote_pane_remote_terminal_ids <-> remote_pane_terminals: exactly
        // the same key set (Task 9b's remote-terminal-id storage), in both
        // directions.
        for key in self.remote_pane_remote_terminal_ids.keys() {
            assert!(
                self.remote_pane_terminals.contains_key(key),
                "remote_pane_remote_terminal_ids tracks {key:?}, but remote_pane_terminals has no entry for it"
            );
        }
        for key in self.remote_pane_terminals.keys() {
            assert!(
                self.remote_pane_remote_terminal_ids.contains_key(key),
                "remote_pane_terminals tracks {key:?}, but remote_pane_remote_terminal_ids has no entry for it"
            );
        }
    }

    /// `LinkDown` handling: advances the backoff state machine, syncs the
    /// display map, retires the (already-exited) loop thread, and -- unless
    /// the link just went terminally `Offline` -- respawns a fresh transport
    /// and event loop for the next attempt. Reconnect policy intentionally
    /// lives here (the link's owner), not in the poll loop itself, per the
    /// Task 6/8 reviews' "LinkDown at most once per loop lifetime" contract:
    /// each respawned loop is a fresh lifetime with its own single-LinkDown
    /// budget.
    ///
    /// Reconnect decision: no explicit backoff delay is injected here.
    /// `on_disconnect`'s attempt counter still bounds the worst case to
    /// `MAX_RECONNECT_ATTEMPTS` respawns before the link goes `Offline`
    /// (terminal for auto-retry; manual retry is detach+attach), and in
    /// production a real `ssh` connection attempt paces each retry with its
    /// own connect latency. A fast-failing transport (e.g. a closed unix
    /// socket in tests) can burn through the whole attempt budget almost
    /// immediately -- acceptable for now, but a real timed backoff before
    /// each respawn is a documented follow-up (would need a scheduled-task
    /// deadline like `next_auto_update_check`, not a blocking sleep on the
    /// main loop thread).
    #[cfg(unix)]
    fn handle_host_link_down(&mut self, host: crate::server::host_link::HostLinkId) {
        use crate::server::host_link::LinkState;
        let next_state = self.host_links.on_disconnect(&host);
        self.sync_host_link_display(&host);
        let Some(runtime) = self.host_link_runtimes.remove(&host) else {
            // No runtime: the link was detached concurrently. Nothing to
            // join or respawn.
            return;
        };
        // The loop thread already exited (it sends LinkDown right before
        // returning), so this join is immediate.
        let HostLinkRuntime {
            generation,
            event_loop,
            transport,
        } = runtime;
        event_loop.join();
        match next_state {
            Some(LinkState::Reconnecting { attempt }) => {
                debug!(host = %host.0, attempt, "host link down; reconnecting");
                // Reuse the same generation + transport materials -- a
                // reconnect is the same logical attach lifetime, and reusing
                // the handle means no per-attempt config read / ssh-config
                // write.
                self.spawn_host_link_runtime(host, generation, transport);
            }
            Some(LinkState::Offline) => {
                warn!(
                    host = %host.0,
                    "host link exhausted reconnect attempts; now offline (manual retry = detach + attach)"
                );
                // `transport` (and its ssh guard) drop here.
            }
            _ => {
                // Connecting/Connected are never produced by `on_disconnect`,
                // and `None` means the link was detached concurrently. Drop
                // the transport materials either way.
            }
        }
    }

    /// The ONLY place `server::host_link::LinkState` is converted to
    /// `app::state::HostLinkDisplayState` (mirrored `TerminalHostTag`
    /// conversion happens right here too), per the Task 7 sync contract on
    /// `AppState::host_links`. Removes the entry if the link was detached
    /// out from under a caller (defensive; `host.detach` also removes it
    /// directly).
    #[cfg(unix)]
    fn sync_host_link_display(&mut self, host: &crate::server::host_link::HostLinkId) {
        let tag = crate::terminal::TerminalHostTag::new(host.0.clone());
        match self.host_links.state(host) {
            Some(state) => {
                self.app
                    .state
                    .host_links
                    .insert(tag, host_link_display_state(state));
            }
            None => {
                self.app.state.host_links.remove(&tag);
            }
        }
    }

    fn sync_visible_server_config_diagnostic(&mut self, uses_local_keybindings: bool) {
        let visible = if uses_local_keybindings {
            &self.server_config_diagnostic_without_keybindings
        } else {
            &self.server_config_diagnostic
        };
        if self.app.state.config_diagnostic == self.server_config_diagnostic
            || self.app.state.config_diagnostic == self.server_config_diagnostic_without_keybindings
        {
            self.app.state.config_diagnostic = visible.clone();
        }
    }

    #[cfg(unix)]
    fn restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let api_tx = self
            .api_tx
            .clone()
            .ok_or_else(|| io::Error::other("cannot restore api socket without api sender"))?;
        let api_server = api::start_server(api_tx, self.app.event_hub.clone())?;

        let client_path = client_socket_path();
        prepare_socket_path(&client_path)?;
        let listener = bind_local_listener(&client_path)?;
        restrict_socket_permissions(&client_path)?;
        let client_socket_identity = socket_file_identity(&client_path)?;
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        self.api_server = Some(api_server);
        self.client_listener = listener;
        self.client_socket_path = client_path;
        self.client_socket_identity = client_socket_identity;
        Ok(())
    }

    #[cfg(unix)]
    fn wait_then_restore_public_sockets_after_failed_handoff(&mut self) -> io::Result<()> {
        let timeout = crate::server::handoff::COMMIT_TIMEOUT + Duration::from_secs(2);
        wait_for_old_public_sockets_to_close(timeout)?;
        self.restore_public_sockets_after_failed_handoff()
    }

    #[cfg(unix)]
    fn rollback_handoff_before_commit(
        &mut self,
        socket_path: &Path,
        paused_terminal_ids: &[crate::terminal::TerminalId],
    ) {
        for terminal_id in paused_terminal_ids {
            if let Some(runtime) = self.app.terminal_runtimes.get(terminal_id) {
                runtime.set_handoff_reader_paused(false);
            }
        }
        self.handoff_in_progress = false;
        let _ = std::fs::remove_file(socket_path);
    }

    #[cfg(unix)]
    fn nudge_handoff_panes_on_first_client_attach(&mut self) {
        if !self.pending_handoff_repaint_nudge {
            return;
        }
        self.pending_handoff_repaint_nudge = false;
        self.app
            .terminal_runtimes
            .nudge_child_redraw_after_handoff();
    }

    #[cfg(not(unix))]
    fn nudge_handoff_panes_on_first_client_attach(&mut self) {}

    fn reload_server_config(&mut self, notify_success: bool) -> crate::config::ConfigReloadReport {
        let server_keybindings = self.server_keybindings.clone();
        apply_keybindings(&mut self.app, &server_keybindings);
        let report = self.app.apply_config_from_disk(notify_success);
        self.app.take_config_reloaded_from_disk();
        self.server_keybindings = app_keybindings(&self.app);
        let (server_config_diagnostic, server_config_diagnostic_without_keybindings) =
            server_config_diagnostic_summaries(&report.diagnostics);
        self.server_config_diagnostic = server_config_diagnostic;
        self.server_config_diagnostic_without_keybindings =
            server_config_diagnostic_without_keybindings;
        self.sync_foreground_client_state();
        report
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
        let next_foreground = latest_app_client(&self.clients);
        let changed = next_foreground != self.foreground_client_id;
        self.foreground_client_id = next_foreground;
        self.sync_foreground_client_state();
        changed
    }

    fn app_client_count(&self) -> usize {
        self.clients
            .values()
            .filter(|client| client.is_full_app_client() && client.writer.is_some())
            .count()
    }

    fn has_app_client(&self) -> bool {
        self.app_client_count() > 0
    }

    fn remove_client(&mut self, client_id: u64) -> bool {
        let was_foreground = self.foreground_client_id == Some(client_id);
        self.send_client_graphics_cleanup(client_id);
        let removed = self.clients.remove(&client_id);
        if let Some(removed) = removed {
            crate::server::clipboard_image::remove_files(removed.staged_clipboard_files);
            if let ClientConnectionMode::TerminalAttach { terminal_id } = removed.mode {
                self.terminal_attach_owners.remove(&terminal_id);
                if let Some(terminal_id) = self.terminal_id_by_string(&terminal_id) {
                    self.app
                        .state
                        .direct_attach_resize_locks
                        .remove(&terminal_id);
                }
            }
        }
        if was_foreground {
            self.promote_latest_remaining_client()
        } else {
            false
        }
    }

    fn client_removal_needs_shared_resize(&self, client_id: u64) -> bool {
        if self.foreground_client_id == Some(client_id) {
            return true;
        }
        matches!(
            self.clients.get(&client_id).map(|client| &client.mode),
            Some(
                ClientConnectionMode::TerminalAttach { .. }
                    | ClientConnectionMode::TerminalObserve { .. }
            )
        ) && self.foreground_client_id.is_some()
    }

    fn remove_client_and_resize_if_needed(&mut self, client_id: u64) {
        let needs_shared_resize = self.client_removal_needs_shared_resize(client_id);
        let foreground_changed = self.remove_client(client_id);
        if needs_shared_resize || foreground_changed {
            self.resize_shared_runtime_to_effective_size();
        }
    }

    fn send_client_graphics_cleanup(&mut self, client_id: u64) {
        let (writer, bytes) = match self.clients.get_mut(&client_id) {
            Some(client) => {
                let bytes = client.graphics_cache.clear_bytes();
                (client.writer.as_ref().cloned(), bytes)
            }
            None => return,
        };
        if bytes.is_empty() {
            return;
        }
        let Some(writer) = writer else {
            return;
        };
        let Ok(serialized) = Self::frame_server_message(&ServerMessage::Graphics { bytes }) else {
            return;
        };
        let _ = writer.control.send(serialized);
    }

    fn send_all_clients_graphics_cleanup(&mut self) {
        let client_ids = self.clients.keys().copied().collect::<Vec<_>>();
        for client_id in client_ids {
            self.send_client_graphics_cleanup(client_id);
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

        if !client.update_host_theme_from_events(events) {
            return false;
        }

        if self.foreground_client_id == Some(client_id) {
            let mut changed = self.app.set_host_terminal_appearance_state(
                client.host_terminal_appearance,
                client.host_terminal_appearance_explicit,
            );
            changed |= self.app.set_host_terminal_theme(client.host_terminal_theme);
            if changed {
                self.resize_shared_runtime_to_effective_size_before_input();
            }
            changed
        } else {
            false
        }
    }

    fn update_client_outer_focus_from_events(
        &mut self,
        client_id: u64,
        events: &[crate::raw_input::RawInputEvent],
    ) {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        let Some(next_focus) = client.update_outer_focus_from_events(events) else {
            return;
        };
        if self.foreground_client_id == Some(client_id) {
            self.app.state.outer_terminal_focus = Some(next_focus);
        }
    }

    /// Accepts pending client connections from the non-blocking listener.
    #[cfg(unix)]
    fn accept_client_connections(&mut self) -> io::Result<()> {
        if self.handoff_in_progress {
            return reject_pending_client_connections(&self.client_listener);
        }
        accept_pending_client_connections(
            &self.client_listener,
            &mut self.next_client_id,
            &self.should_quit,
            &self.server_event_tx,
        )
    }

    /// Windows named-pipe clients can block in connect unless the server has a
    /// pending blocking accept. The dedicated accept thread handles that path.
    #[cfg(windows)]
    fn accept_client_connections(&mut self) -> io::Result<()> {
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

    fn terminal_id_by_string(&self, terminal_id: &str) -> Option<crate::terminal::TerminalId> {
        self.app
            .state
            .terminals
            .keys()
            .find(|id| id.to_string() == terminal_id)
            .cloned()
    }

    fn runtime_for_terminal_id_string(
        &self,
        terminal_id: &str,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        let terminal_id = self.terminal_id_by_string(terminal_id)?;
        self.app.terminal_runtimes.get(&terminal_id)
    }

    fn resolve_terminal_target_id_string(&self, target: &str) -> Option<String> {
        if self.terminal_id_by_string(target).is_some() {
            return Some(target.to_owned());
        }
        self.app
            .resolve_terminal_target(target)
            .ok()
            .map(|resolved| resolved.terminal_id)
    }

    fn write_client_clipboard_image(
        &mut self,
        client_id: u64,
        extension: &str,
        data: &[u8],
    ) -> std::io::Result<String> {
        let staged = crate::server::clipboard_image::stage(client_id, extension, data)?;
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.staged_clipboard_files.push(staged.path);
        }
        info!(client_id, bytes = data.len(), path = %staged.paste_text, "staged client clipboard image");
        Ok(staged.paste_text)
    }

    fn paste_client_clipboard_image_path(&mut self, client_id: u64, path: String) -> bool {
        if let Some(ClientConnection {
            mode: ClientConnectionMode::TerminalAttach { terminal_id },
            ..
        }) = self.clients.get(&client_id)
        {
            if let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) {
                let payload = paste_payload_for_runtime(runtime, &path);
                if let Err(err) = runtime.try_send_bytes(Bytes::from(payload)) {
                    warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach clipboard image paste failed");
                }
            }
            return true;
        }

        let foreground_changed = self.promote_client_to_foreground(client_id);
        if foreground_changed {
            self.resize_shared_runtime_to_effective_size_before_input();
        }
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.request_semantic_redraw_after_input();
        }
        self.app.route_client_events(
            vec![crate::raw_input::RawInputEvent::Paste(path)],
            self.foreground_client_id == Some(client_id),
        );
        true
    }

    fn resolve_terminal_session_target(
        &mut self,
        client_id: u64,
        target: &str,
        action: &str,
    ) -> Option<String> {
        if !self.client_is_pending_terminal_mode(client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        format!(
                            "terminal session {action} failed: connection is not pending terminal session"
                        ),
                    ),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return None;
        }

        let Some(terminal_id) = self.resolve_terminal_target_id_string(target) else {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(format!(
                        "terminal session {action} failed: terminal target {target} not found"
                    )),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return None;
        };

        Some(terminal_id)
    }

    fn observe_terminal_client(&mut self, client_id: u64, target: String) -> bool {
        let Some(terminal_id) = self.resolve_terminal_session_target(client_id, &target, "observe")
        else {
            return false;
        };

        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let (cols, rows) = client.terminal_size;
        client.mode = ClientConnectionMode::TerminalObserve {
            terminal_id: terminal_id.clone(),
        };
        client.pending_terminal_attach = false;
        client.render_state.reset_baseline();
        client.last_activity = stamp;
        let was_foreground = self.foreground_client_id == Some(client_id);
        if was_foreground {
            self.promote_latest_remaining_client();
        }

        info!(client_id, cols, rows, terminal_id = %terminal_id, "terminal observe client connected");
        true
    }

    fn control_terminal_client(&mut self, client_id: u64, target: String, takeover: bool) -> bool {
        let Some(terminal_id) = self.resolve_terminal_session_target(client_id, &target, "control")
        else {
            return false;
        };

        self.attach_terminal_client(client_id, terminal_id, takeover)
    }

    fn handle_terminal_attach_scroll(
        &mut self,
        client_id: u64,
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    ) -> bool {
        let Some(ClientConnection {
            mode: ClientConnectionMode::TerminalAttach { terminal_id },
            ..
        }) = self.clients.get(&client_id)
        else {
            return false;
        };
        let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) else {
            return false;
        };

        if let Err(err) =
            apply_terminal_attach_scroll(runtime, source, direction, lines, column, row, modifiers)
        {
            warn!(client_id, terminal_id = %terminal_id, err = %err, "terminal attach scroll failed");
        }
        true
    }

    fn pane_effective_state(&self, pane_id: crate::layout::PaneId) -> crate::detect::AgentState {
        self.app
            .state
            .workspaces
            .iter()
            .find_map(|ws| {
                ws.tabs.iter().find_map(|tab| {
                    let pane = tab.panes.get(&pane_id)?;
                    self.app
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .map(|terminal| terminal.state)
                })
            })
            .unwrap_or(crate::detect::AgentState::Unknown)
    }

    fn pane_effective_agent_label(&self, pane_id: crate::layout::PaneId) -> Option<String> {
        self.app.state.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                let pane = tab.panes.get(&pane_id)?;
                self.app
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| terminal.effective_agent_label())
                    .map(str::to_string)
            })
        })
    }

    fn forward_pane_state_update_notifications_to_clients(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
    ) {
        if self.app.state.toast_config.delay_seconds != 0 {
            return;
        }

        let is_active_tab = self
            .app
            .state
            .pane_is_in_active_tab(update.ws_idx, update.pane_id);
        let suppress_active_tab_notifications =
            self.active_tab_suppresses_notifications(is_active_tab);

        if self.app.state.sound.allows(update.known_agent) {
            if let Some(sound) =
                crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                    suppress_active_tab_notifications,
                    update.previous_state,
                    update.state,
                    update.previous_agent_label.as_deref(),
                    update.agent_label.as_deref(),
                )
            {
                self.send_notify_to_foreground_client(
                    protocol::NotifyKind::Sound,
                    sound_notify_message(sound),
                    None,
                );
            }
        }

        if !should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            return;
        }
        let Some(kind) = crate::app::actions::notification_toast_for_pane_state_update(
            suppress_active_tab_notifications,
            update,
        ) else {
            return;
        };
        let Some(ws) = self.app.state.workspaces.get(update.ws_idx) else {
            return;
        };
        let Some(agent_label) = update.agent_label.as_deref() else {
            return;
        };
        let event_text = match kind {
            crate::app::state::ToastKind::NeedsAttention => "needs attention",
            crate::app::state::ToastKind::Finished => "finished",
            crate::app::state::ToastKind::UpdateInstalled => "updated",
        };
        let workspace_label =
            ws.display_name_from(&self.app.state.terminals, &self.app.terminal_runtimes);
        let context = crate::app::actions::notification_context(
            ws,
            &workspace_label,
            update.ws_idx,
            update.pane_id,
        );
        self.send_notify_to_foreground_client(
            toast_notify_kind(self.app.state.toast_config.delivery)
                .expect("toast forwarding requires a client notification kind"),
            format!("{agent_label} {event_text}"),
            non_empty_body(&context),
        );
    }

    fn forward_agent_notification_delivery(
        &mut self,
        delivery: &crate::app::state::AgentNotificationDelivery,
    ) {
        if let Some(sound) = delivery.sound {
            self.send_notify_to_foreground_client(
                protocol::NotifyKind::Sound,
                sound_notify_message(sound),
                None,
            );
        }

        if should_forward_toast_to_clients(self.app.state.toast_config.delivery) {
            if let Some(toast) = &delivery.client_notification {
                self.send_notify_to_foreground_client(
                    toast_notify_kind(self.app.state.toast_config.delivery)
                        .expect("toast forwarding requires a client notification kind"),
                    &toast.title,
                    non_empty_body(&toast.context),
                );
            }
        }
    }

    fn send_notify_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl Into<String>,
        body: Option<String>,
    ) -> bool {
        self.send_to_foreground_client(ServerMessage::Notify {
            kind,
            message: message.into(),
            body,
        })
    }

    fn send_flat_toast_to_foreground_client(
        &mut self,
        kind: protocol::NotifyKind,
        message: impl AsRef<str>,
    ) -> bool {
        let (title, body) = crate::terminal_notify::split_message(message.as_ref());
        self.send_notify_to_foreground_client(kind, title, body.map(str::to_string))
    }

    fn handle_notification_show_api(
        &mut self,
        id: String,
        params: api::schema::NotificationShowParams,
    ) -> String {
        use api::schema::{NotificationShowReason, ResponseResult};

        let Some(title) = sanitize_notification_text(&params.title, 80) else {
            return serde_json::to_string(&api::schema::ErrorResponse {
                id,
                error: api::schema::ErrorBody {
                    code: "invalid_params".into(),
                    message: "notification title is empty".into(),
                },
            })
            .unwrap_or_else(|_| "{}".to_string());
        };

        match self.app.state.toast_config.delivery {
            config::ToastDelivery::Off => {
                return serde_json::to_string(&api::schema::SuccessResponse {
                    id,
                    result: ResponseResult::NotificationShow {
                        shown: false,
                        reason: NotificationShowReason::Disabled,
                    },
                })
                .unwrap_or_else(|_| "{}".to_string());
            }
            config::ToastDelivery::Herdr => {
                let sound = params.sound;
                let response = self.app.handle_api_request_after_internal_events_drained(
                    api::schema::Request {
                        id,
                        method: api::schema::Method::NotificationShow(params),
                    },
                );
                if notification_show_response_shown(&response) {
                    self.forward_api_notification_sound(sound);
                }
                return response;
            }
            config::ToastDelivery::Terminal | config::ToastDelivery::System => {}
        }

        let body = params
            .body
            .as_deref()
            .and_then(|body| sanitize_notification_text(body, 240));
        if self.app.api_notification_rate_limited(Instant::now()) {
            return serde_json::to_string(&api::schema::SuccessResponse {
                id,
                result: ResponseResult::NotificationShow {
                    shown: false,
                    reason: NotificationShowReason::RateLimited,
                },
            })
            .unwrap_or_else(|_| "{}".to_string());
        }
        let kind = toast_notify_kind(self.app.state.toast_config.delivery)
            .expect("terminal/system delivery has notify kind");
        let shown = self.send_notify_to_foreground_client(kind, title, body);
        if shown {
            self.app.mark_api_notification_shown(Instant::now());
            self.forward_api_notification_sound(params.sound);
        }
        let reason = if shown {
            NotificationShowReason::Shown
        } else {
            NotificationShowReason::NoForegroundClient
        };

        serde_json::to_string(&api::schema::SuccessResponse {
            id,
            result: ResponseResult::NotificationShow { shown, reason },
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    fn handle_client_window_title_api(&mut self, id: String, title: Option<String>) -> String {
        use api::schema::{ClientWindowTitleReason, ResponseResult};

        let title = match title {
            Some(title) => match sanitize_window_title_text(&title, 200) {
                Some(title) => Some(title),
                None => {
                    return serde_json::to_string(&api::schema::ErrorResponse {
                        id,
                        error: api::schema::ErrorBody {
                            code: "invalid_params".into(),
                            message: "window title is empty".into(),
                        },
                    })
                    .unwrap_or_else(|_| "{}".to_string());
                }
            },
            None => None,
        };
        let set_title = title.is_some();
        let changed = self.send_to_foreground_client(ServerMessage::WindowTitle { title });
        let reason = match (changed, set_title) {
            (true, true) => ClientWindowTitleReason::Set,
            (true, false) => ClientWindowTitleReason::Cleared,
            (false, _) => ClientWindowTitleReason::NoForegroundClient,
        };
        serde_json::to_string(&api::schema::SuccessResponse {
            id,
            result: ResponseResult::ClientWindowTitle { changed, reason },
        })
        .unwrap_or_else(|_| "{}".to_string())
    }

    fn forward_api_notification_sound(&mut self, sound: api::schema::NotificationShowSound) {
        let Some(sound) = sound.to_sound() else {
            return;
        };
        self.send_notify_to_foreground_client(
            protocol::NotifyKind::Sound,
            sound_notify_message(sound),
            None,
        );
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
                let data = base64::engine::general_purpose::STANDARD.encode(content.as_slice());
                if self.send_to_foreground_client(ServerMessage::Clipboard { data }) {
                    self.app.show_clipboard_feedback();
                }
                true
            }
            AppEvent::StateChanged { pane_id, agent, .. } => {
                // Capture toast before handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = *agent;

                // Find the previous effective state of this pane before the event
                // is processed. Notifications must follow effective state changes,
                // not raw fallback reports that may be masked by hook authority.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

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

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::HookStateReported {
                pane_id,
                agent_label,
                ..
            } => {
                // Hook reports can be stale or no-op after sequence rejection.
                // Forward only effective state changes observed after handling.
                let toast_before = self.app.state.toast.clone();
                let pane_id_val = *pane_id;
                let agent_val = crate::detect::parse_agent_label(agent_label);

                // Capture the previous effective state for this pane. Hook reports
                // are already folded into pane.state; raw hook transitions must not
                // produce a second notification path.
                let prev_state = self.pane_effective_state(pane_id_val);
                let prev_agent_label = self.pane_effective_agent_label(pane_id_val);

                self.sync_foreground_client_state();
                self.app.handle_internal_event(ev);

                // Forward sound notification based on the effective transition when
                // server-side sound policy allows it.
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

                let next_state = self.pane_effective_state(pane_id_val);
                let next_agent_label = self.pane_effective_agent_label(pane_id_val);

                if self.app.state.toast_config.delay_seconds == 0
                    && self.app.state.sound.allows(agent_val)
                {
                    if let Some(sound) =
                        crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                            next_agent_label.as_deref(),
                        )
                    {
                        self.send_notify_to_foreground_client(
                            protocol::NotifyKind::Sound,
                            sound_notify_message(sound),
                            None,
                        );
                    }
                }

                let toast_msg = if self.app.state.toast_config.delay_seconds == 0
                    && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
                {
                    if self.app.state.toast.is_some() && self.app.state.toast != toast_before {
                        self.app
                            .state
                            .toast
                            .as_ref()
                            .map(|toast| format!("{}: {}", toast.title, toast.context))
                    } else {
                        toast_message_from_state_change(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                            pane_id_val,
                            suppress_active_tab_notifications,
                            prev_state,
                            next_state,
                            prev_agent_label.as_deref(),
                        )
                    }
                } else {
                    None
                };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                let toast_before = self.app.state.toast.clone();
                let version = version.clone();
                let install_command = install_command.clone();

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
                                "v{version} available: {}",
                                crate::update::update_install_instruction(&install_command)
                            ))
                        }
                    } else {
                        None
                    };

                if let Some(msg) = toast_msg {
                    self.send_flat_toast_to_foreground_client(
                        toast_notify_kind(self.app.state.toast_config.delivery)
                            .expect("toast forwarding requires a client notification kind"),
                        msg,
                    );
                }

                true
            }
            AppEvent::PaneDied { pane_id } => {
                let pane_id_val = *pane_id;
                let terminal_id = self.app.state.workspaces.iter().find_map(|ws| {
                    ws.tabs.iter().find_map(|tab| {
                        tab.panes
                            .get(pane_id)
                            .map(|pane| pane.attached_terminal_id.to_string())
                    })
                });
                if let Some(update) = self
                    .app
                    .state
                    .publish_pane_process_exit_if_agent(pane_id_val)
                {
                    self.app.emit_pane_state_update(&update);
                    self.forward_pane_state_update_notifications_to_clients(&update);
                }

                self.app.handle_internal_event(ev);

                if self.app.find_pane(pane_id_val).is_none() {
                    if let Some(terminal_id) = terminal_id {
                        self.shutdown_terminal_stream_clients(
                            &terminal_id,
                            format!("terminal {terminal_id} exited"),
                        );
                    }
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
    ///   `ServerMessage::Notify` to the foreground client for terminal/system delivery.
    fn drain_internal_events_with_forwarding(&mut self) -> bool {
        self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT)
            .1
    }

    fn drain_all_internal_events_with_forwarding(&mut self) -> bool {
        let mut changed = false;
        loop {
            let (had_event, batch_changed) =
                self.drain_internal_events_with_forwarding_up_to(crate::app::APP_EVENT_DRAIN_LIMIT);
            changed |= batch_changed;
            if !had_event {
                break;
            }
        }
        changed
    }

    fn drain_internal_events_with_forwarding_up_to(&mut self, limit: usize) -> (bool, bool) {
        let mut had_event = false;
        let mut changed = false;
        for _ in 0..limit {
            let Ok(ev) = self.app.event_rx.try_recv() else {
                break;
            };
            had_event = true;
            changed |= self.handle_internal_event_with_forwarding(ev);
        }
        (had_event, changed)
    }

    fn drain_client_config_reload_request(&mut self) {
        if !self.app.state.request_client_config_reload {
            return;
        }
        self.app.state.request_client_config_reload = false;
        self.send_to_all_clients(ServerMessage::ReloadSoundConfig);
    }

    /// Encodes a server message into a length-prefixed frame.
    fn frame_server_message(msg: &ServerMessage) -> Result<Vec<u8>, protocol::FramingError> {
        Self::frame_server_message_with_max(msg, MAX_FRAME_SIZE)
    }

    /// Encodes a server message using an explicit payload cap.
    fn frame_server_message_with_max(
        msg: &ServerMessage,
        max_frame_size: usize,
    ) -> Result<Vec<u8>, protocol::FramingError> {
        let mut framed = Vec::new();
        protocol::write_message(&mut framed, msg)?;
        let payload_len = framed.len().saturating_sub(4);
        if payload_len > max_frame_size {
            return Err(protocol::FramingError::Oversized {
                claimed: payload_len,
                max: max_frame_size,
            });
        }
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
                if writer.control.send(serialized.clone()).is_err() {
                    debug!(client_id, "client writer channel closed during broadcast");
                    broken_clients.push(client_id);
                }
            }
        }

        // Remove broken clients.
        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
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
                if writer.control.send(serialized).is_err() {
                    debug!(
                        client_id,
                        "client writer channel closed during targeted send"
                    );
                    self.remove_client_and_resize_if_needed(client_id);
                    return false;
                }
            }
            true
        } else {
            false
        }
    }

    fn shutdown_terminal_stream_clients(&mut self, terminal_id: &str, reason: String) {
        let client_ids = terminal_stream_client_ids(&self.clients, terminal_id);

        for client_id in client_ids {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(reason.clone()),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    fn send_terminal_stream_detach_shutdown(&mut self, client_id: u64) {
        if matches!(
            self.clients.get(&client_id).map(|client| &client.mode),
            Some(
                ClientConnectionMode::TerminalAttach { .. }
                    | ClientConnectionMode::TerminalObserve { .. }
            )
        ) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some("detached".to_owned()),
                },
            );
        }
    }

    #[cfg(unix)]
    fn disconnect_all_clients_for_handoff(&mut self) {
        let client_ids = self.clients.keys().copied().collect::<Vec<_>>();
        for client_id in client_ids {
            self.send_client_graphics_cleanup(client_id);
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        "live update in progress; reconnect after handoff completes".to_owned(),
                    ),
                },
            );
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.writer = None;
            }
            let _ = self.remove_client(client_id);
        }
        self.foreground_client_id = None;
        self.sync_foreground_client_state();
        self.resize_shared_runtime_to_effective_size();
    }

    fn attach_terminal_client(
        &mut self,
        client_id: u64,
        terminal_id: String,
        takeover: bool,
    ) -> bool {
        if !self.client_is_pending_terminal_mode(client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(
                        "terminal attach failed: connection is not pending terminal attach"
                            .to_owned(),
                    ),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return false;
        }

        let Some(real_terminal_id) = self.terminal_id_by_string(&terminal_id) else {
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some(format!(
                        "terminal attach failed: terminal {terminal_id} not found"
                    )),
                },
            );
            self.remove_client_and_resize_if_needed(client_id);
            return false;
        };

        if let Some(existing_owner) = self.terminal_attach_owners.get(&terminal_id).copied() {
            if existing_owner != client_id && !takeover {
                self.send_to_client(
                    client_id,
                    ServerMessage::ServerShutdown {
                        reason: Some(format!(
                            "terminal attach failed: terminal {terminal_id} already has an attached client; retry with --takeover"
                        )),
                    },
                );
                self.remove_client_and_resize_if_needed(client_id);
                return false;
            }
            if existing_owner != client_id {
                self.send_to_client(
                    existing_owner,
                    ServerMessage::ServerShutdown {
                        reason: Some("terminal attach taken over".to_owned()),
                    },
                );
                self.remove_client_and_resize_if_needed(existing_owner);
            }
        }

        let stamp = self.allocate_activity_stamp();
        let Some(client) = self.clients.get_mut(&client_id) else {
            return false;
        };
        let (cols, rows) = client.terminal_size;
        let cell_size = client.cell_size;
        client.mode = ClientConnectionMode::TerminalAttach {
            terminal_id: terminal_id.clone(),
        };
        client.pending_terminal_attach = false;
        client.render_state.reset_baseline();
        client.last_activity = stamp;
        let was_foreground = self.foreground_client_id == Some(client_id);
        if was_foreground {
            self.promote_latest_remaining_client();
        }

        info!(client_id, cols, rows, terminal_id = %terminal_id, "terminal attach client connected");
        self.terminal_attach_owners
            .insert(terminal_id.clone(), client_id);
        self.app
            .state
            .direct_attach_resize_locks
            .insert(real_terminal_id.clone());
        self.app
            .start_pending_agent_resume_for_terminal(&real_terminal_id, rows, cols, true);
        if let Some(runtime) = self.app.terminal_runtimes.get(&real_terminal_id) {
            runtime.resize(rows, cols, cell_size.width_px, cell_size.height_px);
        }
        true
    }

    fn client_is_pending_terminal_mode(&self, client_id: u64) -> bool {
        self.clients.get(&client_id).is_some_and(|client| {
            client.pending_terminal_attach && matches!(client.mode, ClientConnectionMode::App)
        })
    }

    /// Consumes `AppState::requested_remote_pane_focus` -- the cross-layer
    /// signal set by ANY focus path (a sidebar click / keyboard nav
    /// resolving `AgentFocusTarget::Remote`, OR `AppState::
    /// focus_pane_in_workspace` requesting a blur on any local pane focus
    /// change) -- and applies it server-side. click/keyboard/API input
    /// handling lives in `app/`/`ui/` and must not import server types, so
    /// it leaves this signal on `AppState` for the server to drain.
    ///
    /// Drained once per main-loop iteration from
    /// `handle_deferred_requests_headless`, NOT from a per-path handler:
    /// `focus_pane_in_workspace` (which sets the `Blur`) is reachable from
    /// the JSON API pane-focus path, plugin actions, and agent-notification
    /// focus as well as from client input, and only the client-input path
    /// runs `handle_client_input_events`. Draining in the same central
    /// per-iteration handler that already drains every other deferred
    /// `AppState` request flag (`request_new_workspace`, ...) guarantees
    /// exactly-once coverage of every focus path -- it runs after both API
    /// (`run()` step 3) and client-input (step 5) processing and before
    /// render, so no path can leave a pending `Focus`/`Blur` undrained and
    /// leak a remote attach + runtime. Returns true if a request was
    /// applied, so the caller can request a render. A `Focus` set by client
    /// input is still applied in the same iteration it was produced (before
    /// that iteration's render), preserving the prior same-tick behavior.
    #[cfg(unix)]
    fn apply_requested_remote_pane_focus(&mut self) -> bool {
        match self.app.state.requested_remote_pane_focus.take() {
            Some(crate::app::state::RemotePaneFocusRequest::Focus(terminal_id)) => {
                self.focus_remote_pane(terminal_id);
                true
            }
            Some(crate::app::state::RemotePaneFocusRequest::Blur) => {
                self.blur_focused_remote_pane();
                true
            }
            None => false,
        }
    }

    /// Multi-host / remote-pane focus is unix-only (see `focused_remote_pane`'s
    /// doc comment); on other targets there is nothing to apply, but the
    /// signal must still be drained so it never lingers into a later tick.
    #[cfg(not(unix))]
    fn apply_requested_remote_pane_focus(&mut self) -> bool {
        self.app.state.requested_remote_pane_focus = None;
        false
    }

    /// Handles a server event. Returns true if the event requires a re-render.
    fn handle_client_input_events(
        &mut self,
        client_id: u64,
        events: Vec<crate::raw_input::RawInputEvent>,
    ) -> bool {
        let host_surface_redraw = crate::raw_input::events_require_host_surface_redraw(
            &events,
            self.app.state.redraw_on_focus_gained,
        );
        if let Some(client) = self.clients.get_mut(&client_id) {
            if host_surface_redraw {
                client.request_full_redraw();
                client.render_pending = true;
            } else {
                // Ensure semantic clients receive one post-input frame even if the
                // semantic buffer compares equal. Terminal-ANSI clients must keep their
                // server-side blit baseline; resetting it here forces a full redraw on
                // every keypress and makes remote sessions feel extremely slow.
                client.request_semantic_redraw_after_input();
            }
        }
        self.update_client_outer_focus_from_events(client_id, &events);
        let interaction = events_include_interaction(&events);
        let foreground_changed = if interaction {
            self.promote_client_to_foreground(client_id)
        } else {
            false
        };
        if foreground_changed {
            self.resize_shared_runtime_to_effective_size_before_input();
        }
        let theme_changed = self.update_client_host_theme_from_events(client_id, &events);
        self.app
            .route_client_events(events, self.foreground_client_id == Some(client_id));
        // A remote-pane focus/blur request this input produced is drained
        // centrally in `handle_deferred_requests_headless` (same iteration,
        // before render), NOT here -- the JSON API / plugin / agent focus
        // paths that can also set it never run this handler. See
        // `apply_requested_remote_pane_focus`.
        if self.app.take_config_reloaded_from_disk() {
            self.reload_server_config(false);
        } else {
            self.sync_foreground_client_state();
        }

        if self.app.state.detach_requested {
            self.app.state.detach_requested = false;
            info!(client_id, "client detach requested via keybind");

            self.send_client_graphics_cleanup(client_id);
            self.send_to_client(
                client_id,
                ServerMessage::ServerShutdown {
                    reason: Some("detached".to_owned()),
                },
            );

            if let Some(client) = self.clients.get_mut(&client_id) {
                client.writer = None;
            }

            false
        } else {
            foreground_changed || theme_changed || interaction
        }
    }

    fn handle_server_event(&mut self, ev: ServerEvent) -> bool {
        if self.handoff_in_progress && Self::ignore_client_event_during_handoff(&ev) {
            return false;
        }

        match ev {
            ServerEvent::ClientConnected {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                keybindings,
                writer,
                render_encoding,
                direct_attach_requested,
            } => {
                if self.handoff_in_progress {
                    if let Ok(message) =
                        Self::frame_server_message(&ServerMessage::ServerShutdown {
                            reason: Some(
                                "live update in progress; reconnect after handoff completes"
                                    .to_owned(),
                            ),
                        })
                    {
                        let _ = writer.control.send(message);
                    }
                    return false;
                }
                let first_app_client = !direct_attach_requested && self.app_client_count() == 0;
                info!(
                    client_id,
                    cols,
                    rows,
                    cell_width_px,
                    cell_height_px,
                    ?render_encoding,
                    "client connected"
                );
                let last_activity = self.allocate_activity_stamp();
                self.clients.insert(
                    client_id,
                    ClientConnection::new_with_mode(
                        ClientConnectionMode::App,
                        keybindings,
                        (cols, rows),
                        crate::kitty_graphics::HostCellSize {
                            width_px: cell_width_px,
                            height_px: cell_height_px,
                        },
                        crate::terminal_theme::TerminalTheme::default(),
                        None,
                        last_activity,
                        render_encoding,
                        direct_attach_requested,
                        Some(writer),
                    ),
                );
                if !direct_attach_requested {
                    self.foreground_client_id = Some(client_id);
                }
                if first_app_client {
                    self.app.mark_git_status_refresh_due(Instant::now());
                }
                self.sync_foreground_client_state();
                self.resize_shared_runtime_to_effective_size();
                self.nudge_handoff_panes_on_first_client_attach();
                true
            }
            ServerEvent::ClientAttachTerminal {
                client_id,
                terminal_id,
                takeover,
            } => self.attach_terminal_client(client_id, terminal_id, takeover),
            ServerEvent::ClientObserveTerminal { client_id, target } => {
                self.observe_terminal_client(client_id, target)
            }
            ServerEvent::ClientControlTerminal {
                client_id,
                target,
                takeover,
            } => self.control_terminal_client(client_id, target, takeover),
            ServerEvent::ClientAttachScroll {
                client_id,
                source,
                direction,
                lines,
                column,
                row,
                modifiers,
            } => self.handle_terminal_attach_scroll(
                client_id, source, direction, lines, column, row, modifiers,
            ),
            ServerEvent::ClientInput { client_id, data } => {
                if self.handoff_in_progress {
                    debug!(
                        client_id,
                        len = data.len(),
                        "ignored client input during handoff"
                    );
                    return false;
                }
                debug!(client_id, len = data.len(), "client input received");
                if let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalAttach { terminal_id },
                    ..
                }) = self.clients.get(&client_id)
                {
                    if let Some(runtime) = self.runtime_for_terminal_id_string(terminal_id) {
                        if let Err(err) = apply_terminal_attach_input(runtime, data) {
                            warn!(client_id, terminal_id = %terminal_id, err = %err);
                        }
                    }
                    return true;
                }
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                let events = if let Some(client) = self.clients.get_mut(&client_id) {
                    let mut events = client.raw_input.push(&data);
                    // The thin client only forwards a bare ESC after its local input timeout.
                    if data.as_slice() == b"\x1b" {
                        events.extend(client.raw_input.flush_timeout());
                    }
                    events
                } else {
                    Vec::new()
                };
                self.handle_client_input_events(client_id, events)
            }
            ServerEvent::ClientInputEvents { client_id, events } => {
                if self.handoff_in_progress {
                    debug!(
                        client_id,
                        len = events.len(),
                        "ignored client input events during handoff"
                    );
                    return false;
                }
                debug!(
                    client_id,
                    len = events.len(),
                    "client input events received"
                );
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                let events = events
                    .iter()
                    .map(crate::protocol::ClientInputEvent::to_raw_input_event)
                    .collect();
                self.handle_client_input_events(client_id, events)
            }
            ServerEvent::ClientClipboardImage {
                client_id,
                extension,
                data,
            } => {
                debug!(
                    client_id,
                    len = data.len(),
                    extension = %extension,
                    "client clipboard image received"
                );
                if matches!(
                    self.clients.get(&client_id).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalObserve { .. })
                ) {
                    return false;
                }
                match self.write_client_clipboard_image(client_id, &extension, &data) {
                    Ok(path) => self.paste_client_clipboard_image_path(client_id, path),
                    Err(err) => {
                        warn!(client_id, err = %err, "failed to stage client clipboard image");
                        true
                    }
                }
            }
            ServerEvent::ClientResize {
                client_id,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            } => {
                info!(
                    client_id,
                    cols, rows, cell_width_px, cell_height_px, "client resize"
                );
                let direct_terminal_id = if let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalAttach { terminal_id },
                    terminal_size,
                    cell_size,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    *cell_size = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    render_state.reset_baseline();
                    Some(terminal_id.clone())
                } else {
                    None
                };
                if let Some(terminal_id) = direct_terminal_id {
                    if let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) {
                        runtime.resize(rows, cols, cell_width_px, cell_height_px);
                    }
                    return true;
                }
                if let Some(ClientConnection {
                    mode: ClientConnectionMode::TerminalObserve { .. },
                    terminal_size,
                    cell_size,
                    render_state,
                    ..
                }) = self.clients.get_mut(&client_id)
                {
                    *terminal_size = (cols, rows);
                    *cell_size = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                    render_state.reset_baseline();
                    return true;
                }
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.terminal_size = (cols, rows);
                    client.cell_size = crate::kitty_graphics::HostCellSize {
                        width_px: cell_width_px,
                        height_px: cell_height_px,
                    };
                }
                self.promote_client_to_foreground(client_id);
                self.resize_shared_runtime_to_effective_size();
                true
            }
            ServerEvent::ClientDetach { client_id } => {
                info!(client_id, "client detached");
                self.send_terminal_stream_detach_shutdown(client_id);
                self.remove_client_and_resize_if_needed(client_id);
                true
            }
            ServerEvent::ClientDisconnected { client_id } => {
                info!(client_id, "client disconnected");
                self.remove_client_and_resize_if_needed(client_id);
                true
            }
            ServerEvent::ClientWriterDrained { client_id } => {
                let Some(client) = self.clients.get_mut(&client_id) else {
                    return false;
                };
                if client.render_pending {
                    client.render_pending = false;
                    true
                } else {
                    false
                }
            }
            #[cfg(unix)]
            ServerEvent::HostEvent(envelope) => {
                self.handle_host_event(envelope);
                true
            }
            #[cfg(unix)]
            ServerEvent::RemotePaneAttachEstablished {
                host,
                terminal_id,
                local_pane,
                generation,
                focus_epoch,
                attach,
            } => {
                self.handle_remote_pane_attach_established(
                    host,
                    terminal_id,
                    local_pane,
                    generation,
                    focus_epoch,
                    attach,
                );
                true
            }
            #[cfg(unix)]
            ServerEvent::RemotePaneAttachFailed {
                host,
                terminal_id,
                reason,
            } => {
                self.handle_remote_pane_attach_failed(host, terminal_id, reason);
                true
            }
            ServerEvent::QuitSignal => {
                // The quit check at the top of the loop handles this.
                // No render needed — the next iteration will initiate shutdown.
                false
            }
        }
    }

    fn ignore_client_event_during_handoff(ev: &ServerEvent) -> bool {
        !matches!(
            ev,
            ServerEvent::ClientConnected { .. }
                | ServerEvent::ClientDisconnected { .. }
                | ServerEvent::ClientWriterDrained { .. }
                | ServerEvent::QuitSignal
        )
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
        self.handle_api_request_with_shutdown_check_inner(msg, false)
    }

    fn handle_api_request_with_shutdown_check_inner(
        &mut self,
        msg: api::ApiRequestMessage,
        skip_default_workspace_for_request: bool,
    ) -> bool {
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

        if let api::schema::Method::ServerLiveHandoff(params) = &msg.request.method {
            let response = match self.perform_live_handoff(params.clone()) {
                Ok(()) => serde_json::to_string(&api::schema::SuccessResponse {
                    id: msg.request.id,
                    result: api::schema::ResponseResult::Ok {},
                }),
                Err(err) => serde_json::to_string(&api::schema::ErrorResponse {
                    id: msg.request.id,
                    error: api::schema::ErrorBody {
                        code: "handoff_failed".into(),
                        message: err.to_string(),
                    },
                }),
            }
            .unwrap_or_else(|_| "{}".to_string());
            let _ = msg.respond_to.send(response);
            return true;
        }

        if let api::schema::Method::NotificationShow(params) = &msg.request.method {
            let response =
                self.handle_notification_show_api(msg.request.id.clone(), params.clone());
            let _ = msg.respond_to.send(response);
            return true;
        }

        // Host-link state (`HostLinkRegistry`/`RemotePaneRegistry`) lives on
        // `HeadlessServer`, not `App` -- `app` sits below `server` in the
        // dependency graph and can't hold server-layer newtypes -- so these
        // three methods are intercepted here, the same way `ServerLiveHandoff`
        // is above, instead of reaching `App::handle_api_request_*`.
        if let api::schema::Method::HostAttach(params) = &msg.request.method {
            let response = self.handle_host_attach_api(msg.request.id.clone(), params.clone());
            let _ = msg.respond_to.send(response);
            return true;
        }
        if matches!(&msg.request.method, api::schema::Method::HostList(_)) {
            let response = self.handle_host_list_api(msg.request.id.clone());
            let _ = msg.respond_to.send(response);
            return true;
        }
        if let api::schema::Method::HostDetach(params) = &msg.request.method {
            let response = self.handle_host_detach_api(msg.request.id.clone(), params.clone());
            let _ = msg.respond_to.send(response);
            return true;
        }

        match &msg.request.method {
            api::schema::Method::ClientWindowTitleSet(params) => {
                let response = self.handle_client_window_title_api(
                    msg.request.id.clone(),
                    Some(params.title.clone()),
                );
                let _ = msg.respond_to.send(response);
                return true;
            }
            api::schema::Method::ClientWindowTitleClear(_) => {
                let response = self.handle_client_window_title_api(msg.request.id.clone(), None);
                let _ = msg.respond_to.send(response);
                return true;
            }
            _ => {}
        }

        let mut changed = api::request_changes_ui(&msg.request);
        let skip_default_workspace = skip_default_workspace_for_request
            || matches!(
                &msg.request.method,
                api::schema::Method::ServerStop(_) | api::schema::Method::ServerLiveHandoff(_)
            );
        changed |= self.drain_all_internal_events_with_forwarding();

        // Capture toast and effective pane states before the API call so we can
        // forward resulting client-local notifications. API requests like
        // pane.report_agent trigger handle_internal_event internally, which
        // bypasses drain_internal_events_with_forwarding. Headless mode disables
        // local sound playback, so sound notifications need to be forwarded here.
        let toast_before = self.app.state.toast.clone();
        let pane_states_before: Vec<(
            usize,
            crate::layout::PaneId,
            crate::detect::AgentState,
            Option<String>,
        )> = {
            let terminals = &self.app.state.terminals;
            self.app
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs.iter().flat_map(move |tab| {
                        tab.panes.iter().filter_map(move |(&pane_id, pane)| {
                            terminals.get(&pane.attached_terminal_id).map(|terminal| {
                                (
                                    ws_idx,
                                    pane_id,
                                    terminal.state,
                                    terminal.effective_agent_label().map(str::to_string),
                                )
                            })
                        })
                    })
                })
                .collect()
        };

        self.sync_foreground_client_state();
        if matches!(
            &msg.request.method,
            api::schema::Method::WorktreeCreate(_) | api::schema::Method::WorktreeRemove(_)
        ) {
            let deferred_changed = self
                .app
                .handle_deferred_worktree_api_request(msg.request, msg.respond_to);
            return changed | deferred_changed;
        }
        let response = if matches!(
            &msg.request.method,
            api::schema::Method::ServerReloadConfig(_)
        ) {
            let report = self.reload_server_config(true);
            serde_json::to_string(&api::schema::SuccessResponse {
                id: msg.request.id.clone(),
                result: api::schema::ResponseResult::ConfigReload {
                    status: report.status,
                    diagnostics: report.diagnostics,
                },
            })
            .unwrap_or_else(|err| {
                serde_json::to_string(&api::schema::ErrorResponse {
                    id: String::new(),
                    error: api::schema::ErrorBody {
                        code: "serialization_error".into(),
                        message: err.to_string(),
                    },
                })
                .unwrap_or_else(|_| "{}".to_string())
            })
        } else {
            self.app
                .handle_api_request_after_internal_events_drained(msg.request)
        };
        let _ = msg.respond_to.send(response);

        // Forward new toast state only when a client-local delivery mode is selected.
        // Herdr delivery renders the toast in-frame and must not ask clients to
        // show a terminal or system notification.
        let toast_after = self.app.state.toast.clone();
        let forwarded_toast_from_state = if should_forward_toast_to_clients(
            self.app.state.toast_config.delivery,
        ) && toast_after.is_some()
            && toast_after != toast_before
        {
            if let Some(toast) = &toast_after {
                debug!(title = %toast.title, body = %toast.context, "forwarding toast notification from API request");
                self.send_notify_to_foreground_client(
                    toast_notify_kind(self.app.state.toast_config.delivery)
                        .expect("toast forwarding requires a client notification kind"),
                    &toast.title,
                    non_empty_body(&toast.context),
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // Forward notifications for effective pane state changes that occurred
        // during the API request. Hook authority is already folded into
        // pane.state, so raw hook transitions must not produce separate sounds.
        for (ws_idx, pane_id, prev_state, prev_agent_label) in &pane_states_before {
            let pane_after = self
                .app
                .state
                .workspaces
                .get(*ws_idx)
                .and_then(|ws| ws.tabs.iter().find_map(|tab| tab.panes.get(pane_id)));

            let Some(pane_after) = pane_after else {
                continue;
            };

            let Some(terminal_after) = self
                .app
                .state
                .terminals
                .get(&pane_after.attached_terminal_id)
            else {
                continue;
            };

            let new_state = terminal_after.state;
            if new_state == *prev_state {
                continue;
            }

            let is_active_tab = self.app.state.pane_is_in_active_tab(*ws_idx, *pane_id);
            let suppress_active_tab_notifications =
                self.active_tab_suppresses_notifications(is_active_tab);

            let agent = terminal_after.effective_known_agent();
            let agent_label = terminal_after.effective_agent_label().map(str::to_string);

            debug!(
                ws_idx,
                pane_id = pane_id.raw(),
                prev_state = ?prev_state,
                new_state = ?new_state,
                agent = ?agent,
                "pane effective state changed during API request, checking notification"
            );

            if !forwarded_toast_from_state
                && self.app.state.toast_config.delay_seconds == 0
                && should_forward_toast_to_clients(self.app.state.toast_config.delivery)
            {
                if let Some(kind) =
                    crate::app::actions::notification_toast_for_state_change_with_agent_labels(
                        suppress_active_tab_notifications,
                        *prev_state,
                        new_state,
                        prev_agent_label.as_deref(),
                        agent_label.as_deref(),
                    )
                {
                    if let Some(agent_label) = self
                        .app
                        .state
                        .terminals
                        .get(&pane_after.attached_terminal_id)
                        .and_then(|terminal| terminal.effective_agent_label())
                    {
                        let event_text = match kind {
                            crate::app::state::ToastKind::NeedsAttention => "needs attention",
                            crate::app::state::ToastKind::Finished => "finished",
                            crate::app::state::ToastKind::UpdateInstalled => "updated",
                        };
                        let workspace_label = self.app.state.workspaces[*ws_idx].display_name_from(
                            &self.app.state.terminals,
                            &self.app.terminal_runtimes,
                        );
                        let context = crate::app::actions::notification_context(
                            &self.app.state.workspaces[*ws_idx],
                            &workspace_label,
                            *ws_idx,
                            *pane_id,
                        );
                        self.send_notify_to_foreground_client(
                            toast_notify_kind(self.app.state.toast_config.delivery)
                                .expect("toast forwarding requires a client notification kind"),
                            format!("{agent_label} {event_text}"),
                            non_empty_body(&context),
                        );
                    }
                }
            }

            // Forward sound notification when server-side sound policy allows it.
            // Clients still decide locally whether they can execute the side effect.
            if self.app.state.toast_config.delay_seconds == 0 && self.app.state.sound.allows(agent)
            {
                if let Some(sound) =
                    crate::app::actions::notification_sound_for_state_change_with_agent_labels(
                        suppress_active_tab_notifications,
                        *prev_state,
                        new_state,
                        prev_agent_label.as_deref(),
                        agent_label.as_deref(),
                    )
                {
                    debug!(sound = ?sound, "forwarding sound notification from API request");
                    self.send_notify_to_foreground_client(
                        protocol::NotifyKind::Sound,
                        sound_notify_message(sound),
                        None,
                    );
                }
            }
        }

        if !skip_default_workspace && latest_app_client(&self.clients).is_some() {
            changed |= self.app.ensure_default_workspace();
        }

        changed
    }

    fn stream_host_mouse_capture_mode(&mut self) {
        let enabled = self
            .app
            .state
            .should_capture_host_mouse_from(&self.app.terminal_runtimes);
        let serialized = match Self::frame_server_message(&ServerMessage::MouseCapture { enabled })
        {
            Ok(framed) => framed,
            Err(err) => {
                warn!(err = %err, "failed to serialize mouse capture mode for clients");
                return;
            }
        };

        let mut broken_clients: Vec<u64> = Vec::new();
        for (&client_id, client) in &mut self.clients {
            if !client.is_full_app_client() {
                continue;
            }
            if client.host_mouse_capture_active == Some(enabled) {
                continue;
            }
            let Some(writer) = &client.writer else {
                continue;
            };
            if writer.control.send(serialized.clone()).is_err() {
                debug!(
                    client_id,
                    "client writer channel closed during mouse capture update"
                );
                broken_clients.push(client_id);
                continue;
            }
            client.host_mouse_capture_active = Some(enabled);
        }

        for client_id in broken_clients {
            self.remove_client_and_resize_if_needed(client_id);
        }
    }

    /// Renders the current state to client-sized virtual buffers and streams
    /// frames to all connected clients.
    fn render_retained_pty_update_and_stream(&mut self) -> bool {
        crate::render_prof::event("retained.attempt");
        let retained_started = crate::render_prof::timer();
        macro_rules! retained_fallback {
            ($reason:literal) => {{
                crate::render_prof::event(concat!("retained_fallback.", $reason));
                crate::render_prof::duration_since("retained.total", retained_started);
                return false;
            }};
        }
        macro_rules! retained_success {
            ($reason:literal) => {{
                crate::render_prof::event("retained.success");
                crate::render_prof::event(concat!("retained_success.", $reason));
                crate::render_prof::duration_since("retained.total", retained_started);
                return true;
            }};
        }

        if !self.retained_pty_update_allowed_by_app_state() {
            retained_fallback!("unsafe_app_state");
        }

        let render_targets = render_targets(&self.clients, self.foreground_client_id);
        let [(client_id, (cols, rows), cell_size, _is_foreground, mode)] =
            render_targets.as_slice()
        else {
            retained_fallback!("multiple_or_no_target");
        };
        if !matches!(mode, ClientConnectionMode::App) {
            retained_fallback!("not_app_client");
        }
        let Some(client) = self.clients.get(client_id) else {
            retained_fallback!("client_missing");
        };
        if client.render_pending {
            retained_fallback!("render_pending");
        }
        if self.app.state.kitty_graphics_enabled && !client.graphics_cache.is_empty() {
            retained_fallback!("graphics_cache_active");
        }
        if client.graphics_surface_reset_pending {
            retained_fallback!("graphics_surface_reset");
        }
        if self.app.state.kitty_graphics_enabled
            && cell_size.is_known()
            && crate::kitty_graphics::has_visible_pane_graphics(
                &self.app.state,
                &self.app.terminal_runtimes,
                *cell_size,
            )
        {
            retained_fallback!("visible_kitty_graphics");
        }
        let Some(mut frame) = client.render_state.last_frame().cloned() else {
            retained_fallback!("no_last_frame");
        };
        if frame.width != *cols || frame.height != *rows {
            retained_fallback!("frame_size_mismatch");
        }
        frame.graphics.clear();

        let Some(ws_idx) = self.app.state.active else {
            retained_fallback!("no_active_workspace");
        };
        let pane_infos = self.app.state.view.pane_infos.clone();
        if pane_infos.is_empty() {
            retained_fallback!("no_pane_info");
        }

        let mut touched = false;
        for info in pane_infos {
            if !rect_fits_frame(info.inner_rect, &frame) {
                retained_fallback!("pane_rect_outside_frame");
            }
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                ws_idx,
                info.id,
            ) else {
                retained_fallback!("missing_runtime");
            };
            match runtime.collect_dirty_patch(info.inner_rect.width, info.inner_rect.height) {
                crate::pane::TerminalDirtyPatchOutcome::Clean => {
                    crate::render_prof::event("retained.pane_clean");
                }
                crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                    retained_fallback!("dirty_patch_fallback");
                }
                crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => {
                    crate::render_prof::event("retained.pane_patch");
                    crate::render_prof::counter("retained.patch_rows", patch.rows.len() as u64);
                    if dirty_patch_intersects_hyperlinks(&frame, info.inner_rect, &patch) {
                        retained_fallback!("hyperlink_intersection");
                    }
                    if !apply_terminal_dirty_patch(&mut frame, info.inner_rect, patch) {
                        retained_fallback!("patch_apply_failed");
                    }
                    touched = true;
                }
            }
        }

        let previous_cursor = frame.cursor.clone();
        frame.cursor = crate::server::render_stream::focused_terminal_cursor(
            &self.app.state,
            &self.app.terminal_runtimes,
        );
        let cursor_changed = frame.cursor != previous_cursor;

        if !touched && !cursor_changed {
            retained_success!("clean_no_cursor_change");
        }

        let mut broken_clients = Vec::new();
        let sent = self.send_retained_frame_to_client(*client_id, frame, &mut broken_clients);
        for broken_client in broken_clients {
            self.remove_client_and_resize_if_needed(broken_client);
        }
        if sent {
            retained_success!("sent");
        }
        retained_fallback!("send_failed");
    }

    fn retained_pty_update_allowed_by_app_state(&self) -> bool {
        self.app.state.mode == app::Mode::Terminal
            && self.app.state.selection.is_none()
            && self.app.state.copy_mode.is_none()
            && self.app.state.context_menu.is_none()
            && self.app.state.toast.is_none()
            && self.app.state.copy_feedback.is_none()
            && !self.app.full_redraw_pending
    }

    fn send_retained_frame_to_client(
        &mut self,
        client_id: u64,
        frame: FrameData,
        broken_clients: &mut Vec<u64>,
    ) -> bool {
        let Some(client) = self.clients.get_mut(&client_id) else {
            crate::render_prof::event("retained_send_fallback.client_missing");
            return false;
        };
        let Some(writer) = client.writer.as_ref().cloned() else {
            crate::render_prof::event("retained_send_fallback.writer_missing");
            return false;
        };
        let prepare_started = crate::render_prof::timer();
        let Some(prepared) = client.render_state.prepare_frame(frame) else {
            client.render_pending = false;
            crate::render_prof::event("retained_send.skip_identical");
            crate::render_prof::duration_since("retained_send.prepare_frame", prepare_started);
            return true;
        };
        crate::render_prof::duration_since("retained_send.prepare_frame", prepare_started);
        let serialize_started = crate::render_prof::timer();
        let serialized = match Self::frame_server_message(prepared.message()) {
            Ok(framed) => {
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                framed
            }
            Err(protocol::FramingError::Oversized { claimed, max }) => {
                warn!(
                    client_id,
                    claimed, max, "skipping oversized retained frame for client"
                );
                crate::render_prof::event("retained_send_fallback.serialize_oversized");
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                return false;
            }
            Err(err) => {
                warn!(client_id, err = %err, "failed to serialize retained frame for client");
                broken_clients.push(client_id);
                crate::render_prof::event("retained_send_fallback.serialize_error");
                crate::render_prof::duration_since("retained_send.serialize", serialize_started);
                return false;
            }
        };
        crate::render_prof::counter("retained_send.bytes", serialized.len() as u64);

        let send_started = crate::render_prof::timer();
        match writer.render.try_send(serialized) {
            Ok(()) => {
                client.render_pending = false;
                client.render_state.commit_sent_frame(prepared);
                crate::render_prof::event("retained_send.sent");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                true
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                client.render_pending = true;
                crate::render_prof::event("retained_send_fallback.queue_full");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                debug!(
                    client_id,
                    "render queue full, deferring latest retained frame"
                );
                false
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                debug!(client_id, "client writer channel closed, marking as broken");
                broken_clients.push(client_id);
                crate::render_prof::event("retained_send_fallback.writer_disconnected");
                crate::render_prof::duration_since("retained_send.try_send", send_started);
                false
            }
        }
    }

    fn render_and_stream(&mut self) {
        let full_started = crate::render_prof::timer();
        let render_targets = render_targets(&self.clients, self.foreground_client_id);

        if render_targets.is_empty() {
            let (cols, rows) = self.effective_size;
            let area = Rect::new(0, 0, cols, rows);
            let resize_panes = self.app.state.view.pane_infos.is_empty();
            let render_started = crate::render_prof::timer();
            let _ = crate::server::render_stream::render_virtual_with_runtime_registry(
                &mut self.app.state,
                &self.app.terminal_runtimes,
                area,
                resize_panes,
                crate::kitty_graphics::HostCellSize::default(),
            );
            crate::render_prof::duration_since("full_render.render_virtual", render_started);
            self.app.full_redraw_pending = false;
            crate::render_prof::duration_since("full_render.total", full_started);
            debug!(
                cols,
                rows, resize_panes, "rendered virtual frame with no attached clients"
            );
            return;
        }

        let mut broken_clients: Vec<u64> = Vec::new();
        let mut deferred_frame = false;
        for (client_id, (cols, rows), cell_size, is_foreground, mode) in render_targets {
            let area = Rect::new(0, 0, cols, rows);
            let is_app_client = matches!(mode, ClientConnectionMode::App);
            let mut frame = match mode {
                ClientConnectionMode::App => {
                    let render_started = crate::render_prof::timer();
                    let (mut buffer, mut cursor) =
                        if self.app.state.kitty_graphics_enabled && cell_size.is_known() {
                            crate::server::render_stream::render_virtual_with_runtime_registry(
                                &mut self.app.state,
                                &self.app.terminal_runtimes,
                                area,
                                is_foreground,
                                cell_size,
                            )
                        } else {
                            crate::server::render_stream::render_virtual_with_runtime_registry(
                                &mut self.app.state,
                                &self.app.terminal_runtimes,
                                area,
                                is_foreground,
                                crate::kitty_graphics::HostCellSize::default(),
                            )
                        };
                    crate::render_prof::duration_since(
                        "full_render.render_virtual",
                        render_started,
                    );

                    // Task 9b, HALF 2 (VIEW-ONLY): when a remote pane is
                    // focused and its runtime is registered, substitute its
                    // live grid for the workspace panes in the content rect
                    // -- reusing `render_terminal_virtual` (no bespoke grid
                    // renderer), pure (only rewrites this client's own
                    // buffer/cursor, no server/app state touched here).
                    // `terminal_area` was just computed for THIS client's
                    // own size by the `render_virtual_with_runtime_registry`
                    // call above.
                    let focused_remote_pane_runtime = self.focused_remote_pane_render_target();
                    if let Some(runtime) = focused_remote_pane_runtime {
                        crate::server::render_stream::overlay_focused_remote_pane(
                            &mut buffer,
                            &mut cursor,
                            runtime,
                            self.app.state.view.terminal_area,
                        );
                    }
                    let remote_pane_focused = focused_remote_pane_runtime.is_some();

                    let hyperlinks_started = crate::render_prof::timer();
                    // Local pane hyperlinks would refer to inner_rects now
                    // covered by the remote pane's grid; skip them while a
                    // remote pane is shown instead of the workspace panes.
                    let hyperlinks = if remote_pane_focused {
                        Vec::new()
                    } else {
                        crate::server::render_stream::visible_hyperlinks(
                            &self.app.state,
                            &self.app.terminal_runtimes,
                        )
                    };
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
                ClientConnectionMode::TerminalAttach { terminal_id }
                | ClientConnectionMode::TerminalObserve { terminal_id } => {
                    let Some(runtime) = self.runtime_for_terminal_id_string(&terminal_id) else {
                        self.send_to_client(
                            client_id,
                            ServerMessage::ServerShutdown {
                                reason: Some(format!(
                                    "terminal attach ended: terminal {terminal_id} not found"
                                )),
                            },
                        );
                        broken_clients.push(client_id);
                        continue;
                    };
                    let render_started = crate::render_prof::timer();
                    let (buffer, cursor) =
                        crate::server::render_stream::render_terminal_virtual(runtime, area);
                    crate::render_prof::duration_since(
                        "full_render.render_terminal_virtual",
                        render_started,
                    );
                    let hyperlinks_started = crate::render_prof::timer();
                    let hyperlinks = runtime.visible_hyperlinks(area);
                    crate::render_prof::duration_since(
                        "full_render.visible_hyperlinks",
                        hyperlinks_started,
                    );
                    let frame_started = crate::render_prof::timer();
                    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(
                        &buffer,
                        cursor,
                        &hyperlinks,
                    );
                    crate::render_prof::duration_since("full_render.frame_build", frame_started);
                    frame
                }
            };

            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let mut next_graphics_cache = client.graphics_cache.clone();
            let graphics_surface_reset_pending = client.graphics_surface_reset_pending;
            if is_app_client && self.app.state.kitty_graphics_enabled && cell_size.is_known() {
                if graphics_surface_reset_pending {
                    frame.graphics = next_graphics_cache.clear_bytes();
                }
                let graphics_started = crate::render_prof::timer();
                frame
                    .graphics
                    .extend(crate::kitty_graphics::encode_local_pane_graphics(
                        &self.app.state,
                        &self.app.terminal_runtimes,
                        cell_size,
                        &mut next_graphics_cache,
                    ));
                crate::render_prof::duration_since("full_render.graphics_encode", graphics_started);
            } else {
                frame.graphics = next_graphics_cache.clear_bytes();
            }

            let Some(writer) = client.writer.as_ref().cloned() else {
                crate::render_prof::event("full_render.writer_missing");
                continue;
            };

            let mut commit_graphics_cache = true;
            if frame.graphics.len() > MAX_GRAPHICS_FRAME_SIZE {
                warn!(
                    client_id,
                    graphics_bytes = frame.graphics.len(),
                    max = MAX_GRAPHICS_FRAME_SIZE,
                    "dropping oversized graphics payload for client frame"
                );
                frame.graphics.clear();
                commit_graphics_cache = false;
            }

            let max_frame_size = if frame.graphics.is_empty() {
                MAX_FRAME_SIZE
            } else {
                MAX_GRAPHICS_FRAME_SIZE
            };
            let has_graphics = !frame.graphics.is_empty();
            let prepare_started = crate::render_prof::timer();
            let Some(mut prepared) = client.render_state.prepare_frame(frame) else {
                client.render_pending = false;
                crate::render_prof::event("full_render.skip_identical");
                crate::render_prof::duration_since("full_render.prepare_frame", prepare_started);
                continue;
            };
            crate::render_prof::duration_since("full_render.prepare_frame", prepare_started);

            let serialize_started = crate::render_prof::timer();
            let serialized = match Self::frame_server_message_with_max(
                prepared.message(),
                max_frame_size,
            ) {
                Ok(framed) => {
                    crate::render_prof::duration_since("full_render.serialize", serialize_started);
                    framed
                }
                Err(protocol::FramingError::Oversized { claimed, max }) if has_graphics => {
                    warn!(
                        client_id,
                        claimed, max, "dropping graphics from oversized frame for client"
                    );
                    let Some(mut text_only_frame) = prepared.into_frame() else {
                        crate::render_prof::event("full_render.serialize_error");
                        crate::render_prof::duration_since(
                            "full_render.serialize",
                            serialize_started,
                        );
                        continue;
                    };
                    text_only_frame.graphics.clear();
                    let Some(text_only_prepared) =
                        client.render_state.prepare_frame(text_only_frame)
                    else {
                        client.render_pending = false;
                        crate::render_prof::event("full_render.skip_identical_text_only");
                        crate::render_prof::duration_since(
                            "full_render.serialize",
                            serialize_started,
                        );
                        continue;
                    };
                    let framed = match Self::frame_server_message(text_only_prepared.message()) {
                        Ok(framed) => framed,
                        Err(err) => {
                            warn!(client_id, err = %err, "failed to serialize text-only frame for client");
                            broken_clients.push(client_id);
                            crate::render_prof::event("full_render.serialize_error");
                            crate::render_prof::duration_since(
                                "full_render.serialize",
                                serialize_started,
                            );
                            continue;
                        }
                    };
                    prepared = text_only_prepared;
                    commit_graphics_cache = false;
                    crate::render_prof::duration_since("full_render.serialize", serialize_started);
                    framed
                }
                Err(protocol::FramingError::Oversized { claimed, max }) => {
                    warn!(
                        client_id,
                        claimed, max, "skipping oversized frame for client"
                    );
                    crate::render_prof::event("full_render.serialize_oversized");
                    crate::render_prof::duration_since("full_render.serialize", serialize_started);
                    continue;
                }
                Err(err) => {
                    warn!(client_id, err = %err, "failed to serialize frame for client");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.serialize_error");
                    crate::render_prof::duration_since("full_render.serialize", serialize_started);
                    continue;
                }
            };
            crate::render_prof::counter("full_render.bytes", serialized.len() as u64);

            let send_started = crate::render_prof::timer();
            match writer.render.try_send(serialized) {
                Ok(()) => {
                    client.render_pending = false;
                    if commit_graphics_cache {
                        client.graphics_cache = next_graphics_cache;
                        client.graphics_surface_reset_pending = false;
                    }
                    client.render_state.commit_sent_frame(prepared);
                    crate::render_prof::event("full_render.sent");
                    crate::render_prof::duration_since("full_render.try_send", send_started);
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.render_pending = true;
                    deferred_frame = true;
                    crate::render_prof::event("full_render.queue_full");
                    crate::render_prof::duration_since("full_render.try_send", send_started);
                    debug!(client_id, "render queue full, deferring latest frame");
                    continue;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    debug!(client_id, "client writer channel closed, marking as broken");
                    broken_clients.push(client_id);
                    crate::render_prof::event("full_render.writer_disconnected");
                    crate::render_prof::duration_since("full_render.try_send", send_started);
                    continue;
                }
            }
        }

        if !broken_clients.is_empty() {
            for client_id in broken_clients {
                self.remove_client_and_resize_if_needed(client_id);
            }
        }

        let (cols, rows) = self.effective_size;
        if !deferred_frame {
            self.app.full_redraw_pending = false;
        }
        crate::render_prof::duration_since("full_render.total", full_started);
        debug!(cols, rows, foreground_client_id = ?self.foreground_client_id, "rendered virtual frame(s)");
    }

    /// Handle scheduled tasks for the headless server.
    ///
    /// Similar to `App::handle_scheduled_tasks` but without resize polling
    /// (the server doesn't have a terminal to resize).
    fn handle_scheduled_tasks_headless(&mut self, now: Instant, geometry_dirty: bool) -> bool {
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
            .state
            .next_pending_agent_notification_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let previous_toast = self.app.state.toast.clone();
            let mut deliveries = self.app.state.drain_due_agent_notifications(now);
            if !deliveries.is_empty() {
                self.app
                    .refresh_agent_notification_delivery_contexts(&mut deliveries);
                self.app.sync_toast_deadline(previous_toast);
                for delivery in &deliveries {
                    self.forward_agent_notification_delivery(delivery);
                }
                changed = true;
            }
        }

        if self
            .app
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.copy_feedback_deadline = None;
            self.app.state.copy_feedback = None;
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
            .selection_autoscroll_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.tick_selection_autoscroll(now);
            changed = true;
        }

        changed |= self.app.clear_due_selection_highlight(now);

        if self.has_app_client() {
            self.app.start_git_status_refresh_if_due(now);
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
            .next_agent_manifest_update_check
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.run_agent_manifest_update_check();
        }

        if self
            .app
            .session_save_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.app.save_session_now();
        }

        if let Some(deadline) = self
            .app
            .agent_metadata_deadline
            .filter(|deadline| now >= *deadline)
        {
            let previous_toast = self.app.state.toast.clone();
            for update in self.app.state.expire_agent_metadata_at(deadline, now) {
                self.app
                    .refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
                self.app.emit_pane_state_update(&update);
            }
            self.app.sync_agent_metadata_deadline();
            changed = true;
        }

        if geometry_dirty || self.foreground_client_id.is_none() {
            self.app.pending_agent_resume_deadline = None;
        } else {
            self.app.sync_pending_agent_resume_deadline(now);
            changed |= self
                .app
                .start_pending_agent_resumes(self.app.pending_agent_resume_due(now));
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

        // Clear client-local host graphics, then send ServerShutdown to all connected clients.
        self.send_all_clients_graphics_cleanup();
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
            self.send_all_clients_graphics_cleanup();
            let shutdown_msg = ServerMessage::ServerShutdown {
                reason: Some("server is shutting down".to_owned()),
            };
            self.send_to_all_clients(shutdown_msg);

            // Give writer threads a moment to flush before closing.
            std::thread::sleep(Duration::from_millis(50));
        }

        // Drain remaining API requests with server_unavailable.
        self.drain_api_requests_with_shutdown_check();

        // Stop and join every per-host event loop thread.
        #[cfg(unix)]
        self.stop_all_host_link_runtimes();

        // Close all client connections.
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);

        // Remove socket files.
        self.cleanup_sockets()?;

        Ok(())
    }

    /// Removes socket files created by the server.
    fn cleanup_sockets(&self) -> io::Result<()> {
        if let Err(err) =
            remove_socket_file_if_owned(&self.client_socket_path, &self.client_socket_identity)
        {
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
        // Symmetry with `complete_shutdown`: stop+join per-host loop threads
        // even on an abrupt drop (a no-op if shutdown already drained them).
        #[cfg(unix)]
        self.stop_all_host_link_runtimes();
        let staged_files = self
            .clients
            .drain()
            .flat_map(|(_, client)| client.staged_clipboard_files)
            .collect::<Vec<_>>();
        crate::server::clipboard_image::remove_files(staged_files);
        let _ = self.cleanup_sockets();
    }
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

fn sanitize_notification_text(value: &str, max_chars: usize) -> Option<String> {
    let mut sanitized = String::new();
    let mut previous_space = false;
    for ch in value.chars() {
        let replacement = if ch == '\n' || ch == '\r' || ch == '\t' {
            Some(' ')
        } else if ch.is_control() {
            None
        } else {
            Some(ch)
        };
        let Some(ch) = replacement else {
            continue;
        };
        if ch.is_whitespace() {
            if previous_space {
                continue;
            }
            previous_space = true;
            sanitized.push(' ');
        } else {
            previous_space = false;
            sanitized.push(ch);
        }
        if sanitized.chars().count() >= max_chars {
            break;
        }
    }
    let sanitized = sanitized.trim().to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn sanitize_window_title_text(value: &str, max_chars: usize) -> Option<String> {
    let sanitized = value
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}') && !ch.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn server_config_diagnostic_summaries(diagnostics: &[String]) -> (Option<String>, Option<String>) {
    let without_keybindings = diagnostics
        .iter()
        .filter(|diagnostic| !is_keybinding_config_diagnostic(diagnostic))
        .cloned()
        .collect::<Vec<_>>();
    (
        config::config_diagnostic_summary(diagnostics),
        config::config_diagnostic_summary(&without_keybindings),
    )
}

fn is_keybinding_config_diagnostic(diagnostic: &str) -> bool {
    diagnostic.contains("keybinding") || diagnostic.contains("keys.")
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the headless server. This is the entry point called from main.rs.
pub fn run_server() -> io::Result<()> {
    init_logging();
    crate::platform::raise_server_nofile_limit();

    let args: Vec<String> = std::env::args().collect();
    if args.get(2).map(String::as_str) == Some("--handoff-import") {
        let socket_path = args
            .get(3)
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff socket"))?;
        let token = args
            .get(4)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing handoff token"))?;
        return run_handoff_import_server(&socket_path, token);
    }

    let loaded_config = config::Config::load();
    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();

    // Start the JSON API socket server.
    let _api_server = match api::start_server(api_tx.clone(), event_hub.clone()) {
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
        .map_err(io::Error::other)?;

    let result = rt.block_on(async {
        // Create the App (with AppState, event channels, etc.).
        let mut app = app::App::new(
            &loaded_config.config,
            no_session,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub,
        );
        seed_startup_workspace_if_empty(&mut app);

        // The server runs headless — disable local notification side effects.
        // Sound and terminal notifications are forwarded to connected clients
        // as ServerMessage::Notify instead of emitted by the server process.
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;

        // Host links (Task 10) must be re-attached AFTER the HeadlessServer
        // exists -- attaching spawns the per-host poll loop onto
        // `host_event_tx`, which only exists once the server's event bridge
        // is running. Take the restored list out of `app` before it moves
        // into `HeadlessServer::new`.
        let restored_hosts = std::mem::take(&mut app.restored_hosts);

        // Create the headless server.
        let mut server = match HeadlessServer::new(
            app,
            &loaded_config.diagnostics,
            Some(api_tx.clone()),
            Some(_api_server),
        ) {
            Ok(server) => server,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                eprintln!("error: herdr server is already running");
                eprintln!("client socket: {}", client_socket_path().display());
                std::process::exit(1);
            }
            Err(err) => return Err(err),
        };
        server.restore_attached_hosts(restored_hosts);

        info!(
            api_socket = %api::socket_path().display(),
            client_socket = %client_socket_path().display(),
            "herdr server started"
        );
        print_ready_message(&api::socket_path(), &client_socket_path());

        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

fn seed_startup_workspace_if_empty(app: &mut app::App) {
    let Some(cwd) = take_startup_cwd() else {
        return;
    };

    if !app.state.workspaces.is_empty() {
        info!(
            cwd = %cwd.display(),
            "restored session already has workspaces; ignoring startup cwd"
        );
        return;
    }

    match app.create_workspace_with_options(cwd.clone(), true) {
        Ok(_) => {
            info!(cwd = %cwd.display(), "created startup workspace");
        }
        Err(err) => {
            warn!(cwd = %cwd.display(), err = %err, "failed to create startup workspace");
            app.state.mode = app::Mode::Navigate;
        }
    }
}

fn take_startup_cwd() -> Option<PathBuf> {
    let cwd = std::env::var_os(crate::server::autodetect::STARTUP_CWD_ENV_VAR)?;
    std::env::remove_var(crate::server::autodetect::STARTUP_CWD_ENV_VAR);
    (!cwd.is_empty()).then(|| PathBuf::from(cwd))
}

#[cfg(unix)]
fn run_handoff_import_server(socket_path: &Path, token: &str) -> io::Result<()> {
    let loaded_config = config::Config::load();
    let mut received = crate::server::handoff::receive(socket_path, token)?;
    crate::server::handoff::log_import_result(received.manifest.panes.len());

    let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_hub = api::EventHub::default();

    let mut imports = HashMap::new();
    for (pane, fd) in received.manifest.panes.into_iter().zip(received.fds) {
        let pane_id = pane.pane_id;
        imports.insert(
            pane_id,
            crate::handoff_runtime::ImportedHandoffRuntime {
                master_fd: fd,
                state: pane,
            },
        );
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let result = rt.block_on(async {
        let mut app = app::App::new_from_handoff(
            &loaded_config.config,
            config::config_diagnostic_summary(&loaded_config.diagnostics),
            api_rx,
            event_hub.clone(),
            &received.manifest.snapshot,
            &mut imports,
        )?;
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;
        // Persisted host links must be re-attached AFTER the HeadlessServer
        // (and its host_event bridge) exists, so take them out before `app`
        // moves into `HeadlessServer::new`. Mirrors `run_server`'s ordering.
        let restored_hosts = std::mem::take(&mut app.restored_hosts);
        crate::server::handoff::report_restored(&mut received.stream)?;
        if std::env::var("HERDR_TEST_HANDOFF_IMPORT_FAIL").as_deref() == Ok("after_restored") {
            return Err(io::Error::other(
                "test handoff import failure after restored",
            ));
        }
        wait_for_old_public_sockets_to_close(Duration::from_secs(5))?;

        let api_server = api::start_server(api_tx.clone(), event_hub.clone())?;
        let mut server = HeadlessServer::new(
            app,
            &loaded_config.diagnostics,
            Some(api_tx.clone()),
            Some(api_server),
        )?;
        server.restore_attached_hosts(restored_hosts);
        crate::server::handoff::report_ready(&mut received.stream)?;
        crate::server::handoff::wait_committed(&mut received.stream)?;
        server.app.assume_handoff_ownership();
        server.app.unpause_handoff_readers();
        server.pending_handoff_repaint_nudge = true;
        if let Err(err) = crate::server::handoff::report_owned(&mut received.stream) {
            warn!(err = %err, "failed to report handoff ownership; continuing as owner");
        }
        info!("handoff import server started");
        print_ready_message(&api::socket_path(), &client_socket_path());
        server.run().await
    });

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("server");
    result
}

#[cfg(unix)]
fn wait_for_old_public_sockets_to_close(timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let api_socket = api::socket_path();
    let client_socket = client_socket_path();
    while Instant::now() < deadline {
        let api_open = api_socket.exists() && crate::ipc::connect_local_stream(&api_socket).is_ok();
        let client_open =
            client_socket.exists() && crate::ipc::connect_local_stream(&client_socket).is_ok();
        if !api_open && !client_open {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "old server sockets did not close before handoff import bind",
    ))
}

#[cfg(not(unix))]
fn run_handoff_import_server(_socket_path: &Path, _token: &str) -> io::Result<()> {
    Err(io::Error::other("live handoff is only supported on Unix"))
}

fn print_ready_message(api_socket: &Path, client_socket: &Path) {
    eprintln!("herdr server running; you can use any herdr CLI command in another terminal.");
    eprintln!("api socket: {}", api_socket.display());
    eprintln!("client socket: {}", client_socket.display());
    eprintln!(
        "logs: {}",
        crate::session::data_dir()
            .join("herdr-server.log")
            .display()
    );
    eprintln!("did you mean to open the Herdr TUI? run `herdr`; you do not need `herdr server`.");
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

    use crate::app::AppState;
    use crate::protocol::CursorState;

    fn test_headless_server() -> HeadlessServer {
        test_headless_server_with_event_hub(api::EventHub::default())
    }

    fn test_headless_server_with_event_hub(event_hub: api::EventHub) -> HeadlessServer {
        let config = crate::config::Config::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(&config, true, None, api_rx, event_hub);
        app.state.local_sound_playback = false;
        app.local_terminal_notifications = false;
        test_headless_server_from_app(app)
    }

    /// Wraps an already-built `App` in a test `HeadlessServer` (socket +
    /// host-event bridge wired up like `new()`). Lets tests exercise a
    /// server whose app came from something other than `App::new` -- e.g.
    /// the handoff-import path's `App::new_from_handoff`.
    fn test_headless_server_from_app(app: crate::app::App) -> HeadlessServer {
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
        let listener = bind_local_listener(&socket_path).expect("bind test listener");
        let client_socket_identity =
            socket_file_identity(&socket_path).expect("test listener socket identity");
        #[cfg(unix)]
        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .expect("set listener nonblocking");
        let (server_event_tx, server_event_rx) = mpsc::channel(64);
        #[cfg(windows)]
        let should_quit = Arc::new(AtomicBool::new(false));
        #[cfg(windows)]
        spawn_windows_client_accept_thread(listener, should_quit.clone(), server_event_tx.clone());
        let server_keybindings = app_keybindings(&app);
        #[cfg(unix)]
        let host_event_tx = spawn_host_event_bridge(server_event_tx.clone());

        HeadlessServer {
            app,
            #[cfg(unix)]
            api_tx: None,
            #[cfg(unix)]
            api_server: None,
            #[cfg(unix)]
            client_listener: listener,
            client_socket_path: socket_path,
            client_socket_identity,
            clients: HashMap::new(),
            #[cfg(unix)]
            next_client_id: 1,
            foreground_client_id: None,
            server_keybindings,
            server_config_diagnostic: None,
            server_config_diagnostic_without_keybindings: None,
            terminal_attach_owners: HashMap::new(),
            next_activity_stamp: 1,
            effective_size: (MIN_COLS, MIN_ROWS),
            shutting_down: false,
            handoff_in_progress: false,
            #[cfg(unix)]
            pending_handoff_repaint_nudge: false,
            #[cfg(unix)]
            should_quit: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            should_quit,
            server_event_rx,
            server_event_tx,
            #[cfg(unix)]
            host_links: crate::server::host_link::HostLinkRegistry::default(),
            #[cfg(unix)]
            remote_panes: crate::server::remote_pane::RemotePaneRegistry::default(),
            #[cfg(unix)]
            remote_pane_terminals: HashMap::new(),
            #[cfg(unix)]
            remote_pane_remote_terminal_ids: HashMap::new(),
            #[cfg(unix)]
            focused_remote_pane: None,
            #[cfg(unix)]
            focused_remote_pane_attach: None,
            #[cfg(unix)]
            remote_pane_focus_epoch: 0,
            #[cfg(unix)]
            host_link_runtimes: HashMap::new(),
            #[cfg(unix)]
            next_host_generation: 1,
            #[cfg(unix)]
            host_event_tx,
            #[cfg(unix)]
            host_transport_override_for_test: None,
        }
    }

    fn shutdown_test_runtimes(server: &mut HeadlessServer) {
        for (_, runtime) in server.app.terminal_runtimes.drain() {
            runtime.shutdown();
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

    fn frame_text(frame: &FrameData) -> String {
        frame
            .cells
            .chunks(usize::from(frame.width))
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn read_server_shutdown_reason(bytes: Vec<u8>) -> Option<String> {
        match read_server_message(bytes) {
            ServerMessage::ServerShutdown { reason } => reason,
            other => panic!("expected shutdown, got {other:?}"),
        }
    }

    #[test]
    fn headless_api_request_drains_all_pending_internal_events_before_reading_state() {
        let mut server = test_headless_server();
        for i in 0..=crate::app::APP_EVENT_DRAIN_LIMIT {
            server
                .app
                .event_tx
                .try_send(AppEvent::UpdateReady {
                    version: format!("4.0.{i}"),
                    install_command: "herdr install".into(),
                })
                .unwrap();
        }

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "headless_stop_after_events".into(),
                    method: api::schema::Method::ServerStop(api::schema::EmptyParams::default()),
                },
                respond_to,
            })
        );
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(response["result"]["type"], "ok");
        let expected_version = format!("4.0.{}", crate::app::APP_EVENT_DRAIN_LIMIT);
        assert_eq!(
            server.app.state.update_available.as_deref(),
            Some(expected_version.as_str())
        );
        assert!(server.app.event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn headless_deferred_workspace_create_uses_runtime_events() {
        let event_hub = api::EventHub::default();
        let mut server = test_headless_server_with_event_hub(event_hub.clone());

        server.app.state.request_new_workspace = true;

        assert!(server.handle_deferred_requests_headless());
        assert!(!server.app.state.request_new_workspace);
        assert_eq!(
            event_hub
                .events_after(0)
                .into_iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![
                api::schema::EventKind::WorkspaceCreated,
                api::schema::EventKind::TabCreated,
                api::schema::EventKind::PaneCreated,
            ]
        );
        shutdown_test_runtimes(&mut server);
    }

    #[tokio::test]
    async fn headless_deferred_named_tab_create_uses_runtime_events() {
        let event_hub = api::EventHub::default();
        let mut server = test_headless_server_with_event_hub(event_hub.clone());
        server
            .app
            .create_workspace_with_options(std::env::temp_dir(), true)
            .unwrap();
        let after_setup = event_hub.current_sequence();

        server.app.state.request_new_tab = true;
        server.app.state.requested_new_tab_name = Some("ops".into());

        assert!(server.handle_deferred_requests_headless());
        assert!(!server.app.state.request_new_tab);
        assert_eq!(server.app.state.requested_new_tab_name, None);
        let events = event_hub.events_after(after_setup);
        assert_eq!(
            events
                .iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![
                api::schema::EventKind::TabCreated,
                api::schema::EventKind::PaneCreated,
            ]
        );
        let tab_created = events
            .iter()
            .find_map(|(_, event)| match &event.data {
                api::schema::EventData::TabCreated { tab } => Some(tab),
                _ => None,
            })
            .expect("tab created event");
        assert_eq!(tab_created.label, "ops");
        shutdown_test_runtimes(&mut server);
    }

    fn test_client_writer() -> (
        ClientWriter,
        std::sync::mpsc::Receiver<Vec<u8>>,
        std::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        let (render_tx, render_rx) = std::sync::mpsc::sync_channel(1);
        (
            ClientWriter::test_channel(control_tx, render_tx),
            control_rx,
            render_rx,
        )
    }

    fn retained_test_server(
        initial_screen: &[u8],
    ) -> (
        HeadlessServer,
        std::sync::mpsc::Receiver<Vec<u8>>,
        crate::layout::PaneId,
    ) {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        workspace.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, initial_screen),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        (server, client_rx, pane_id)
    }

    fn assert_frame_data_eq(actual: &FrameData, expected: &FrameData) {
        assert_eq!(
            (actual.width, actual.height),
            (expected.width, expected.height)
        );
        assert_eq!(actual.cursor, expected.cursor, "cursor mismatch");
        assert_eq!(actual.hyperlinks, expected.hyperlinks, "hyperlink mismatch");
        assert_eq!(actual.graphics, expected.graphics, "graphics mismatch");
        assert_eq!(
            actual.cells.len(),
            expected.cells.len(),
            "cell length mismatch"
        );
        for (idx, (actual_cell, expected_cell)) in
            actual.cells.iter().zip(expected.cells.iter()).enumerate()
        {
            assert_eq!(
                actual_cell,
                expected_cell,
                "cell mismatch at index {idx} (x={}, y={})",
                idx % usize::from(actual.width),
                idx / usize::from(actual.width),
            );
        }
    }

    #[test]
    fn foreground_client_applies_client_keybindings() {
        let mut server = test_headless_server();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_tab = "prefix+t"
"#,
        )
        .unwrap();
        let local_keybindings = local_config.live_keybinds().unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            writer: writer_a,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('a')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+t"));

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('b')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_tab
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+c"));
    }

    #[test]
    fn local_keybinding_client_hides_server_keybinding_warnings() {
        let mut server = test_headless_server();
        let diagnostics = vec![
            "unsafe direct keybinding: keys.close_pane = \"x\" would intercept typing".to_owned(),
            "theme warning".to_owned(),
        ];
        let (full, without_keybindings) = server_config_diagnostic_summaries(&diagnostics);
        server.server_config_diagnostic = full.clone();
        server.server_config_diagnostic_without_keybindings = without_keybindings.clone();
        server.app.state.config_diagnostic = full;
        let local_keybindings = crate::config::Config::default().live_keybinds().unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            writer: writer_a,
        }));
        assert_eq!(server.app.state.config_diagnostic, without_keybindings);

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.config_diagnostic,
            server.server_config_diagnostic
        );
    }

    #[test]
    fn local_keybinding_client_keeps_local_keybindings_after_settings_save() {
        let path = std::env::temp_dir().join(format!(
            "herdr-headless-settings-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, "onboarding = false\n").unwrap();
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut server = test_headless_server();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_workspace = "prefix+n"
next_tab = ""
"#,
        )
        .unwrap();
        let local_keybindings = local_config.live_keybinds().unwrap();
        let (writer, _control, _render) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_keybindings)),
            direct_attach_requested: false,
            writer,
        }));
        server.app.state.mode = crate::app::Mode::Settings;
        server.app.state.settings.section = crate::app::state::SettingsSection::Toast;
        server.app.state.settings.list.selected = 1;

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\r".to_vec(),
        }));

        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('a')
        );
        assert!(server
            .app
            .state
            .keybinds
            .new_workspace
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
        assert!(server.app.state.toast.is_none());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("delivery = \"herdr\""));

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_server_keybindings_apply_valid_subset_after_settings_save_without_caching_local_keybindings(
    ) {
        let path = std::env::temp_dir().join(format!(
            "herdr-headless-invalid-settings-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(
            &path,
            "onboarding = false\n[keys]\nnew_workspace = \"x\"\n[ui.toast]\ndelivery = \"off\"\n",
        )
        .unwrap();
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

        let mut server = test_headless_server();
        let previous_server_config: crate::config::Config =
            toml::from_str("[keys]\nprefix = \"ctrl+c\"\nnew_workspace = \"prefix+m\"\n").unwrap();
        server.server_keybindings = previous_server_config.live_keybinds().unwrap();
        let local_config: crate::config::Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
new_workspace = "prefix+n"
next_tab = ""
"#,
        )
        .unwrap();
        let (writer_a, _control_a, _render_a) = test_client_writer();
        let (writer_b, _control_b, _render_b) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: Some(Box::new(local_config.live_keybinds().unwrap())),
            direct_attach_requested: false,
            writer: writer_a,
        }));
        server.app.state.mode = crate::app::Mode::Settings;
        server.app.state.settings.section = crate::app::state::SettingsSection::Toast;
        server.app.state.settings.list.selected = 1;

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\r".to_vec(),
        }));

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            writer: writer_b,
        }));
        assert_eq!(
            server.app.state.prefix_code,
            crossterm::event::KeyCode::Char('b')
        );
        assert!(!server
            .app
            .state
            .keybinds
            .new_workspace
            .bindings
            .iter()
            .any(|binding| binding.label == "prefix+n"));
        assert!(server.app.state.keybinds.new_workspace.bindings.is_empty());

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn terminal_attach_rejects_missing_terminal_and_removes_client() {
        let mut server = test_headless_server();
        let (writer, control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        }));
        assert!(server.clients.contains_key(&7));

        assert!(
            !server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: "term_missing".to_owned(),
                takeover: false,
            })
        );
        assert!(!server.clients.contains_key(&7));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(
            reason,
            Some("terminal attach failed: terminal term_missing not found".to_owned())
        );
    }

    fn with_terminal_session_test_server(
        test: impl FnOnce(&mut HeadlessServer, crate::terminal::TerminalId, String, String),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
        let terminal_id_string = terminal_id.to_string();
        let public_pane_id = format!("{}:p1", workspace.id);
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );

        test(&mut server, terminal_id, terminal_id_string, public_pane_id);

        drop(server);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    fn connect_pending_terminal_client(server: &mut HeadlessServer, client_id: u64) {
        let _control_rx = connect_pending_terminal_client_with_control_rx(server, client_id);
    }

    fn connect_pending_terminal_client_with_control_rx(
        server: &mut HeadlessServer,
        client_id: u64,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (writer, control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id,
            cols: 100,
            rows: 30,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        }));
        control_rx
    }

    #[test]
    fn terminal_observe_allows_multiple_clients_without_attach_ownership() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            let initial_size = server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size();

            for client_id in [7, 8] {
                connect_pending_terminal_client(server, client_id);
                assert!(
                    server.handle_server_event(ServerEvent::ClientObserveTerminal {
                        client_id,
                        target: terminal_id_string.clone(),
                    })
                );
            }

            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
            assert_eq!(
                server
                    .app
                    .terminal_runtimes
                    .get(&terminal_id)
                    .expect("runtime")
                    .current_size(),
                initial_size
            );
            assert_eq!(
                terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
                2
            );
        });
    }

    #[test]
    fn terminal_observe_resolves_public_pane_id() {
        with_terminal_session_test_server(|server, terminal_id, _, public_pane_id| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: public_pane_id,
                })
            );

            assert!(matches!(
                server.clients.get(&7).map(|client| &client.mode),
                Some(ClientConnectionMode::TerminalObserve { terminal_id: observed })
                    if observed == &terminal_id.to_string()
            ));
        });
    }

    #[test]
    fn terminal_control_resolves_public_pane_id_and_takes_ownership() {
        with_terminal_session_test_server(
            |server, terminal_id, terminal_id_string, public_pane_id| {
                connect_pending_terminal_client(server, 7);
                assert!(
                    server.handle_server_event(ServerEvent::ClientControlTerminal {
                        client_id: 7,
                        target: public_pane_id,
                        takeover: false,
                    })
                );

                assert!(matches!(
                    server.clients.get(&7).map(|client| &client.mode),
                    Some(ClientConnectionMode::TerminalAttach { terminal_id: attached })
                        if attached == &terminal_id_string
                ));
                assert_eq!(
                    server.terminal_attach_owners.get(&terminal_id_string),
                    Some(&7)
                );
                assert!(server
                    .app
                    .state
                    .direct_attach_resize_locks
                    .contains(&terminal_id));
            },
        );
    }

    #[test]
    fn terminal_control_rejects_second_controller_without_takeover() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                !server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                    takeover: false,
                })
            );

            assert!(server.clients.contains_key(&7));
            assert!(!server.clients.contains_key(&8));
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
        });
    }

    #[test]
    fn terminal_control_takeover_replaces_existing_controller() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                    takeover: true,
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.clients.contains_key(&8));
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&8)
            );
        });
    }

    #[test]
    fn terminal_observe_can_coexist_with_terminal_control() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    takeover: false,
                })
            );

            connect_pending_terminal_client(server, 8);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 8,
                    target: terminal_id_string.clone(),
                })
            );

            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
            assert!(matches!(
                server.clients.get(&8).map(|client| &client.mode),
                Some(ClientConnectionMode::TerminalObserve { terminal_id })
                    if terminal_id == &terminal_id_string
            ));
            assert_eq!(
                terminal_stream_client_ids(&server.clients, &terminal_id_string).len(),
                2
            );
        });
    }

    #[test]
    fn terminal_control_detach_sends_shutdown_before_removal() {
        with_terminal_session_test_server(|server, _terminal_id, terminal_id_string, _| {
            let control_rx = connect_pending_terminal_client_with_control_rx(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientControlTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                    takeover: false,
                })
            );

            assert!(server.handle_server_event(ServerEvent::ClientDetach { client_id: 7 }));

            assert!(!server.clients.contains_key(&7));
            assert!(!server
                .terminal_attach_owners
                .contains_key(&terminal_id_string));
            let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
            assert_eq!(reason, Some("detached".to_owned()));
        });
    }

    #[test]
    fn terminal_observe_rejects_later_attach_upgrade() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                })
            );
            assert!(
                !server.handle_server_event(ServerEvent::ClientAttachTerminal {
                    client_id: 7,
                    terminal_id: terminal_id_string,
                    takeover: true,
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
        });
    }

    #[test]
    fn terminal_attach_rejects_later_observe_and_clears_ownership() {
        with_terminal_session_test_server(|server, terminal_id, terminal_id_string, _| {
            connect_pending_terminal_client(server, 7);
            assert!(
                server.handle_server_event(ServerEvent::ClientAttachTerminal {
                    client_id: 7,
                    terminal_id: terminal_id_string.clone(),
                    takeover: false,
                })
            );
            assert_eq!(
                server.terminal_attach_owners.get(&terminal_id_string),
                Some(&7)
            );
            assert!(server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));

            assert!(
                !server.handle_server_event(ServerEvent::ClientObserveTerminal {
                    client_id: 7,
                    target: terminal_id_string.clone(),
                })
            );

            assert!(!server.clients.contains_key(&7));
            assert!(server.terminal_attach_owners.is_empty());
            assert!(!server
                .app
                .state
                .direct_attach_resize_locks
                .contains(&terminal_id));
        });
    }

    fn app_client_marks_git_refresh_due_on_first_attach(render_encoding: RenderEncoding) {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let future = Instant::now() + Duration::from_secs(60);
        server.app.last_git_remote_status_refresh = future;
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding,
            keybindings: None,
            direct_attach_requested: false,
            writer,
        }));

        assert!(server.has_app_client());
        assert!(server
            .app
            .git_refresh_deadline()
            .is_some_and(|deadline| deadline <= Instant::now()));
    }

    #[test]
    fn terminal_ansi_app_client_enables_headless_git_refresh() {
        app_client_marks_git_refresh_due_on_first_attach(RenderEncoding::TerminalAnsi);
    }

    #[test]
    fn pending_terminal_attach_client_does_not_enable_headless_git_refresh() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        }));

        assert!(!server.has_app_client());
        assert_eq!(
            server.app.next_headless_loop_deadline_with_git_refresh(
                Instant::now(),
                false,
                server.has_app_client()
            ),
            None
        );
    }

    #[test]
    fn writerless_app_client_does_not_enable_headless_git_refresh() {
        let mut server = test_headless_server();
        server
            .app
            .state
            .workspaces
            .push(crate::workspace::Workspace::test_new("test"));
        let (writer, _control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::SemanticFrame,
            keybindings: None,
            direct_attach_requested: false,
            writer,
        }));
        assert!(server.has_app_client());

        server.clients.get_mut(&7).expect("client").writer = None;

        assert!(!server.has_app_client());
        assert_eq!(
            server.app.next_headless_loop_deadline_with_git_refresh(
                Instant::now(),
                false,
                server.has_app_client()
            ),
            None
        );
    }

    #[test]
    fn semantic_app_client_marks_git_refresh_due_on_first_attach() {
        app_client_marks_git_refresh_due_on_first_attach(RenderEncoding::SemanticFrame);
    }

    #[test]
    fn terminal_attach_client_exits_when_attached_pane_dies() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("attached");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .to_string();
        let (writer, control_rx, _render_rx) = test_client_writer();

        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 7,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 7,
                terminal_id: terminal_id.clone(),
                takeover: false,
            })
        );
        assert_eq!(server.terminal_attach_owners.get(&terminal_id), Some(&7));

        assert!(server.handle_internal_event_with_forwarding(AppEvent::PaneDied { pane_id }));

        assert!(!server.clients.contains_key(&7));
        assert!(!server.terminal_attach_owners.contains_key(&terminal_id));
        let reason = read_server_shutdown_reason(control_rx.recv().expect("shutdown message"));
        assert_eq!(reason, Some(format!("terminal {terminal_id} exited")));
    }

    #[test]
    fn terminal_attach_scroll_moves_attached_runtime_viewport() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::Wheel,
            AttachScrollDirection::Up,
            3,
            None,
            None,
            0,
        )
        .expect("scroll up");
        let metrics = runtime.scroll_metrics().expect("scroll metrics");
        assert_eq!(metrics.offset_from_bottom, 3);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::Wheel,
            AttachScrollDirection::Down,
            2,
            None,
            None,
            0,
        )
        .expect("scroll down");
        let metrics = runtime.scroll_metrics().expect("scroll metrics");
        assert_eq!(metrics.offset_from_bottom, 1);
        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn terminal_attach_input_resets_scrolled_viewport() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );

        runtime.scroll_up(4);
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            4
        );

        apply_terminal_attach_input(&runtime, b"x".to_vec()).expect("attach input");
        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded input"),
            Bytes::from("x")
        );

        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn terminal_attach_page_key_host_scrolls_plain_terminal() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::PageKey {
                input: b"\x1b[5~".to_vec(),
            },
            AttachScrollDirection::Up,
            4,
            None,
            None,
            0,
        )
        .expect("page key scroll");

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            4
        );
        assert!(input_rx.try_recv().is_err());
        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn terminal_attach_page_key_forwards_when_mouse_reporting() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = b"\x1b[?1000h".to_vec();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );
        runtime.scroll_up(3);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::PageKey {
                input: b"\x1b[5~".to_vec(),
            },
            AttachScrollDirection::Up,
            4,
            None,
            None,
            0,
        )
        .expect("page key forward");

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded page key"),
            Bytes::from_static(b"\x1b[5~")
        );
        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn terminal_attach_page_key_forwards_in_alternate_screen_without_mouse_reporting() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut bytes = b"\x1b[?1049h".to_vec();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                20, 5, 4096, &bytes, 4,
            );
        runtime.scroll_up(3);

        apply_terminal_attach_scroll(
            &runtime,
            AttachScrollSource::PageKey {
                input: b"\x1b[5~".to_vec(),
            },
            AttachScrollDirection::Up,
            4,
            None,
            None,
            0,
        )
        .expect("page key forward");

        assert_eq!(
            runtime
                .scroll_metrics()
                .expect("scroll metrics")
                .offset_from_bottom,
            0
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded page key"),
            Bytes::from_static(b"\x1b[5~")
        );
        drop(runtime);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn headless_scheduled_tasks_expire_agent_metadata() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("metadata");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::HookStateReported {
                pane_id,
                source: "herdr:pi".into(),
                agent_label: "pi".into(),
                state: crate::detect::AgentState::Working,
                message: None,
                custom_status: None,
                seq: None,
                session_ref: None,
            })
        );
        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::HookMetadataReported {
                pane_id,
                source: "user:pi-display".into(),
                agent_label: Some("pi".into()),
                applies_to_source: Some("herdr:pi".into()),
                title: None,
                display_agent: None,
                custom_status: Some("short lived".into()),
                state_labels: HashMap::new(),
                clear_title: false,
                clear_display_agent: false,
                clear_custom_status: false,
                clear_state_labels: false,
                seq: None,
                ttl: Some(Duration::from_millis(1)),
            })
        );

        let deadline = server
            .app
            .agent_metadata_deadline
            .expect("metadata deadline");
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane")
            .attached_terminal_id
            .clone();
        assert_eq!(
            server
                .app
                .state
                .terminals
                .get(&terminal_id)
                .expect("terminal")
                .effective_custom_status()
                .as_deref(),
            Some("short lived")
        );

        assert!(server.handle_scheduled_tasks_headless(deadline + Duration::from_millis(1), false));

        assert_eq!(server.app.agent_metadata_deadline, None);
        assert_eq!(
            server
                .app
                .state
                .terminals
                .get(&terminal_id)
                .expect("terminal")
                .effective_custom_status(),
            None
        );
        assert!(server
            .app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| {
                event.event == crate::api::schema::EventKind::PaneAgentStatusChanged
                    && matches!(
                        &event.data,
                        crate::api::schema::EventData::PaneAgentStatusChanged {
                            custom_status,
                            ..
                        } if custom_status.is_none()
                    )
            }));
    }

    #[test]
    fn headless_scheduled_tasks_clears_disabled_agent_manifest_update_deadline() {
        let mut server = test_headless_server();
        let now = Instant::now();
        server.app.next_agent_manifest_update_check = Some(now - Duration::from_millis(1));

        assert!(!server.handle_scheduled_tasks_headless(now, false));
        assert_eq!(server.app.next_agent_manifest_update_check, None);
    }

    #[tokio::test]
    async fn headless_scheduled_tasks_do_not_start_pending_agent_resume_when_geometry_dirty() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                server.app.state.host_terminal_theme,
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.effective_size = (100, 30);
        server.app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
        };
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        server.app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!server.handle_scheduled_tasks_headless(Instant::now(), true));
        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(server.app.pending_agent_resume_deadline.is_none());
    }

    #[tokio::test]
    async fn headless_scheduled_tasks_do_not_start_pending_agent_resume_without_foreground_client()
    {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.foreground_client_id = None;
        server.effective_size = (80, 24);
        server.app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
        };
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        server.app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        assert!(!server.handle_scheduled_tasks_headless(Instant::now(), false));
        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(server.app.pending_agent_resume_deadline.is_none());
    }

    #[tokio::test]
    async fn headless_pre_input_resize_does_not_start_pending_agent_resume() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        server.app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.ensure_test_terminals();
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                server.app.state.host_terminal_theme,
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.effective_size = (100, 30);
        server.app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
        };
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });
        server.app.pending_agent_resume_deadline = Some(Instant::now() - Duration::from_millis(1));

        server.resize_shared_runtime_to_effective_size_before_input();

        assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());
        assert!(server
            .app
            .state
            .terminals
            .get(&terminal_id)
            .expect("test terminal should still exist")
            .pending_agent_resume_plan
            .is_some());
        assert!(server.app.pending_agent_resume_deadline.is_none());
    }

    #[test]
    fn virtual_render_produces_nonempty_buffer() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (buffer, _cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        assert_eq!(buffer.area.width, 80);
        assert_eq!(buffer.area.height, 24);
    }

    #[test]
    fn virtual_render_without_frame_cursor_keeps_cursor_hidden() {
        let mut state = AppState::test_new();
        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert_eq!(cursor, None);
    }

    #[tokio::test]
    async fn virtual_render_preserves_explicit_frame_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
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
                shape: cursor.as_ref().map(|c| c.shape).unwrap_or(0),
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_preserves_hidden_focused_pane_cursor_position() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
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
                shape: cursor.as_ref().map(|c| c.shape).unwrap_or(0),
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_during_synchronized_output() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left");
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.test_process_pty_bytes(b"\x1b[?2026h\x1b[2;3H");
        assert!(runtime.synchronized_output_active());

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, false);

        assert_eq!(
            cursor, None,
            "child cursor positions are unstable while synchronized output is active"
        );
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_during_synchronized_output_resize() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left");
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let initial_area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, initial_area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.test_process_pty_bytes(b"\x1b[?2026h\x1b[2;3H");
        assert!(runtime.synchronized_output_active());

        let resized_area = Rect::new(0, 0, 100, 30);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, resized_area, true);

        assert_eq!(
            cursor, None,
            "pre-resize synchronized output should suppress the cursor even if resize clears the mode"
        );
    }

    #[tokio::test]
    async fn virtual_render_exposes_hidden_pane_cursor_when_reveal_hidden_for_cjk_ime() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
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
                shape: state.cjk_ime_cursor_shape,
            })
        );
    }

    #[tokio::test]
    async fn virtual_render_keeps_cursor_hidden_when_scrolled_back_even_with_reveal_hidden_for_cjk_ime(
    ) {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.scroll_up(6);
        assert!(crate::ui::pane_is_scrolled_back(runtime));

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "scrolled-back focused pane should keep the cursor hidden even when reveal_hidden_cursor_for_cjk_ime is true; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_fallback_cursor_when_viewport_none_and_reveal_hidden_for_cjk_ime() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        // Feed only ?25l with no prior cursor movement — exercises the fallback
        // path for TUIs whose viewport has no cursor position.
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);
        let pane = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("focused pane info");

        assert_eq!(
            cursor,
            Some(CursorState {
                x: pane.inner_rect.x,
                y: pane.inner_rect.y,
                visible: true,
                shape: state.cjk_ime_cursor_shape,
            }),
            "fallback should anchor at pane top-left with the configured shape",
        );
    }

    #[tokio::test]
    async fn virtual_render_skips_reveal_when_focused_pane_has_no_detected_agent() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        // Filter only Claude, but the test pane has no detected agent, so the
        // reveal must not apply.
        state.cjk_ime_agent_filter_configured = true;
        state.cjk_ime_agents = vec![crate::detect::Agent::Claude];
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "agent filter should suppress reveal when the focused pane's detected agent is not on the list; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_skips_reveal_when_agent_filter_has_no_valid_entries() {
        let mut state = AppState::test_new();
        state.reveal_hidden_cursor_for_cjk_ime = true;
        state.cjk_ime_agent_filter_configured = true;
        state.cjk_ime_agents = Vec::new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left\x1b[?25l"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "agent filter with no valid entries should suppress reveal; got {cursor:?}",
        );
    }

    #[tokio::test]
    async fn virtual_render_omits_focused_pane_cursor_while_mobile_switcher_open() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        ws.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"left"),
        );

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Navigate;

        let area = Rect::new(0, 0, 44, 24);
        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert_eq!(cursor, None);
    }

    #[tokio::test]
    async fn virtual_render_hides_focused_pane_cursor_while_scrolled_back() {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let mut bytes = Vec::new();
        for line in 0..80 {
            bytes.extend_from_slice(format!("line {line:02}\r\n").as_bytes());
        }
        let runtime =
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 4096, &bytes);
        ws.insert_test_runtime(pane_id, runtime);

        state.workspaces = vec![ws];
        state.active = Some(0);
        state.selected = 0;
        state.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 80, 24);
        let _ = crate::server::render_stream::render_virtual(&mut state, area, true);
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let runtime = state
            .runtime_for_pane(&terminal_runtimes, pane_id)
            .expect("pane runtime after initial render");
        runtime.scroll_up(6);
        assert!(crate::ui::pane_is_scrolled_back(runtime));

        let (_buffer, cursor) =
            crate::server::render_stream::render_virtual(&mut state, area, true);

        assert!(
            cursor.as_ref().is_none_or(|cursor| !cursor.visible),
            "cursor: {cursor:?}"
        );
    }

    #[test]
    fn latest_active_client_drives_shared_size_theme_and_fallback() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (160, 45),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
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
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
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
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
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
    fn foreground_client_without_host_theme_clears_previous_host_theme() {
        let mut server = test_headless_server();
        let known_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x10,
                g: 0x20,
                b: 0x30,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x40,
                g: 0x50,
                b: 0x60,
            }),
        };
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                known_theme,
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.app.state.host_terminal_theme, known_theme);

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(
            server.app.state.host_terminal_theme,
            crate::terminal_theme::TerminalTheme::default()
        );
    }

    #[test]
    fn foreground_client_appearance_controls_auto_theme() {
        let mut server = test_headless_server();
        server.app.state.theme_runtime.auto_switch = true;
        server.app.state.theme_runtime.dark_name = "catppuccin".to_string();
        server.app.state.theme_runtime.light_name = "catppuccin-latte".to_string();
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: None,
                    background: Some(crate::terminal_theme::RgbColor { r: 0, g: 0, b: 0 }),
                },
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme {
                    foreground: None,
                    background: Some(crate::terminal_theme::RgbColor {
                        r: 255,
                        g: 255,
                        b: 255,
                    }),
                },
                None,
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        assert!(server.promote_client_to_foreground(1));
        assert_eq!(server.app.state.theme_name, "catppuccin");

        assert!(server.promote_client_to_foreground(2));
        assert_eq!(server.app.state.theme_name, "catppuccin-latte");
    }

    #[test]
    fn color_scheme_change_event_is_inert_on_server() {
        let mut server = test_headless_server();
        let initial_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x10,
                g: 0x20,
                b: 0x30,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x40,
                g: 0x50,
                b: 0x60,
            }),
        };
        server.app.state.host_terminal_theme = initial_theme;
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                initial_theme,
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );

        let changed = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: crate::raw_input::GHOSTTY_COLOR_SCHEME_DARK_REPORT.to_vec(),
        });

        assert!(!changed);
        assert_eq!(server.foreground_client_id, None);
        assert_eq!(server.clients[&1].host_terminal_theme, initial_theme);
        assert_eq!(server.app.state.host_terminal_theme, initial_theme);
    }

    #[test]
    fn focus_lost_updates_client_without_promoting_foreground() {
        let mut server = test_headless_server();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
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
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                2,
                RenderEncoding::SemanticFrame,
                None,
            ),
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
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
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
    fn app_client_lone_escape_closes_navigate_mode() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Navigate;
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b".to_vec(),
        }));

        assert_eq!(server.app.state.mode, crate::app::Mode::Terminal);
    }

    #[test]
    fn semantic_client_input_events_route_through_app_input() {
        let mut server = test_headless_server();
        server.app.state.mode = crate::app::Mode::Onboarding;
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Enter,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }],
        }));

        assert_eq!(server.app.state.mode, crate::app::Mode::Settings);
        assert_eq!(
            server.app.state.settings.section,
            crate::app::state::SettingsSection::Integrations
        );
    }

    #[test]
    fn semantic_client_escape_closes_keybind_help() {
        let mut server = test_headless_server();
        server.app.state.mode = crate::app::Mode::KeybindHelp;
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Esc,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }],
        }));

        assert_eq!(server.app.state.mode, crate::app::Mode::Navigate);
    }

    #[test]
    fn semantic_client_down_scrolls_keybind_help() {
        let mut server = test_headless_server();
        server.app.state.mode = crate::app::Mode::KeybindHelp;
        server.clients.insert(
            1,
            ClientConnection::new(
                (100, 30),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        assert!(server.app.state.keybind_help_max_scroll() > 0);
        assert!(server.handle_server_event(ServerEvent::ClientInputEvents {
            client_id: 1,
            events: vec![crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Down,
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            }],
        }));

        assert_eq!(server.app.state.mode, crate::app::Mode::KeybindHelp);
        assert_eq!(server.app.state.keybind_help.scroll, 1);
    }

    #[tokio::test]
    async fn split_default_background_response_updates_theme_without_forwarding_tail() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let focused = workspace.focused_pane_id().unwrap();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 1);
        workspace.tabs[0].runtimes.insert(focused, runtime);
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(true),
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let _ = server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b]".to_vec(),
        });
        assert!(rx.try_recv().is_err());

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"11;#123456\x07".to_vec(),
        }));

        assert!(rx.try_recv().is_err());
        assert_eq!(
            server.clients[&1].host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
        assert_eq!(
            server.app.state.host_terminal_theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            })
        );
    }

    #[test]
    fn render_and_stream_uses_each_client_terminal_size() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (desktop_tx, _desktop_control_rx, desktop_rx) = test_client_writer();
        let (phone_tx, _phone_control_rx, phone_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(desktop_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(phone_tx),
            ),
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

    #[tokio::test]
    async fn resize_shared_runtime_resizes_background_tabs() {
        let mut server = test_headless_server();
        let mut workspace = crate::workspace::Workspace::test_new("test");
        let background_tab = workspace.test_add_tab(Some("background"));
        let active_pane = workspace.tabs[0].root_pane;
        let background_pane = workspace.tabs[background_tab].root_pane;
        workspace.tabs[0].runtimes.insert(
            active_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        workspace.tabs[background_tab].runtimes.insert(
            background_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();

        let terminal_area = server.app.state.view.terminal_area;
        let expected = (terminal_area.height, terminal_area.width.saturating_sub(1));
        assert_eq!(
            server
                .app
                .state
                .runtime_for_pane(&server.app.terminal_runtimes, active_pane)
                .unwrap()
                .current_size(),
            expected
        );
        assert_eq!(
            server
                .app
                .state
                .runtime_for_pane(&server.app.terminal_runtimes, background_pane)
                .unwrap()
                .current_size(),
            expected
        );
    }

    #[test]
    fn terminal_attach_disconnect_restores_app_pane_size() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let _runtime_guard = rt.enter();
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("test");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal id").clone();
        let terminal_id_string = terminal_id.to_string();
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.app.terminal_runtimes.insert(
            terminal_id.clone(),
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                None,
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();
        server.resize_shared_runtime_to_effective_size();
        let expected_app_size = server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .current_size();
        assert_ne!(expected_app_size, (24, 80));

        let (writer, _control_rx, _render_rx) = test_client_writer();
        assert!(server.handle_server_event(ServerEvent::ClientConnected {
            client_id: 2,
            cols: 80,
            rows: 24,
            cell_width_px: 0,
            cell_height_px: 0,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings: None,
            direct_attach_requested: true,
            writer,
        }));
        assert!(
            server.handle_server_event(ServerEvent::ClientAttachTerminal {
                client_id: 2,
                terminal_id: terminal_id_string.clone(),
                takeover: false,
            })
        );
        assert_eq!(server.foreground_client_id, Some(1));
        assert!(server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            (24, 80)
        );

        assert!(server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 2 }));

        assert!(!server
            .app
            .state
            .direct_attach_resize_locks
            .contains(&terminal_id));
        assert_eq!(
            server
                .app
                .terminal_runtimes
                .get(&terminal_id)
                .expect("runtime")
                .current_size(),
            expected_app_size
        );
        drop(server);
        drop(_runtime_guard);
        rt.shutdown_timeout(Duration::from_millis(100));
    }

    #[test]
    fn render_and_stream_sends_terminal_frame_for_terminal_ansi_client() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();

        match read_server_message(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("terminal frame"),
        ) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 1);
                assert_eq!((frame.width, frame.height), (80, 24));
                assert!(frame.full);
                assert!(!frame.bytes.is_empty());
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
    }

    #[test]
    fn terminal_ansi_input_does_not_reset_blit_baseline() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );

        assert!(!server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: Vec::new(),
        }));
        server.render_and_stream();

        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn outer_focus_gained_forces_terminal_ansi_full_redraw() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        server.render_and_stream();

        match read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()) {
            ServerMessage::Terminal(frame) => {
                assert_eq!(frame.seq, 2);
                assert!(frame.full);
            }
            other => panic!("expected terminal frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn outer_focus_gained_client_render_pending_survives_semantic_render_queue_full() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial semantic frame");

        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        server
            .clients
            .get(&1)
            .unwrap()
            .writer
            .as_ref()
            .unwrap()
            .render
            .try_send(queued)
            .expect("pre-fill render queue");

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));
        assert!(server.clients.get(&1).unwrap().render_pending);

        server.render_and_stream();

        assert!(server.clients.get(&1).unwrap().render_pending);
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        assert!(server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 1 }));
        server.render_and_stream();

        assert!(!server.clients.get(&1).unwrap().render_pending);
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::Frame(_)
        ));
    }

    #[test]
    fn outer_focus_gained_does_not_force_terminal_ansi_full_redraw_when_disabled() {
        let mut server = test_headless_server();
        server.app.state.redraw_on_focus_gained = false;
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial terminal frame");

        server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        });
        server.render_and_stream();

        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
    }

    #[test]
    fn outer_focus_gained_does_not_mark_semantic_render_pending_when_disabled() {
        let mut server = test_headless_server();
        server.app.state.redraw_on_focus_gained = false;
        let (client_tx, _client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        assert!(server.handle_server_event(ServerEvent::ClientInput {
            client_id: 1,
            data: b"\x1b[I".to_vec(),
        }));

        assert!(!server.clients.get(&1).unwrap().render_pending);
        assert!(!server.app.full_redraw_pending);
        assert_eq!(server.clients[&1].outer_terminal_focus, Some(true));
        assert_eq!(server.app.state.outer_terminal_focus, Some(true));
    }

    #[test]
    fn full_render_queue_does_not_advance_terminal_ansi_baseline() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        client_tx
            .render
            .try_send(queued)
            .expect("pre-fill render queue");

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();

        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            0
        );
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn writer_drained_retries_pending_terminal_ansi_render() {
        let mut server = test_headless_server();
        let (client_tx, _client_control_rx, client_rx) = test_client_writer();
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        client_tx
            .render
            .try_send(queued)
            .expect("pre-fill render queue");

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::TerminalAnsi,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        server.render_and_stream();
        assert!(server.clients.get(&1).unwrap().render_pending);
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        assert!(server.handle_server_event(ServerEvent::ClientWriterDrained { client_id: 1 }));
        server.render_and_stream();

        match read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()) {
            ServerMessage::Terminal(frame) => assert_eq!(frame.seq, 1),
            other => panic!("expected terminal frame, got {other:?}"),
        }
        assert_eq!(
            server
                .clients
                .get(&1)
                .unwrap()
                .render_state
                .terminal_seq()
                .unwrap(),
            1
        );
        assert!(!server.clients.get(&1).unwrap().render_pending);
    }

    #[test]
    fn render_and_stream_skips_identical_frame_sends() {
        let mut server = test_headless_server();
        server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("test")];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (client_tx, _client_control_rx, client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
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

    #[tokio::test]
    async fn retained_pty_update_streams_dirty_row_from_last_frame() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.render_and_stream();
        let first = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        assert!(first.cells.iter().any(|cell| cell.symbol == "a"));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
        let patched = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame"),
        );
        assert!(patched.cells.iter().any(|cell| cell.symbol == "Z"));
        assert_eq!((patched.width, patched.height), (80, 24));
    }

    #[tokio::test]
    async fn retained_pty_update_declines_while_toast_is_visible() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".to_owned(),
            context: "background · 2".to_owned(),
            position: None,
            target: None,
        });
        server.render_and_stream();
        let initial = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        assert!(
            frame_text(&initial).contains("pi needs attention"),
            "expected initial full frame to include toast text"
        );

        let toast_row = server.app.state.view.toast_hit_area.y;
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let pane_row = toast_row
            .checked_sub(inner_rect.y)
            .expect("toast should overlap the pane")
            + 1;
        assert!(pane_row <= inner_rect.height);
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(format!("\x1b[{pane_row};1Hzzzz").as_bytes());

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "retained path should not stream a frame that can overwrite toast cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_declines_while_copy_feedback_is_visible() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: "copied to clipboard".to_owned(),
        });
        server.render_and_stream();
        let initial = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("initial frame"),
        );
        let initial_text = frame_text(&initial);
        assert!(
            initial_text.contains("copied to clipboard"),
            "expected initial full frame to include copy feedback"
        );

        let feedback_row = initial_text
            .lines()
            .position(|line| line.contains("copied to clipboard"))
            .expect("copy feedback row") as u16;
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let pane_row = feedback_row
            .checked_sub(inner_rect.y)
            .expect("copy feedback should overlap the pane")
            + 1;
        assert!(pane_row <= inner_rect.height);
        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(format!("\x1b[{pane_row};1Hzzzz").as_bytes());

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(
            client_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "retained path should not stream a frame that can overwrite copy feedback cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_matches_full_render_frame() {
        let initial = b"\x1b[6 qleft \xe4\xb8\xad";
        let update = b"\r\x1b[44mZ\x1b[0m";
        let (mut retained_server, retained_rx, retained_pane_id) = retained_test_server(initial);
        let (mut full_server, full_rx, full_pane_id) = retained_test_server(initial);

        retained_server.render_and_stream();
        let _ = retained_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial retained baseline");
        full_server.render_and_stream();
        let _ = full_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial full baseline");

        retained_server
            .app
            .state
            .runtime_for_pane_in_workspace(
                &retained_server.app.terminal_runtimes,
                0,
                retained_pane_id,
            )
            .expect("retained runtime")
            .test_process_pty_bytes(update);
        full_server
            .app
            .state
            .runtime_for_pane_in_workspace(&full_server.app.terminal_runtimes, 0, full_pane_id)
            .expect("full runtime")
            .test_process_pty_bytes(update);

        assert!(retained_server.render_retained_pty_update_and_stream());
        full_server.render_and_stream();

        let retained_frame = read_server_frame(
            retained_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame"),
        );
        let full_frame = read_server_frame(
            full_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full frame"),
        );
        assert_frame_data_eq(&retained_frame, &full_frame);
    }

    #[tokio::test]
    async fn retained_pty_update_streams_cursor_only_change() {
        let initial = b"abcd";
        let update = b"\x1b[D";
        let (mut retained_server, retained_rx, retained_pane_id) = retained_test_server(initial);
        let (mut full_server, full_rx, full_pane_id) = retained_test_server(initial);

        retained_server.render_and_stream();
        let _ = retained_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial retained baseline");
        full_server.render_and_stream();
        let _ = full_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial full baseline");

        retained_server
            .app
            .state
            .runtime_for_pane_in_workspace(
                &retained_server.app.terminal_runtimes,
                0,
                retained_pane_id,
            )
            .expect("retained runtime")
            .test_process_pty_bytes(update);
        full_server
            .app
            .state
            .runtime_for_pane_in_workspace(&full_server.app.terminal_runtimes, 0, full_pane_id)
            .expect("full runtime")
            .test_process_pty_bytes(update);

        assert!(retained_server.render_retained_pty_update_and_stream());
        full_server.render_and_stream();

        let retained_frame = read_server_frame(
            retained_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained cursor frame"),
        );
        let full_frame = read_server_frame(
            full_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full cursor frame"),
        );
        assert_frame_data_eq(&retained_frame, &full_frame);
    }

    #[tokio::test]
    async fn retained_pty_update_declines_unsafe_mode_without_consuming_dirty_rows() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        server.app.state.mode = crate::app::Mode::Navigate;
        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        server.app.state.mode = crate::app::Mode::Terminal;
        assert!(server.render_retained_pty_update_and_stream());
        let patched = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame after safe mode"),
        );
        assert!(patched.cells.iter().any(|cell| cell.symbol == "Z"));
    }

    #[tokio::test]
    async fn headless_full_render_clears_full_redraw_pending_for_future_retained_updates() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.full_redraw_pending = true;

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("full redraw frame");
        assert!(!server.app.full_redraw_pending);

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
    }

    #[tokio::test]
    async fn retained_pty_update_declines_when_patch_would_stale_hyperlinks() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"link");
        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");
        let inner_rect = server.app.state.view.pane_infos[0].inner_rect;
        let client = server.clients.get_mut(&1).unwrap();
        let mut frame = client.render_state.last_frame().unwrap().clone();
        frame.hyperlinks = vec!["https://example.com".to_owned()];
        let hyperlink_idx =
            usize::from(inner_rect.y) * usize::from(frame.width) + usize::from(inner_rect.x);
        frame.cells[hyperlink_idx].hyperlink = Some(0);
        let prepared = client
            .render_state
            .prepare_frame(frame)
            .expect("hyperlink frame differs");
        client.render_state.commit_sent_frame(prepared);

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rplain");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());

        server.render_and_stream();
        let full = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("full frame after hyperlink overwrite"),
        );
        assert!(
            full.cells.iter().all(|cell| cell.hyperlink.is_none()),
            "full render should clear overwritten hyperlink cells"
        );
    }

    #[tokio::test]
    async fn retained_pty_update_allows_kitty_enabled_empty_graphics_cache() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.kitty_graphics_enabled = true;
        server.clients.get_mut(&1).unwrap().cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(server.render_retained_pty_update_and_stream());
        let retained = read_server_frame(
            client_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("retained frame with kitty enabled"),
        );
        assert!(retained.cells.iter().any(|cell| cell.symbol == "Z"));
    }

    #[tokio::test]
    async fn retained_pty_update_declines_when_graphics_cache_has_content() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        server.app.state.kitty_graphics_enabled = true;
        let client = server.clients.get_mut(&1).unwrap();
        client.cell_size = crate::kitty_graphics::HostCellSize {
            width_px: 10,
            height_px: 20,
        };

        server.render_and_stream();
        let _ = client_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("initial frame");
        server
            .clients
            .get_mut(&1)
            .unwrap()
            .graphics_cache
            .test_mark_non_empty();

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[tokio::test]
    async fn full_redraw_pending_survives_full_render_queue_full() {
        let (mut server, client_rx, pane_id) = retained_test_server(b"aaaa");
        let queued = HeadlessServer::frame_server_message(&ServerMessage::ReloadSoundConfig)
            .expect("serialize dummy message");
        server
            .clients
            .get(&1)
            .unwrap()
            .writer
            .as_ref()
            .unwrap()
            .render
            .try_send(queued)
            .expect("pre-fill render queue");
        server.app.full_redraw_pending = true;

        server.render_and_stream();

        assert!(server.app.full_redraw_pending);
        assert!(server.clients.get(&1).unwrap().render_pending);
        assert!(matches!(
            read_server_message(client_rx.recv_timeout(Duration::from_millis(100)).unwrap()),
            ServerMessage::ReloadSoundConfig
        ));

        let runtime = server
            .app
            .state
            .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.test_process_pty_bytes(b"\rZ");

        assert!(!server.render_retained_pty_update_and_stream());
        assert!(client_rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn client_config_reload_request_refreshes_attached_clients() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.app.state.request_client_config_reload = true;

        server.drain_client_config_reload_request();

        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("client config reload message"),
        ) {
            ServerMessage::ReloadSoundConfig => {}
            other => panic!("expected ReloadSoundConfig, got {other:?}"),
        }
        assert!(!server.app.state.request_client_config_reload);
    }

    #[test]
    fn clipboard_write_targets_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert_eq!(
            server
                .app
                .state
                .copy_feedback
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("copied to clipboard")
        );
        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground clipboard message"),
        ) {
            ServerMessage::Clipboard { data } => assert_eq!(data, "dGVzdA=="),
            other => panic!("expected clipboard message, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive clipboard writes"
        );
    }

    #[test]
    fn clipboard_write_without_foreground_client_does_not_show_feedback() {
        let mut server = test_headless_server();
        server.foreground_client_id = None;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert!(
            server.app.state.copy_feedback.is_none(),
            "clipboard feedback should only show when a foreground client can receive the write"
        );
    }

    #[test]
    fn clipboard_write_failed_foreground_send_does_not_show_feedback() {
        let mut server = test_headless_server();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();
        drop(foreground_control_rx);

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(1);

        let changed = server.handle_internal_event_with_forwarding(AppEvent::ClipboardWrite {
            content: b"test".to_vec(),
        });

        assert!(changed);
        assert!(
            server.app.state.copy_feedback.is_none(),
            "clipboard feedback should only show after the foreground client receives the write"
        );
        assert!(
            !server.clients.contains_key(&1),
            "failed targeted send should remove the broken foreground client"
        );
    }

    #[test]
    fn client_local_notifications_target_foreground_client_only() {
        let mut server = test_headless_server();
        let (background_tx, background_control_rx, _background_rx) = test_client_writer();
        let (foreground_tx, foreground_control_rx, _foreground_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (120, 40),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(background_tx),
            ),
        );
        server.clients.insert(
            2,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                2,
                RenderEncoding::SemanticFrame,
                Some(foreground_tx),
            ),
        );
        server.foreground_client_id = Some(2);
        server.sync_foreground_client_state();

        assert!(server.send_to_foreground_client(ServerMessage::Notify {
            kind: protocol::NotifyKind::Toast,
            message: "pi finished".to_string(),
            body: Some("workspace 1".to_string()),
        }));

        match read_server_message(
            foreground_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("foreground toast message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Toast);
                assert_eq!(message, "pi finished");
                assert_eq!(body.as_deref(), Some("workspace 1"));
            }
            other => panic!("expected toast notify, got {other:?}"),
        }
        assert!(
            background_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "background client should not receive client-local notifications"
        );
    }

    #[test]
    fn herdr_toast_delivery_keeps_toast_in_frame_without_client_notify() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
            version: "9.9.9".to_string(),
            install_command: "herdr update".into(),
        });

        assert!(changed);
        assert!(server.app.state.toast.is_some());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "herdr delivery should render in-frame instead of forwarding a client-local notification"
        );
    }

    #[test]
    fn system_toast_delivery_forwards_system_notify_kind() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let changed = server.handle_internal_event_with_forwarding(AppEvent::UpdateReady {
            version: "9.9.9".to_string(),
            install_command: "herdr update".into(),
        });

        assert!(changed);
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("system toast message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "v9.9.9 available");
                assert_eq!(
                    body.as_deref(),
                    Some("detach, run `herdr update`, then follow its restart guidance")
                );
            }
            other => panic!("expected system toast notify, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_forwards_system_notification_to_foreground_client() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: Some("api workspace".into()),
                        position: Some(crate::config::ToastHerdrPosition::TopLeft),
                        sound: api::schema::NotificationShowSound::Request,
                    },
                ),
            },
            respond_to,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api notification message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api sound message"),
        );

        match first {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "build failed");
                assert_eq!(body.as_deref(), Some("api workspace"));
            }
            other => panic!("expected api notification, got {other:?}"),
        }
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Sound);
                assert_eq!(message, "agent attention");
                assert!(body.is_none());
            }
            other => panic!("expected api sound, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_preserves_colon_in_forwarded_title() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build: failed".into(),
                        body: Some("api workspace".into()),
                        position: None,
                        sound: api::schema::NotificationShowSound::None,
                    },
                ),
            },
            respond_to,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api notification message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "build: failed");
                assert_eq!(body.as_deref(), Some("api workspace"));
            }
            other => panic!("expected api notification, got {other:?}"),
        }
    }

    #[test]
    fn notification_show_api_validates_empty_title_before_disabled_delivery() {
        let mut server = test_headless_server();
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Off;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "\n\t".into(),
                        body: None,
                        position: None,
                        sound: api::schema::NotificationShowSound::None,
                    },
                ),
            },
            respond_to,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.error.code, "invalid_params");
        assert_eq!(parsed.error.message, "notification title is empty");
    }

    #[test]
    fn notification_show_api_reports_no_foreground_client() {
        let mut server = test_headless_server();
        server.foreground_client_id = None;
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "notify".into(),
                method: api::schema::Method::NotificationShow(
                    api::schema::NotificationShowParams {
                        title: "build failed".into(),
                        body: None,
                        position: None,
                        sound: api::schema::NotificationShowSound::Request,
                    },
                ),
            },
            respond_to,
        });

        assert!(changed);
        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: false,
                reason: api::schema::NotificationShowReason::NoForegroundClient,
            }
        );
    }

    #[test]
    fn notification_show_api_herdr_toast_expires_headless() {
        let mut server = test_headless_server();
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "notify".into(),
                    method: api::schema::Method::NotificationShow(
                        api::schema::NotificationShowParams {
                            title: "build failed".into(),
                            body: None,
                            position: None,
                            sound: api::schema::NotificationShowSound::None,
                        },
                    ),
                },
                respond_to,
            })
        );

        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        let deadline = server.app.toast_deadline.expect("api toast deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));
        assert!(server.app.state.toast.is_none());
        assert!(server.app.toast_deadline.is_none());
    }

    #[test]
    fn notification_show_api_forwards_sound_for_herdr_delivery() {
        let mut server = test_headless_server();
        let (client_tx, client_control_rx, _client_rx) = test_client_writer();

        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "notify".into(),
                    method: api::schema::Method::NotificationShow(
                        api::schema::NotificationShowParams {
                            title: "build failed".into(),
                            body: None,
                            position: None,
                            sound: api::schema::NotificationShowSound::Done,
                        },
                    ),
                },
                respond_to,
            })
        );

        let response = response_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        let parsed: api::schema::SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            parsed.result,
            api::schema::ResponseResult::NotificationShow {
                shown: true,
                reason: api::schema::NotificationShowReason::Shown,
            }
        );
        match read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("api sound message"),
        ) {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::Sound);
                assert_eq!(message, "agent done");
                assert!(body.is_none());
            }
            other => panic!("expected api sound, got {other:?}"),
        }
    }

    #[test]
    fn delayed_agent_notification_forwards_after_deadline() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        server.app.state.toast_config.delay_seconds = 1;

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let changed = server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
            pane_id,
            agent: Some(crate::detect::Agent::Pi),
            state: crate::detect::AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: Instant::now(),
        });

        assert!(changed);
        assert!(server.app.state.toast.is_none());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "delayed transition should not notify immediately"
        );

        let deadline = server
            .app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));

        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed sound message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed toast message"),
        );

        assert!(matches!(
            first,
            ServerMessage::Notify {
                kind: protocol::NotifyKind::Sound,
                ..
            }
        ));
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "pi needs attention");
                assert_eq!(body.as_deref(), Some("background · 1"));
            }
            other => panic!("expected delayed system toast, got {other:?}"),
        }
        assert!(server.app.state.pending_agent_notifications.is_empty());
    }

    #[test]
    fn delayed_active_tab_unfocused_agent_notification_forwards_after_deadline() {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("active");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.ensure_test_terminals();
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.app.state.toast_config.delivery = crate::config::ToastDelivery::System;
        server.app.state.toast_config.delay_seconds = 1;

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                Some(false),
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        assert!(
            server.handle_internal_event_with_forwarding(AppEvent::StateChanged {
                pane_id,
                agent: Some(crate::detect::Agent::Pi),
                state: crate::detect::AgentState::Blocked,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
                observed_at: Instant::now(),
            })
        );
        assert!(server.app.state.toast.is_none());
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "delayed transition should not notify immediately"
        );

        let deadline = server
            .app
            .state
            .next_pending_agent_notification_deadline()
            .expect("pending notification deadline");
        assert!(server.handle_scheduled_tasks_headless(deadline, false));

        let first = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed sound message"),
        );
        let second = read_server_message(
            client_control_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("delayed toast message"),
        );

        assert!(matches!(
            first,
            ServerMessage::Notify {
                kind: protocol::NotifyKind::Sound,
                ..
            }
        ));
        match second {
            ServerMessage::Notify {
                kind,
                message,
                body,
            } => {
                assert_eq!(kind, protocol::NotifyKind::SystemToast);
                assert_eq!(message, "pi needs attention");
                assert_eq!(body.as_deref(), Some("active · 1"));
            }
            other => panic!("expected delayed system toast, got {other:?}"),
        }
    }

    #[test]
    fn stale_api_agent_report_does_not_forward_done_sound() {
        let mut server = test_headless_server();
        let background = crate::workspace::Workspace::test_new("background");
        let pane_id = background.tabs[0].root_pane;
        let public_pane_id = format!("{}:p1", background.id);
        let foreground = crate::workspace::Workspace::test_new("foreground");
        server.app.state.workspaces = vec![background, foreground];
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "herdr:pi".into(),
                "pi".into(),
                crate::detect::AgentState::Working,
                None,
                Some(20),
            );
        server.app.state.active = Some(1);
        server.app.state.selected = 1;
        server.app.state.mode = crate::app::Mode::Terminal;

        let (client_tx, client_control_rx, _client_rx) = test_client_writer();
        server.clients.insert(
            1,
            ClientConnection::new(
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                crate::terminal_theme::TerminalTheme::default(),
                None,
                1,
                RenderEncoding::SemanticFrame,
                Some(client_tx),
            ),
        );
        server.foreground_client_id = Some(1);
        server.sync_foreground_client_state();

        let (respond_to, response_rx) = std::sync::mpsc::channel();
        let changed = server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request: api::schema::Request {
                id: "stale".into(),
                method: api::schema::Method::PaneReportAgent(api::schema::PaneReportAgentParams {
                    pane_id: public_pane_id,
                    source: "herdr:pi".into(),
                    agent: "pi".into(),
                    state: api::schema::PaneAgentState::Idle,
                    message: None,
                    custom_status: None,
                    seq: Some(19),
                    agent_session_id: None,
                    agent_session_path: None,
                }),
            },
            respond_to,
        });

        assert!(changed);
        assert!(response_rx.recv_timeout(Duration::from_millis(100)).is_ok());
        assert_eq!(
            server.app.state.terminals.get(&terminal_id).unwrap().state,
            crate::detect::AgentState::Working
        );
        assert!(
            client_control_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "stale idle report must not forward a done sound"
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

    // -----------------------------------------------------------------
    // host.* lifecycle (Task 9)
    //
    // Mirrors the fake-remote pattern from src/server/remote_pane.rs's own
    // tests: a real `UnixListener` scripted to play canned wire-format
    // responses (built from the real API structs, so the test can't drift
    // from the wire shape), fed to the real `host.attach`/`host.list`/
    // `host.detach` handlers through `host_transport_override_for_test`
    // instead of a real `ssh` binary/target.
    // -----------------------------------------------------------------

    #[cfg(unix)]
    mod host_lifecycle {
        use super::*;
        use crate::server::host_transport::{LinkTransport, UnixSocketTransport};
        use std::io::{BufRead, BufReader, Read, Write};
        use std::os::unix::net::{UnixListener, UnixStream};

        fn unique_host_test_dir(name: &str) -> std::path::PathBuf {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
        }

        fn read_fake_request(conn: &mut UnixStream) -> api::schema::Request {
            let mut line = String::new();
            BufReader::new(&mut *conn).read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }

        fn write_fake_line<T: serde::Serialize>(conn: &mut UnixStream, value: &T) {
            let encoded = serde_json::to_string(value).unwrap();
            conn.write_all(encoded.as_bytes()).unwrap();
            conn.write_all(b"\n").unwrap();
            conn.flush().unwrap();
        }

        fn fake_pane(pane_id: &str) -> api::schema::PaneInfo {
            api::schema::PaneInfo {
                pane_id: pane_id.to_string(),
                terminal_id: format!("term_{pane_id}"),
                workspace_id: "ws1".to_string(),
                tab_id: "tab1".to_string(),
                focused: false,
                cwd: None,
                foreground_cwd: None,
                label: None,
                agent: None,
                title: None,
                display_agent: None,
                agent_status: api::schema::AgentStatus::Idle,
                custom_status: None,
                state_labels: HashMap::new(),
                agent_session: None,
                revision: 0,
            }
        }

        /// Installs a transport override pointing every `host.attach` at
        /// `sock`, so the real production ssh-building path (and its config
        /// I/O) is never exercised in tests.
        fn use_fake_remote_socket(server: &mut HeadlessServer, sock: std::path::PathBuf) {
            let factory: std::sync::Arc<HostTransportFactory> =
                std::sync::Arc::new(move |_host: &str| {
                    Box::new(UnixSocketTransport {
                        api_socket: sock.clone(),
                        client_socket: sock.clone(),
                    }) as Box<dyn LinkTransport>
                });
            server.host_transport_override_for_test = Some(factory);
        }

        fn host_list_hosts(server: &mut HeadlessServer) -> Vec<serde_json::Value> {
            let response = server.handle_host_list_api("test:host:list".to_string());
            let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
            parsed["result"]["hosts"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        }

        /// Drains whatever `HostEvent`s the bridge thread has forwarded so
        /// far and returns the current `host.list` entry for `host`, or
        /// `None` if it isn't attached (yet).
        fn poll_host_entry(server: &mut HeadlessServer, host: &str) -> Option<serde_json::Value> {
            server.drain_server_events();
            host_list_hosts(server)
                .into_iter()
                .find(|entry| entry["host"] == host)
        }

        fn wait_for_host_state(
            server: &mut HeadlessServer,
            host: &str,
            state: &str,
        ) -> serde_json::Value {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(entry) = poll_host_entry(server, host) {
                    if entry["state"] == state {
                        return entry;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for host '{host}' to reach state '{state}'"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn host_attach_reports_connected_and_pane_count_then_detach_clears_it() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-attach");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();

                // Connection A: pane.list snapshot with two panes.
                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(request.method, api::schema::Method::PaneList(_)));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::PaneList {
                            panes: vec![fake_pane("w1:p1"), fake_pane("w1:p2")],
                        },
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                drop(conn);

                // Connection B: events.subscribe, ack then held open until
                // host.detach closes the client side.
                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(
                    request.method,
                    api::schema::Method::EventsSubscribe(_)
                ));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::SubscriptionStarted {},
                    },
                );
                let mut buf = [0u8; 1];
                let _ = conn.read(&mut buf);
            });

            use_fake_remote_socket(&mut server, sock);

            let attach_response = server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            assert!(
                !attach_response.contains("\"error\""),
                "attach failed: {attach_response}"
            );

            let entry = wait_for_host_state(&mut server, "workbox", "connected");
            assert_eq!(entry["pane_count"], 2);

            // The sidebar's display map (Task 7 seam) must agree with the
            // API's view -- both are driven by the same single mutation
            // point (`sync_host_link_display`).
            assert_eq!(
                server
                    .app
                    .state
                    .host_links
                    .get(&crate::terminal::TerminalHostTag::new("workbox")),
                Some(&crate::app::state::HostLinkDisplayState::Connected)
            );

            let detach_response = server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            assert!(
                !detach_response.contains("\"error\""),
                "detach failed: {detach_response}"
            );
            assert!(host_list_hosts(&mut server).is_empty());
            assert!(!server
                .app
                .state
                .host_links
                .contains_key(&crate::terminal::TerminalHostTag::new("workbox")));

            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn detaching_an_unattached_host_is_a_not_found_error() {
            let mut server = test_headless_server();
            let response = server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "ghost".to_string(),
                },
            );
            assert!(response.contains("\"error\""));
            assert!(response.contains("not_found"));
        }

        #[test]
        fn attaching_an_already_attached_host_is_rejected() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-attach-twice");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            // Never accepted: the second attach must be rejected before
            // opening any transport at all.
            let _listener = UnixListener::bind(&sock).unwrap();
            use_fake_remote_socket(&mut server, sock);

            let first = server.handle_host_attach_api(
                "test:host:attach:1".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            assert!(!first.contains("\"error\""), "{first}");

            let second = server.handle_host_attach_api(
                "test:host:attach:2".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            assert!(second.contains("\"error\""));
            assert!(second.contains("already_attached"));

            server.stop_host_link_runtime(&crate::server::host_link::HostLinkId(
                "workbox".to_string(),
            ));
            let _ = fs::remove_dir_all(&dir);
        }

        /// Pins the reconnect wiring this task adds on top of Task 3/6's
        /// pure state machine: a link that drops degrades to
        /// `reconnecting`, and -- because `handle_host_link_down` respawns
        /// a fresh transport + event loop per attempt -- keeps failing
        /// reconnect attempts until `on_disconnect`'s attempt cap lands it
        /// on the terminal `offline` state. The fake remote closes its
        /// listener after the very first round, so every respawned attempt
        /// fails to even connect (immediate `ECONNREFUSED`, no fresh
        /// `Snapshot`), which is what makes the attempt counter actually
        /// climb instead of resetting on each retry's own success.
        #[test]
        fn host_link_down_escalates_to_offline_after_repeated_reconnect_failures() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-reconnect");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(request.method, api::schema::Method::PaneList(_)));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::PaneList {
                            panes: vec![fake_pane("w1:p1")],
                        },
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                drop(conn);

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(
                    request.method,
                    api::schema::Method::EventsSubscribe(_)
                ));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::SubscriptionStarted {},
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                // `listener` drops here: every later connect attempt from a
                // respawned loop fails instantly instead of hanging.
            });

            use_fake_remote_socket(&mut server, sock);

            let attach_response = server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            assert!(!attach_response.contains("\"error\""), "{attach_response}");

            // First observe the (transient) reconnecting state the initial
            // drop produces...
            wait_for_host_state(&mut server, "workbox", "reconnecting");
            // ...then confirm the repeated-failure escalation actually
            // reaches the terminal offline state instead of retrying
            // forever.
            wait_for_host_state(&mut server, "workbox", "offline");

            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        fn remote_pane_info(id: &str) -> crate::server::remote_pane::RemotePaneInfo {
            crate::server::remote_pane::RemotePaneInfo {
                remote_pane_id: id.to_string(),
                remote_terminal_id: format!("term_{id}"),
                agent_status: api::schema::AgentStatus::Idle,
                label: None,
                agent: None,
                title: None,
                display_agent: None,
                custom_status: None,
            }
        }

        fn host_pane_count(server: &mut HeadlessServer) -> u64 {
            host_list_hosts(server)
                .iter()
                .find(|entry| entry["host"] == "workbox")
                .and_then(|entry| entry["pane_count"].as_u64())
                .unwrap_or_default()
        }

        /// Serves one attach round on `listener`: a `pane.list` returning
        /// `panes`, then an `events.subscribe` ack. Returns the held-open
        /// subscribe connection so the caller can keep it alive (dropping it
        /// would look like a link drop to the loop).
        fn serve_attach_round(
            listener: &UnixListener,
            panes: Vec<api::schema::PaneInfo>,
        ) -> UnixStream {
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_fake_request(&mut conn);
            assert!(matches!(request.method, api::schema::Method::PaneList(_)));
            write_fake_line(
                &mut conn,
                &api::schema::SuccessResponse {
                    id: request.id,
                    result: api::schema::ResponseResult::PaneList { panes },
                },
            );
            conn.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);

            let (mut conn, _) = listener.accept().unwrap();
            let request = read_fake_request(&mut conn);
            assert!(matches!(
                request.method,
                api::schema::Method::EventsSubscribe(_)
            ));
            write_fake_line(
                &mut conn,
                &api::schema::SuccessResponse {
                    id: request.id,
                    result: api::schema::ResponseResult::SubscriptionStarted {},
                },
            );
            conn
        }

        /// The detach+reattach correlation race, closed by the generation
        /// stamp on every `HostEventEnvelope`: after `detach x && attach x`,
        /// a `Snapshot` the *old* generation's loop sent (or, here, one
        /// synthesized with the old generation) must NOT reconcile against
        /// the new generation's registry. Drives `handle_host_event`
        /// directly with a synthetic stale envelope (wiring one through the
        /// real async bridge would be racy), which is the unit-level check
        /// the review called for.
        #[test]
        fn stale_generation_snapshot_is_dropped_after_reattach() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-generation");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();
                // Round 1: one pane. Keep the subscribe conn alive until the
                // detach stops the round-1 loop.
                let _round1 = serve_attach_round(&listener, vec![fake_pane("w1:p1")]);
                // Round 2 (after reattach): two panes, held open.
                let round2 =
                    serve_attach_round(&listener, vec![fake_pane("w1:p1"), fake_pane("w1:p2")]);
                // Park keeping round 2 open; nextest reaps this on exit.
                let mut buf = [0u8; 1];
                let mut held = round2;
                let _ = held.read(&mut buf);
            });

            use_fake_remote_socket(&mut server, sock);

            // Round 1: attach, capture generation G1.
            server.handle_host_attach_api(
                "test:attach:1".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");
            let gen1 = server
                .host_generation_for_test("workbox")
                .expect("attached host has a generation");
            assert_eq!(host_pane_count(&mut server), 1);

            // Detach, then reattach: G2 must differ from G1.
            server.handle_host_detach_api(
                "test:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            server.handle_host_attach_api(
                "test:attach:2".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");
            let gen2 = server
                .host_generation_for_test("workbox")
                .expect("reattached host has a generation");
            assert_ne!(gen1, gen2, "reattach must bump the generation");
            assert_eq!(host_pane_count(&mut server), 2);

            // A Snapshot stamped with the OLD generation carrying only
            // w1:p1 would, if applied, release w1:p2 (missing from it) and
            // drop the count to 1. The generation guard must drop it.
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation: gen1,
                event: crate::server::remote_pane::HostEvent::Snapshot {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    panes: vec![remote_pane_info("w1:p1")],
                },
            });
            assert_eq!(
                host_pane_count(&mut server),
                2,
                "stale-generation snapshot must be dropped, not reconciled"
            );

            // Positive control: a CURRENT-generation snapshot IS applied.
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation: gen2,
                event: crate::server::remote_pane::HostEvent::Snapshot {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    panes: vec![
                        remote_pane_info("w1:p1"),
                        remote_pane_info("w1:p2"),
                        remote_pane_info("w1:p3"),
                    ],
                },
            });
            assert_eq!(
                host_pane_count(&mut server),
                3,
                "current-generation snapshot must reconcile normally"
            );

            server.handle_host_detach_api(
                "test:detach:2".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        // -------------------------------------------------------------
        // Task 9b: adopt remote panes as host-tagged TerminalState entries
        // -------------------------------------------------------------

        /// Every host-tagged terminal currently in `app.state.terminals` for
        /// `host`, sorted by `TerminalId` for stable assertions.
        fn host_tagged_terminals<'a>(
            server: &'a HeadlessServer,
            host: &str,
        ) -> Vec<&'a crate::terminal::TerminalState> {
            let tag = crate::terminal::TerminalHostTag::new(host);
            let mut terminals: Vec<&crate::terminal::TerminalState> = server
                .app
                .state
                .terminals
                .values()
                .filter(|terminal| terminal.host.as_ref() == Some(&tag))
                .collect();
            terminals.sort_by_key(|terminal| terminal.id.to_string());
            terminals
        }

        #[test]
        fn snapshot_adopts_host_tagged_terminals_with_status_and_agent_label() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-snapshot-adopt");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(request.method, api::schema::Method::PaneList(_)));
                let mut working_pane = fake_pane("w1:p1");
                working_pane.agent_status = api::schema::AgentStatus::Working;
                working_pane.agent = Some("claude".to_string());
                let mut done_pane = fake_pane("w1:p2");
                done_pane.agent_status = api::schema::AgentStatus::Done;
                done_pane.display_agent = Some("Codex".to_string());
                done_pane.custom_status = Some("reviewed PR #42".to_string());
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::PaneList {
                            panes: vec![working_pane, done_pane],
                        },
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                drop(conn);

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(
                    request.method,
                    api::schema::Method::EventsSubscribe(_)
                ));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::SubscriptionStarted {},
                    },
                );
                let mut buf = [0u8; 1];
                let _ = conn.read(&mut buf);
            });

            use_fake_remote_socket(&mut server, sock);
            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            let terminals = host_tagged_terminals(&server, "workbox");
            assert_eq!(terminals.len(), 2, "both snapshot panes must be adopted");

            let claude = terminals
                .iter()
                .find(|terminal| terminal.agent_name.as_deref() == Some("claude"))
                .expect(
                    "pane reporting agent \"claude\" seeds agent_name from RemotePaneInfo.agent",
                );
            assert_eq!(
                claude.state,
                crate::detect::AgentState::Working,
                "AgentStatus::Working must invert to AgentState::Working"
            );

            let codex = terminals
                .iter()
                .find(|terminal| terminal.agent_name.as_deref() == Some("Codex"))
                .expect(
                    "pane with no agent but a display_agent falls back to seeding agent_name from it",
                );
            assert_eq!(
                codex.state,
                crate::detect::AgentState::Idle,
                "AgentStatus::Done must invert to AgentState::Idle"
            );

            // Fidelity fix (Task 9b): `TerminalState::state` alone collapses
            // `Done` and `Idle` into `AgentState::Idle`, so the sidebar's
            // true 5-way status has to come from `AppState::remote_pane_display`
            // instead.
            let claude_display = server
                .app
                .state
                .remote_pane_display
                .get(&claude.id)
                .expect("claude's terminal has a remote_pane_display entry");
            assert!(
                claude_display.seen,
                "AgentStatus::Working must decode to seen=true"
            );
            let codex_display = server
                .app
                .state
                .remote_pane_display
                .get(&codex.id)
                .expect("codex's terminal has a remote_pane_display entry");
            assert!(
                !codex_display.seen,
                "AgentStatus::Done must decode to seen=false so the sidebar renders \"done\", not \"idle\""
            );
            assert_eq!(
                codex_display.custom_status.as_deref(),
                Some("reviewed PR #42"),
                "custom_status must route into remote_pane_display too"
            );

            server.assert_remote_pane_terminal_bijection_for_test();

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn status_changed_updates_adopted_terminal_state_without_adopting() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-status-changed");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 1); // "w1:p0"
            use_fake_remote_socket(&mut server, sock);

            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            let terminal_id = host_tagged_terminals(&server, "workbox")
                .first()
                .expect("adopted terminal")
                .id
                .clone();
            assert_eq!(
                server.app.state.terminals[&terminal_id].state,
                crate::detect::AgentState::Idle
            );

            let generation = server.host_generation_for_test("workbox").unwrap();
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::StatusChanged {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    remote_pane_id: "w1:p0".to_string(),
                    status: api::schema::AgentStatus::Blocked,
                },
            });
            assert_eq!(
                server.app.state.terminals[&terminal_id].state,
                crate::detect::AgentState::Blocked,
                "StatusChanged must update the already-adopted terminal's state"
            );
            assert!(
                server.app.state.remote_pane_display[&terminal_id].seen,
                "Blocked must decode to seen=true (only Done maps to seen=false)"
            );

            // A later StatusChanged carrying AgentStatus::Done must flip the
            // fidelity bit even though TerminalState::state alone can't tell
            // Done and Idle apart (Task 9b fidelity fix).
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::StatusChanged {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    remote_pane_id: "w1:p0".to_string(),
                    status: api::schema::AgentStatus::Done,
                },
            });
            assert_eq!(
                server.app.state.terminals[&terminal_id].state,
                crate::detect::AgentState::Idle,
                "AgentStatus::Done must still invert to the collapsed AgentState::Idle"
            );
            assert!(
                !server.app.state.remote_pane_display[&terminal_id].seen,
                "AgentStatus::Done must decode to seen=false so the sidebar can tell it apart from Idle"
            );

            // Unknown pane id: no-op -- must not adopt, must not panic.
            let before = server.app.state.terminals.len();
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::StatusChanged {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    remote_pane_id: "w1:p9-ghost".to_string(),
                    status: api::schema::AgentStatus::Working,
                },
            });
            assert_eq!(
                server.app.state.terminals.len(),
                before,
                "an unmapped pane id must not create a terminal"
            );

            server.assert_remote_pane_terminal_bijection_for_test();

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn pane_closed_removes_the_host_tagged_terminal() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-pane-closed");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 1); // "w1:p0"
            use_fake_remote_socket(&mut server, sock);

            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");
            let terminals = host_tagged_terminals(&server, "workbox");
            assert_eq!(terminals.len(), 1);
            let terminal_id = terminals[0].id.clone();
            assert!(server
                .app
                .state
                .remote_pane_display
                .contains_key(&terminal_id));

            let generation = server.host_generation_for_test("workbox").unwrap();
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::PaneClosed {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    remote_pane_id: "w1:p0".to_string(),
                },
            });

            assert!(
                host_tagged_terminals(&server, "workbox").is_empty(),
                "PaneClosed must remove the pane's host-tagged terminal"
            );
            assert!(
                !server
                    .app
                    .state
                    .remote_pane_display
                    .contains_key(&terminal_id),
                "PaneClosed must also remove the terminal's remote_pane_display entry"
            );
            server.assert_remote_pane_terminal_bijection_for_test();

            // Idempotent: closing an already-closed pane is a no-op, not a
            // panic.
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::PaneClosed {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    remote_pane_id: "w1:p0".to_string(),
                },
            });

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn snapshot_reconciliation_removes_terminal_for_pane_missing_from_a_later_snapshot() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-snapshot-removal");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 2); // "w1:p0", "w1:p1"
            use_fake_remote_socket(&mut server, sock);

            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");
            assert_eq!(host_tagged_terminals(&server, "workbox").len(), 2);

            let generation = server.host_generation_for_test("workbox").unwrap();
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation,
                event: crate::server::remote_pane::HostEvent::Snapshot {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    panes: vec![remote_pane_info("w1:p0")],
                },
            });

            assert_eq!(
                host_tagged_terminals(&server, "workbox").len(),
                1,
                "reconciliation must remove the host-tagged terminal for a pane absent from the new snapshot"
            );
            server.assert_remote_pane_terminal_bijection_for_test();

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn host_detach_removes_its_host_tagged_terminals() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-detach-terminals");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 2);
            use_fake_remote_socket(&mut server, sock);

            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");
            assert_eq!(
                host_tagged_terminals(&server, "workbox").len(),
                2,
                "a connected host should have adopted two host-tagged terminals"
            );
            server.assert_remote_pane_terminal_bijection_for_test();

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );

            assert!(
                host_tagged_terminals(&server, "workbox").is_empty(),
                "detach must remove every host-tagged terminal for the detached host"
            );
            server.assert_remote_pane_terminal_bijection_for_test();

            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        // -------------------------------------------------------------
        // Task 10: persistence + restore of host links
        // -------------------------------------------------------------

        /// Spawns a fake remote herdr endpoint that behaves like a freshly
        /// attached host: answers exactly one `pane.list` with `pane_count`
        /// fake panes, then acks `events.subscribe` and blocks until the
        /// client closes the connection. Factored out of
        /// `host_attach_reports_connected_and_pane_count_then_detach_clears_it`'s
        /// inline remote thread so the Task 10 tests below can reuse the
        /// same fake-remote shape without touching that approved test.
        fn spawn_fake_host_remote(
            sock: std::path::PathBuf,
            pane_count: usize,
        ) -> std::thread::JoinHandle<()> {
            std::thread::spawn(move || {
                let listener = UnixListener::bind(&sock).unwrap();

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(request.method, api::schema::Method::PaneList(_)));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::PaneList {
                            panes: (0..pane_count)
                                .map(|i| fake_pane(&format!("w1:p{i}")))
                                .collect(),
                        },
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                drop(conn);

                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(
                    request.method,
                    api::schema::Method::EventsSubscribe(_)
                ));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::SubscriptionStarted {},
                    },
                );
                let mut buf = [0u8; 1];
                let _ = conn.read(&mut buf);
            })
        }

        /// Builds the `hosts` list `capture()` would save right now, the
        /// same way `perform_live_handoff` does: read back from the
        /// authoritative `HostLinkRegistry` (which the server, unlike `App`,
        /// can see directly).
        fn captured_hosts(server: &HeadlessServer) -> Vec<String> {
            crate::persist::capture(
                &server.app.state.workspaces,
                &server.app.state.terminals,
                &server.app.terminal_runtimes,
                server.app.state.active,
                server.app.state.selected,
                server.app.state.sidebar_width,
                server.app.state.sidebar_section_split,
                server.app.state.collapsed_space_keys.clone(),
                crate::persist::HostSnapshot::from_host_ids(
                    server.host_links.iter().map(|link| link.id.0.clone()),
                ),
            )
            .hosts
            .into_iter()
            .map(|h| h.host)
            .collect()
        }

        #[test]
        fn capture_saves_only_currently_attached_hosts() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-save");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 1);
            use_fake_remote_socket(&mut server, sock);

            assert!(captured_hosts(&server).is_empty());

            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            assert_eq!(captured_hosts(&server), vec!["workbox".to_string()]);

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );

            // A detached host must NOT persist.
            assert!(captured_hosts(&server).is_empty());

            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn restore_reattaches_persisted_hosts_through_the_attach_lifecycle() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-restore");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote = spawn_fake_host_remote(sock.clone(), 1);
            use_fake_remote_socket(&mut server, sock);

            // Mirrors what `run_server` does at startup: a session snapshot
            // loaded from disk carries a persisted host list, re-attached
            // through the exact same lifecycle `host.attach` uses -- no
            // separate restore-specific attach path.
            server.restore_attached_hosts(vec![crate::persist::HostSnapshot {
                host: "workbox".to_string(),
            }]);

            let entry = wait_for_host_state(&mut server, "workbox", "connected");
            assert_eq!(entry["pane_count"], 1);
            assert_eq!(captured_hosts(&server), vec!["workbox".to_string()]);

            // Detach so the fake remote's held-open connection B closes and
            // its thread can be joined -- the per-host poll loop owns that
            // socket until the link is torn down, so without this the remote
            // thread's blocking `read` (and thus `join`) never returns.
            // Mirrors the attach-path test's cleanup.
            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn restore_of_an_unreachable_host_degrades_like_any_attach() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("host-restore-unreachable");
            fs::create_dir_all(&dir).unwrap();
            // No listener bound at this path -- the transport's connection
            // attempts fail immediately, exactly like attaching to a host
            // that is down. There is no special restore case: the link just
            // rides the normal Connecting -> Reconnecting -> Offline path.
            let sock = dir.join("nobody-listening.sock");
            use_fake_remote_socket(&mut server, sock);

            server.restore_attached_hosts(vec![crate::persist::HostSnapshot {
                host: "ghost-box".to_string(),
            }]);

            wait_for_host_state(&mut server, "ghost-box", "offline");

            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn handoff_import_reattaches_persisted_hosts() {
            // `run_handoff_import_server` is a process entry point that can't
            // run in-process, so this mirrors its exact ordering: build the
            // receiving `App` from a handoff snapshot carrying a persisted
            // host list, take the restored hosts out BEFORE the server (and
            // its host-event bridge) is built, then re-attach AFTER -- the
            // same take-before-new / restore-after-new shape `run_server`
            // uses. RED before the receive-side wire (`new_from_handoff`
            // left `restored_hosts` empty, so nothing re-attached and this
            // timed out); GREEN after.
            let config = crate::config::Config::default();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let snapshot = crate::persist::SessionSnapshot {
                version: 0,
                workspaces: vec![],
                active: None,
                selected: 0,
                sidebar_width: None,
                sidebar_section_split: None,
                collapsed_space_keys: Default::default(),
                hosts: vec![crate::persist::HostSnapshot {
                    host: "workbox".to_string(),
                }],
            };
            let mut imports = HashMap::new();
            let mut app = crate::app::App::new_from_handoff(
                &config,
                None,
                api_rx,
                api::EventHub::default(),
                &snapshot,
                &mut imports,
            )
            .expect("new_from_handoff should succeed for an empty-workspace snapshot");

            // The receive-side wiring under test: new_from_handoff must carry
            // the persisted host list forward into `restored_hosts`.
            let restored_hosts = std::mem::take(&mut app.restored_hosts);

            let mut server = test_headless_server_from_app(app);
            let dir = unique_host_test_dir("host-handoff-restore");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");
            let remote = spawn_fake_host_remote(sock.clone(), 1);
            use_fake_remote_socket(&mut server, sock);

            server.restore_attached_hosts(restored_hosts);

            let entry = wait_for_host_state(&mut server, "workbox", "connected");
            assert_eq!(entry["pane_count"], 1);
            assert_eq!(captured_hosts(&server), vec!["workbox".to_string()]);

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        // -------------------------------------------------------------
        // Task 11: genuine two-server integration (a real remote, not a
        // scripted fake)
        //
        // Every test above scripts a bare `UnixListener` to play canned
        // wire-format lines -- enough to pin the state machine's wiring, but
        // it never proves the real remote API (a real `pane.list` handler, a
        // real `events.subscribe` stream backed by a real `EventHub`) is
        // what a live host link actually round-trips against. `RealRemoteServer`
        // below spins up a second, REAL `HeadlessServer` -- the exact
        // `HeadlessServer::new` + `run()` path `run_server()` drives in
        // production, just constructed directly instead of through main's
        // env-var/CLI plumbing -- with one real workspace/pane, and the test
        // attaches the server-under-test to it via `UnixSocketTransport`.
        //
        // Why this lives here instead of in `tests/multi_host.rs`: the only
        // hook that lets a host link skip real ssh is
        // `host_transport_override_for_test`, a private field on
        // `HeadlessServer` set directly (not through any public setter) --
        // see `use_fake_remote_socket` above. `herdr` has no `[lib]` target
        // (only a `[[bin]]`), so `tests/*.rs` integration tests link against
        // NONE of the crate's Rust internals; every existing integration
        // test drives the compiled binary as a child process and talks to it
        // purely over the JSON/wire socket protocol (see
        // `tests/server_headless.rs`'s `spawn_server`). `CARGO_BIN_EXE_herdr`
        // -- the mechanism those tests use to find that binary -- is itself
        // only defined for genuine integration-test targets, not for a bin
        // crate's own `#[cfg(test)]` unit tests (confirmed empirically: a
        // probe placed in this file failed to compile with "environment
        // variable `CARGO_BIN_EXE_herdr` not defined at compile time"). There
        // is no existing feature-gated seam either. So a genuinely
        // transport-swapped two-server test can only be written here, next
        // to the fake-remote tests it strengthens; `tests/multi_host.rs`
        // documents this and carries the homelab manual checklist instead.

        /// A second, real `HeadlessServer` running its own `run()` loop on a
        /// background thread with real API + client sockets. Kept
        /// deliberately separate from `use_fake_remote_socket`'s scripted
        /// `UnixListener`: this one answers `pane.list` and
        /// `events.subscribe` through the actual app/API stack, off one real
        /// workspace/pane seeded at construction.
        struct RealRemoteServer {
            api_socket: std::path::PathBuf,
            client_socket: std::path::PathBuf,
            quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
            wake: mpsc::Sender<ServerEvent>,
            thread: std::thread::JoinHandle<()>,
        }

        impl RealRemoteServer {
            /// Binds real sockets at `api_socket_path` (deriving the client
            /// path the same way `client_socket_path()` does), seeds one
            /// real pane, and runs the real `run()` loop on a background
            /// thread -- mirrors `run_server()`'s own construction order
            /// exactly (create App, seed workspace, bind API socket, build
            /// `HeadlessServer`, run).
            fn spawn(api_socket_path: std::path::PathBuf) -> RealRemoteServer {
                // `App` holds a `Box<dyn PrefixInputSource>` (and similar
                // trait objects), so neither it nor `HeadlessServer` is
                // `Send` -- unlike `run_server()`, which builds them on
                // whatever thread ends up calling `rt.block_on`, this helper
                // must build them ON the background thread itself and only
                // ship the (Send) handles the caller needs back over a
                // channel, instead of constructing here and moving them in.
                let thread_path = api_socket_path.clone();
                let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                let thread = std::thread::spawn(move || {
                    // Build + enter the runtime BEFORE constructing `App`:
                    // pane/PTY creation spawns tokio tasks internally (see
                    // `pane.rs`), which panics ("no reactor running") outside
                    // an active Tokio context -- mirrors `run_server()`
                    // building `app`/`server` inside its own `rt.block_on`.
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .expect("build tokio runtime for the second real server");
                    rt.block_on(async move {
                        std::env::set_var(api::SOCKET_PATH_ENV_VAR, &thread_path);

                        let config = crate::config::Config::default();
                        let (api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
                        let event_hub = api::EventHub::default();
                        let mut app =
                            crate::app::App::new(&config, true, None, api_rx, event_hub.clone());
                        app.state.local_sound_playback = false;
                        app.local_terminal_notifications = false;
                        app.create_workspace_with_options(std::env::temp_dir(), true)
                            .expect("seed one real pane on the second server");

                        let api_server = api::start_server(api_tx.clone(), event_hub)
                            .expect("bind real api socket for the second server");
                        let mut server =
                            HeadlessServer::new(app, &[], Some(api_tx), Some(api_server))
                                .expect("construct the second real headless server");

                        let client_socket = server.client_socket_path.clone();
                        let quit = std::sync::Arc::clone(&server.should_quit);
                        let wake = server.server_event_tx.clone();
                        let _ = ready_tx.send((client_socket, quit, wake));

                        let _ = server.run().await;
                    });
                    rt.shutdown_timeout(Duration::from_millis(100));
                });

                let (client_socket, quit, wake) = ready_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("second real server failed to start");

                RealRemoteServer {
                    api_socket: api_socket_path,
                    client_socket,
                    quit,
                    wake,
                    thread,
                }
            }

            /// Signals the real `run()` loop to shut down (the same
            /// `should_quit` + wake path Ctrl+C uses) and blocks until its
            /// thread exits. By the time this returns, both real sockets are
            /// closed and their files removed
            /// (`complete_shutdown`/`ServerHandle::drop`) -- a genuine "the
            /// remote server is gone", not merely a severed connection, so a
            /// fresh `spawn` can immediately rebind the same paths.
            fn kill(self) {
                self.quit.store(true, std::sync::atomic::Ordering::Release);
                let _ = self.wake.try_send(ServerEvent::QuitSignal);
                let _ = self.thread.join();
            }

            fn transport_factory(&self) -> std::sync::Arc<HostTransportFactory> {
                let api_socket = self.api_socket.clone();
                let client_socket = self.client_socket.clone();
                std::sync::Arc::new(move |_host: &str| {
                    Box::new(UnixSocketTransport {
                        api_socket: api_socket.clone(),
                        client_socket: client_socket.clone(),
                    }) as Box<dyn LinkTransport>
                })
            }
        }

        /// The full lifecycle end to end against a real second server:
        /// attach observes `connected` plus the real pane count; killing it
        /// degrades the link through `reconnecting` to the terminal
        /// `offline` state (matching
        /// `host_link_down_escalates_to_offline_after_repeated_reconnect_failures`'s
        /// already-proven "fast-failing transport burns the attempt budget
        /// almost immediately" behavior, just against a real dead socket
        /// instead of a closed fake listener); and because
        /// `HostLinkRegistry::on_disconnect` documents `Offline` as terminal
        /// for automatic retries ("manual retry is modeled as detach +
        /// attach" -- see `host_link.rs`), reviving the link after the real
        /// server restarts goes through a fresh `host.detach` + `host.attach`,
        /// exactly like a homelab user manually reattaching after noticing a
        /// host went offline.
        #[test]
        fn attach_reconnect_offline_and_restart_against_a_real_second_server() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("two-real-servers");
            fs::create_dir_all(&dir).unwrap();
            let api_socket_path = dir.join("remote-b-api.sock");

            let remote_b = RealRemoteServer::spawn(api_socket_path.clone());
            server.host_transport_override_for_test = Some(remote_b.transport_factory());

            let attach_response = server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "homelab-b".to_string(),
                },
            );
            assert!(!attach_response.contains("\"error\""), "{attach_response}");

            let entry = wait_for_host_state(&mut server, "homelab-b", "connected");
            assert_eq!(
                entry["pane_count"], 1,
                "the real second server seeded exactly one pane"
            );

            // Kill the real second server: a genuine process-equivalent
            // death (full graceful teardown of its own run loop and both
            // real sockets), not a scripted disconnect.
            remote_b.kill();

            wait_for_host_state(&mut server, "homelab-b", "reconnecting");
            wait_for_host_state(&mut server, "homelab-b", "offline");

            // Restart on the SAME socket paths -- the homelab equivalent of
            // the server process coming back up.
            let remote_b = RealRemoteServer::spawn(api_socket_path.clone());
            // `Offline` never auto-retries (see `on_disconnect`'s doc
            // comment on `host_link.rs`); manual retry is detach + attach.
            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "homelab-b".to_string(),
                },
            );
            server.host_transport_override_for_test = Some(remote_b.transport_factory());
            let attach_response = server.handle_host_attach_api(
                "test:host:attach:2".to_string(),
                api::schema::HostAttachParams {
                    host: "homelab-b".to_string(),
                },
            );
            assert!(!attach_response.contains("\"error\""), "{attach_response}");

            let entry = wait_for_host_state(&mut server, "homelab-b", "connected");
            assert_eq!(
                entry["pane_count"], 1,
                "the restarted second server also seeded exactly one pane"
            );

            server.handle_host_detach_api(
                "test:host:detach:2".to_string(),
                api::schema::HostDetachParams {
                    host: "homelab-b".to_string(),
                },
            );
            remote_b.kill();
            std::env::remove_var(api::SOCKET_PATH_ENV_VAR);
            let _ = fs::remove_dir_all(&dir);
        }

        // -------------------------------------------------------------
        // Task 9b commit 4: focus a remote pane -> attach over ssh -> render
        // its live grid (VIEW-ONLY: routing local input to it is a follow-up
        // commit). `focus_remote_pane` has no production caller yet --
        // clicking a remote sidebar entry needs the typed `AgentFocusTarget`
        // refactor described on its doc comment, deferred to a follow-up
        // commit -- so these tests call it directly, exactly the seam that
        // follow-up's click handler will call into.
        // -------------------------------------------------------------

        /// Registers a fake foreground client and returns its CONTROL
        /// channel receiver -- NOT the render channel `retained_test_server`
        /// reads elsewhere in this file. `send_to_client` (which
        /// `send_notify_to_foreground_client` goes through) writes through
        /// `writer.control`, so this is what a test needs in order to
        /// observe a `Notify` actually being sent.
        fn attach_fake_foreground_client(
            server: &mut HeadlessServer,
        ) -> std::sync::mpsc::Receiver<Vec<u8>> {
            let (client_tx, client_control_rx, _client_render_rx) = test_client_writer();
            server.clients.insert(
                1,
                ClientConnection::new(
                    (80, 24),
                    crate::kitty_graphics::HostCellSize::default(),
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    1,
                    RenderEncoding::SemanticFrame,
                    Some(client_tx),
                ),
            );
            server.foreground_client_id = Some(1);
            client_control_rx
        }

        /// Blocking-decodes one framed `ServerMessage::Notify` off a fake
        /// client's control channel.
        fn recv_notify(
            rx: &std::sync::mpsc::Receiver<Vec<u8>>,
        ) -> (protocol::NotifyKind, String, Option<String>) {
            let bytes = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("expected a Notify on the client control channel");
            match read_server_message(bytes) {
                ServerMessage::Notify {
                    kind,
                    message,
                    body,
                } => (kind, message, body),
                other => panic!("expected Notify, got {other:?}"),
            }
        }

        /// Completes the real client wire handshake for a `RemotePaneAttach`
        /// terminal channel on the scripted-remote side: reads `Hello`,
        /// replies `Welcome`, reads `AttachTerminal` (asserting it names
        /// `expected_terminal_id`). Mirrors `remote_pane.rs`'s own
        /// `accept_and_complete_attach_handshake` test helper (private to
        /// that module, so re-implemented here rather than shared
        /// cross-module for one helper).
        fn complete_attach_handshake(conn: &mut UnixStream, expected_terminal_id: &str) {
            let hello: crate::protocol::ClientMessage =
                protocol::read_message(conn, MAX_FRAME_SIZE).unwrap();
            match hello {
                crate::protocol::ClientMessage::Hello {
                    version,
                    requested_encoding,
                    launch_mode,
                    ..
                } => {
                    assert_eq!(version, crate::protocol::PROTOCOL_VERSION);
                    assert_eq!(requested_encoding, RenderEncoding::TerminalAnsi);
                    assert_eq!(
                        launch_mode,
                        crate::protocol::ClientLaunchMode::TerminalAttach
                    );
                }
                other => panic!("expected Hello, got {other:?}"),
            }
            protocol::write_message(
                conn,
                &ServerMessage::Welcome {
                    version: crate::protocol::PROTOCOL_VERSION,
                    encoding: RenderEncoding::TerminalAnsi,
                    error: None,
                },
            )
            .unwrap();

            let attach: crate::protocol::ClientMessage =
                protocol::read_message(conn, MAX_FRAME_SIZE).unwrap();
            match attach {
                crate::protocol::ClientMessage::AttachTerminal {
                    terminal_id,
                    takeover,
                } => {
                    assert_eq!(terminal_id, expected_terminal_id);
                    assert!(!takeover, "attach must not request takeover");
                }
                other => panic!("expected AttachTerminal, got {other:?}"),
            }
        }

        /// Row 0 of `terminal_id`'s runtime, rendered through the exact same
        /// `render_terminal_virtual` the real render branch
        /// (`overlay_focused_remote_pane`) reuses, trimmed of trailing
        /// blanks so assertions don't have to hard-code padding.
        fn rendered_row0_trimmed(
            server: &HeadlessServer,
            terminal_id: &crate::terminal::TerminalId,
            width: u16,
            height: u16,
        ) -> String {
            let runtime = server
                .app
                .terminal_runtimes
                .get(terminal_id)
                .expect("runtime should be registered");
            let (buffer, _cursor) = crate::server::render_stream::render_terminal_virtual(
                runtime,
                Rect::new(0, 0, width, height),
            );
            (0..width)
                .map(|x| buffer[(x, 0)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        }

        /// The full happy path end to end: `focus_remote_pane` attaches off
        /// the main loop, the established runtime is registered and fed by
        /// generation-stamped `TerminalBytes` (proven with BOTH a stale- and
        /// a current-generation synthetic envelope, so the generation-stamp
        /// gotcha the plan calls out is actually exercised, not just
        /// asserted by reading the code), the render branch's own
        /// `render_terminal_virtual` shows the live content, and blurring
        /// tears the runtime + attach down again with nothing left over.
        ///
        /// `#[tokio::test]`: `spawn_remote_fed` (via
        /// `handle_remote_pane_attach_established`) needs an active Tokio
        /// runtime, unlike every other `host_lifecycle` test above, which
        /// never constructs a real `TerminalRuntime`.
        #[tokio::test]
        async fn focus_remote_pane_attaches_feeds_generation_stamped_frames_and_tears_down_on_blur()
        {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("focus-remote-pane");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();

                // Connection 1: pane.list snapshot with one pane. fake_pane's
                // terminal_id ("term_w1:p1") is what focus_remote_pane's
                // attach() AttachTerminals against.
                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(request.method, api::schema::Method::PaneList(_)));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::PaneList {
                            panes: vec![fake_pane("w1:p1")],
                        },
                    },
                );
                conn.shutdown(std::net::Shutdown::Both).ok();
                drop(conn);

                // Connection 2: events.subscribe ack, held open.
                let (mut conn, _) = listener.accept().unwrap();
                let request = read_fake_request(&mut conn);
                assert!(matches!(
                    request.method,
                    api::schema::Method::EventsSubscribe(_)
                ));
                write_fake_line(
                    &mut conn,
                    &api::schema::SuccessResponse {
                        id: request.id,
                        result: api::schema::ResponseResult::SubscriptionStarted {},
                    },
                );

                // Connection 3: the terminal channel focus_remote_pane's
                // off-loop attach opens. Play the handshake, then write the
                // SAME Terminal frame several times with small gaps: the
                // off-loop thread's RemotePaneAttachEstablished handback and
                // this reader's first TerminalBytes race independently for
                // the shared server_event channel, so repeating the frame
                // guarantees at least one arrives AFTER the runtime is
                // registered instead of being silently dropped as "not
                // attached yet" -- exactly like a real remote's periodic
                // repaints.
                let (mut conn, _) = listener.accept().unwrap();
                complete_attach_handshake(&mut conn, "term_w1:p1");
                for _ in 0..10 {
                    // The test's main thread may already have blurred (and
                    // so force-closed/detached) by the time a later
                    // iteration writes -- a real "the client already
                    // stopped watching" race, not a bug in this scripted
                    // remote. Stop quietly instead of unwrapping.
                    if protocol::write_message(
                        &mut conn,
                        &ServerMessage::Terminal(crate::protocol::TerminalFrame {
                            seq: 1,
                            width: 80,
                            height: 24,
                            full: true,
                            bytes: b"remote output".to_vec(),
                        }),
                    )
                    .is_err()
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let mut buf = [0u8; 64];
                let _ = conn.read(&mut buf);
            });

            use_fake_remote_socket(&mut server, sock);
            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            let terminal_id = host_tagged_terminals(&server, "workbox")
                .first()
                .expect("remote pane should be adopted as a host-tagged terminal")
                .id
                .clone();

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Wait for the off-loop attach's success handback to register
            // the remote-fed runtime (RemotePaneAttachEstablished).
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                server.drain_server_events();
                if server.app.terminal_runtimes.get(&terminal_id).is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the remote-fed runtime to register"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                server.focused_remote_pane_attach.is_some(),
                "the established RemotePaneAttach handle should be stored"
            );

            // Wait for a real, generation-stamped TerminalBytes from the
            // reader thread to actually reach and feed the runtime (proving
            // frames are NOT silently dropped by the generation guard).
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                server.drain_server_events();
                if rendered_row0_trimmed(&server, &terminal_id, 80, 24) == "remote output" {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for a real generation-stamped TerminalBytes frame to feed the runtime"
                );
                std::thread::sleep(Duration::from_millis(20));
            }

            // Resolve the local PaneId for the synthetic envelopes below by
            // constructing the RemotePaneKey directly, rather than reaching
            // into focus_remote_pane's own private lookup helper.
            let key = crate::server::remote_pane::RemotePaneKey {
                host: crate::server::host_link::HostLinkId("workbox".to_string()),
                remote_pane_id: "w1:p1".to_string(),
            };
            let local_pane = server
                .remote_panes
                .local_for(&key)
                .expect("remote pane should be adopted");
            let current_generation = server
                .host_generation_for_test("workbox")
                .expect("attached host has a generation");

            // GENERATION GOTCHA proof: an envelope stamped with the WRONG
            // generation must be dropped by handle_host_event's guard
            // rather than fed to the runtime.
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation: current_generation.wrapping_add(1),
                event: crate::server::remote_pane::HostEvent::TerminalBytes {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    local_pane,
                    width: 80,
                    height: 24,
                    bytes: b"\x1b[2J\x1b[Hstale generation".to_vec(),
                },
            });
            assert_eq!(
                rendered_row0_trimmed(&server, &terminal_id, 80, 24),
                "remote output",
                "a stale-generation TerminalBytes must be dropped, not fed"
            );

            // Positive control: the SAME kind of event, correctly stamped
            // with the host's CURRENT generation, IS fed.
            server.handle_host_event(crate::server::remote_pane::HostEventEnvelope {
                generation: current_generation,
                event: crate::server::remote_pane::HostEvent::TerminalBytes {
                    host: crate::server::host_link::HostLinkId("workbox".to_string()),
                    local_pane,
                    width: 80,
                    height: 24,
                    bytes: b"\x1b[2J\x1b[Hsecond frame".to_vec(),
                },
            });
            assert_eq!(
                rendered_row0_trimmed(&server, &terminal_id, 80, 24),
                "second frame",
                "a current-generation TerminalBytes must be fed to the runtime"
            );

            // Blur: teardown must remove the runtime and detach the live
            // attach so nothing leaks across a focus change.
            server.blur_focused_remote_pane();
            assert_eq!(server.focused_remote_pane, None);
            assert!(server.focused_remote_pane_attach.is_none());
            assert!(server.app.terminal_runtimes.get(&terminal_id).is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// `attach()` itself failing (e.g. the remote's `Welcome` rejects
        /// the handshake -- unknown terminal id, version mismatch, etc.)
        /// hands back `ServerEvent::RemotePaneAttachFailed`. This must clear
        /// `focused_remote_pane` (so a stale "attaching..." state doesn't
        /// linger) and notify the foreground client, without ever
        /// registering a runtime.
        #[test]
        fn focus_remote_pane_attach_failure_clears_focus_and_notifies_without_registering_a_runtime(
        ) {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("focus-remote-pane-attach-fail");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();
                let _round = serve_attach_round(&listener, vec![fake_pane("w1:p1")]);

                // Connection 3: the terminal channel. Reject the handshake
                // outright via Welcome{error: Some(_)} -- attach() must
                // return Err without ever writing AttachTerminal.
                let (mut conn, _) = listener.accept().unwrap();
                let _hello: crate::protocol::ClientMessage =
                    protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
                protocol::write_message(
                    &mut conn,
                    &ServerMessage::Welcome {
                        version: crate::protocol::PROTOCOL_VERSION,
                        encoding: RenderEncoding::TerminalAnsi,
                        error: Some("no such terminal".to_string()),
                    },
                )
                .unwrap();
            });

            use_fake_remote_socket(&mut server, sock);
            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            let terminal_id = host_tagged_terminals(&server, "workbox")
                .first()
                .expect("remote pane should be adopted as a host-tagged terminal")
                .id
                .clone();
            let control_rx = attach_fake_foreground_client(&mut server);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                server.drain_server_events();
                if server.focused_remote_pane.is_none() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the attach failure handback to clear focus"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "a failed attach must never register a runtime"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            let (kind, message, body) = recv_notify(&control_rx);
            assert_eq!(kind, protocol::NotifyKind::Toast);
            assert!(message.contains("workbox"), "message was: {message}");
            assert!(
                body.as_deref()
                    .unwrap_or_default()
                    .contains("no such terminal"),
                "body was: {body:?}"
            );

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// A mid-attach refusal: `attach()` succeeds at write time (so
        /// `RemotePaneAttachEstablished` may already have registered a
        /// runtime), but the reader sees a `ServerShutdown` BEFORE any
        /// `Terminal` frame -- the remote refused this terminal id. Must
        /// converge to focus cleared + no runtime registered + a
        /// notification, regardless of which handback (established vs.
        /// this `HostEvent::AttachFailed`) the two independent threads
        /// happen to deliver first.
        ///
        /// `#[tokio::test]`: the established-first ordering can construct a
        /// real `TerminalRuntime` via `spawn_remote_fed`, which needs an
        /// active Tokio runtime.
        #[tokio::test]
        async fn remote_refusing_the_terminal_attach_mid_stream_clears_focus_and_tears_down() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("focus-remote-pane-refused");
            fs::create_dir_all(&dir).unwrap();
            let sock = dir.join("remote-api.sock");

            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();
                let _round = serve_attach_round(&listener, vec![fake_pane("w1:p1")]);

                let (mut conn, _) = listener.accept().unwrap();
                complete_attach_handshake(&mut conn, "term_w1:p1");
                protocol::write_message(
                    &mut conn,
                    &ServerMessage::ServerShutdown {
                        reason: Some("terminal not found".to_string()),
                    },
                )
                .unwrap();
            });

            use_fake_remote_socket(&mut server, sock);
            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(&mut server, "workbox", "connected");

            let terminal_id = host_tagged_terminals(&server, "workbox")
                .first()
                .expect("remote pane should be adopted as a host-tagged terminal")
                .id
                .clone();
            let control_rx = attach_fake_foreground_client(&mut server);

            server.focus_remote_pane(terminal_id.clone());

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                server.drain_server_events();
                if server.focused_remote_pane.is_none()
                    && server.app.terminal_runtimes.get(&terminal_id).is_none()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the mid-attach refusal to settle"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(server.focused_remote_pane_attach.is_none());

            let (kind, message, body) = recv_notify(&control_rx);
            assert_eq!(kind, protocol::NotifyKind::Toast);
            assert!(message.contains("workbox"), "message was: {message}");
            assert!(
                body.as_deref()
                    .unwrap_or_default()
                    .contains("terminal not found"),
                "body was: {body:?}"
            );

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// Shared setup for the `handle_remote_pane_attach_established` guard
        /// tests below: attaches a fake "workbox" host with one adopted
        /// remote pane and returns its `terminal_id`. The caller drives
        /// `focus_remote_pane` and manipulates server state to break exactly
        /// one guard before draining the established handback.
        fn attached_workbox_terminal_id(
            server: &mut HeadlessServer,
            dir: &std::path::Path,
        ) -> (crate::terminal::TerminalId, std::thread::JoinHandle<()>) {
            let sock = dir.join("remote-api.sock");
            let remote_sock = sock.clone();
            let remote = std::thread::spawn(move || {
                let listener = UnixListener::bind(&remote_sock).unwrap();
                let _round = serve_attach_round(&listener, vec![fake_pane("w1:p1")]);

                // The terminal channel `focus_remote_pane`'s off-loop attach
                // opens: complete the handshake (attach() then returns Ok
                // and hands back RemotePaneAttachEstablished), then block
                // reading so this thread -- and `remote.join()` -- only
                // finishes once the guard branch under test rejects the
                // handback and `attach.detach()` closes the connection.
                // Correctness proof: if a bug registered the stale attach
                // instead, this connection stays open and `remote.join()`
                // below would hang instead of returning promptly.
                let (mut conn, _) = listener.accept().unwrap();
                complete_attach_handshake(&mut conn, "term_w1:p1");
                let mut buf = [0u8; 64];
                let _ = conn.read(&mut buf);
            });

            use_fake_remote_socket(server, sock);
            server.handle_host_attach_api(
                "test:host:attach".to_string(),
                api::schema::HostAttachParams {
                    host: "workbox".to_string(),
                },
            );
            wait_for_host_state(server, "workbox", "connected");

            let terminal_id = host_tagged_terminals(server, "workbox")
                .first()
                .expect("remote pane should be adopted as a host-tagged terminal")
                .id
                .clone();
            (terminal_id, remote)
        }

        /// Drains for a bounded window, giving the off-loop attach thread's
        /// handback time to arrive and be rejected.
        fn drain_for(server: &mut HeadlessServer, duration: Duration) {
            let deadline = Instant::now() + duration;
            while Instant::now() < deadline {
                server.drain_server_events();
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// Direct coverage for the `!still_focused` guard branch: if focus
        /// moved on to something else (or was cleared, as simulated here)
        /// while the off-loop ssh handshake was in flight, the established
        /// handback must detach the just-established attach and register
        /// nothing, rather than reviving a focus the user already left.
        #[tokio::test]
        async fn attach_established_handback_is_dropped_when_focus_moved_on() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("attach-established-focus-moved");
            fs::create_dir_all(&dir).unwrap();

            let (terminal_id, remote) = attached_workbox_terminal_id(&mut server, &dir);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Simulate focus moving elsewhere (e.g. the user focused a local
            // pane, which requests a blur) while the handshake above is
            // still in flight.
            server.focused_remote_pane = None;

            drain_for(&mut server, Duration::from_secs(2));

            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "a stale established handback must not register a runtime once focus moved on"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// Direct coverage for the `!pane_still_adopted` guard branch: if the
        /// adopted terminal was released (e.g. a `Snapshot` reconciliation
        /// dropped it) while the off-loop ssh handshake was in flight, the
        /// established handback must detach and register nothing.
        #[tokio::test]
        async fn attach_established_handback_is_dropped_when_pane_no_longer_adopted() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("attach-established-pane-released");
            fs::create_dir_all(&dir).unwrap();

            let (terminal_id, remote) = attached_workbox_terminal_id(&mut server, &dir);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Simulate the pane being released while attaching.
            server.app.state.terminals.remove(&terminal_id);

            drain_for(&mut server, Duration::from_secs(2));

            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "a stale established handback must not register a runtime for a released pane"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// Direct coverage for the `!generation_current` guard branch: if the
        /// host link's generation no longer matches what this attach attempt
        /// captured (e.g. a detach/reconnect happened while the off-loop ssh
        /// handshake was in flight), the established handback must detach
        /// and register nothing rather than keep a channel stamped with a
        /// now-superseded generation.
        #[tokio::test]
        async fn attach_established_handback_is_dropped_when_host_generation_changed() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("attach-established-generation-changed");
            fs::create_dir_all(&dir).unwrap();

            let (terminal_id, remote) = attached_workbox_terminal_id(&mut server, &dir);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Bump the live host link's generation past what this attach
            // attempt captured, simulating a detach/reconnect race.
            let host = crate::server::host_link::HostLinkId("workbox".to_string());
            {
                let runtime = server
                    .host_link_runtimes
                    .get_mut(&host)
                    .expect("host link runtime should exist");
                runtime.generation = runtime.generation.wrapping_add(1);
            }

            drain_for(&mut server, Duration::from_secs(2));

            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "a stale established handback must not register a runtime after the host link's generation changed"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// Direct coverage for the attach-epoch guard: a same-terminal
        /// handback from an attach attempt superseded by a NEWER focus
        /// attempt on the SAME `terminal_id` (rapid refocus A -> B -> A)
        /// must be rejected even though `still_focused`/`generation_current`/
        /// `pane_still_adopted` all still pass -- `terminal_id` equality
        /// alone cannot tell a stale attach apart from the current one once
        /// focus can return to the same terminal. Simulated directly by
        /// bumping `remote_pane_focus_epoch` past what this attempt
        /// captured, rather than choreographing a real A -> B -> A race
        /// (which would need multiple adopted panes and cannot deterministically
        /// order two independent off-loop threads' socket connects).
        #[tokio::test]
        async fn attach_established_handback_is_dropped_when_focus_epoch_is_stale() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("attach-established-epoch-stale");
            fs::create_dir_all(&dir).unwrap();

            let (terminal_id, remote) = attached_workbox_terminal_id(&mut server, &dir);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Simulate a newer focus attempt on the SAME terminal having
            // already superseded this in-flight one.
            server.remote_pane_focus_epoch += 1;

            drain_for(&mut server, Duration::from_secs(2));

            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "a stale-epoch established handback must not register a runtime"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }

        /// The API-path blur leak, pinned. A client focuses a remote pane
        /// (attach established, remote-fed runtime registered), then a JSON
        /// API `pane.focus` of a LOCAL pane -- the API path, which never runs
        /// `handle_client_input_events` -- must still tear the remote pane
        /// down. `focus_pane_in_workspace` (the API handler's sink) sets a
        /// `Blur` request; the central per-iteration drain in
        /// `handle_deferred_requests_headless` must apply it. If the drain
        /// only ran on the client-input path (the bug), `focused_remote_pane`
        /// would stay `Some`, the ssh channel + runtime would leak, and the
        /// fake remote's blocking read would never return -- so
        /// `remote.join()` here would hang rather than fail loudly.
        ///
        /// `#[tokio::test]`: the established handback registers a real
        /// `TerminalRuntime` via `spawn_remote_fed`, which needs an active
        /// Tokio runtime.
        #[tokio::test]
        async fn api_local_pane_focus_blurs_focused_remote_pane_via_deferred_drain() {
            let mut server = test_headless_server();
            let dir = unique_host_test_dir("api-focus-blur");
            fs::create_dir_all(&dir).unwrap();

            let (terminal_id, remote) = attached_workbox_terminal_id(&mut server, &dir);

            server.focus_remote_pane(terminal_id.clone());
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // Wait for the off-loop attach's success handback to register the
            // remote-fed runtime, so the blur below has something to remove.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                server.drain_server_events();
                if server.app.terminal_runtimes.get(&terminal_id).is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the remote-fed runtime to register"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(server.focused_remote_pane_attach.is_some());

            // Seed a LOCAL workspace with a pane the API can focus.
            server
                .app
                .state
                .workspaces
                .push(crate::workspace::Workspace::test_new("local"));
            server.app.state.ensure_test_terminals();
            server.app.state.active = Some(0);
            let local_pane = server.app.state.workspaces[0].tabs[0].root_pane;

            // Drive a real JSON API `pane.focus` of the local pane (NOT client
            // input). `p_1_<raw>` addresses ws index 0 / this pane's raw id.
            let (respond_to, response_rx) = std::sync::mpsc::channel();
            server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "test:pane:focus:local".into(),
                    method: api::schema::Method::PaneFocus(api::schema::PaneTarget {
                        pane_id: format!("p_1_{}", local_pane.raw()),
                    }),
                },
                respond_to,
            });
            let response: serde_json::Value =
                serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap())
                    .unwrap();
            assert_eq!(
                response["result"]["type"], "pane_info",
                "pane.focus should succeed: {response}"
            );

            // The API path set a Blur request but -- unlike client input --
            // did NOT drain it: the remote pane is still focused at this point.
            assert_eq!(
                server.app.state.requested_remote_pane_focus,
                Some(crate::app::state::RemotePaneFocusRequest::Blur)
            );
            assert_eq!(server.focused_remote_pane, Some(terminal_id.clone()));

            // The central per-iteration deferred drain applies the Blur.
            assert!(server.handle_deferred_requests_headless());

            assert_eq!(server.app.state.requested_remote_pane_focus, None);
            assert_eq!(
                server.focused_remote_pane, None,
                "an API-path local focus must blur the focused remote pane"
            );
            assert!(
                server.app.terminal_runtimes.get(&terminal_id).is_none(),
                "the remote-fed runtime must be torn down, not leaked"
            );
            assert!(server.focused_remote_pane_attach.is_none());

            server.handle_host_detach_api(
                "test:host:detach".to_string(),
                api::schema::HostDetachParams {
                    host: "workbox".to_string(),
                },
            );
            // If the blur leaked, the attach would still be open and this
            // join would hang instead of returning.
            remote.join().unwrap();
            let _ = fs::remove_dir_all(&dir);
        }
    }
}
