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

mod input;
pub(crate) mod slots;

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use tracing::{debug, info, warn};

use crate::protocol::render_ansi;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientKeybindings, ClientLaunchMode,
    ClientMessage, NotifyKind, RenderEncoding, ServerMessage, MAX_CLIPBOARD_IMAGE_PAYLOAD,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};
use crate::server::socket_paths::client_socket_path;

static RECEIVED_KITTY_GRAPHICS_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

/// State tracking for the thin client.
struct ClientState {
    /// Stateful semantic-frame encoder used when the server sends FrameData.
    blit_encoder: render_ansi::BlitEncoder,
    /// Whether host mouse capture is currently active.
    mouse_capture_active: bool,
    /// The terminal size we reported to the server in our last Hello/Resize.
    reported_size: (u16, u16),
    /// The cell pixel size (width, height) we last reported alongside
    /// `reported_size`. Carried so an in-process server switch can re-assert the
    /// full geometry — cols/rows AND cell px — to the newly-active server (#77).
    reported_cell_size: (u32, u32),
    /// Client-local sound playback config, refreshed on server request.
    sound_config: crate::config::SoundConfig,
    /// Whether this client may write Kitty graphics bytes to its host terminal.
    kitty_graphics_enabled: bool,
    /// Direct attach prefix escape state. None for full-app clients.
    attach_escape: Option<AttachEscapeState>,
    /// Rows scrolled for one direct-attach wheel notch.
    mouse_scroll_lines: usize,
    /// Whether outer focus gain should force a full host-terminal redraw.
    redraw_on_focus_gained: bool,
}

#[derive(Debug, Default)]
struct AttachEscapeState {
    pending_prefix: bool,
}

#[derive(Debug)]
enum AttachInputAction {
    Forward(Vec<u8>),
    Scroll {
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    Detach,
    None,
}

impl AttachEscapeState {
    fn filter_input(
        &mut self,
        data: Vec<u8>,
        viewport_rows: u16,
        mouse_scroll_lines: usize,
    ) -> AttachInputAction {
        const PREFIX: u8 = 0x02; // Ctrl+B

        let mut output = Vec::with_capacity(data.len());
        for byte in data {
            if self.pending_prefix {
                self.pending_prefix = false;
                match byte {
                    b'q' => return AttachInputAction::Detach,
                    PREFIX => output.push(PREFIX),
                    other => {
                        output.push(PREFIX);
                        output.push(other);
                    }
                }
                continue;
            }

            if byte == PREFIX {
                self.pending_prefix = true;
            } else {
                output.push(byte);
            }
        }

        if output.is_empty() {
            AttachInputAction::None
        } else if let Some(action) =
            attach_scroll_action(&output, viewport_rows, mouse_scroll_lines)
        {
            action
        } else {
            AttachInputAction::Forward(output)
        }
    }
}

fn attach_scroll_action(
    data: &[u8],
    viewport_rows: u16,
    mouse_scroll_lines: usize,
) -> Option<AttachInputAction> {
    let mut events = crate::raw_input::parse_raw_input_bytes_sync(data);
    if events.len() != 1 {
        return None;
    }

    match events.pop()? {
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            let direction = match mouse.kind {
                MouseEventKind::ScrollUp => AttachScrollDirection::Up,
                MouseEventKind::ScrollDown => AttachScrollDirection::Down,
                _ => return Some(AttachInputAction::None),
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::Wheel,
                direction,
                lines: mouse_scroll_lines.max(1).min(u16::MAX as usize) as u16,
                column: Some(mouse.column),
                row: Some(mouse.row),
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            let direction = match key.code {
                KeyCode::PageUp => AttachScrollDirection::Up,
                KeyCode::PageDown => AttachScrollDirection::Down,
                _ => return None,
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::PageKey {
                    input: data.to_vec(),
                },
                direction,
                lines: viewport_rows.saturating_sub(1).max(1),
                column: None,
                row: None,
                modifiers: KeyModifiers::empty().bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && key.kind == KeyEventKind::Release
                && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) =>
        {
            Some(AttachInputAction::None)
        }
        _ => None,
    }
}

impl ClientState {
    fn request_full_redraw(&mut self) {
        self.blit_encoder = render_ansi::BlitEncoder::new();
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Path of the launcher's switch file. Set by the outermost herdr process;
/// a client that receives SwitchServer writes the SSH target here and exits,
/// and the launcher chains into `herdr --remote <target>`.
pub const SWITCH_FILE_ENV_VAR: &str = "HERDR_SWITCH_FILE";

/// Env var the launcher sets on the leg it re-attaches after a FAILED server
/// switch (#63). The client lifts it into the Hello `notice` so the server
/// renders `switch to <name> failed: <reason>` top-right — the user lands
/// back on the previous server, told why, never stranded at a shell. Cleared
/// (unset) once consumed so it shows on the first attach only.
pub const SWITCH_NOTICE_ENV_VAR: &str = "HERDR_SWITCH_NOTICE";

/// Env var the launcher sets on a leg it chains into AFTER a seamless switch
/// held the host terminal (#63/#69). The previous leg's process exited holding
/// the alt-screen + raw mode (a frozen frame) so the swap had no blip; this
/// next leg therefore INHERITS a held terminal even though its own
/// [`SWITCH_HANDOFF_PENDING`] flag starts clear (it is a different process for
/// remote legs, and explicitly cleared for local ones). The value tells this
/// leg to arm its held-terminal restore guard, so if it dies in the #52 retry
/// window — ctrl-c, signal, error, panic — before it repaints, it reclaims the
/// host terminal instead of leaving the user stranded behind the frozen frame.
pub const HELD_TERMINAL_ENV_VAR: &str = "HERDR_TERMINAL_HELD";

/// Take the launcher's one-shot attach notice, if set, clearing it so a later
/// in-leg handshake retry (#38 live-handoff) does not repeat it.
fn take_attach_notice() -> Option<String> {
    let notice = std::env::var(SWITCH_NOTICE_ENV_VAR)
        .ok()
        .filter(|n| !n.is_empty());
    if notice.is_some() {
        std::env::remove_var(SWITCH_NOTICE_ENV_VAR);
    }
    notice
}

/// Payload the client records in the launcher's switch file: the next attach
/// target plus the fleet snapshot the next leg carries into its handshake
/// (hub-and-spoke down-gossip). Same-binary launcher and client share the
/// format, so no cross-version concerns.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecordedSwitch {
    /// SSH destination of the next leg, or [`protocol::HOME_SWITCH_TARGET`]
    /// for "re-attach local".
    pub target: String,
    /// Fleet snapshot from the server the client is leaving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<protocol::FleetSnapshot>,
    /// Workspace id to focus once the next leg attaches (#66). Set only when
    /// going home from an origin-workspace row: the launcher fires
    /// `workspace focus` against the local server post-attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_workspace: Option<String>,
}

/// Set while this client is exiting to hand off to a pre-connected next leg
/// (a sidebar server switch). The terminal teardown reads it to KEEP the
/// alternate screen — the last rendered frame stays frozen on screen while
/// the launcher establishes the next leg, instead of flashing the host
/// shell ("blip to the terminal", #63). The next leg's `ratatui::init()`
/// re-enters the alt-screen (idempotent) and paints over the frozen frame.
static SWITCH_HANDOFF_PENDING: AtomicBool = AtomicBool::new(false);

/// True while the host terminal is INHERITED-held by a previous leg (#69): the
/// launcher chained this leg in after a seamless switch, so the OS terminal is
/// in alt-screen + raw mode showing a frozen frame, but no live guard in THIS
/// process owns it yet (the previous leg's process is gone). Set at leg start
/// from [`HELD_TERMINAL_ENV_VAR`]; cleared the moment any full terminal restore
/// runs ([`restore_terminal_state`]'s real-exit branch) or a handoff is
/// re-armed. While true, an abnormal exit must reclaim the terminal.
static INHERITED_TERMINAL_HOLD: AtomicBool = AtomicBool::new(false);

/// Whether the host terminal is currently held with nothing live to reclaim
/// it: either this leg set the switch-handoff hold, or it inherited one from a
/// previous leg. Used by [`HeldRestoreGuard`] to decide whether an abnormal
/// exit must force a restore.
fn host_terminal_is_held() -> bool {
    SWITCH_HANDOFF_PENDING.load(Ordering::Acquire)
        || INHERITED_TERMINAL_HOLD.load(Ordering::Acquire)
}

/// Record a server-switch target for the launcher. Returns false when no
/// launcher registered a switch file (nothing to chain into).
fn record_switch_target(
    ssh_target: &str,
    fleet: Option<&protocol::FleetSnapshot>,
    focus_workspace: Option<&str>,
) -> bool {
    let Ok(path) = std::env::var(SWITCH_FILE_ENV_VAR) else {
        return false;
    };
    if path.is_empty() {
        return false;
    }
    let payload = RecordedSwitch {
        target: ssh_target.to_string(),
        fleet: fleet.cloned(),
        focus_workspace: focus_workspace.map(str::to_string),
    };
    let Ok(json) = serde_json::to_string(&payload) else {
        return false;
    };
    std::fs::write(&path, json).is_ok()
}

/// Read and clear a recorded switch target, if any.
pub fn take_switch_target(path: &std::path::Path) -> Option<RecordedSwitch> {
    let contents = std::fs::read_to_string(path).ok()?;
    let _ = std::fs::remove_file(path);
    if let Ok(switch) = serde_json::from_str::<RecordedSwitch>(&contents) {
        return (!switch.target.is_empty()).then_some(switch);
    }
    // Bare-target fallback (defensive; the writer is always this binary).
    let target = contents.trim().to_string();
    (!target.is_empty()).then_some(RecordedSwitch {
        target,
        fleet: None,
        focus_workspace: None,
    })
}

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
                    Some("switching") => {
                        write!(f, "switching server")?;
                    }
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "detached from remote server")?;
                            write!(f, "\nRun `{reattach_command}` to reattach")?;
                        } else {
                            write!(f, "detached from server")?;
                            write!(
                                f,
                                "\nRun `{}` to reattach",
                                crate::session::local_attach_command()
                            )?;
                        }
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

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. It enables mouse capture
/// so wheel events can drive the attached viewport or be forwarded to child
/// programs that requested mouse input.
fn setup_direct_attach_terminal() -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, true)
}

fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    // try_init, not init: raw-mode/terminal IO can fail on the mid-switch
    // terminal (held fds, broken mux) and the panicking init() aborted via a
    // DOUBLE panic (its own hook's eprint also failed) -- #95. A failure here
    // rides the leg loop's normal error rail instead (relaunch + notice).
    ratatui::try_init().map_err(|e| io::Error::other(format!("terminal init failed: {e}")))?;

    if enable_client_protocols {
        if mouse_capture {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        execute!(
            io::stdout(),
            EnableBracketedPaste,
            EnableFocusChange,
            PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
        )?;
    } else if mouse_capture {
        execute!(io::stdout(), EnableMouseCapture)?;
    } else {
        execute!(io::stdout(), DisableMouseCapture)?;
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(|| {
            crate::input::host_modify_other_keys_mode(
                std::env::var("TMUX").is_ok(),
                std::env::var("TERM_PROGRAM").ok().as_deref(),
                std::env::var_os("WEZTERM_PANE").is_some(),
            )
        })
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    Ok(TerminalGuard {
        reset_modify_other_keys: modify_other_keys_mode.is_some(),
    })
}

/// Guard that restores the terminal when dropped.
struct TerminalGuard {
    reset_modify_other_keys: bool,
}

fn write_terminal_restore_postlude(writer: &mut impl io::Write) -> io::Result<()> {
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

fn restore_terminal_state(reset_modify_other_keys: bool) {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture
    );

    // Seamless server switch (#63): keep the alternate screen and raw mode so
    // the last frame stays frozen while the launcher brings up the next leg —
    // the host shell never flashes. `ratatui::restore()` would leave the
    // alt-screen and drop raw mode; skip it. The next leg's `ratatui::init()`
    // re-enters both and paints over the frozen frame. A real exit (detach,
    // error, quit) clears the flag and restores fully as before.
    if SWITCH_HANDOFF_PENDING.load(Ordering::Acquire) {
        let _ = io::stdout().flush();
        return;
    }

    // A real exit reclaims the terminal: this leg now owns the restore, so any
    // inherited hold from a previous leg (#69) is superseded and must not
    // trigger a second force-restore on the way out.
    INHERITED_TERMINAL_HOLD.store(false, Ordering::Release);
    ratatui::restore();
    let _ = write_terminal_restore_postlude(&mut io::stdout());
}

/// Guard covering the held-terminal window of one client leg (#69). The
/// alt-screen + raw mode may be HELD across this leg in two ways: this leg's
/// own SwitchServer sets [`SWITCH_HANDOFF_PENDING`], or a PREVIOUS leg's
/// subprocess exited holding the terminal and this leg inherited it before its
/// first paint. Either way, if the leg unwinds, errors, or is signalled
/// (ctrl-c / SIGTERM) before it hands off to a real next leg — including the
/// `std::process::exit` paths that skip [`TerminalGuard::drop`] — this guard
/// reclaims the host terminal so the user is never left at a frozen frame with
/// raw mode on. A clean handoff into the next leg disarms it via
/// [`HeldRestoreGuard::into_handoff`] so the hold survives exactly where it is
/// meant to: into the next leg's repaint.
struct HeldRestoreGuard {
    armed: bool,
}

impl HeldRestoreGuard {
    fn new() -> Self {
        Self { armed: true }
    }

    /// Disarm: this leg is exiting by handing the held terminal to the next
    /// leg (a recorded SwitchServer). The hold must survive process exit.
    fn into_handoff(mut self) {
        self.armed = false;
    }
}

impl Drop for HeldRestoreGuard {
    fn drop(&mut self) {
        // Restore only if the terminal is actually still held — by this leg's
        // own pending switch-handoff or an inherited hold from a previous leg
        // (#69). A leg that exited cleanly already restored (both flags clear),
        // so the common path is a no-op.
        if self.armed && host_terminal_is_held() {
            force_restore_host_terminal();
        }
    }
}

/// Best-effort full terminal restore for the launcher (#63) and for any
/// client exit that leaves the host terminal HELD (#69). A leg that switched
/// away held the alternate screen + raw mode so the swap was seamless; if the
/// chain ultimately dies with no leg left to reclaim the screen — the launcher
/// finishing, a held leg crashing/ctrl-c'ing before it repaints, a panic or
/// signal — the user must not be stranded in a frozen alt-screen with raw mode
/// on. Emits the full recovery sequence (pop kitty keyboard flags, reset
/// modifyOtherKeys, disable mouse/paste/focus, leave alt-screen, raw off,
/// cursor on) so a held terminal is always reclaimable. Idempotent and safe to
/// call when nothing was held.
pub fn force_restore_host_terminal() {
    SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);
    INHERITED_TERMINAL_HOLD.store(false, Ordering::Release);
    let _ = crossterm::terminal::disable_raw_mode();
    // Reset xterm modifyOtherKeys unconditionally: a held leg may have enabled
    // it (tmux/host-specific) and exited without resetting. Harmless on hosts
    // that never set it.
    let _ = io::stdout().write_all(b"\x1b[>4;0m");
    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        crossterm::terminal::LeaveAlternateScreen,
    );
    let _ = write_terminal_restore_postlude(&mut io::stdout());
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(self.reset_modify_other_keys);
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

fn requested_render_encoding() -> RenderEncoding {
    match std::env::var("HERDR_RENDER_ENCODING").ok().as_deref() {
        Some("terminal-ansi" | "terminal_ansi" | "ansi") => RenderEncoding::TerminalAnsi,
        _ => RenderEncoding::SemanticFrame,
    }
}

/// Fleet snapshot handed to this client process by the remote launcher
/// (hub-and-spoke down-gossip). Locally-attached clients have none.
fn carried_fleet_snapshot() -> Option<protocol::FleetSnapshot> {
    let raw = std::env::var(crate::remote::FLEET_SNAPSHOT_ENV_VAR).ok()?;
    match serde_json::from_str(&raw) {
        Ok(fleet) => Some(fleet),
        Err(err) => {
            warn!(err = %err, "ignoring malformed fleet snapshot from launcher");
            None
        }
    }
}

fn requested_keybindings() -> ClientKeybindings {
    match std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR)
        .ok()
        .as_deref()
    {
        Some("local") => crate::config::Config::load()
            .config
            .local_keybindings_profile_toml()
            .map(|keys_toml| ClientKeybindings::Local { keys_toml })
            .unwrap_or(ClientKeybindings::Server),
        _ => ClientKeybindings::Server,
    }
}

/// Performs the client→server handshake.
///
/// Sends Hello with the terminal size and protocol version, reads the Welcome
/// response. Returns Ok(()) on success, or an error if the server rejects us.
fn do_handshake(
    stream: &mut UnixStream,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    direct_attach_requested: bool,
    host_theme: Option<crate::terminal_theme::TerminalTheme>,
) -> Result<RenderEncoding, ClientError> {
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // Send Hello.
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        keybindings: requested_keybindings(),
        launch_mode: if direct_attach_requested {
            ClientLaunchMode::TerminalAttach
        } else {
            ClientLaunchMode::App
        },
        fleet: carried_fleet_snapshot(),
        host_theme,
        notice: take_attach_notice(),
    };
    protocol::write_message(stream, &hello)
        .map_err(|e| ClientError::ConnectionFailed(io::Error::other(e.to_string())))?;

    // Read Welcome.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(ClientError::ConnectionFailed)?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    // Clearing the read timeout can fail with EINVAL on a peer that already
    // half-closed the socket right after the Welcome (a dying mid-handoff
    // server). The Welcome is already in hand, so a failure here must not mask
    // a live-handoff refusal as a bare connection error — ignore it and let the
    // refusal classification below drive the retry (#69).
    let _ = stream.set_read_timeout(None);

    match welcome {
        ServerMessage::Welcome {
            version,
            encoding,
            error,
        } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, ?encoding, "handshake succeeded");
            Ok(encoding)
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
    Resize(u16, u16, u32, u32),
    /// Server message received. Carries the slot key of the reader that read
    /// it (#65) so the loop can drop messages from a non-active slot and only
    /// paint/act on the active one.
    ServerMessage(String, ServerMessage),
    /// Server reader thread exited (connection lost). Carries the slot key (#65)
    /// so a warm slot's death demotes that slot silently while only the active
    /// slot's death tears the session down.
    ServerDisconnected(String),
    /// A background warm-all dial OR an on-demand switch dial succeeded: this
    /// slot now holds a paused connection. Carries the slot key, the writable
    /// stream half, an optional `SshStdioBridge` that the slot's connection
    /// must own (so Drop tears down the ssh transport with the slot), and the
    /// generation counter the dial was spawned with (#93). At apply time the
    /// loop must compare `gen` against the live pending-switch or warm-sweep
    /// gen for `key` — a stale event drops its stream so the server sees the
    /// disconnect and never flips the active slot.
    SlotWarmed {
        gen: u64,
        key: String,
        stream: UnixStream,
        bridge: Option<crate::remote::SshStdioBridge>,
    },
    /// A background warm-all dial OR an on-demand switch dial failed: keep the
    /// slot cold with backoff. The generation counter mirrors `SlotWarmed`;
    /// stale failures are dropped so a re-armed switch's UI is never disturbed
    /// by an earlier attempt's outcome (#93).
    SlotDialFailed { gen: u64, key: String, err: String },
    /// Timer tick.
    Timer,
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(
        requested_render_encoding(),
        None,
        None,
        "connecting to server",
    )
}

/// Runs a direct terminal attach client.
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        RenderEncoding::TerminalAnsi,
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

// ---------------------------------------------------------------------------
// Host theme capture (pre-handshake)
// ---------------------------------------------------------------------------

/// How long to wait for the host terminal's OSC 10/11 color replies before
/// the handshake. Terminals that support the query answer within a few
/// milliseconds; one that never answers costs this once per attach leg.
const HOST_THEME_CAPTURE_TIMEOUT: Duration = Duration::from_millis(300);

/// Poll granularity while waiting for color replies.
const HOST_THEME_CAPTURE_POLL_MS: i32 = 25;

/// Captures the host terminal's default colors (OSC 10/11) before the
/// handshake so they ride the `Hello` and a remote/spoke server can adopt
/// them at attach time (#47). This runs on every attach leg, so a
/// SwitchServer relaunch re-captures from the same host terminal without any
/// launcher plumbing.
///
/// Returns the captured theme plus every raw byte read while waiting —
/// normally just the color replies, but any early keystrokes are preserved
/// and forwarded to the server as the session's first input.
fn capture_host_terminal_theme() -> (Option<crate::terminal_theme::TerminalTheme>, Vec<u8>) {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return (None, Vec::new());
    }
    // Raw mode so the replies are readable immediately instead of sitting in
    // the line discipline; restored before the handshake either way.
    if crossterm::terminal::enable_raw_mode().is_err() {
        return (None, Vec::new());
    }
    let captured = read_host_theme_replies();
    let _ = crossterm::terminal::disable_raw_mode();
    captured
}

fn read_host_theme_replies() -> (Option<crate::terminal_theme::TerminalTheme>, Vec<u8>) {
    use std::io::Read;

    if write_host_terminal_theme_query(io::stdout()).is_err() {
        return (None, Vec::new());
    }

    let mut buf = Vec::new();
    let mut theme = crate::terminal_theme::TerminalTheme::default();
    let deadline = Instant::now() + HOST_THEME_CAPTURE_TIMEOUT;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    while Instant::now() < deadline {
        match input::stdin_read_ready(&reader, HOST_THEME_CAPTURE_POLL_MS) {
            Some(true) => {
                let mut scratch = [0u8; 1024];
                match reader.read(&mut scratch) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&scratch[..n]);
                        theme = theme_from_capture_buffer(&buf);
                        if theme.foreground.is_some() && theme.background.is_some() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            Some(false) => {}
            None => break,
        }
    }

    if theme.is_empty() {
        debug!("host terminal did not answer the default color query");
    } else {
        info!(?theme, "captured host terminal theme for handshake");
    }
    ((!theme.is_empty()).then_some(theme), buf)
}

fn theme_from_capture_buffer(buf: &[u8]) -> crate::terminal_theme::TerminalTheme {
    let mut theme = crate::terminal_theme::TerminalTheme::default();
    for event in crate::raw_input::parse_raw_input_bytes_sync(buf) {
        if let crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } = event {
            theme = theme.with_color(kind, color);
        }
    }
    theme
}

// ---------------------------------------------------------------------------
// Live-handoff attach retry (#38)
// ---------------------------------------------------------------------------

/// Pause between attach attempts while a live handoff is in progress.
const HANDOFF_RETRY_INTERVAL: Duration = Duration::from_millis(200);

/// Total budget for waiting out a live handoff before surfacing the error.
const HANDOFF_RETRY_WINDOW: Duration = Duration::from_secs(30);

/// A session that ran at least this long counts as a real attach: the next
/// live-handoff refusal opens a fresh retry window instead of draining the
/// previous one. Shorter sessions (e.g. refused right at ClientConnected)
/// keep consuming the window so a flapping server cannot retry forever.
const HANDOFF_SESSION_RESET_THRESHOLD: Duration = Duration::from_secs(5);

const HANDOFF_SPINNER: [char; 4] = ['|', '/', '-', '\\'];

/// One attach attempt's failure, tagged by the phase it failed in.
enum AttachAttemptError {
    /// Connecting or handshaking failed; nothing was drawn yet.
    Handshake(ClientError),
    /// The terminal could not be prepared. Never retried.
    TerminalSetup(io::Error),
    /// The established session ended with an error.
    Session(ClientError),
}

impl AttachAttemptError {
    fn is_live_handoff_refusal(&self) -> bool {
        match self {
            AttachAttemptError::Handshake(ClientError::HandshakeRejected { error, .. }) => {
                error == protocol::LIVE_HANDOFF_ATTACH_NOTICE
            }
            AttachAttemptError::Handshake(ClientError::ServerShutdown {
                reason: Some(reason),
            })
            | AttachAttemptError::Session(ClientError::ServerShutdown {
                reason: Some(reason),
            }) => reason == protocol::LIVE_HANDOFF_ATTACH_NOTICE,
            _ => false,
        }
    }

    fn into_client_error(self) -> Result<ClientError, io::Error> {
        match self {
            AttachAttemptError::Handshake(err) | AttachAttemptError::Session(err) => Ok(err),
            AttachAttemptError::TerminalSetup(err) => Err(err),
        }
    }
}

/// Retry state for attaches refused during a live update handoff (#38).
#[derive(Default)]
struct HandoffRetry {
    deadline: Option<Instant>,
    status_line_shown: bool,
    spinner_frame: usize,
    /// When the first refusal opened the window — drives the elapsed-seconds
    /// counter shown to the user so a reconnect reads as live progress, not a
    /// frozen hang (#69).
    started: Option<Instant>,
    /// Whether the status was painted directly onto a held alt-screen (a
    /// frozen frame from a previous leg). Governs how it is cleared.
    painted_held: bool,
}

impl HandoffRetry {
    /// Returns true when the failed attach attempt should be retried after a
    /// short pause. The first live-handoff refusal opens a ~30s window;
    /// within it, transient connect/handshake failures (the old server
    /// dying, the new one binding the socket) are retried too. A session
    /// that ran long enough to be a real attach resets the window, so a
    /// later handoff gets a fresh budget.
    fn should_retry(&mut self, err: &AttachAttemptError, attempt_duration: Duration) -> bool {
        let now = Instant::now();
        if matches!(err, AttachAttemptError::Session(_))
            && attempt_duration >= HANDOFF_SESSION_RESET_THRESHOLD
        {
            self.deadline = None;
        }
        if err.is_live_handoff_refusal() && self.deadline.is_none() {
            self.deadline = Some(now + HANDOFF_RETRY_WINDOW);
            self.started = Some(now);
            return true;
        }
        let Some(deadline) = self.deadline else {
            return false;
        };
        if now >= deadline {
            return false;
        }
        match err {
            AttachAttemptError::Session(_) => err.is_live_handoff_refusal(),
            AttachAttemptError::TerminalSetup(_) => false,
            // A newer server rejected us: the handoff completed onto a
            // protocol this client cannot speak. Retrying cannot succeed —
            // surface the upgrade guidance immediately. Everything else
            // (refused or dropped connections, EOFs from the dying server)
            // is expected churn inside the window.
            AttachAttemptError::Handshake(ClientError::HandshakeRejected { version, .. }) => {
                *version <= PROTOCOL_VERSION
            }
            AttachAttemptError::Handshake(_) => true,
        }
    }

    fn pause_before_retry(&mut self) {
        self.show_status_line();
        std::thread::sleep(HANDOFF_RETRY_INTERVAL);
    }

    /// The reconnect message shown to the user, with a live elapsed-seconds
    /// counter so the wait reads as progress rather than a frozen hang (#69).
    fn status_text(&self) -> String {
        let secs = self.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
        format!("herdr: handoff in progress, reconnecting… ({secs}s)")
    }

    /// Paint the reconnect status while waiting. When a previous leg left the
    /// host terminal HELD (a frozen frame in the alt-screen, #63/#69), the bare
    /// `\r` stderr line lands at an unknown cursor position behind the frame
    /// and is effectively invisible — the user sees a dead client. So overlay a
    /// styled status bar on the bottom row of the held screen with absolute
    /// positioning, saving/restoring the cursor around it. Otherwise (a plain
    /// in-leg reconnect on the host shell) keep the in-place stderr line.
    fn show_status_line(&mut self) {
        use std::io::IsTerminal;
        let frame = HANDOFF_SPINNER[self.spinner_frame % HANDOFF_SPINNER.len()];
        self.spinner_frame = self.spinner_frame.wrapping_add(1);
        let text = self.status_text();

        if host_terminal_is_held() {
            // Bottom row, reverse-video bar, cursor parked then restored so the
            // overlay never disturbs the frozen frame underneath.
            let mut out = io::stdout();
            let _ = write!(
                out,
                "\x1b7\x1b[9999;1H\x1b[K\x1b[7m {frame} {text} \x1b[0m\x1b8"
            );
            let _ = out.flush();
            self.painted_held = true;
            self.status_line_shown = true;
            return;
        }

        if io::stderr().is_terminal() {
            eprint!("\r\x1b[K{frame} {text}");
            let _ = io::stderr().flush();
        } else if !self.status_line_shown {
            eprintln!("{text}");
        }
        self.status_line_shown = true;
    }

    fn clear_status_line(&mut self) {
        use std::io::IsTerminal;
        if self.painted_held {
            // Erase the bottom-row overlay we painted on the held screen.
            let mut out = io::stdout();
            let _ = write!(out, "\x1b7\x1b[9999;1H\x1b[K\x1b8");
            let _ = out.flush();
            self.painted_held = false;
        } else if self.status_line_shown && io::stderr().is_terminal() {
            eprint!("\r\x1b[K");
            let _ = io::stderr().flush();
        }
        self.status_line_shown = false;
    }
}

/// Runs one complete attach attempt: connect, handshake, terminal setup, and
/// the client session loop. Phase-tagged errors let the caller decide what is
/// retryable during a live handoff.
#[allow(clippy::too_many_arguments)]
fn run_attach_attempt(
    socket_path: &Path,
    requested_encoding: RenderEncoding,
    attach_request: Option<&(String, bool)>,
    direct_attach: bool,
    kitty_graphics_enabled: bool,
    host_theme: Option<crate::terminal_theme::TerminalTheme>,
    pending_stdin: &mut Option<Vec<u8>>,
    stdin_rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
    rt: &tokio::runtime::Runtime,
    should_quit: &Arc<AtomicBool>,
    sound_config: &crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
) -> Result<(), AttachAttemptError> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| AttachAttemptError::Handshake(ClientError::ConnectionFailed(err)))?;

    // Read the terminal geometry before the handshake, while still outside
    // raw mode. Re-read on every attempt so a resize that happens during a
    // handoff window is honored.
    let (cols, rows, cell_width_px, cell_height_px) =
        current_terminal_geometry(kitty_graphics_enabled);

    // Perform handshake while the stream is still in blocking mode.
    let negotiated_encoding = do_handshake(
        &mut stream,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        attach_request.is_some(),
        host_theme,
    )
    .map_err(AttachAttemptError::Handshake)?;

    if let Some((terminal_id, takeover)) = attach_request {
        let attach = ClientMessage::AttachTerminal {
            terminal_id: terminal_id.clone(),
            takeover: *takeover,
        };
        write_to_server(&mut stream, &attach)
            .map_err(|err| AttachAttemptError::Handshake(ClientError::ConnectionLost(err)))?;
    }

    // Now set up the terminal. This must happen AFTER the handshake succeeds,
    // so we don't leave the terminal in raw mode if the server rejects us.
    let _guard = if direct_attach {
        setup_direct_attach_terminal()
    } else {
        setup_terminal(false)
    }
    .map_err(AttachAttemptError::TerminalSetup)?;

    let attach_escape = direct_attach.then(AttachEscapeState::default);
    let initial_input = pending_stdin.take();
    let result = rt.block_on(run_client_loop(
        stream,
        cols,
        rows,
        should_quit.clone(),
        sound_config.clone(),
        mouse_scroll_lines,
        redraw_on_focus_gained,
        kitty_graphics_enabled,
        false,
        negotiated_encoding,
        attach_escape,
        initial_input,
        stdin_rx,
    ));

    // Restore the terminal before the caller prints anything.
    drop(_guard);

    result.map_err(AttachAttemptError::Session)
}

fn run_client_with_mode(
    requested_encoding: RenderEncoding,
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    log_message: &'static str,
) -> io::Result<()> {
    init_logging();

    // Each leg starts with no pending handoff: clear the flag a previous
    // in-process leg may have left set (#63) so a clean exit of THIS leg
    // restores the host terminal fully.
    SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);

    // If the launcher chained this leg in after a seamless switch, the host
    // terminal is INHERITED-held (frozen frame, raw mode) until this leg
    // repaints (#69). Record it so an abnormal exit in the retry window
    // reclaims the terminal instead of stranding the user behind the frame.
    if std::env::var(HELD_TERMINAL_ENV_VAR).is_ok() {
        INHERITED_TERMINAL_HOLD.store(true, Ordering::Release);
        // One-shot: only this immediate next leg inherits the hold.
        std::env::remove_var(HELD_TERMINAL_ENV_VAR);
    }
    // Reclaim the host terminal on ANY abnormal exit of this leg while it is
    // still held — ctrl-c/signal, error return, or the `std::process::exit`
    // paths below that skip TerminalGuard::drop (#69). Disarmed only when this
    // leg hands the held terminal off to a real next leg.
    let held_restore = HeldRestoreGuard::new();

    let loaded_config = crate::config::Config::load();
    let mouse_scroll_lines = loaded_config.config.ui.mouse_scroll_lines();
    let redraw_on_focus_gained = loaded_config.config.ui.redraw_on_focus_gained;
    let sound_config = loaded_config.config.ui.sound;
    let direct_attach_requested = attach_request.is_some();
    let kitty_graphics_enabled =
        loaded_config.config.experimental.kitty_graphics && !direct_attach_requested;
    let direct_attach = attach_escape.is_some();

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    // Capture the host terminal theme before the handshake so it rides the
    // Hello (#47). Direct terminal attaches mirror a single pane and never
    // report a theme (unchanged behavior).
    let (host_theme, capture_leftover) = if direct_attach_requested {
        (None, Vec::new())
    } else {
        capture_host_terminal_theme()
    };
    let mut pending_stdin = (!capture_leftover.is_empty()).then_some(capture_leftover);

    // Spawn the stdin reader thread once, after the theme capture released
    // stdin. It outlives individual attach attempts so a handoff retry never
    // leaves typed bytes stranded in a session-scoped reader.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let should_quit = Arc::new(AtomicBool::new(false));
    let stdin_quit = should_quit.clone();
    std::thread::spawn(move || {
        input::stdin_reader_loop(stdin_tx, &stdin_quit);
    });

    // Install a panic hook to restore the terminal on panic. A panic is always
    // a real exit, so it must NEVER honor the switch-handoff hold — force a
    // full restore (leave alt-screen, raw off, pop kitty flags) so a panic
    // mid-held-handoff cannot strand the shell (#69).
    let in_tmux = std::env::var("TMUX").is_ok();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if host_terminal_is_held() {
            force_restore_host_terminal();
        } else {
            restore_terminal_state(in_tmux);
        }
        // Best-effort diagnostic first: if the chained hook dies (ratatui's
        // hook eprints, and a dead stderr made that a double-panic abort with
        // NO message -- #95), the user still gets this line when stderr
        // works, and a hook panic is contained instead of aborting.
        let _ = writeln!(io::stderr(), "herdr client panic: {info}");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| original_hook(info)));
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    // Install a termination handler (SIGINT/SIGTERM/SIGHUP via ctrlc's
    // `termination` feature). It runs on ctrlc's own thread — signal-safe — and
    // only flips `should_quit`; the attach/retry loop observes it and exits
    // through the held-restore path so a signal mid-held-handoff cannot strand
    // the shell (#69).
    let quit_flag = should_quit.clone();
    let _ = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    });

    // Attach, retrying while the server refuses with the live-handoff notice
    // (#38): ~200ms pauses for up to ~30s behind a single status line, then
    // the original error.
    let mut handoff_retry = HandoffRetry::default();
    let result = loop {
        let attempt_started = Instant::now();
        match run_attach_attempt(
            &socket_path,
            requested_encoding,
            attach_request.as_ref(),
            direct_attach,
            kitty_graphics_enabled,
            host_theme,
            &mut pending_stdin,
            &mut stdin_rx,
            &rt,
            &should_quit,
            &sound_config,
            mouse_scroll_lines,
            redraw_on_focus_gained,
        ) {
            Ok(()) => break Ok(()),
            Err(err) => {
                if should_quit.load(Ordering::Acquire)
                    || !handoff_retry.should_retry(&err, attempt_started.elapsed())
                {
                    break Err(err);
                }
                handoff_retry.pause_before_retry();
            }
        }
    };
    handoff_retry.clear_status_line();

    if let Err(attempt_err) = result {
        let err = match attempt_err.into_client_error() {
            Ok(client_err) => client_err,
            Err(setup_err) => {
                // Terminal setup failed: nothing of ours owns the screen, but a
                // previous leg may still be holding it. Reclaim before we leave
                // (the guard would also catch this return, but be explicit).
                if host_terminal_is_held() {
                    force_restore_host_terminal();
                }
                held_restore.into_handoff();
                eprintln!("herdr: failed to set up terminal: {setup_err}");
                rt.shutdown_timeout(Duration::from_millis(100));
                crate::logging::shutdown("client");
                return Err(setup_err);
            }
        };

        // A clean switch hands the held terminal to the next leg: keep the
        // hold (disarm the restore) so the swap stays blip-free (#63). Any
        // other exit must reclaim the terminal if it is still held (#69).
        let switching = matches!(
            &err,
            ClientError::ServerShutdown { reason: Some(reason) } if reason == "switching"
        );
        if switching {
            held_restore.into_handoff();
        } else if host_terminal_is_held() {
            // ctrl-c in the retry window, a dropped/refused connection, or any
            // error while a previous leg's frozen frame is still up: the
            // process::exit below skips Drop, so reclaim the terminal now.
            force_restore_host_terminal();
            held_restore.into_handoff();
        }

        eprintln!("herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        crate::logging::shutdown("client");

        if matches!(
            err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached" || reason == "switching"
        ) {
            return Ok(());
        }

        std::process::exit(1);
    }

    // Clean leg exit: the terminal is already fully restored. Disarm so the
    // guard does not second-guess it.
    held_restore.into_handoff();
    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    Ok(())
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread (owned by the caller, survives attach retries)
///   → sends raw input chunks to the main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
#[allow(clippy::too_many_arguments)]
async fn run_client_loop(
    stream: UnixStream,
    cols: u16,
    rows: u16,
    should_quit: Arc<AtomicBool>,
    sound_config: crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
    kitty_graphics_enabled: bool,
    mouse_capture_active: bool,
    negotiated_encoding: RenderEncoding,
    attach_escape: Option<AttachEscapeState>,
    initial_input: Option<Vec<u8>>,
    stdin_rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> Result<(), ClientError> {
    let (_, _, initial_cell_width, initial_cell_height) =
        current_terminal_geometry(kitty_graphics_enabled);
    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active,
        reported_size: (cols, rows),
        reported_cell_size: (initial_cell_width, initial_cell_height),
        sound_config,
        kitty_graphics_enabled,
        attach_escape,
        mouse_scroll_lines,
        redraw_on_focus_gained,
    };
    debug!(?negotiated_encoding, "client render encoding active");

    // Channel for events from the resize and server reader threads. The
    // stdin reader outlives this session (handoff retries reuse it), so it
    // has its own channel passed in by the caller.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);

    if state.attach_escape.is_none() {
        query_host_terminal_theme();
    }

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    std::thread::spawn(move || {
        resize_poll_loop(resize_tx, cols, rows, kitty_graphics_enabled, &resize_quit);
    });

    let max_frame_size = if kitty_graphics_enabled {
        MAX_GRAPHICS_FRAME_SIZE
    } else {
        MAX_FRAME_SIZE
    };

    // Spawn the active-slot reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    // The reader carries its OWN quit flag (in addition to `should_quit`) so a
    // connection-slots flip can retire just this reader and bind a new one to
    // the slot that became active, without tearing the whole loop down (#65).
    let mut active_reader_quit = Arc::new(AtomicBool::new(false));
    // The active slot's key. Reader threads tag every event with their slot
    // key so the loop can tell a frame/disconnect from the active slot apart
    // from one from a warm (background) slot (#65). When slots are disabled
    // this is just the home sentinel and every event matches.
    let mut active_slot_key = active_slot_key();
    spawn_slot_reader(
        active_slot_key.clone(),
        stream.try_clone().map_err(ClientError::ConnectionFailed)?,
        event_tx.clone(),
        should_quit.clone(),
        active_reader_quit.clone(),
        max_frame_size,
    );

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut write_stream = stream;
    write_stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // Connection slots (#65): when enabled, build the slot manager over the
    // active connection and background-dial the warm-all fleet. The active
    // slot's write stream is `write_stream`; a warm switch flips it in process
    // without releasing the terminal. When disabled, `slot_manager` is None and
    // the legacy exit-and-relaunch leg path drives every switch.
    let mut slot_manager = build_slot_manager(&write_stream, max_frame_size);

    // Bytes consumed from stdin during the pre-handshake theme capture:
    // forward them as the session's first input so no keystroke is lost. The
    // server parses any color replies in here exactly like live ones.
    if let Some(data) = initial_input {
        let msg = ClientMessage::Input { data };
        if let Err(e) = write_to_server(&mut write_stream, &msg) {
            return Err(ClientError::ConnectionLost(e));
        }
    }

    // Connection-slots warm-all dialing bookkeeping (#65/#93): keys of slots a
    // background dial is currently in flight for, mapped to the generation the
    // dial was spawned with. The dialer fires on the timer tick; events use
    // the gen to drop stale outcomes (cancel/re-switch races, #93).
    let mut slot_dials_in_flight: HashMap<String, u64> = HashMap::new();
    let mut last_slot_dial_sweep = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .unwrap_or_else(Instant::now);
    // Monotonic generation source for every dial event (warm sweep + cold
    // switch) so stale outcomes drop their streams instead of flipping the
    // active slot (#93). Cancel/re-switch bumps it.
    let mut next_dial_gen: u64 = 1;
    // Live cancellable switch state (#93). `Some` while a switch is in flight
    // OR briefly during a success/cancel/failure beat; `None` otherwise. The
    // popup paints only while `Some`, and Esc is only intercepted while `Some`.
    let mut pending_switch: Option<PendingSwitch> = None;
    // A lone 0x1b chunk held for ESC_DEBOUNCE before deciding cancel vs the
    // first byte of a split key sequence.
    let mut pending_esc: Option<(Instant, Vec<u8>)> = None;
    // Esc-grace: after a successful switch, swallow a lone Esc chunk arriving
    // within this window so muscle-memory `Esc to cancel` from the popup does
    // not land as a key inside the new session.
    let mut esc_grace_until: Option<Instant> = None;
    // Last popup paint timestamp; throttle to POPUP_REPAINT_INTERVAL so the
    // Timer-driven repaint never thrashes the host terminal.
    let mut last_popup_paint: Option<Instant> = None;

    // Main event loop.
    let mut stdin_closed = false;
    while !should_quit.load(Ordering::Acquire) {
        let event = tokio::select! {
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            data = stdin_rx.recv(), if !stdin_closed => match data {
                Some(data) => ClientLoopEvent::StdinInput(data),
                None => {
                    // Stdin hit EOF and the reader thread exited; keep
                    // serving frames and other events.
                    stdin_closed = true;
                    ClientLoopEvent::Timer
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(100)) => ClientLoopEvent::Timer,
        };

        match event {
            ClientLoopEvent::StdinInput(data) => {
                // Cancellable switch popup (#93): Esc is intercepted ONLY while
                // a switch is pending. A chunk equal to exactly `[0x1b]` is the
                // bare-Esc keypress; longer sequences starting with `0x1b` are
                // arrow/function/CSI keys and pass through untouched. Esc-grace
                // after a successful switch likewise swallows a lone Esc that
                // arrived too late to mean cancel.
                // A held bare Esc (debounce below) joins the next chunk: a
                // TTY under load can deliver Esc and `[A` in separate reads,
                // and treating that lone first byte as cancel would
                // phantom-cancel during arrow spam. If more bytes follow
                // within the debounce window the joined chunk is a key
                // sequence and passes through whole.
                let data: Vec<u8> = if let Some((_, held)) = pending_esc.take() {
                    let mut joined = held;
                    joined.extend_from_slice(&data);
                    joined
                } else {
                    data
                };
                if is_bare_esc_chunk(&data)
                    && (pending_switch.is_some()
                        || esc_grace_until.is_some_and(|u| Instant::now() < u))
                {
                    // Hold the lone Esc briefly: a real Esc cancels when the
                    // debounce deadline passes on the Timer arm; a sequence
                    // start gets joined to its continuation instead.
                    pending_esc = Some((Instant::now() + ESC_DEBOUNCE, data));
                    continue;
                }
                let data = if let Some(attach_escape) = &mut state.attach_escape {
                    match attach_escape.filter_input(
                        data,
                        state.reported_size.1,
                        state.mouse_scroll_lines,
                    ) {
                        AttachInputAction::Forward(data) => data,
                        AttachInputAction::Scroll {
                            source,
                            direction,
                            lines,
                            column,
                            row,
                            modifiers,
                        } => {
                            let msg = ClientMessage::AttachScroll {
                                source,
                                direction,
                                lines,
                                column,
                                row,
                                modifiers,
                            };
                            if let Err(e) = write_to_server(&mut write_stream, &msg) {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            continue;
                        }
                        AttachInputAction::Detach => {
                            let _ = write_to_server(&mut write_stream, &ClientMessage::Detach);
                            return Ok(());
                        }
                        AttachInputAction::None => continue,
                    }
                } else {
                    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                    if crate::raw_input::events_require_host_surface_redraw(
                        &events,
                        state.redraw_on_focus_gained,
                    ) {
                        state.request_full_redraw();
                    }
                    data
                };
                if should_bridge_clipboard_image_paste(&data) {
                    if let Some(image) = crate::platform::read_clipboard_image() {
                        if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                            warn!(
                                bytes = image.bytes.len(),
                                max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                                "local clipboard image is too large to bridge"
                            );
                            continue;
                        }
                        info!(
                            bytes = image.bytes.len(),
                            extension = image.extension,
                            "bridging local clipboard image paste to remote server"
                        );
                        let msg = ClientMessage::ClipboardImage {
                            extension: image.extension.to_owned(),
                            data: image.bytes,
                        };
                        if let Err(e) = write_to_server(&mut write_stream, &msg) {
                            return Err(ClientError::ConnectionLost(e));
                        }
                        continue;
                    }
                    info!(
                        "clipboard image paste trigger received, but local clipboard has no image"
                    );
                }
                let msg = ClientMessage::Input { data };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::Resize(new_cols, new_rows, cell_width_px, cell_height_px) => {
                state.reported_size = (new_cols, new_rows);
                state.reported_cell_size = (cell_width_px, cell_height_px);
                let msg = ClientMessage::Resize {
                    cols: new_cols,
                    rows: new_rows,
                    cell_width_px,
                    cell_height_px,
                };
                if let Err(e) = write_to_server(&mut write_stream, &msg) {
                    return Err(ClientError::ConnectionLost(e));
                }
            }
            ClientLoopEvent::ServerMessage(slot_key, msg) => {
                // Apply-time slot check (#65): a frame or message from a slot
                // that is no longer the active one (a queued frame from the old
                // reader after a flip, or a warm slot that never paused in time)
                // must not paint over or act on the active session. Drop it. The
                // sole exceptions are lifecycle signals from a NON-active slot,
                // which demote that slot silently instead of being dropped.
                let is_shutdown = matches!(msg, ServerMessage::ServerShutdown { .. });
                match slots::route_slot_event(&slot_key, &active_slot_key, is_shutdown) {
                    slots::SlotRouting::Apply => {}
                    slots::SlotRouting::DemoteDead => {
                        // A warm slot's server is going away: demote it to cold
                        // silently (the #65 ghost). The active session is
                        // untouched; a later switch re-dials it.
                        if let Some(manager) = slot_manager.as_mut() {
                            manager.handle_dead(&slots::SlotTarget::from_key(&slot_key));
                        }
                        debug!(slot = %slot_key, "warm slot server shut down; demoted silently");
                        continue;
                    }
                    slots::SlotRouting::Drop => {
                        // Stale frame or other non-active-slot traffic: drop.
                        debug!(slot = %slot_key, "dropping message from non-active slot");
                        continue;
                    }
                }
                match msg {
                    ServerMessage::Frame(frame_data) => {
                        let encoded = state.blit_encoder.encode(&frame_data, false);
                        let mut stdout = io::stdout();
                        let graphics = if state.kitty_graphics_enabled {
                            frame_data.graphics.as_slice()
                        } else {
                            &[]
                        };
                        let _ = write_encoded_frame_with_graphics(
                            &mut stdout,
                            &encoded.bytes,
                            graphics,
                        );
                        let _ = stdout.flush();
                        state.blit_encoder.commit(frame_data, encoded);
                    }
                    ServerMessage::Terminal(frame) => {
                        if state.kitty_graphics_enabled
                            && contains_kitty_graphics_bytes(&frame.bytes)
                        {
                            record_received_kitty_graphics(&frame.bytes);
                        }
                        let mut stdout = io::stdout();
                        let _ = stdout.write_all(&frame.bytes);
                        let _ = stdout.flush();
                    }
                    ServerMessage::Graphics { bytes } => {
                        if state.kitty_graphics_enabled {
                            record_received_kitty_graphics(&bytes);
                            let mut stdout = io::stdout();
                            let _ = stdout.write_all(&bytes);
                            let _ = stdout.flush();
                        }
                    }
                    ServerMessage::ServerShutdown { reason } => {
                        return Err(ClientError::ServerShutdown { reason });
                    }
                    ServerMessage::SwitchServer {
                        ssh_target,
                        fleet,
                        focus_workspace,
                    } => {
                        // Slots-enabled path (#93): try a WARM flip first, else
                        // arm a cancellable cold dial under the popup. The legacy
                        // exit-and-relaunch path below stays in place for the
                        // slots-DISABLED branch only — every touchpoint guards
                        // on `slot_manager` being `Some` (the #76 discipline).
                        //
                        // `focus_workspace`: today the slots path has no
                        // client→server FocusWorkspace message, so we drop it
                        // here with a note. The legacy legs path keeps focus
                        // support; the gap closes when the protocol grows a
                        // focus-after-attach message (tracked alongside #75).
                        if let Some(manager) = slot_manager.as_mut() {
                            let _ = &focus_workspace;
                            let _ = &fleet;
                            let target = slots::SlotTarget::from_key(&ssh_target);
                            match manager.flip_to(&target) {
                                Ok(Some(new_stream)) => {
                                    // Warm flip: apply in-process.
                                    apply_slot_flip(
                                        new_stream,
                                        &target,
                                        &mut active_reader_quit,
                                        &mut active_slot_key,
                                        &mut write_stream,
                                        &mut state,
                                        &event_tx,
                                        &should_quit,
                                        max_frame_size,
                                    )?;
                                    // PopupGuard invariant: if a switch was in
                                    // flight when the server preempted us with
                                    // a different warm flip, clear the popup.
                                    if pending_switch.is_some() {
                                        pending_switch = None;
                                        clear_switch_popup(&mut state);
                                    }
                                    continue;
                                }
                                Ok(None) => {
                                    // Cold/unknown OR already-active. Already-active
                                    // is a no-op (registry returned AlreadyActive).
                                    if manager.registry.is_active(&target) {
                                        continue;
                                    }
                                    // Arm a cancellable cold dial under the popup.
                                    // Cancel/re-switch bumps the gen so any
                                    // earlier dial's outcome drops at apply time.
                                    let gen = next_dial_gen;
                                    next_dial_gen = next_dial_gen.wrapping_add(1);
                                    let prev_display = slot_display_label(
                                        &slots::SlotTarget::from_key(&active_slot_key),
                                    );
                                    let target_display = slot_display_label(&target);
                                    pending_switch = Some(PendingSwitch {
                                        gen,
                                        target: target.clone(),
                                        previous_display: prev_display,
                                        target_display,
                                        started_at: Instant::now(),
                                        outcome_beat: None,
                                    });
                                    let geometry = (
                                        state.reported_size.0,
                                        state.reported_size.1,
                                        state.reported_cell_size.0,
                                        state.reported_cell_size.1,
                                    );
                                    spawn_switch_dial(
                                        gen,
                                        target.clone(),
                                        geometry,
                                        negotiated_encoding,
                                        event_tx.clone(),
                                    );
                                    if let Some(p) = pending_switch.as_ref() {
                                        paint_switch_popup(p, state.reported_size, Instant::now());
                                    }
                                    continue;
                                }
                                Err(err) => {
                                    warn!(err = %err, target = %ssh_target, "slot flip failed; demoting");
                                    manager.handle_dead(&target);
                                    // Surface in the popup as a switch failure
                                    // beat; the user sees the reason and the
                                    // active slot is unchanged.
                                    let prev_display = slot_display_label(
                                        &slots::SlotTarget::from_key(&active_slot_key),
                                    );
                                    let target_display = slot_display_label(&target);
                                    let beat_until = Instant::now() + POPUP_FAILURE_BEAT;
                                    pending_switch = Some(PendingSwitch {
                                        gen: next_dial_gen,
                                        target: target.clone(),
                                        previous_display: prev_display,
                                        target_display: target_display.clone(),
                                        started_at: Instant::now(),
                                        outcome_beat: Some((
                                            beat_until,
                                            format!("switch to {target_display} failed: {err}"),
                                        )),
                                    });
                                    next_dial_gen = next_dial_gen.wrapping_add(1);
                                    if let Some(p) = pending_switch.as_ref() {
                                        paint_switch_popup(p, state.reported_size, Instant::now());
                                    }
                                    continue;
                                }
                            }
                        }
                        // Record the target for the launcher's attach loop, then
                        // exit exactly like a detach. The outermost herdr process
                        // reads the file and starts the next leg.
                        if record_switch_target(
                            &ssh_target,
                            fleet.as_ref(),
                            focus_workspace.as_deref(),
                        ) {
                            // Hold the alternate screen across the handoff so the
                            // host shell never flashes between legs (#63).
                            SWITCH_HANDOFF_PENDING.store(true, Ordering::Release);
                            return Err(ClientError::ServerShutdown {
                                reason: Some("switching".to_string()),
                            });
                        }
                        // No launcher to chain into (e.g. bare `herdr client`):
                        // stay attached and let the user switch manually.
                        eprintln!("herdr: server requested switch to {ssh_target}, but no launcher is present (HERDR_SWITCH_FILE unset)");
                    }
                    ServerMessage::Notify { kind, message } => {
                        handle_notify(kind, &message, &state.sound_config);
                    }
                    ServerMessage::Clipboard { data } => {
                        forward_clipboard(&data);
                        let _ = io::stdout().flush();
                    }
                    ServerMessage::ReloadSoundConfig => {
                        reload_local_client_config(
                            &mut state.sound_config,
                            &mut state.redraw_on_focus_gained,
                        );
                    }
                    ServerMessage::MouseCapture { enabled } => {
                        let desired = enabled;
                        if desired != state.mouse_capture_active {
                            set_mouse_capture(desired).map_err(ClientError::ConnectionFailed)?;
                            state.mouse_capture_active = desired;
                        }
                    }
                    ServerMessage::Welcome { .. } => {
                        debug!("received unexpected Welcome in main loop");
                    }
                }
            }
            ClientLoopEvent::ServerDisconnected(slot_key) => {
                // Only the ACTIVE slot's death tears the session down with
                // today's ConnectionLost semantics (#65). A warm slot dying is
                // a silent demote — the ghost the design intends; the active
                // session keeps painting.
                match slots::route_slot_event(&slot_key, &active_slot_key, true) {
                    slots::SlotRouting::Apply => {
                        return Err(ClientError::ConnectionLost(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "server closed connection",
                        )));
                    }
                    // A reader disconnect is always a lifecycle death, so a
                    // non-active slot routes here (never to Drop).
                    slots::SlotRouting::DemoteDead | slots::SlotRouting::Drop => {
                        if let Some(manager) = slot_manager.as_mut() {
                            manager.handle_dead(&slots::SlotTarget::from_key(&slot_key));
                        }
                        debug!(slot = %slot_key, "warm slot disconnected; demoted silently");
                        continue;
                    }
                }
            }
            ClientLoopEvent::SlotWarmed {
                gen,
                key,
                stream,
                bridge,
            } => {
                // Disposition (#93): a SWITCH dial's success — gen matches the
                // pending switch — applies as a slot flip AND tears the popup
                // down. A SWEEP dial's success — gen matches the sweep entry
                // for that key — registers the slot as warm. Any other gen is
                // STALE: drop the stream (the server sees disconnect) and let
                // the bridge drop teardown the ssh transport.
                let pending_view = pending_switch
                    .as_ref()
                    .map(|p| (p.gen, p.target.key(), p.outcome_beat.is_some()));
                let sweep_gen = slot_dials_in_flight.get(&key).copied();
                let disposition = classify_dial_event(gen, &key, pending_view, sweep_gen);
                if disposition == DialDisposition::Pending {
                    if let Some(manager) = slot_manager.as_mut() {
                        let target = slots::SlotTarget::from_key(&key);
                        // Register the new connection as warm (with bridge
                        // ownership), then flip to it in-process.
                        let conn = slots::SlotConnection {
                            target: target.clone(),
                            write_stream: stream,
                            bridge,
                        };
                        if let Err(err) = manager.add_warm(conn) {
                            warn!(target = %key, err = %err, "switch-dial warmed slot but pause failed");
                            // Treat as failure-beat.
                            if let Some(p) = pending_switch.as_mut() {
                                p.outcome_beat = Some((
                                    Instant::now() + POPUP_FAILURE_BEAT,
                                    format!("switch to {} failed: {err}", p.target_display),
                                ));
                                paint_switch_popup(p, state.reported_size, Instant::now());
                            }
                            continue;
                        }
                        match manager.flip_to(&target) {
                            Ok(Some(new_stream)) => {
                                // Teardown popup BEFORE applying further events
                                // (PopupGuard discipline #93).
                                pending_switch = None;
                                clear_switch_popup(&mut state);
                                esc_grace_until = Some(Instant::now() + ESC_GRACE_AFTER_SUCCESS);
                                apply_slot_flip(
                                    new_stream,
                                    &target,
                                    &mut active_reader_quit,
                                    &mut active_slot_key,
                                    &mut write_stream,
                                    &mut state,
                                    &event_tx,
                                    &should_quit,
                                    max_frame_size,
                                )?;
                            }
                            Ok(None) | Err(_) => {
                                // Unexpected — the slot was just registered.
                                // Surface as a failure beat and clear.
                                if let Some(p) = pending_switch.as_mut() {
                                    p.outcome_beat = Some((
                                        Instant::now() + POPUP_FAILURE_BEAT,
                                        format!("switch to {} failed", p.target_display),
                                    ));
                                    paint_switch_popup(p, state.reported_size, Instant::now());
                                }
                            }
                        }
                    }
                    // Switch dials are tracked outside the sweep map; nothing
                    // to remove there.
                } else if disposition == DialDisposition::Sweep {
                    slot_dials_in_flight.remove(&key);
                    if let Some(manager) = slot_manager.as_mut() {
                        let target = slots::SlotTarget::from_key(&key);
                        let conn = slots::SlotConnection {
                            target,
                            write_stream: stream,
                            bridge,
                        };
                        if let Err(err) = manager.add_warm(conn) {
                            debug!(target = %key, err = %err, "failed to pause newly warmed slot");
                        } else {
                            debug!(target = %key, "slot warmed and paused");
                        }
                    }
                } else {
                    // Stale event: drop the stream + bridge. Plain drop closes
                    // the socket so the server sees a disconnect (#93).
                    debug!(target = %key, gen, "stale SlotWarmed; dropping stream");
                    // Detached drop: SshStdioBridge::drop joins its listener
                    // thread and can block on a live ssh child's teardown for
                    // seconds -- never on the loop thread.
                    std::thread::spawn(move || {
                        drop(stream);
                        drop(bridge);
                    });
                }
            }
            ClientLoopEvent::SlotDialFailed { gen, key, err } => {
                let pending_view = pending_switch
                    .as_ref()
                    .map(|p| (p.gen, p.target.key(), p.outcome_beat.is_some()));
                let sweep_gen = slot_dials_in_flight.get(&key).copied();
                let disposition = classify_dial_event(gen, &key, pending_view, sweep_gen);
                if disposition == DialDisposition::Pending {
                    if let Some(manager) = slot_manager.as_mut() {
                        manager
                            .registry
                            .mark_dial_failed(&slots::SlotTarget::from_key(&key), Instant::now());
                    }
                    if let Some(p) = pending_switch.as_mut() {
                        p.outcome_beat = Some((
                            Instant::now() + POPUP_FAILURE_BEAT,
                            format!("switch to {} failed: {err}", p.target_display),
                        ));
                        paint_switch_popup(p, state.reported_size, Instant::now());
                    }
                } else if disposition == DialDisposition::Sweep {
                    slot_dials_in_flight.remove(&key);
                    if let Some(manager) = slot_manager.as_mut() {
                        manager
                            .registry
                            .mark_dial_failed(&slots::SlotTarget::from_key(&key), Instant::now());
                    }
                } else {
                    debug!(target = %key, gen, "stale SlotDialFailed; dropping");
                }
            }
            ClientLoopEvent::Timer => {
                // Debounced bare Esc: nothing followed it, so it was a real
                // Esc keypress -- cancel (while pending) or swallow (grace).
                if pending_esc
                    .as_ref()
                    .is_some_and(|(d, _)| Instant::now() >= *d)
                {
                    pending_esc = None;
                    if pending_switch.is_some() {
                        next_dial_gen = next_dial_gen.wrapping_add(1);
                        if let Some(p) = pending_switch.as_mut() {
                            p.gen = next_dial_gen;
                            p.outcome_beat = Some((
                                Instant::now() + POPUP_CANCEL_BEAT,
                                "cancelled \u{2713}".to_string(),
                            ));
                            paint_switch_popup(p, state.reported_size, Instant::now());
                        }
                    }
                }
                // Warm-all dial sweep (#65): periodically dial the registry's
                // pending cold slots so a later switch to any fleet server is an
                // instant flip. Throttled; in-flight slots are skipped.
                if let Some(manager) = slot_manager.as_mut() {
                    let now = Instant::now();
                    if now.duration_since(last_slot_dial_sweep) >= Duration::from_secs(2) {
                        last_slot_dial_sweep = now;
                        let geometry = (state.reported_size.0, state.reported_size.1, 0, 0);
                        for effect in manager.registry.pending_dials(now) {
                            if let slots::SlotEffect::Dial(target) = effect {
                                // Only attempt targets with a REAL reachable
                                // socket (#65, blocker 3): home and the active
                                // leg's bridge. An additional peer has no bridge
                                // in stage 1 (#75) — leave it cold-by-design so a
                                // switch falls back to the relaunch leg, rather
                                // than dialing a path that never exists.
                                let Some(socket_path) = slot_socket_path(&target) else {
                                    continue;
                                };
                                let key = target.key().to_string();
                                if slot_dials_in_flight.contains_key(&key) {
                                    continue;
                                }
                                let gen = next_dial_gen;
                                next_dial_gen = next_dial_gen.wrapping_add(1);
                                slot_dials_in_flight.insert(key, gen);
                                spawn_warm_dial(
                                    gen,
                                    target.clone(),
                                    socket_path,
                                    geometry,
                                    negotiated_encoding,
                                    event_tx.clone(),
                                );
                            }
                        }
                    }
                }
                // Pending-switch upkeep: clear an expired outcome beat OR
                // repaint the live counter. Repaint is throttled so the loop
                // never thrashes the host terminal.
                if let Some(p) = pending_switch.as_ref() {
                    let now = Instant::now();
                    if let Some((until, _)) = p.outcome_beat.as_ref() {
                        if now >= *until {
                            pending_switch = None;
                            last_popup_paint = None;
                            clear_switch_popup(&mut state);
                        } else if last_popup_paint
                            .map(|t| now.saturating_duration_since(t) >= POPUP_REPAINT_INTERVAL)
                            .unwrap_or(true)
                        {
                            paint_switch_popup(p, state.reported_size, now);
                            last_popup_paint = Some(now);
                        }
                    } else if last_popup_paint
                        .map(|t| now.saturating_duration_since(t) >= POPUP_REPAINT_INTERVAL)
                        .unwrap_or(true)
                    {
                        paint_switch_popup(p, state.reported_size, now);
                        last_popup_paint = Some(now);
                    }
                } else {
                    last_popup_paint = None;
                }
                // Esc-grace expiry: just let it lapse — the StdinInput arm
                // checks `Instant::now() < esc_grace_until` itself.
                if let Some(until) = esc_grace_until {
                    if Instant::now() >= until {
                        esc_grace_until = None;
                    }
                }
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
// Slot flip success path (shared by warm flip and switch-dial success, #93)
// ---------------------------------------------------------------------------

/// Apply a successful slot flip: retire the old active reader, bind a new
/// reader to the slot that just became active, flip the active-slot tag (so
/// queued stale frames from the old reader are dropped from here on), swap
/// the write stream, re-assert geometry to the new slot (#77), and request a
/// full host-surface redraw. Shared by the warm-flip arm and the cold-dial
/// success arm (#93) so they cannot drift.
#[allow(clippy::too_many_arguments)]
fn apply_slot_flip(
    new_stream: UnixStream,
    target: &slots::SlotTarget,
    active_reader_quit: &mut Arc<AtomicBool>,
    active_slot_key: &mut String,
    write_stream: &mut UnixStream,
    state: &mut ClientState,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
) -> Result<(), ClientError> {
    active_reader_quit.store(true, Ordering::Release);
    let new_quit = Arc::new(AtomicBool::new(false));
    let read_clone = new_stream
        .try_clone()
        .map_err(ClientError::ConnectionFailed)?;
    let new_key = target.key().to_string();
    spawn_slot_reader(
        new_key.clone(),
        read_clone,
        event_tx.clone(),
        should_quit.clone(),
        new_quit.clone(),
        max_frame_size,
    );
    *active_reader_quit = new_quit;
    // Flip the active-slot tag BEFORE any further events apply, so queued
    // stale frames from the old reader are dropped from here on (#65).
    *active_slot_key = new_key;
    *write_stream = new_stream;
    let _ = write_stream.set_nonblocking(false);
    // Re-assert geometry to the slot we just made active (#77).
    let (cur_cols, cur_rows) = state.reported_size;
    let (cur_cw, cur_ch) = state.reported_cell_size;
    let resize = ClientMessage::Resize {
        cols: cur_cols,
        rows: cur_rows,
        cell_width_px: cur_cw,
        cell_height_px: cur_ch,
    };
    if let Err(e) = write_to_server(write_stream, &resize) {
        return Err(ClientError::ConnectionLost(e));
    }
    state.request_full_redraw();
    Ok(())
}

// ---------------------------------------------------------------------------
// Cancellable switch popup (#93)
// ---------------------------------------------------------------------------

/// State for an in-flight cancellable server switch (#93). Lives on the client
/// loop while the popup is up and clears the moment we land, cancel, or beat
/// out a failure notice. The popup paints only while this is `Some`; Esc is
/// intercepted only while this is `Some`; every exit edge MUST clear the popup
/// + request a full redraw of the active slot (the PopupGuard discipline).
#[derive(Debug, Clone)]
struct PendingSwitch {
    /// Generation counter the dial(s) carry. A SlotWarmed/SlotDialFailed event
    /// with a different gen is stale (cancel/re-switch superseded it) and is
    /// dropped — its stream too.
    gen: u64,
    /// The slot we are switching to.
    target: slots::SlotTarget,
    /// Display label of the slot we will return to on cancel/failure. Used in
    /// the popup subtitle.
    previous_display: String,
    /// Display label of the target we are switching to. Used in the popup
    /// title.
    target_display: String,
    /// When the switch was armed; drives the elapsed-seconds counter and the
    /// tone schedule (neutral / yellow / "host not responding" / "retry window
    /// ending soon").
    started_at: Instant,
    /// A terminal beat — `cancelled ✓`, `switch to … failed: …` — that the
    /// popup shows briefly before clearing. When set, the popup repaints this
    /// instead of the live title until `until` passes.
    outcome_beat: Option<(Instant, String)>,
}

/// How often the popup is allowed to repaint. Bounded so a busy loop never
/// flickers the box; the elapsed counter renders whole seconds anyway, so
/// 4-5x/sec is plenty.
const POPUP_REPAINT_INTERVAL: Duration = Duration::from_millis(220);

/// What the loop should do with a gen-stamped dial outcome event (#93).
/// Pure function so the cancel/re-switch race table is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DialDisposition {
    /// The event matches the live pending switch's `(gen, key)`: apply it as
    /// a switch-success flip or a switch-failure beat. Pending switches with
    /// an active outcome_beat already terminated; further dial events for
    /// them are stale.
    Pending,
    /// The event matches the warm-sweep entry for this key: register as warm
    /// (or mark dial failure for backoff).
    Sweep,
    /// The event's gen is stale (cancel/re-switch superseded it). Drop the
    /// stream so the server sees disconnect; never flip the active slot.
    Stale,
}

/// Classify a `SlotWarmed`/`SlotDialFailed` event against the live pending
/// switch and the sweep map. Caller passes the pending switch's `(gen, key,
/// has_outcome_beat)` and the sweep entry's gen for `event_key` (if any).
fn classify_dial_event(
    event_gen: u64,
    event_key: &str,
    pending: Option<(u64, &str, bool)>,
    sweep_gen_for_key: Option<u64>,
) -> DialDisposition {
    if let Some((p_gen, p_key, p_has_beat)) = pending {
        if !p_has_beat && p_gen == event_gen && p_key == event_key {
            return DialDisposition::Pending;
        }
    }
    if sweep_gen_for_key == Some(event_gen) {
        return DialDisposition::Sweep;
    }
    DialDisposition::Stale
}

/// True for a bare-Esc keypress chunk (#93). Longer chunks starting with
/// `0x1b` are arrow/function/CSI sequences and must NOT be intercepted as
/// cancel. Pure fn so the esc-chunk classification table can assert on every
/// shape without driving the event loop.
fn is_bare_esc_chunk(data: &[u8]) -> bool {
    data == [0x1b]
}

/// Tone schedule (#93 spec): neutral → yellow → host-not-responding subtitle →
/// retry-window-ending hint. Switches by elapsed-since-armed.
const POPUP_YELLOW_AT: Duration = Duration::from_secs(3);
const POPUP_UNRESPONSIVE_AT: Duration = Duration::from_secs(10);
const POPUP_RETRY_ENDING_AT: Duration = Duration::from_secs(25);

/// How long after a successful switch a lone Esc chunk is swallowed instead
/// of being forwarded to the new active server. Catches muscle-memory cancel
/// presses landing inside the just-arrived session.
const ESC_GRACE_AFTER_SUCCESS: Duration = Duration::from_millis(150);

/// How long the failure beat stays up before the popup clears.
const POPUP_FAILURE_BEAT: Duration = Duration::from_secs(2);

/// How long the cancel-confirmation beat stays up.
/// Hold a lone Esc this long before treating it as a real Esc rather
/// than the first byte of a split key sequence.
const ESC_DEBOUNCE: Duration = Duration::from_millis(30);
const POPUP_CANCEL_BEAT: Duration = Duration::from_millis(600);

/// Render the popup over the active slot's still-live frame. Plain raw-ANSI
/// box centered via the reported terminal size, cursor saved/restored so the
/// underlying frame is never disturbed (the #72 show_status_line precedent).
fn paint_switch_popup(pending: &PendingSwitch, reported_size: (u16, u16), now: Instant) {
    let (cols, rows) = reported_size;
    if cols < 12 || rows < 6 {
        return;
    }
    let lines = popup_lines(pending, now);
    // Box geometry: 50 wide, 4 inner rows (top border + 2 text + bottom).
    let box_w: u16 = 50.min(cols.saturating_sub(2));
    let box_h: u16 = 4;
    let start_col = (cols.saturating_sub(box_w)) / 2 + 1;
    let start_row = (rows.saturating_sub(box_h)) / 2 + 1;

    // Compose: save cursor, draw 4 rows, restore.
    let mut out = String::with_capacity(512);
    out.push_str("\x1b7"); // save cursor
    out.push_str("\x1b[?25l"); // hide cursor while painting
                               // Tone selects foreground color.
    let (border_ansi, text_ansi) = popup_tone_ansi(pending, now);
    // Top border.
    let border_w = (box_w as usize).saturating_sub(2);
    out.push_str(&format!(
        "\x1b[{};{}H{}┌{:─<border_w$}┐\x1b[0m",
        start_row, start_col, border_ansi, "",
    ));
    // Two text rows. Each row is "│ <content padded> │".
    let inner_w = (box_w as usize).saturating_sub(4);
    for (i, line) in lines.iter().take(2).enumerate() {
        let truncated: String = line.chars().take(inner_w).collect();
        let pad = inner_w.saturating_sub(truncated.chars().count());
        let spaces = " ".repeat(pad);
        out.push_str(&format!(
            "\x1b[{};{}H{}│\x1b[0m {}{}{} {}│\x1b[0m",
            start_row + 1 + i as u16,
            start_col,
            border_ansi,
            text_ansi,
            truncated,
            spaces,
            border_ansi
        ));
    }
    // Bottom border.
    out.push_str(&format!(
        "\x1b[{};{}H{}└{:─<border_w$}┘\x1b[0m",
        start_row + box_h - 1,
        start_col,
        border_ansi,
        "",
    ));
    out.push_str("\x1b[?25h"); // show cursor
    out.push_str("\x1b8"); // restore cursor
    let mut stdout = io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

/// The two text lines the popup shows, derived purely from `pending` + `now`.
/// Pure function so the tone schedule + outcome beats are unit-testable.
fn popup_lines(pending: &PendingSwitch, now: Instant) -> [String; 2] {
    if let Some((_, msg)) = pending.outcome_beat.as_ref() {
        return [msg.clone(), String::new()];
    }
    let elapsed = now.saturating_duration_since(pending.started_at);
    let secs = elapsed.as_secs();
    let title = format!("switching to {}…  {}s", pending.target_display, secs);
    let subtitle = if elapsed >= POPUP_RETRY_ENDING_AT {
        format!(
            "retry window ending soon — [esc] returns to {}",
            pending.previous_display
        )
    } else if elapsed >= POPUP_UNRESPONSIVE_AT {
        format!(
            "host not responding — [esc] returns to {}",
            pending.previous_display
        )
    } else {
        format!("[esc] cancel · returns to {}", pending.previous_display)
    };
    [title, subtitle]
}

/// Tone-driven ANSI prefixes (border, text). Neutral → yellow ramp at 3s,
/// 10s+, then the late "retry window ending" hint stays yellow.
fn popup_tone_ansi(pending: &PendingSwitch, now: Instant) -> (&'static str, &'static str) {
    if pending.outcome_beat.is_some() {
        // Beats render in neutral; the message itself is the signal.
        return ("\x1b[37m", "\x1b[1m");
    }
    let elapsed = now.saturating_duration_since(pending.started_at);
    if elapsed >= POPUP_YELLOW_AT {
        // Yellow border + bold yellow text from 3s onward (covers 3-10s,
        // 10s+, and 25s+ subtitle shifts uniformly).
        ("\x1b[33m", "\x1b[1;33m")
    } else {
        // Neutral 0-3s: white border, bold default text.
        ("\x1b[37m", "\x1b[1m")
    }
}

/// Clear the popup region by triggering a full app redraw on the active slot.
/// We do not selectively erase the box rows: the active slot is still live
/// under the popup and its next Frame paints over them naturally. The redraw
/// nudge ensures that next frame is FULL (no diff against the popup overlay).
fn clear_switch_popup(state: &mut ClientState) {
    // Erase any overlay residue by repainting a blank region on top first; the
    // server's full-redraw will then resettle the underlying frame cleanly.
    let mut out = io::stdout();
    let _ = write!(out, "\x1b7\x1b[?25l\x1b[2J\x1b[?25h\x1b8");
    let _ = out.flush();
    state.request_full_redraw();
}

/// Display label for a slot target — the ssh destination, or `home` for the
/// reserved sentinel. Used in the popup's title and `returns to <…>` line.
fn slot_display_label(target: &slots::SlotTarget) -> String {
    match target {
        slots::SlotTarget::Home => "home".to_string(),
        slots::SlotTarget::Ssh(t) => t.clone(),
    }
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Spawn the reader thread for one slot's stream. The reader stops when either
/// the loop-wide `should_quit` or this slot's `reader_quit` flips — a
/// connection-slots flip retires the old active reader via `reader_quit` while
/// the loop keeps running (#65).
fn spawn_slot_reader(
    slot_key: String,
    stream: UnixStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: Arc<AtomicBool>,
    reader_quit: Arc<AtomicBool>,
    max_frame_size: usize,
) {
    std::thread::spawn(move || {
        let combined = CombinedQuit {
            global: should_quit,
            slot: reader_quit,
        };
        server_reader_thread(slot_key, stream, event_tx, &combined, max_frame_size);
    });
}

/// A reader quits when EITHER its loop-wide flag or its per-slot flag is set.
struct CombinedQuit {
    global: Arc<AtomicBool>,
    slot: Arc<AtomicBool>,
}

impl CombinedQuit {
    fn load(&self, order: Ordering) -> bool {
        self.global.load(order) || self.slot.load(order)
    }
}

/// The active slot's target for this client leg: the ssh target carried by a
/// remote leg, or home for a local attach. The whole loop keys off this so the
/// initial reader, the slot manager, and the active-slot tagging agree (#65).
fn active_slot_target() -> slots::SlotTarget {
    std::env::var(crate::remote::ACTIVE_SSH_TARGET_ENV_VAR)
        .ok()
        .filter(|t| !t.is_empty())
        .map(slots::SlotTarget::Ssh)
        .unwrap_or(slots::SlotTarget::Home)
}

/// The active slot's key string (the home sentinel or the ssh target).
fn active_slot_key() -> String {
    active_slot_target().key().to_string()
}

/// Build the connection-slots manager when `[slots] enabled`, owning the active
/// connection (a clone of the active write stream) and the warm-all target
/// list derived from config peers plus the carried fleet snapshot. Returns None
/// when slots are disabled — the legacy leg path then drives switches.
fn build_slot_manager(
    active_write_stream: &UnixStream,
    _max_frame_size: usize,
) -> Option<slots::SlotManager> {
    let loaded = crate::config::Config::load();
    let slots_config = loaded.config.slots.clone();
    if !slots_config.enabled {
        return None;
    }

    // The active slot is whatever leg launched this client: home for a local
    // attach, the ssh target for a remote leg (carried in REATTACH/remote env).
    let active_target = active_slot_target();

    // Warm-all targets: locally-configured peers (a hub knows its fleet) plus
    // the carried snapshot's peers and origin (a spoke learns its fleet from
    // the down-gossip, #73). Home is always included.
    let config_peers: Vec<String> = loaded
        .config
        .peers
        .iter()
        .map(|p| p.ssh_target().to_string())
        .collect();
    let mut carried: Vec<String> = Vec::new();
    if let Some(fleet) = carried_fleet_snapshot() {
        carried.extend(fleet.peers.iter().map(|p| p.ssh_target.clone()));
        if let Some(origin) = fleet.origin_summary.as_ref() {
            carried.push(origin.ssh_target.clone());
        }
    }
    let targets = slots::warm_all_targets(&config_peers, &carried, slots_config.max);

    let active_conn = slots::SlotConnection {
        target: active_target,
        write_stream: active_write_stream.try_clone().ok()?,
        bridge: None,
    };
    Some(slots::SlotManager::new(
        active_conn,
        targets,
        slots_config.max,
    ))
}

/// Background-dial a warm slot: connect its socket and complete the handshake
/// so the server holds a session, then report the writable stream back to the
/// loop (which pauses it). Runs on a detached thread — a slow or failing dial
/// must never stall the active paint path (#65). Stage 1 warms slots whose
/// transport is reachable as a local socket: home (the local client socket) and
/// any peer whose ssh-stdio bridge socket is already live; a cold peer with no
/// bridge stays cold and a switch to it falls back to the relaunch leg.
fn spawn_warm_dial(
    gen: u64,
    target: slots::SlotTarget,
    socket_path: std::path::PathBuf,
    geometry: (u16, u16, u32, u32),
    requested_encoding: RenderEncoding,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
) {
    std::thread::spawn(move || {
        let key = target.key().to_string();
        let (cols, rows, cell_width_px, cell_height_px) = geometry;
        let dialed = (|| -> Result<UnixStream, ClientError> {
            let mut stream =
                UnixStream::connect(&socket_path).map_err(ClientError::ConnectionFailed)?;
            // A warm slot is a full app client, like the active one. It carries
            // no host theme (the active slot owns the host terminal) and no
            // notice; the handshake just establishes a paused session.
            do_handshake(
                &mut stream,
                cols,
                rows,
                cell_width_px,
                cell_height_px,
                requested_encoding,
                false,
                None,
            )?;
            Ok(stream)
        })();
        let event = match dialed {
            Ok(stream) => ClientLoopEvent::SlotWarmed {
                gen,
                key,
                stream,
                bridge: None,
            },
            Err(err) => {
                debug!(target = %key, err = %err, "warm-all dial failed; slot stays cold");
                ClientLoopEvent::SlotDialFailed {
                    gen,
                    key,
                    err: err.to_string(),
                }
            }
        };
        let _ = event_tx.blocking_send(event);
    });
}

/// Spawn an on-demand SWITCH dial for a cold slot (#93). Targets with a live
/// local socket (home, the active ssh leg's bridge) just dial that socket;
/// bridge-less ssh peers get a NON-INTERACTIVE client-side `SshStdioBridge`
/// built first, then dial the bridge's forwarded socket. The bridge rides the
/// success event so the slot's connection owns it for the slot's lifetime —
/// `SlotConnection::Drop` tears the transport down when the slot dies or is
/// demoted. On failure (or a stale outcome at apply time) the bridge is
/// dropped on this thread.
fn spawn_switch_dial(
    gen: u64,
    target: slots::SlotTarget,
    geometry: (u16, u16, u32, u32),
    requested_encoding: RenderEncoding,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
) {
    std::thread::spawn(move || {
        let key = target.key().to_string();
        let (cols, rows, cell_width_px, cell_height_px) = geometry;
        let dialed: Result<(UnixStream, Option<crate::remote::SshStdioBridge>), ClientError> =
            (|| {
                // Path A: the slot already has a live transport (home, or the
                // active leg's launcher-owned bridge). Just dial it.
                if let Some(path) = slot_socket_path(&target) {
                    let mut stream =
                        UnixStream::connect(&path).map_err(ClientError::ConnectionFailed)?;
                    do_handshake(
                        &mut stream,
                        cols,
                        rows,
                        cell_width_px,
                        cell_height_px,
                        requested_encoding,
                        false,
                        None,
                    )?;
                    return Ok((stream, None));
                }
                // Path B: cold ssh peer with no bridge — build one
                // non-interactively, then dial the forwarded socket. The
                // bridge is returned to the loop and stored on the slot.
                match &target {
                    slots::SlotTarget::Home => Err(ClientError::ConnectionFailed(
                        io::Error::other("home slot has no socket path"),
                    )),
                    slots::SlotTarget::Ssh(t) => {
                        let (bridge, sock) = crate::remote::start_switch_bridge_noninteractive(t)
                            .map_err(ClientError::ConnectionFailed)?;
                        // The bridge listener may need a moment to be ready
                        // (it binds synchronously, but ssh dial latency hides
                        // here on first connect). Retry briefly on connect
                        // refused to avoid spurious failures.
                        let mut stream = connect_with_brief_retry(&sock)
                            .map_err(ClientError::ConnectionFailed)?;
                        do_handshake(
                            &mut stream,
                            cols,
                            rows,
                            cell_width_px,
                            cell_height_px,
                            requested_encoding,
                            false,
                            None,
                        )?;
                        Ok((stream, Some(bridge)))
                    }
                }
            })();
        let event = match dialed {
            Ok((stream, bridge)) => ClientLoopEvent::SlotWarmed {
                gen,
                key,
                stream,
                bridge,
            },
            Err(err) => {
                debug!(target = %key, err = %err, "switch dial failed");
                ClientLoopEvent::SlotDialFailed {
                    gen,
                    key,
                    err: err.to_string(),
                }
            }
        };
        let _ = event_tx.blocking_send(event);
    });
}

/// Brief blocking retry around `UnixStream::connect` for a freshly-bound
/// bridge socket. The listener bind is synchronous but the ssh child takes
/// time to attach its stdio pair on the first accept — a stale ConnectionRefused
/// here would mis-classify as a dial failure.
fn connect_with_brief_retry(path: &std::path::Path) -> io::Result<UnixStream> {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(e)
                if e.kind() == io::ErrorKind::ConnectionRefused
                    || e.kind() == io::ErrorKind::NotFound =>
            {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Socket path for a slot target with a REAL, already-reachable transport, or
/// `None` when the target has no live socket yet (#65, blocker 3).
///
/// - Home: the local client socket — always reachable.
/// - The ACTIVE ssh leg: its ssh-stdio bridge socket, created by the LAUNCHER
///   (`run_remote`) and handed down explicitly via `HERDR_ACTIVE_BRIDGE_SOCKET`.
///   The client is a separate child, so it CANNOT recompute that path from
///   `local_forward_socket_path` (which keys on `std::process::id()` — the
///   launcher's pid, never the client's). Recomputing it was the bug: every ssh
///   warm-dial missed and the slot stayed cold.
/// - Any OTHER ssh peer: there is no bridge for it in stage 1, so it has no
///   reachable socket. It returns `None` and stays cold-by-design; a switch to
///   it falls through to the exit-and-relaunch leg that bootstraps the bridge.
///   Warming additional peers over their own bridges is stage-2 scope (#75).
fn slot_socket_path(target: &slots::SlotTarget) -> Option<std::path::PathBuf> {
    match target {
        slots::SlotTarget::Home => Some(crate::server::socket_paths::client_socket_path()),
        slots::SlotTarget::Ssh(t) => {
            // Only the active leg's bridge socket is live; match it by the ssh
            // target the launcher tagged this client with.
            let active = std::env::var(crate::remote::ACTIVE_SSH_TARGET_ENV_VAR).ok();
            if active.as_deref() == Some(t.as_str()) {
                std::env::var_os(crate::remote::ACTIVE_BRIDGE_SOCKET_ENV_VAR)
                    .map(std::path::PathBuf::from)
                    .filter(|p| !p.as_os_str().is_empty())
            } else {
                None
            }
        }
    }
}

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    slot_key: String,
    mut stream: UnixStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &CombinedQuit,
    max_frame_size: usize,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(slot_key));
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage(slot_key.clone(), msg))
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(slot_key));
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
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(slot_key));
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
    protocol::write_message(stream, msg).map_err(|e| io::Error::other(e.to_string()))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn reload_local_client_config(
    sound_config: &mut crate::config::SoundConfig,
    redraw_on_focus_gained: &mut bool,
) {
    match crate::config::load_live_config() {
        Ok(loaded) => {
            for diagnostic in loaded.config.ui.sound.diagnostics() {
                warn!(diagnostic = %diagnostic, "local sound config diagnostic");
            }
            *sound_config = loaded.config.ui.sound;
            *redraw_on_focus_gained = loaded.config.ui.redraw_on_focus_gained;
            debug!("reloaded local client config");
        }
        Err(diagnostics) => {
            warn!(diagnostics = ?diagnostics, "failed to reload local client config; keeping current client config");
        }
    }
}

fn handle_notify(kind: NotifyKind, message: &str, sound_config: &crate::config::SoundConfig) {
    handle_notify_with_notifiers(
        kind,
        message,
        sound_config,
        crate::terminal_notify::show_notification,
        crate::platform::show_desktop_notification,
    );
}

fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            let (title, body) = crate::terminal_notify::split_message(message);
            if let Err(err) = show_terminal_notification(title, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            let (title, body) = crate::terminal_notify::split_message(message);
            if let Err(err) = show_system_notification(title, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        "attention clear" => Some(crate::sound::Sound::AllClear),
        _ => None,
    }
}

fn should_bridge_clipboard_image_paste(data: &[u8]) -> bool {
    if data == b"\x1b[200~\x1b[201~" {
        return true;
    }

    let events = crate::raw_input::parse_raw_input_bytes_sync(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && key.modifiers == crossterm::event::KeyModifiers::CONTROL
                && matches!(key.code, crossterm::event::KeyCode::Char('v' | 'V'))
    )
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
// Frame output
// ---------------------------------------------------------------------------

fn write_encoded_frame_with_graphics(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &[u8],
) -> io::Result<()> {
    writer.write_all(encoded)?;
    if graphics.is_empty() {
        return Ok(());
    }

    record_received_kitty_graphics(graphics);
    writer.write_all(b"\x1b7")?;
    writer.write_all(graphics)?;
    writer.write_all(b"\x1b8")
}

fn contains_kitty_graphics_bytes(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

fn record_received_kitty_graphics(bytes: &[u8]) {
    let ids = kitty_graphics_image_ids(bytes);
    if ids.is_empty() {
        return;
    }
    let set = RECEIVED_KITTY_GRAPHICS_IDS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        set.extend(ids);
    }
}

fn clear_received_kitty_graphics(mut writer: impl io::Write) -> io::Result<()> {
    let Some(set) = RECEIVED_KITTY_GRAPHICS_IDS.get() else {
        return Ok(());
    };
    let Ok(mut set) = set.lock() else {
        return Ok(());
    };
    for id in set.drain() {
        write!(writer, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    }
    writer.flush()
}

fn kitty_graphics_image_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut index = 0usize;
    while let Some(start) = find_subslice(&bytes[index..], b"\x1b_G") {
        let command_start = index + start + 3;
        let Some(end) = find_subslice(&bytes[command_start..], b"\x1b\\") else {
            break;
        };
        let command = &bytes[command_start..command_start + end];
        if let Some(id) = kitty_graphics_command_image_id(command) {
            ids.push(id);
        }
        index = command_start + end + 2;
    }
    ids
}

fn kitty_graphics_command_image_id(command: &[u8]) -> Option<u32> {
    let header_end = command
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(command.len());
    for part in command[..header_end].split(|byte| *byte == b',') {
        let Some(value) = part.strip_prefix(b"i=") else {
            continue;
        };
        let text = std::str::from_utf8(value).ok()?;
        if let Ok(id) = text.parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Resize polling
// ---------------------------------------------------------------------------

fn current_terminal_geometry(kitty_graphics_enabled: bool) -> (u16, u16, u32, u32) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if !kitty_graphics_enabled {
        return (cols, rows, 0, 0);
    }
    let Ok(size) = crossterm::terminal::window_size() else {
        return (cols, rows, 8, 16);
    };
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return (cols, rows, 8, 16);
    }
    (
        cols,
        rows,
        (size.width as u32 / size.columns as u32).max(1),
        (size.height as u32 / size.rows as u32).max(1),
    )
}

/// Polls the terminal size and sends resize events when it changes.
fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    kitty_graphics_enabled: bool,
    should_quit: &Arc<AtomicBool>,
) {
    let (_, _, initial_cell_width, initial_cell_height) =
        current_terminal_geometry(kitty_graphics_enabled);
    let mut last_size = (
        initial_cols,
        initial_rows,
        initial_cell_width,
        initial_cell_height,
    );
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let new_size = current_terminal_geometry(kitty_graphics_enabled);
        if new_size != last_size {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(
                    new_size.0, new_size.1, new_size.2, new_size.3,
                ))
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
    crate::logging::init_file_logging("herdr-client.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    fn refusal_session_error() -> AttachAttemptError {
        AttachAttemptError::Session(ClientError::ServerShutdown {
            reason: Some(protocol::LIVE_HANDOFF_ATTACH_NOTICE.to_string()),
        })
    }

    fn refusal_handshake_error() -> AttachAttemptError {
        AttachAttemptError::Handshake(ClientError::HandshakeRejected {
            version: PROTOCOL_VERSION,
            error: protocol::LIVE_HANDOFF_ATTACH_NOTICE.to_string(),
        })
    }

    fn transient_handshake_error() -> AttachAttemptError {
        AttachAttemptError::Handshake(ClientError::ConnectionFailed(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "refused",
        )))
    }

    #[test]
    fn live_handoff_refusals_are_recognized_in_both_phases() {
        assert!(refusal_session_error().is_live_handoff_refusal());
        assert!(refusal_handshake_error().is_live_handoff_refusal());
        assert!(!transient_handshake_error().is_live_handoff_refusal());
        assert!(!AttachAttemptError::Session(ClientError::ServerShutdown {
            reason: Some("detached".to_string()),
        })
        .is_live_handoff_refusal());
    }

    #[test]
    fn handoff_retry_opens_window_on_refusal_then_retries_transients() {
        let mut retry = HandoffRetry::default();

        // First refusal opens the window and retries.
        assert!(retry.should_retry(&refusal_session_error(), Duration::ZERO));

        // Transient failures inside the window are expected churn: the old
        // server dying, the new one rebinding the socket, or the old server
        // still answering with its older protocol version.
        assert!(retry.should_retry(&transient_handshake_error(), Duration::ZERO));
        assert!(retry.should_retry(&refusal_handshake_error(), Duration::ZERO));
        assert!(retry.should_retry(
            &AttachAttemptError::Handshake(ClientError::HandshakeRejected {
                version: PROTOCOL_VERSION - 1,
                error: "older server".to_string(),
            }),
            Duration::ZERO,
        ));

        // A NEWER server rejecting us means the handoff completed onto a
        // protocol we cannot speak — surface the upgrade guidance now.
        assert!(!retry.should_retry(
            &AttachAttemptError::Handshake(ClientError::HandshakeRejected {
                version: PROTOCOL_VERSION + 1,
                error: "newer server".to_string(),
            }),
            Duration::ZERO,
        ));
    }

    #[test]
    fn handoff_retry_never_starts_without_a_refusal() {
        let mut retry = HandoffRetry::default();
        assert!(!retry.should_retry(&transient_handshake_error(), Duration::ZERO));
        assert!(!retry.should_retry(
            &AttachAttemptError::Session(ClientError::ServerShutdown {
                reason: Some("detached".to_string()),
            }),
            Duration::ZERO,
        ));
        assert!(!retry.should_retry(
            &AttachAttemptError::TerminalSetup(io::Error::other("tty broke")),
            Duration::ZERO,
        ));
    }

    #[test]
    fn handoff_retry_stops_at_the_window_deadline() {
        let mut retry = HandoffRetry {
            deadline: Some(Instant::now() - Duration::from_millis(1)),
            ..HandoffRetry::default()
        };
        assert!(!retry.should_retry(&refusal_handshake_error(), Duration::ZERO));
        assert!(!retry.should_retry(&transient_handshake_error(), Duration::ZERO));
        // A short-lived session refusal does not reopen the expired window:
        // a flapping server cannot keep the client retrying forever.
        assert!(!retry.should_retry(&refusal_session_error(), Duration::from_millis(50)));
    }

    #[test]
    fn handoff_retry_long_session_earns_a_fresh_window() {
        let mut retry = HandoffRetry {
            deadline: Some(Instant::now() - Duration::from_millis(1)),
            ..HandoffRetry::default()
        };
        // The session ran for real before this refusal (a later, separate
        // handoff): a fresh retry window opens.
        assert!(retry.should_retry(&refusal_session_error(), HANDOFF_SESSION_RESET_THRESHOLD));
    }

    /// Serializes tests that mutate the process-global hold flags.
    fn hold_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn status_text_counts_elapsed_seconds_for_visible_progress() {
        // No window open yet: 0s, never silent.
        let fresh = HandoffRetry::default();
        assert_eq!(
            fresh.status_text(),
            "herdr: handoff in progress, reconnecting… (0s)"
        );

        // Once the window opens, the counter ticks up.
        let retry = HandoffRetry {
            started: Some(Instant::now() - Duration::from_secs(3)),
            ..HandoffRetry::default()
        };
        assert_eq!(
            retry.status_text(),
            "herdr: handoff in progress, reconnecting… (3s)"
        );
    }

    #[test]
    fn opening_the_window_starts_the_elapsed_counter() {
        let mut retry = HandoffRetry::default();
        assert!(retry.started.is_none());
        assert!(retry.should_retry(&refusal_session_error(), Duration::ZERO));
        assert!(
            retry.started.is_some(),
            "the elapsed-seconds clock starts when the retry window opens"
        );
    }

    #[test]
    fn host_terminal_is_held_tracks_both_hold_sources() {
        let _guard = hold_test_lock();
        SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);
        INHERITED_TERMINAL_HOLD.store(false, Ordering::Release);
        assert!(!host_terminal_is_held());

        SWITCH_HANDOFF_PENDING.store(true, Ordering::Release);
        assert!(host_terminal_is_held(), "own switch-handoff hold counts");
        SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);

        INHERITED_TERMINAL_HOLD.store(true, Ordering::Release);
        assert!(host_terminal_is_held(), "inherited hold counts");
        INHERITED_TERMINAL_HOLD.store(false, Ordering::Release);
    }

    #[test]
    fn held_restore_guard_clears_an_inherited_hold_on_drop() {
        let _guard = hold_test_lock();
        SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);
        INHERITED_TERMINAL_HOLD.store(true, Ordering::Release);

        // An armed guard dropping while the terminal is held reclaims it:
        // force_restore_host_terminal clears every hold flag.
        {
            let _held = HeldRestoreGuard::new();
        }
        assert!(
            !host_terminal_is_held(),
            "an abnormal exit from a held leg must reclaim the terminal"
        );
    }

    #[test]
    fn held_restore_guard_disarmed_keeps_the_hold_for_the_next_leg() {
        let _guard = hold_test_lock();
        SWITCH_HANDOFF_PENDING.store(true, Ordering::Release);
        INHERITED_TERMINAL_HOLD.store(false, Ordering::Release);

        // A clean handoff disarms the guard: the hold must survive into the
        // next leg's repaint (the no-blip switch, #63).
        {
            let held = HeldRestoreGuard::new();
            held.into_handoff();
        }
        assert!(
            SWITCH_HANDOFF_PENDING.load(Ordering::Acquire),
            "a disarmed guard leaves the hold in place for the next leg"
        );
        SWITCH_HANDOFF_PENDING.store(false, Ordering::Release);
    }

    #[test]
    fn theme_capture_buffer_parses_osc_color_replies() {
        let buf = b"\x1b]10;rgb:cccc/dddd/eeee\x1b\\\x1b]11;#1e1e2e\x07";
        let theme = theme_from_capture_buffer(buf);
        assert_eq!(
            theme.foreground,
            Some(crate::terminal_theme::RgbColor {
                r: 0xcc,
                g: 0xdd,
                b: 0xee,
            })
        );
        assert_eq!(
            theme.background,
            Some(crate::terminal_theme::RgbColor {
                r: 0x1e,
                g: 0x1e,
                b: 0x2e,
            })
        );

        // Keystrokes mixed into the capture window do not corrupt parsing.
        let mixed = b"a\x1b]11;#1e1e2e\x07b";
        let theme = theme_from_capture_buffer(mixed);
        assert!(theme.foreground.is_none());
        assert!(theme.background.is_some());

        assert!(theme_from_capture_buffer(b"plain typing").is_empty());
    }

    #[test]
    fn recorded_switch_roundtrips_with_and_without_fleet() {
        let dir = std::env::temp_dir().join(format!("herdr-switch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("switch");

        // A switch with a carried snapshot survives the file round-trip.
        let fleet = protocol::FleetSnapshot {
            origin: "mba22".to_string(),
            peers: vec![protocol::FleetPeer {
                name: "anvil".to_string(),
                ssh_target: "lars@anvil".to_string(),
                host: Some("anvil".to_string()),
                version: None,
                system: None,
                latency_ms: Some(12),
                workspaces: Vec::new(),
                age_secs: Some(7),
                error: None,
            }],
            origin_summary: None,
        };
        let recorded = RecordedSwitch {
            target: "lars@sage".to_string(),
            fleet: Some(fleet),
            focus_workspace: None,
        };
        std::fs::write(&path, serde_json::to_string(&recorded).unwrap()).unwrap();
        let taken = take_switch_target(&path).expect("switch recorded");
        assert_eq!(taken, recorded);
        // take_* clears the file so a leg never re-runs an old switch.
        assert!(!path.exists());

        // The home sentinel travels as a plain target without a fleet.
        let home = RecordedSwitch {
            target: protocol::HOME_SWITCH_TARGET.to_string(),
            fleet: None,
            focus_workspace: None,
        };
        std::fs::write(&path, serde_json::to_string(&home).unwrap()).unwrap();
        let taken = take_switch_target(&path).expect("home switch recorded");
        assert_eq!(taken.target, protocol::HOME_SWITCH_TARGET);
        assert!(taken.fleet.is_none());

        // Defensive bare-target fallback.
        std::fs::write(&path, "lars@anvil\n").unwrap();
        let taken = take_switch_target(&path).expect("bare target parsed");
        assert_eq!(taken.target, "lars@anvil");
        assert!(taken.fleet.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue #63 part 3: the launcher's one-shot failed-switch notice is
    /// lifted into the Hello and cleared so a later handshake retry (#38) in
    /// the same leg does not repeat it.
    #[test]
    fn take_attach_notice_is_one_shot() {
        let _guard = env_lock().lock().unwrap();
        let _notice = EnvVarGuard::set(SWITCH_NOTICE_ENV_VAR, "switch to sage failed: boom");

        assert_eq!(
            take_attach_notice().as_deref(),
            Some("switch to sage failed: boom")
        );
        // Consumed: a second read (a handshake retry) sees nothing.
        assert!(take_attach_notice().is_none());
    }

    #[test]
    fn take_attach_notice_ignores_empty() {
        let _guard = env_lock().lock().unwrap();
        let _notice = EnvVarGuard::set(SWITCH_NOTICE_ENV_VAR, "");
        assert!(take_attach_notice().is_none());
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            restore_env_var(self.key, self.previous.clone());
        }
    }

    struct EnvVarsRemovedGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarsRemovedGuard {
        fn new(keys: &[&'static str]) -> Self {
            let previous: Vec<_> = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarsRemovedGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.clone() {
                restore_env_var(key, value);
            }
        }
    }

    #[test]
    fn clipboard_image_paste_bridge_triggers_on_ctrl_v_and_empty_paste() {
        assert!(should_bridge_clipboard_image_paste(&[0x16]));
        assert!(should_bridge_clipboard_image_paste(b"\x1b[118;5u"));
        assert!(should_bridge_clipboard_image_paste(b"\x1b[200~\x1b[201~"));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~text\x1b[201~"
        ));
        assert!(!should_bridge_clipboard_image_paste(b"v"));
    }

    #[test]
    fn graphics_bytes_are_written_after_blit_with_saved_cursor() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(
            &mut output,
            b"\x1b[?2026htext\x1b[?2026lcursor",
            b"graphics",
        )
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?2026htext\x1b[?2026lcursor\x1b7graphics\x1b8"
        );
    }

    #[test]
    fn empty_graphics_writes_only_blit_frame() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(&mut output, b"text", b"").unwrap();

        assert_eq!(output, b"text");
    }

    #[test]
    fn terminal_frame_kitty_detection_matches_apc_prefix() {
        assert!(contains_kitty_graphics_bytes(b"text\x1b_Ga=p;\x1b\\"));
        assert!(!contains_kitty_graphics_bytes(b"text\x1b[?2026h"));
    }

    #[test]
    fn kitty_graphics_image_id_parser_tracks_herdr_ids_only() {
        let ids = kitty_graphics_image_ids(
            b"text\x1b_Ga=t,t=d,f=32,s=1,v=1,i=10023,q=2;AAAA\x1b\\\x1b_Ga=p,i=10023,p=7;\x1b\\",
        );
        assert_eq!(ids, vec![10023, 10023]);
    }

    #[test]
    fn kitty_graphics_cleanup_deletes_tracked_images_not_all_images() {
        record_received_kitty_graphics(b"\x1b_Ga=t,i=123,q=2;AAAA\x1b\\");
        let mut output = Vec::new();
        clear_received_kitty_graphics(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("a=d,d=I,i=123"));
        assert!(!text.contains("d=A"));
    }

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
    fn terminal_restore_postlude_restores_visible_default_cursor() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output).unwrap();
        assert_eq!(output, b"\x1b[?25h\x1b[0 q");
    }

    #[test]
    fn attach_escape_detaches_on_prefix_q() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        assert!(matches!(
            escape.filter_input(vec![b'q'], 24, 3),
            AttachInputAction::Detach
        ));
    }

    #[test]
    fn attach_escape_sends_literal_prefix_on_double_prefix() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        match escape.filter_input(vec![0x02], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02]),
            other => panic!("expected forwarded prefix, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_forwards_prefix_before_non_escape_key() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![b'a', 0x02], 24, 3),
            AttachInputAction::Forward(bytes) if bytes == b"a"
        ));
        match escape.filter_input(vec![b'x'], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02, b'x']),
            other => panic!("expected forwarded bytes, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_turns_wheel_into_scroll_action() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[<64;11;6M".to_vec(), 24, 7) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                column,
                row,
                ..
            } => {
                assert_eq!(source, AttachScrollSource::Wheel);
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 7);
                assert_eq!(column, Some(10));
                assert_eq!(row, Some(5));
            }
            other => panic!("expected scroll action, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_swallows_non_wheel_mouse_reports() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(b"\x1b[<0;11;6M".to_vec(), 24, 7),
            AttachInputAction::None
        ));
    }

    #[test]
    fn attach_escape_turns_plain_page_keys_into_scroll_actions() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[5~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-up scroll action, got {other:?}"),
        }

        match escape.filter_input(b"\x1b[6~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[6~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Down);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-down scroll action, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_forwards_modified_page_key() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5;5~".to_vec(), 12, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, b"\x1b[5;5~"),
            other => panic!("expected modified page key to forward, got {other:?}"),
        }
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
    fn client_error_display_detached_default_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            crate::session::SESSION_ENV_VAR,
        ]);
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr` to reattach"),
            "should suggest default reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_named_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr session attach work` to reattach"),
            "should suggest named session reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_remote_reattach_hint_takes_precedence() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should prefer remote reattach command: {msg}"
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
    fn sound_from_notify_message_maps_done() {
        assert_eq!(
            sound_from_notify_message("agent done"),
            Some(crate::sound::Sound::Done)
        );
    }

    #[test]
    fn sound_from_notify_message_maps_attention() {
        assert_eq!(
            sound_from_notify_message("agent attention"),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn sound_from_notify_message_rejects_unknown_payloads() {
        assert_eq!(sound_from_notify_message("toast"), None);
    }

    #[test]
    fn reload_local_client_config_refreshes_redraw_on_focus_gained() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-client-config-reload-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "[ui]\nredraw_on_focus_gained = false\n").unwrap();
        let path_string = path.to_string_lossy().to_string();
        let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
        let mut sound_config = crate::config::SoundConfig::default();
        let mut redraw_on_focus_gained = true;

        reload_local_client_config(&mut sound_config, &mut redraw_on_focus_gained);

        assert!(!redraw_on_focus_gained);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn toast_notify_from_server_is_emitted_even_when_attach_config_was_off() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::Toast,
            "pi finished: workspace 1",
            &sound_config,
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
            |_, _| Ok(false),
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_from_server_uses_system_notifier() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "pi finished: workspace 1",
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
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

    // -----------------------------------------------------------------------
    // Switch-popup tests (#93)
    // -----------------------------------------------------------------------

    fn make_pending(target_key: &str, gen: u64) -> PendingSwitch {
        PendingSwitch {
            gen,
            target: slots::SlotTarget::from_key(target_key),
            previous_display: "home".to_string(),
            target_display: target_key.to_string(),
            started_at: Instant::now(),
            outcome_beat: None,
        }
    }

    // --- gen disposition table -------------------------------------------

    #[test]
    fn classify_pending_match_routes_to_pending() {
        let disposition = classify_dial_event(7, "anvil", Some((7, "anvil", false)), None);
        assert_eq!(disposition, DialDisposition::Pending);
    }

    #[test]
    fn classify_sweep_match_routes_to_sweep() {
        let disposition = classify_dial_event(3, "sage", None, Some(3));
        assert_eq!(disposition, DialDisposition::Sweep);
    }

    #[test]
    fn classify_stale_success_after_cancel_is_dropped() {
        // Cancel bumped pending.gen to 9; an earlier dial's success (gen 7)
        // is stale and MUST drop its stream — never flip the active slot.
        let disposition = classify_dial_event(7, "anvil", Some((9, "anvil", false)), None);
        assert_eq!(disposition, DialDisposition::Stale);
    }

    #[test]
    fn classify_stale_failure_during_outcome_beat_is_dropped() {
        // Once the pending switch is in an outcome_beat (cancel / failure),
        // further dial outcomes are stale by construction.
        let disposition = classify_dial_event(7, "anvil", Some((7, "anvil", true)), None);
        assert_eq!(disposition, DialDisposition::Stale);
    }

    #[test]
    fn classify_wrong_key_with_matching_gen_is_stale() {
        // Pending switch is to "anvil"; an event for "sage" with the same
        // gen never matches.
        let disposition = classify_dial_event(7, "sage", Some((7, "anvil", false)), None);
        assert_eq!(disposition, DialDisposition::Stale);
    }

    #[test]
    fn classify_no_pending_no_sweep_is_stale() {
        let disposition = classify_dial_event(1, "anvil", None, None);
        assert_eq!(disposition, DialDisposition::Stale);
    }

    // --- esc chunk classification ----------------------------------------

    #[test]
    fn esc_chunk_bare_esc_is_intercepted() {
        assert!(is_bare_esc_chunk(&[0x1b]));
    }

    #[test]
    fn esc_chunk_csi_sequence_passes_through() {
        // Arrow up: ESC [ A — must NOT be intercepted as cancel.
        assert!(!is_bare_esc_chunk(&[0x1b, 0x5b, b'A']));
        // F1: ESC O P.
        assert!(!is_bare_esc_chunk(&[0x1b, 0x4f, b'P']));
        // ESC + key combo (alt-a in some terminals): ESC a.
        assert!(!is_bare_esc_chunk(&[0x1b, b'a']));
    }

    #[test]
    fn esc_chunk_empty_or_non_esc_is_not_cancel() {
        assert!(!is_bare_esc_chunk(&[]));
        assert!(!is_bare_esc_chunk(b"a"));
    }

    // --- popup text builder + tone schedule ------------------------------

    #[test]
    fn popup_lines_neutral_window_shows_cancel_subtitle() {
        let p = make_pending("anvil", 1);
        let now = p.started_at + Duration::from_secs(1);
        let lines = popup_lines(&p, now);
        assert!(lines[0].contains("switching to anvil"));
        assert!(lines[0].contains("1s"));
        assert!(lines[1].contains("[esc] cancel"));
        assert!(lines[1].contains("returns to home"));
        // Tone is neutral for the first 3 seconds.
        let (border, _) = popup_tone_ansi(&p, now);
        assert_eq!(border, "\x1b[37m");
    }

    #[test]
    fn popup_lines_yellow_at_three_seconds() {
        let p = make_pending("anvil", 1);
        let now = p.started_at + POPUP_YELLOW_AT;
        let (border, text) = popup_tone_ansi(&p, now);
        assert_eq!(border, "\x1b[33m");
        assert_eq!(text, "\x1b[1;33m");
    }

    #[test]
    fn popup_lines_unresponsive_at_ten_seconds() {
        let p = make_pending("anvil", 1);
        let now = p.started_at + POPUP_UNRESPONSIVE_AT;
        let lines = popup_lines(&p, now);
        assert!(lines[1].contains("host not responding"));
        assert!(lines[1].contains("returns to home"));
    }

    #[test]
    fn popup_lines_retry_ending_late_window() {
        let p = make_pending("anvil", 1);
        let now = p.started_at + POPUP_RETRY_ENDING_AT;
        let lines = popup_lines(&p, now);
        assert!(lines[1].contains("retry window ending soon"));
    }

    #[test]
    fn popup_lines_outcome_beat_overrides_live_title() {
        let mut p = make_pending("anvil", 1);
        p.outcome_beat = Some((
            Instant::now() + Duration::from_secs(1),
            "cancelled".to_string(),
        ));
        let lines = popup_lines(&p, Instant::now());
        assert_eq!(lines[0], "cancelled");
        assert!(lines[1].is_empty());
    }

    // --- cancel returns to previous (loop-level invariant) ---------------

    /// Loop-level invariant: a bare-Esc chunk while a switch is pending
    /// bumps the pending gen, sets a cancel beat, and leaves the active slot
    /// untouched. We model the loop's relevant state inline; the actual loop
    /// calls `classify_dial_event` against `pending_view` so a later dial
    /// success arriving with the old gen is dropped.
    #[test]
    fn cancel_bumps_gen_and_leaves_active_slot_unchanged() {
        let active_slot_key = "<home>".to_string();
        let mut pending = make_pending("anvil", 7);
        let original_gen = pending.gen;

        // Simulate the StdinInput Esc handling: bump gen + beat cancelled.
        let mut next_dial_gen: u64 = pending.gen + 1;
        let new_gen = next_dial_gen;
        pending.gen = new_gen;
        pending.outcome_beat = Some((
            Instant::now() + POPUP_CANCEL_BEAT,
            "cancelled \u{2713}".to_string(),
        ));
        next_dial_gen = next_dial_gen.wrapping_add(1);
        let _ = next_dial_gen;

        // Active slot is untouched by cancel.
        assert_eq!(active_slot_key, "<home>");
        // Gen advanced past the original.
        assert_ne!(pending.gen, original_gen);
        // A late dial-success (carrying the original gen) is STALE — the
        // dispatcher must drop it.
        let disposition = classify_dial_event(
            original_gen,
            "anvil",
            Some((
                pending.gen,
                pending.target.key(),
                pending.outcome_beat.is_some(),
            )),
            None,
        );
        assert_eq!(disposition, DialDisposition::Stale);
    }

    /// Rapid A → cancel → A: the second switch's dial success applies; any
    /// stale success from the first one must drop its stream (the server
    /// sees disconnect) and never flip the active slot. Implemented at the
    /// dispatcher level — the live loop wires this disposition into the
    /// flip/no-flip decision (see SlotWarmed arm).
    #[test]
    fn rapid_switch_cancel_switch_only_second_pending_matches() {
        // First switch arms gen=1.
        let mut pending = make_pending("anvil", 1);
        // User hits Esc: gen advances to 2, beat is set.
        pending.gen = 2;
        pending.outcome_beat = Some((Instant::now() + POPUP_CANCEL_BEAT, "cancelled".to_string()));
        // First dial's late success (gen=1) is stale.
        let d1 = classify_dial_event(
            1,
            "anvil",
            Some((
                pending.gen,
                pending.target.key(),
                pending.outcome_beat.is_some(),
            )),
            None,
        );
        assert_eq!(d1, DialDisposition::Stale);
        // Second switch fires: clear beat, arm gen=3.
        pending.outcome_beat = None;
        pending.gen = 3;
        // Second dial's success (gen=3) is the live pending one.
        let d2 = classify_dial_event(
            3,
            "anvil",
            Some((
                pending.gen,
                pending.target.key(),
                pending.outcome_beat.is_some(),
            )),
            None,
        );
        assert_eq!(d2, DialDisposition::Pending);
        // First dial's success arriving later (gen=1) is still stale.
        let d3 = classify_dial_event(
            1,
            "anvil",
            Some((
                pending.gen,
                pending.target.key(),
                pending.outcome_beat.is_some(),
            )),
            None,
        );
        assert_eq!(d3, DialDisposition::Stale);
    }

    // --- socketpair pattern: real stream + bridge=None reaches apply_slot_flip
    //
    // The end-to-end loop integration (StdinInput Esc → bumped gen → stale
    // dispatch → no active_slot_key flip) is exercised at the dispatcher
    // table above. The here-tested invariant is that apply_slot_flip
    // succeeds against a real socketpair-backed UnixStream and never panics
    // — the same flow both the warm-flip arm and the cold-dial success arm
    // share.
    #[test]
    fn apply_slot_flip_replaces_write_stream_against_socketpair() {
        use std::os::unix::net::UnixStream;
        // Original "active" pair.
        let (old_local, _old_peer) = UnixStream::pair().unwrap();
        // New "next active" pair: this is what the cold dial would return.
        let (new_local, mut new_peer) = UnixStream::pair().unwrap();

        let mut active_reader_quit = Arc::new(AtomicBool::new(false));
        let old_quit_observer = active_reader_quit.clone();
        let mut active_slot_key = "<home>".to_string();
        let mut write_stream = old_local;
        let mut state = ClientState {
            blit_encoder: render_ansi::BlitEncoder::new(),
            mouse_capture_active: false,
            reported_size: (80, 24),
            reported_cell_size: (10, 20),
            sound_config: crate::config::SoundConfig::default(),
            kitty_graphics_enabled: false,
            attach_escape: None,
            mouse_scroll_lines: 3,
            redraw_on_focus_gained: false,
        };
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(8);
        let should_quit = Arc::new(AtomicBool::new(false));
        let target = slots::SlotTarget::Ssh("anvil".to_string());

        apply_slot_flip(
            new_local,
            &target,
            &mut active_reader_quit,
            &mut active_slot_key,
            &mut write_stream,
            &mut state,
            &event_tx,
            &should_quit,
            MAX_FRAME_SIZE,
        )
        .expect("flip should succeed against a live socketpair");

        // Active slot key flipped to the new target.
        assert_eq!(active_slot_key, "anvil");
        // The new stream is what we get; reading on the new peer should see
        // the Resize re-assert that apply_slot_flip just wrote.
        new_peer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let msg: ClientMessage =
            protocol::read_message(&mut new_peer, MAX_FRAME_SIZE).expect("resize");
        match msg {
            ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            } => {
                assert_eq!((cols, rows), (80, 24));
                assert_eq!((cell_width_px, cell_height_px), (10, 20));
            }
            other => panic!("expected Resize, got {other:?}"),
        }
        // The old reader quit flag was raised before the swap.
        assert!(old_quit_observer.load(Ordering::Acquire));
        // And the new active_reader_quit is the freshly-allocated one (not
        // the old observer), so the new reader sees a clean false.
        assert!(!active_reader_quit.load(Ordering::Acquire));
    }
}
