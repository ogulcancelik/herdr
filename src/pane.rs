use std::borrow::Cow;
use std::cell::Cell;
use std::io::{BufWriter, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::style::{Color, Modifier, Style};
use ratatui::{layout::Rect, Frame};
use tokio::sync::{mpsc, Notify};
use tracing::{debug, error, info, warn};
use unicode_width::UnicodeWidthStr;

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;

mod state;

use self::state::stabilize_agent_state;
pub use self::state::{EffectiveStateChange, PaneState};

const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);
const DEFAULT_DETECTION_ROWS: usize = 24;

#[derive(Debug, Clone, Copy)]
struct PendingAgentRelease {
    agent: Agent,
    until: std::time::Instant,
}

fn active_pending_release(
    pending_release: &Mutex<Option<PendingAgentRelease>>,
    now: std::time::Instant,
) -> Option<Agent> {
    let mut pending_release = pending_release.lock().ok()?;
    match *pending_release {
        Some(pending) if now < pending.until => Some(pending.agent),
        Some(_) => {
            *pending_release = None;
            None
        }
        None => None,
    }
}

async fn publish_state_changed_event(
    state_events: mpsc::Sender<AppEvent>,
    pane_id: PaneId,
    agent: Option<Agent>,
    state: AgentState,
) {
    // This runs on the async detector task, not the PTY reader thread.
    // Waiting for queue space here preserves correctness-critical state transitions
    // without blocking pane I/O.
    if let Err(e) = state_events
        .send(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
        })
        .await
    {
        warn!(
            pane = pane_id.raw(),
            err = %e,
            "failed to deliver StateChanged event"
        );
    }
}

const AGENT_MISS_CONFIRMATION_ATTEMPTS: u8 = 6;

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPresence {
    current_agent: Option<Agent>,
    consecutive_misses: u8,
}

impl AgentDetectionPresence {
    fn from_agent(current_agent: Option<Agent>) -> Self {
        Self {
            current_agent,
            consecutive_misses: 0,
        }
    }

    fn current_agent(&self) -> Option<Agent> {
        self.current_agent
    }

    fn observe_process_probe(&mut self, identified_agent: Option<Agent>) -> bool {
        match identified_agent {
            Some(agent) => {
                self.consecutive_misses = 0;
                if Some(agent) == self.current_agent {
                    return false;
                }
                self.current_agent = Some(agent);
                true
            }
            None => {
                if self.current_agent.is_none() {
                    self.consecutive_misses = 0;
                    return false;
                }
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if self.consecutive_misses < AGENT_MISS_CONFIRMATION_ATTEMPTS {
                    return false;
                }
                self.current_agent = None;
                self.consecutive_misses = 0;
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PaneRuntime — PTY, parser, channels, background tasks
// ---------------------------------------------------------------------------

/// PTY runtime for a pane. Owns the terminal, I/O channels, and background tasks.
/// Dropping this shuts down all background tasks and closes the PTY.
pub struct PaneRuntime {
    terminal: Arc<PaneTerminal>,
    sender: mpsc::Sender<Bytes>,
    resize_tx: mpsc::Sender<(u16, u16)>,
    current_size: Cell<(u16, u16)>,
    child_pid: Arc<AtomicU32>,
    kitty_keyboard_flags: Arc<AtomicU16>,
    detect_reset_notify: Arc<Notify>,
    pending_release: Arc<Mutex<Option<PendingAgentRelease>>>,
    // Task handles for deterministic shutdown
    detect_handle: tokio::task::AbortHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub offset_from_bottom: usize,
    pub max_offset_from_bottom: usize,
    pub viewport_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputState {
    pub alternate_screen: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub mouse_protocol_mode: crate::input::MouseProtocolMode,
    pub mouse_protocol_encoding: crate::input::MouseProtocolEncoding,
    pub mouse_alternate_scroll: bool,
}

impl InputState {
    pub fn mouse_reporting_enabled(self) -> bool {
        self.mouse_protocol_mode.reporting_enabled()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessBytesResult {
    request_render: bool,
    clipboard_writes: Vec<Vec<u8>>,
}

struct GhosttyPaneTerminal {
    core: Mutex<GhosttyPaneCore>,
    key_encoder: Mutex<crate::ghostty::KeyEncoder>,
}

struct GhosttyPaneCore {
    terminal: crate::ghostty::Terminal,
    render_state: crate::ghostty::RenderState,
    host_terminal_theme: crate::terminal_theme::TerminalTheme,
    transient_default_color_owner_pgid: Option<u32>,
    default_color_tracker: DefaultColorOscTracker,
    osc52_forwarder: Osc52Forwarder,
}

#[derive(Debug, Default)]
struct DefaultColorOscTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DefaultColorOscTrackerState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
}

impl DefaultColorOscTracker {
    fn observe(&mut self, bytes: &[u8]) -> bool {
        let mut saw_default_color_set = false;

        for &byte in bytes {
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::Ground;
            }
        }

        saw_default_color_set
    }
}

fn is_default_color_set_osc(body: &[u8]) -> bool {
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let command = &body[..separator];
    let value = &body[separator + 1..];
    matches!(command, b"10" | b"11") && !value.is_empty() && value != b"?"
}

/// 256 KiB of base64 ≈ 192 KiB of text — enough for real source-file copies
/// while still bounding memory against stream garbage.
const OSC52_MAX_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Osc52ForwarderState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
}

/// Reconstructs OSC 52 clipboard-write sequences from raw PTY bytes so the
/// main loop can re-emit them. `libghostty-vt` drops `.clipboard_contents`,
/// so child clipboard writes never reach the host terminal unless we forward
/// them ourselves.
#[derive(Debug, Default)]
struct Osc52Forwarder {
    state: Osc52ForwarderState,
    body: Vec<u8>,
    pending: Vec<Vec<u8>>,
}

impl Osc52Forwarder {
    fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                Osc52ForwarderState::Ground => {
                    if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    }
                }
                Osc52ForwarderState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = Osc52ForwarderState::OscBody;
                    } else if byte == 0x1b {
                        self.state = Osc52ForwarderState::Escape;
                    } else {
                        self.state = Osc52ForwarderState::Ground;
                    }
                }
                Osc52ForwarderState::OscBody => match byte {
                    0x07 => {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    }
                    0x1b => self.state = Osc52ForwarderState::OscEscape,
                    _ => self.body.push(byte),
                },
                Osc52ForwarderState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize();
                        self.state = Osc52ForwarderState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = Osc52ForwarderState::OscBody;
                    }
                }
            }

            if self.body.len() > OSC52_MAX_PAYLOAD_BYTES {
                self.body.clear();
                self.state = Osc52ForwarderState::Ground;
            }
        }
    }

    fn finalize(&mut self) {
        if let Some(content) = parse_osc52_clipboard_write(&self.body) {
            self.pending.push(content);
        }
        self.body.clear();
    }

    fn drain_pending(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending)
    }
}

/// Accepts `52;c;<base64>` and `52;;<base64>`.
/// Queries (`?`) are rejected because herdr has no reply path.
/// The payload must decode as base64 before it is forwarded.
fn parse_osc52_clipboard_write(body: &[u8]) -> Option<Vec<u8>> {
    use base64::Engine;

    let rest = body.strip_prefix(b"52;")?;
    let sep = rest.iter().position(|b| *b == b';')?;
    let selector = &rest[..sep];
    let data = &rest[sep + 1..];
    if !(selector.is_empty() || selector == b"c") || data == b"?" {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn foreground_job_is_shell(job: &crate::platform::ForegroundJob, shell_pid: u32) -> bool {
    job.processes.iter().any(|process| process.pid == shell_pid)
}

fn current_transient_default_color_owner(shell_pid: u32) -> Option<u32> {
    let job = crate::detect::foreground_job(shell_pid)?;
    (!foreground_job_is_shell(&job, shell_pid)).then_some(job.process_group_id)
}

fn foreground_job_uses_droid_scrollback_compat(job: &crate::platform::ForegroundJob) -> bool {
    job.processes.iter().any(|process| {
        process.name.eq_ignore_ascii_case("droid")
            || process
                .argv0
                .as_deref()
                .is_some_and(|argv0| argv0.eq_ignore_ascii_case("droid"))
            || process.cmdline.as_deref().is_some_and(|cmdline| {
                cmdline.eq_ignore_ascii_case("droid")
                    || cmdline.starts_with("droid ")
                    || cmdline.to_ascii_lowercase().contains("/droid")
            })
    })
}

fn contains_scrollback_clear_sequence(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\x1b[3J")
        || bytes.windows(5).any(|window| window == b"\x1b[?3J")
}

fn strip_scrollback_clear_sequences<'a>(bytes: &'a [u8]) -> Cow<'a, [u8]> {
    if !contains_scrollback_clear_sequence(bytes) {
        return Cow::Borrowed(bytes);
    }

    let mut filtered = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let remaining = &bytes[index..];
        if remaining.starts_with(b"\x1b[3J") {
            index += 4;
            continue;
        }
        if remaining.starts_with(b"\x1b[?3J") {
            index += 5;
            continue;
        }
        filtered.push(bytes[index]);
        index += 1;
    }

    Cow::Owned(filtered)
}

fn maybe_filter_primary_screen_scrollback_clear<'a>(
    bytes: &'a [u8],
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> Cow<'a, [u8]> {
    // Droid redraws its primary-screen TUI with CSI 3 J, which erases pane
    // scrollback inside herdr. Keep the hack scoped to Droid on the primary
    // screen so normal terminal clear-history behavior still works elsewhere.
    if alternate_screen
        || !contains_scrollback_clear_sequence(bytes)
        || !foreground_job.is_some_and(foreground_job_uses_droid_scrollback_compat)
    {
        return Cow::Borrowed(bytes);
    }

    strip_scrollback_clear_sequences(bytes)
}

#[cfg(target_os = "macos")]
fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    let _ = owner_pgid;
    foreground_job_is_shell(foreground_job, shell_pid)
}

#[cfg(not(target_os = "macos"))]
fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    foreground_job.process_group_id != owner_pgid
        && foreground_job_is_shell(foreground_job, shell_pid)
}

fn write_host_terminal_theme(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
) {
    if let Some(color) = theme.foreground {
        let sequence = crate::terminal_theme::osc_set_default_color_sequence(
            crate::terminal_theme::DefaultColorKind::Foreground,
            color,
        );
        terminal.write(sequence.as_bytes());
    }
    if let Some(color) = theme.background {
        let sequence = crate::terminal_theme::osc_set_default_color_sequence(
            crate::terminal_theme::DefaultColorKind::Background,
            color,
        );
        terminal.write(sequence.as_bytes());
    }
}

fn restore_host_terminal_theme_if_needed(
    core: &mut GhosttyPaneCore,
    pane_id: PaneId,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    let Some(owner_pgid) = core.transient_default_color_owner_pgid else {
        return false;
    };
    if core.host_terminal_theme.is_empty() {
        return false;
    }
    if !should_restore_host_terminal_theme(owner_pgid, shell_pid, alternate_screen, foreground_job)
    {
        return false;
    }

    core.transient_default_color_owner_pgid = None;
    write_host_terminal_theme(&mut core.terminal, core.host_terminal_theme);
    info!(
        pane = pane_id.raw(),
        owner_pgid, "restored host terminal default colors after transient override"
    );
    true
}

struct PaneTerminal {
    ghostty: GhosttyPaneTerminal,
}

impl PaneTerminal {
    fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        shell_pid: u32,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        self.ghostty
            .process_pty_bytes(pane_id, shell_pid, bytes, response_writer)
    }

    fn resize(&self, rows: u16, cols: u16) {
        self.ghostty.resize(rows, cols);
    }

    fn scroll_up(&self, lines: usize) {
        self.ghostty.scroll_up(lines);
    }

    fn scroll_down(&self, lines: usize) {
        self.ghostty.scroll_down(lines);
    }

    fn scroll_reset(&self) {
        self.ghostty.scroll_reset();
    }

    fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.ghostty.set_scroll_offset_from_bottom(lines);
    }

    fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.ghostty.scroll_metrics()
    }

    fn input_state(&self) -> Option<InputState> {
        self.ghostty.input_state()
    }

    fn visible_text(&self) -> String {
        self.ghostty.visible_text()
    }

    fn detection_text(&self) -> String {
        self.ghostty.detection_text()
    }

    fn recent_text(&self, lines: usize) -> String {
        self.ghostty.recent_text(lines)
    }

    fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.ghostty.recent_unwrapped_text(lines)
    }

    fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.ghostty.extract_selection(selection)
    }

    fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.ghostty.render(frame, area, show_cursor);
    }

    fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.ghostty.apply_host_terminal_theme(theme);
    }

    fn has_transient_default_color_override(&self) -> bool {
        self.ghostty.has_transient_default_color_override()
    }

    fn maybe_restore_host_terminal_theme(&self, pane_id: PaneId, shell_pid: u32) -> bool {
        self.ghostty
            .maybe_restore_host_terminal_theme(pane_id, shell_pid)
    }

    fn keyboard_protocol(
        &self,
        fallback: crate::input::KeyboardProtocol,
    ) -> crate::input::KeyboardProtocol {
        self.ghostty.keyboard_protocol().unwrap_or(fallback)
    }

    fn encode_terminal_key(
        &self,
        key: crate::input::TerminalKey,
        protocol: crate::input::KeyboardProtocol,
    ) -> Vec<u8> {
        self.ghostty.encode_terminal_key(key, protocol)
    }

    fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.ghostty
            .encode_mouse_button(kind, column, row, modifiers)
    }

    fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.ghostty
            .encode_mouse_wheel(kind, column, row, modifiers)
    }
}

impl GhosttyPaneTerminal {
    fn new(
        mut terminal: crate::ghostty::Terminal,
        response_writer: mpsc::Sender<Bytes>,
    ) -> std::io::Result<Self> {
        terminal
            .set_write_pty_callback(move |bytes| {
                let _ = response_writer.try_send(Bytes::copy_from_slice(bytes));
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let render_state =
            crate::ghostty::RenderState::new().map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut key_encoder =
            crate::ghostty::KeyEncoder::new().map_err(|e| std::io::Error::other(e.to_string()))?;
        key_encoder.set_from_terminal(&terminal);
        Ok(Self {
            core: Mutex::new(GhosttyPaneCore {
                terminal,
                render_state,
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                transient_default_color_owner_pgid: None,
                default_color_tracker: DefaultColorOscTracker::default(),
                osc52_forwarder: Osc52Forwarder::default(),
            }),
            key_encoder: Mutex::new(key_encoder),
        })
    }

    fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        if let Ok(mut core) = self.core.lock() {
            core.host_terminal_theme = theme;
            core.transient_default_color_owner_pgid = None;
            write_host_terminal_theme(&mut core.terminal, theme);
        }
    }

    fn has_transient_default_color_override(&self) -> bool {
        self.core
            .lock()
            .map(|core| core.transient_default_color_owner_pgid.is_some())
            .unwrap_or(false)
    }

    fn maybe_restore_host_terminal_theme(&self, pane_id: PaneId, shell_pid: u32) -> bool {
        let foreground_job = crate::detect::foreground_job(shell_pid);
        let Ok(mut core) = self.core.lock() else {
            return false;
        };

        let alternate_screen = core
            .terminal
            .active_screen()
            .map(|screen| screen == crate::ghostty::ActiveScreen::Alternate)
            .unwrap_or(false);
        restore_host_terminal_theme_if_needed(
            &mut core,
            pane_id,
            shell_pid,
            alternate_screen,
            foreground_job.as_ref(),
        )
    }

    fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        shell_pid: u32,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        let Ok(mut core) = self.core.lock() else {
            error!(pane = pane_id.raw(), "ghostty core lock poisoned in reader");
            return ProcessBytesResult {
                request_render: false,
                clipboard_writes: Vec::new(),
            };
        };

        if shell_pid > 0 && core.default_color_tracker.observe(bytes) {
            if let Some(owner_pgid) = current_transient_default_color_owner(shell_pid) {
                core.transient_default_color_owner_pgid = Some(owner_pgid);
                debug!(
                    pane = pane_id.raw(),
                    owner_pgid, "tracked transient default color override"
                );
            }
        }

        core.osc52_forwarder.observe(bytes);
        let clipboard_writes = core.osc52_forwarder.drain_pending();

        let alternate_screen = core
            .terminal
            .active_screen()
            .map(|screen| screen == crate::ghostty::ActiveScreen::Alternate)
            .unwrap_or(false);
        let filtered_bytes = if shell_pid > 0 {
            let foreground_job = (!alternate_screen && contains_scrollback_clear_sequence(bytes))
                .then(|| crate::detect::foreground_job(shell_pid))
                .flatten();
            maybe_filter_primary_screen_scrollback_clear(
                bytes,
                alternate_screen,
                foreground_job.as_ref(),
            )
        } else {
            Cow::Borrowed(bytes)
        };
        if filtered_bytes.len() != bytes.len() {
            debug!(
                pane = pane_id.raw(),
                shell_pid, "ignored scrollback clear sequence for droid compatibility"
            );
        }

        core.terminal.write(filtered_bytes.as_ref());
        if let Ok(mut key_encoder) = self.key_encoder.lock() {
            key_encoder.set_from_terminal(&core.terminal);
        }
        let synchronized_output = core
            .terminal
            .mode_get(crate::ghostty::MODE_SYNCHRONIZED_OUTPUT)
            .unwrap_or(false);
        let _ = response_writer;
        ProcessBytesResult {
            request_render: !synchronized_output,
            clipboard_writes,
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if let Ok(mut core) = self.core.lock() {
            let _ = core.terminal.resize(cols, rows);
        }
    }

    fn scroll_up(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_delta(-(lines as isize));
        }
    }

    fn scroll_down(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_delta(lines as isize);
        }
    }

    fn scroll_reset(&self) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_bottom();
        }
    }

    fn set_scroll_offset_from_bottom(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_bottom();
            if lines > 0 {
                core.terminal.scroll_viewport_delta(-(lines as isize));
            }
        }
    }

    fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        let scrollbar = core.terminal.scrollbar().ok()?;
        Some(ScrollMetrics {
            offset_from_bottom: scrollbar
                .total
                .saturating_sub(scrollbar.offset + scrollbar.len),
            max_offset_from_bottom: scrollbar.total.saturating_sub(scrollbar.len),
            viewport_rows: scrollbar.len,
        })
    }

    fn keyboard_protocol(&self) -> Option<crate::input::KeyboardProtocol> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        Some(crate::input::KeyboardProtocol::from_kitty_flags(
            core.terminal.kitty_keyboard_flags().ok()? as u16,
        ))
    }

    fn input_state(&self) -> Option<InputState> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        let alternate_screen =
            core.terminal.active_screen().ok()? == crate::ghostty::ActiveScreen::Alternate;
        let application_cursor = core
            .terminal
            .mode_get(crate::ghostty::MODE_APPLICATION_CURSOR_KEYS)
            .ok()?;
        let bracketed_paste = core
            .terminal
            .mode_get(crate::ghostty::MODE_BRACKETED_PASTE)
            .ok()?;
        let focus_reporting = core
            .terminal
            .mode_get(crate::ghostty::MODE_FOCUS_EVENT)
            .ok()?;
        let mouse_sgr = core
            .terminal
            .mode_get(crate::ghostty::MODE_MOUSE_SGR)
            .ok()?;
        let mouse_utf8 = core
            .terminal
            .mode_get(crate::ghostty::MODE_MOUSE_UTF8)
            .ok()?;
        let mouse_alternate_scroll = core
            .terminal
            .mode_get(crate::ghostty::MODE_MOUSE_ALTERNATE_SCROLL)
            .ok()?;
        let mouse_protocol_mode = if core.terminal.mode_get(1003).ok()? {
            crate::input::MouseProtocolMode::AnyMotion
        } else if core.terminal.mode_get(1002).ok()? {
            crate::input::MouseProtocolMode::ButtonMotion
        } else if core.terminal.mode_get(1000).ok()? {
            crate::input::MouseProtocolMode::PressRelease
        } else if core.terminal.mode_get(9).ok()? {
            crate::input::MouseProtocolMode::Press
        } else {
            crate::input::MouseProtocolMode::None
        };
        let mouse_protocol_encoding = if mouse_sgr {
            crate::input::MouseProtocolEncoding::Sgr
        } else if mouse_utf8 {
            crate::input::MouseProtocolEncoding::Utf8
        } else {
            crate::input::MouseProtocolEncoding::Default
        };
        Some(InputState {
            alternate_screen,
            application_cursor,
            bracketed_paste,
            focus_reporting,
            mouse_protocol_mode,
            mouse_protocol_encoding,
            mouse_alternate_scroll,
        })
    }

    fn encode_terminal_key(
        &self,
        key: crate::input::TerminalKey,
        protocol: crate::input::KeyboardProtocol,
    ) -> Vec<u8> {
        if ghostty_prefers_herdr_text_encoding(key) {
            return crate::input::encode_terminal_key(key, protocol);
        }

        let Some(event) = ghostty_key_event_from_terminal_key(key) else {
            return crate::input::encode_terminal_key(key, protocol);
        };

        let Ok(mut encoder) = self.key_encoder.lock() else {
            return crate::input::encode_terminal_key(key, protocol);
        };
        match encoder.encode(&event) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) | Err(_) => crate::input::encode_terminal_key(key, protocol),
        }
    }

    fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        let mut encoder = ghostty_mouse_encoder_for_terminal(&core.terminal)?;
        let event = ghostty_mouse_event_from_button_kind(kind, column, row, modifiers)?;
        encoder.encode(&event).ok()
    }

    fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        let mut encoder = ghostty_mouse_encoder_for_terminal(&core.terminal)?;
        let event = ghostty_mouse_event_from_wheel_kind(kind, column, row, modifiers)?;
        encoder.encode(&event).ok()
    }

    fn visible_text(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_visible_text(&mut core).ok())
            .unwrap_or_default()
    }

    fn detection_text(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_detection_text(&core).ok())
            .unwrap_or_default()
    }

    fn recent_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text(&core, lines).ok())
            .unwrap_or_default()
    }

    fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text_unwrapped(&core, lines).ok())
            .unwrap_or_default()
    }

    fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_extract_selection(&mut core, selection).ok())
    }

    fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let host_theme = core.host_terminal_theme;
        let GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        if render_state.update(terminal).is_err() {
            return;
        }
        let colors = render_state.colors().ok();
        let default_bg = colors.and_then(|c| ghostty_default_bg(c.background, host_theme));
        let default_fg = colors.map(|c| ghostty_color(c.foreground));

        let mut row_iterator = match crate::ghostty::RowIterator::new() {
            Ok(iterator) => iterator,
            Err(_) => return,
        };
        let mut row_cells = match crate::ghostty::RowCells::new() {
            Ok(cells) => cells,
            Err(_) => return,
        };
        {
            let buf = frame.buffer_mut();
            let mut rows = match render_state.populate_row_iterator(&mut row_iterator) {
                Ok(rows) => rows,
                Err(_) => return,
            };
            let mut y = 0u16;
            while y < area.height && rows.next() {
                let mut cells = match rows.populate_cells(&mut row_cells) {
                    Ok(cells) => cells,
                    Err(_) => break,
                };
                let mut x = 0u16;
                while x < area.width && cells.next() {
                    let wide = cells.wide().unwrap_or(crate::ghostty::CellWide::Narrow);
                    let style = ghostty_cell_style(&cells, default_fg, default_bg);
                    let symbol = ghostty_buffer_symbol(&cells, wide)
                        .unwrap_or_else(|_| ghostty_blank_symbol_for_width(wide).to_string());
                    let cell = &mut buf[(area.x + x, area.y + y)];
                    cell.reset();
                    cell.set_symbol(&symbol);
                    cell.set_style(style);
                    x += 1;
                }
                while x < area.width {
                    let cell = &mut buf[(area.x + x, area.y + y)];
                    ghostty_reset_cell(cell, default_fg, default_bg);
                    x += 1;
                }
                y += 1;
            }
            while y < area.height {
                for x in 0..area.width {
                    let cell = &mut buf[(area.x + x, area.y + y)];
                    ghostty_reset_cell(cell, default_fg, default_bg);
                }
                y += 1;
            }
        }

        if show_cursor && render_state.cursor_visible().ok() == Some(true) {
            if let Ok(Some(cursor)) = render_state.cursor_viewport() {
                if cursor.x < area.width && cursor.y < area.height {
                    frame.set_cursor_position((area.x + cursor.x, area.y + cursor.y));
                }
            }
        }
    }
}

fn ghostty_key_event_from_terminal_key(
    key: crate::input::TerminalKey,
) -> Option<crate::ghostty::KeyEvent> {
    let mut event = crate::ghostty::KeyEvent::new().ok()?;
    event.set_action(match key.kind {
        crossterm::event::KeyEventKind::Press => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS
        }
        crossterm::event::KeyEventKind::Release => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE
        }
        crossterm::event::KeyEventKind::Repeat => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT
        }
    });
    event.set_mods(ghostty_mods_from_key_modifiers(key.modifiers));
    event.set_key(ghostty_key_from_crossterm_key_code(
        key.code,
        key.shifted_codepoint,
    )?);

    if let Some(text) = ghostty_key_text(key) {
        event.set_utf8(&text);
    } else {
        event.set_utf8("");
    }

    if let Some(codepoint) = ghostty_unshifted_codepoint(key) {
        event.set_unshifted_codepoint(codepoint);
    }

    Some(event)
}

fn ghostty_prefers_herdr_text_encoding(key: crate::input::TerminalKey) -> bool {
    matches!(key.code, crossterm::event::KeyCode::Char(_))
}

fn ghostty_mods_from_key_modifiers(modifiers: crossterm::event::KeyModifiers) -> u16 {
    let mut ghostty_mods = 0u16;
    if modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
        ghostty_mods |= crate::ghostty::MOD_SHIFT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
        ghostty_mods |= crate::ghostty::MOD_CTRL;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::ALT) {
        ghostty_mods |= crate::ghostty::MOD_ALT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
        ghostty_mods |= crate::ghostty::MOD_SUPER;
    }
    ghostty_mods
}

fn ghostty_mouse_encoder_for_terminal(
    terminal: &crate::ghostty::Terminal,
) -> Option<crate::ghostty::MouseEncoder> {
    let mut encoder = crate::ghostty::MouseEncoder::new().ok()?;
    encoder.set_from_terminal(terminal);
    let cols = terminal.cols().ok()? as u32;
    let rows = terminal.rows().ok()? as u32;
    encoder.set_size(cols, rows, 1, 1);
    Some(encoder)
}

fn ghostty_mouse_event_from_button_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    let (action, button) = match kind {
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        _ => return None,
    };
    event.set_action(action);
    if let Some(button) = button {
        event.set_button(button);
    } else {
        event.clear_button();
    }
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

fn ghostty_mouse_event_from_wheel_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    event.set_action(crate::ghostty::MOUSE_ACTION_PRESS);
    let button = match kind {
        crossterm::event::MouseEventKind::ScrollUp => crate::ghostty::MOUSE_BUTTON_WHEEL_UP,
        crossterm::event::MouseEventKind::ScrollDown => crate::ghostty::MOUSE_BUTTON_WHEEL_DOWN,
        crossterm::event::MouseEventKind::ScrollLeft => crate::ghostty::MOUSE_BUTTON_WHEEL_LEFT,
        crossterm::event::MouseEventKind::ScrollRight => crate::ghostty::MOUSE_BUTTON_WHEEL_RIGHT,
        _ => return None,
    };
    event.set_button(button);
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

fn ghostty_key_text(key: crate::input::TerminalKey) -> Option<String> {
    match key.code {
        crossterm::event::KeyCode::Char(c) => Some(
            key.shifted_codepoint
                .and_then(char::from_u32)
                .unwrap_or(c)
                .to_string(),
        ),
        _ => None,
    }
}

fn ghostty_unshifted_codepoint(key: crate::input::TerminalKey) -> Option<u32> {
    match key.code {
        crossterm::event::KeyCode::Char(c) => Some(c as u32),
        _ => None,
    }
}

fn ghostty_key_from_crossterm_key_code(
    code: crossterm::event::KeyCode,
    shifted_codepoint: Option<u32>,
) -> Option<u32> {
    use crate::ghostty::ffi;
    use crossterm::event::KeyCode;

    match code {
        KeyCode::Backspace => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE),
        KeyCode::Enter => Some(ffi::GhosttyKey_GHOSTTY_KEY_ENTER),
        KeyCode::Left => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT),
        KeyCode::Right => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT),
        KeyCode::Up => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP),
        KeyCode::Down => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN),
        KeyCode::Home => Some(ffi::GhosttyKey_GHOSTTY_KEY_HOME),
        KeyCode::End => Some(ffi::GhosttyKey_GHOSTTY_KEY_END),
        KeyCode::PageUp => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP),
        KeyCode::PageDown => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN),
        KeyCode::Tab | KeyCode::BackTab => Some(ffi::GhosttyKey_GHOSTTY_KEY_TAB),
        KeyCode::Delete => Some(ffi::GhosttyKey_GHOSTTY_KEY_DELETE),
        KeyCode::Insert => Some(ffi::GhosttyKey_GHOSTTY_KEY_INSERT),
        KeyCode::Esc => Some(ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE),
        KeyCode::F(n) => Some(match n {
            1 => ffi::GhosttyKey_GHOSTTY_KEY_F1,
            2 => ffi::GhosttyKey_GHOSTTY_KEY_F2,
            3 => ffi::GhosttyKey_GHOSTTY_KEY_F3,
            4 => ffi::GhosttyKey_GHOSTTY_KEY_F4,
            5 => ffi::GhosttyKey_GHOSTTY_KEY_F5,
            6 => ffi::GhosttyKey_GHOSTTY_KEY_F6,
            7 => ffi::GhosttyKey_GHOSTTY_KEY_F7,
            8 => ffi::GhosttyKey_GHOSTTY_KEY_F8,
            9 => ffi::GhosttyKey_GHOSTTY_KEY_F9,
            10 => ffi::GhosttyKey_GHOSTTY_KEY_F10,
            11 => ffi::GhosttyKey_GHOSTTY_KEY_F11,
            12 => ffi::GhosttyKey_GHOSTTY_KEY_F12,
            _ => return None,
        }),
        KeyCode::Char(c) => ghostty_key_from_char(c, shifted_codepoint),
        _ => None,
    }
}

fn ghostty_key_from_char(c: char, shifted_codepoint: Option<u32>) -> Option<u32> {
    use crate::ghostty::ffi;

    let base = if let Some(shifted) = shifted_codepoint.and_then(char::from_u32) {
        ghostty_unshifted_ascii_pair(shifted).unwrap_or(c)
    } else {
        c
    };

    match base.to_ascii_lowercase() {
        'a' => Some(ffi::GhosttyKey_GHOSTTY_KEY_A),
        'b' => Some(ffi::GhosttyKey_GHOSTTY_KEY_B),
        'c' => Some(ffi::GhosttyKey_GHOSTTY_KEY_C),
        'd' => Some(ffi::GhosttyKey_GHOSTTY_KEY_D),
        'e' => Some(ffi::GhosttyKey_GHOSTTY_KEY_E),
        'f' => Some(ffi::GhosttyKey_GHOSTTY_KEY_F),
        'g' => Some(ffi::GhosttyKey_GHOSTTY_KEY_G),
        'h' => Some(ffi::GhosttyKey_GHOSTTY_KEY_H),
        'i' => Some(ffi::GhosttyKey_GHOSTTY_KEY_I),
        'j' => Some(ffi::GhosttyKey_GHOSTTY_KEY_J),
        'k' => Some(ffi::GhosttyKey_GHOSTTY_KEY_K),
        'l' => Some(ffi::GhosttyKey_GHOSTTY_KEY_L),
        'm' => Some(ffi::GhosttyKey_GHOSTTY_KEY_M),
        'n' => Some(ffi::GhosttyKey_GHOSTTY_KEY_N),
        'o' => Some(ffi::GhosttyKey_GHOSTTY_KEY_O),
        'p' => Some(ffi::GhosttyKey_GHOSTTY_KEY_P),
        'q' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Q),
        'r' => Some(ffi::GhosttyKey_GHOSTTY_KEY_R),
        's' => Some(ffi::GhosttyKey_GHOSTTY_KEY_S),
        't' => Some(ffi::GhosttyKey_GHOSTTY_KEY_T),
        'u' => Some(ffi::GhosttyKey_GHOSTTY_KEY_U),
        'v' => Some(ffi::GhosttyKey_GHOSTTY_KEY_V),
        'w' => Some(ffi::GhosttyKey_GHOSTTY_KEY_W),
        'x' => Some(ffi::GhosttyKey_GHOSTTY_KEY_X),
        'y' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Y),
        'z' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Z),
        '0' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0),
        '1' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_1),
        '2' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_2),
        '3' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_3),
        '4' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_4),
        '5' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_5),
        '6' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_6),
        '7' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_7),
        '8' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_8),
        '9' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_9),
        '`' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKQUOTE),
        '\\' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSLASH),
        '[' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT),
        ']' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT),
        ',' => Some(ffi::GhosttyKey_GHOSTTY_KEY_COMMA),
        '=' => Some(ffi::GhosttyKey_GHOSTTY_KEY_EQUAL),
        '-' => Some(ffi::GhosttyKey_GHOSTTY_KEY_MINUS),
        '.' => Some(ffi::GhosttyKey_GHOSTTY_KEY_PERIOD),
        '\'' => Some(ffi::GhosttyKey_GHOSTTY_KEY_QUOTE),
        ';' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SEMICOLON),
        '/' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SLASH),
        ' ' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SPACE),
        _ => None,
    }
}

fn ghostty_unshifted_ascii_pair(c: char) -> Option<char> {
    Some(match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}

fn ghostty_visible_text(core: &mut GhosttyPaneCore) -> Result<String, crate::ghostty::Error> {
    let GhosttyPaneCore {
        terminal,
        render_state,
        ..
    } = core;
    render_state.update(terminal)?;
    let mut row_iterator = crate::ghostty::RowIterator::new()?;
    let mut row_cells = crate::ghostty::RowCells::new()?;
    let mut rows = render_state.populate_row_iterator(&mut row_iterator)?;
    let mut lines = Vec::new();
    while rows.next() {
        let mut cells = rows.populate_cells(&mut row_cells)?;
        lines.push(ghostty_line_from_cells(&mut cells)?);
    }
    trim_trailing_blank_rows(&mut lines);
    Ok(lines_to_text(lines))
}

fn ghostty_detection_text(core: &GhosttyPaneCore) -> Result<String, crate::ghostty::Error> {
    let lines = core
        .terminal
        .rows()
        .ok()
        .map(|rows| usize::from(rows).max(1))
        .unwrap_or(DEFAULT_DETECTION_ROWS);
    ghostty_recent_text(core, lines)
}

fn ghostty_recent_text(
    core: &GhosttyPaneCore,
    lines: usize,
) -> Result<String, crate::ghostty::Error> {
    let total_rows = core.terminal.total_rows()?;
    let cols = core.terminal.cols()?;
    let start = total_rows.saturating_sub(lines);
    let mut rows = Vec::with_capacity(total_rows.saturating_sub(start));
    for y in start..total_rows {
        rows.push(ghostty_screen_row(core, cols, y as u32)?);
    }
    trim_trailing_blank_rows(&mut rows);
    Ok(recent_text_from_rows(&rows, lines))
}

fn ghostty_recent_text_unwrapped(
    core: &GhosttyPaneCore,
    lines: usize,
) -> Result<String, crate::ghostty::Error> {
    let total_rows = core.terminal.total_rows()?;
    let cols = core.terminal.cols()?;
    if total_rows == 0 || cols == 0 {
        return Ok(String::new());
    }
    let start = total_rows.saturating_sub(lines) as u32;
    let end = (total_rows.saturating_sub(1)) as u32;
    core.terminal
        .read_text_screen((0, start), (cols.saturating_sub(1), end), false)
}

fn ghostty_extract_selection(
    core: &mut GhosttyPaneCore,
    selection: &crate::selection::Selection,
) -> Result<String, crate::ghostty::Error> {
    let ((start_row, start_col), (end_row, end_col)) = selection.ordered_cells();
    core.terminal
        .read_text_screen((start_col, start_row), (end_col, end_row), false)
}

fn ghostty_screen_row(
    core: &GhosttyPaneCore,
    cols: u16,
    y: u32,
) -> Result<String, crate::ghostty::Error> {
    let mut line = String::new();
    for x in 0..cols {
        let graphemes = core.terminal.screen_graphemes(x, y)?;
        if graphemes.is_empty() {
            line.push(' ');
        } else {
            for codepoint in graphemes {
                if let Some(ch) = char::from_u32(codepoint) {
                    line.push(ch);
                }
            }
        }
    }
    Ok(line.trim_end().to_string())
}

fn ghostty_line_from_cells(
    cells: &mut crate::ghostty::RowCellIter<'_>,
) -> Result<String, crate::ghostty::Error> {
    let mut line = String::new();
    while cells.next() {
        line.push_str(&ghostty_cell_symbol(cells)?);
    }
    Ok(line.trim_end().to_string())
}

fn ghostty_cell_symbol(
    cells: &crate::ghostty::RowCellIter<'_>,
) -> Result<String, crate::ghostty::Error> {
    let graphemes = cells.graphemes()?;
    if graphemes.is_empty() {
        return Ok(" ".to_string());
    }
    let mut text = String::new();
    for codepoint in graphemes {
        if let Some(ch) = char::from_u32(codepoint) {
            text.push(ch);
        }
    }
    if text.is_empty() {
        text.push(' ');
    }
    Ok(text)
}

fn ghostty_blank_symbol_for_width(wide: crate::ghostty::CellWide) -> &'static str {
    match wide {
        crate::ghostty::CellWide::Wide => "  ",
        crate::ghostty::CellWide::SpacerTail => "",
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => " ",
    }
}

fn ghostty_normalize_buffer_symbol(symbol: &str, wide: crate::ghostty::CellWide) -> String {
    let expected_width = match wide {
        crate::ghostty::CellWide::Wide => 2,
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => 1,
        crate::ghostty::CellWide::SpacerTail => 0,
    };
    let actual_width = symbol.width();
    if actual_width == expected_width {
        return symbol.to_string();
    }

    if wide == crate::ghostty::CellWide::Narrow && actual_width == 2 {
        return symbol.to_string();
    }

    ghostty_blank_symbol_for_width(wide).to_string()
}

fn ghostty_buffer_symbol(
    cells: &crate::ghostty::RowCellIter<'_>,
    wide: crate::ghostty::CellWide,
) -> Result<String, crate::ghostty::Error> {
    let symbol = match wide {
        crate::ghostty::CellWide::SpacerTail => String::new(),
        crate::ghostty::CellWide::SpacerHead => " ".to_string(),
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::Wide => {
            ghostty_cell_symbol(cells)?
        }
    };
    Ok(ghostty_normalize_buffer_symbol(&symbol, wide))
}

fn ghostty_reset_cell(
    cell: &mut ratatui::buffer::Cell,
    default_fg: Option<Color>,
    default_bg: Option<Color>,
) {
    cell.reset();
    cell.set_symbol(" ");
    if let Some(bg) = default_bg {
        cell.set_bg(bg);
    }
    if let Some(fg) = default_fg {
        cell.set_fg(fg);
    }
}

fn ghostty_cell_style(
    cells: &crate::ghostty::RowCellIter<'_>,
    default_fg: Option<Color>,
    default_bg: Option<Color>,
) -> Style {
    let style_data = cells.style().unwrap_or_default();
    let mut fg = cells
        .fg_color()
        .ok()
        .flatten()
        .map(ghostty_color)
        .or(default_fg);
    let mut bg = cells
        .bg_color()
        .ok()
        .flatten()
        .map(ghostty_color)
        .or(default_bg);
    if style_data.invisible {
        fg = bg.or(default_bg);
    }
    if style_data.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if let Some(bg) = bg {
        style = style.bg(bg);
    }

    let mut modifiers = Modifier::empty();
    if style_data.bold {
        modifiers |= Modifier::BOLD;
    }
    if style_data.italic {
        modifiers |= Modifier::ITALIC;
    }
    if style_data.faint {
        modifiers |= Modifier::DIM;
    }
    if style_data.blink {
        modifiers |= Modifier::SLOW_BLINK;
    }
    if style_data.underlined {
        modifiers |= Modifier::UNDERLINED;
    }
    if style_data.strikethrough {
        modifiers |= Modifier::CROSSED_OUT;
    }
    style.add_modifier(modifiers)
}

fn ghostty_default_bg(
    color: crate::ghostty::RgbColor,
    host_theme: crate::terminal_theme::TerminalTheme,
) -> Option<Color> {
    if host_theme.background == Some(terminal_theme_color(color)) {
        None
    } else {
        Some(ghostty_color(color))
    }
}

fn terminal_theme_color(color: crate::ghostty::RgbColor) -> crate::terminal_theme::RgbColor {
    crate::terminal_theme::RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn ghostty_color(color: crate::ghostty::RgbColor) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn lines_to_text(lines: Vec<String>) -> String {
    let text = lines.join("\n");
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        // Abort detection task immediately.
        // Reader/writer/resize tasks shut down naturally via channel close
        // and PTY EOF when the rest of PaneRuntime is dropped.
        self.detect_handle.abort();
    }
}

fn trim_trailing_blank_rows(rows: &mut Vec<String>) {
    while rows.last().is_some_and(|row| row.trim().is_empty()) {
        rows.pop();
    }
}

fn recent_text_from_rows(rows: &[String], lines: usize) -> String {
    let start = rows.len().saturating_sub(lines);
    let text = rows[start..].join("\n");
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

fn wait_for_processes_to_exit(pids: &[u32], timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if pids
            .iter()
            .all(|pid| !crate::platform::process_exists(*pid))
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn shutdown_pane_processes(pane_id: PaneId, child_pid: u32) {
    if child_pid == 0 {
        return;
    }

    let mut pids = crate::platform::session_processes(child_pid);
    if pids.is_empty() {
        pids.push(child_pid);
    }
    pids.sort_unstable();
    pids.dedup();

    for (signal, grace) in [
        (
            crate::platform::Signal::Hangup,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Terminate,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Kill,
            std::time::Duration::from_millis(250),
        ),
    ] {
        crate::platform::signal_processes(&pids, signal);
        if wait_for_processes_to_exit(&pids, grace) {
            info!(
                pane = pane_id.raw(),
                pid = child_pid,
                ?signal,
                "pane session terminated"
            );
            return;
        }
    }

    warn!(
        pane = pane_id.raw(),
        pid = child_pid,
        pids = ?pids,
        "pane session still alive after forced shutdown"
    );
}

impl PaneRuntime {
    pub fn shutdown(self, pane_id: PaneId) {
        self.detect_handle.abort();
        shutdown_pane_processes(pane_id, self.child_pid.load(Ordering::Acquire));
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.terminal.apply_host_terminal_theme(theme);
    }

    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        crate::integration::apply_pane_env(&mut cmd, pane_id);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn shell",
        )
    }

    pub fn spawn_shell_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        command: &str,
        extra_env: &[(String, String)],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(command);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        crate::integration::apply_pane_env(&mut cmd, pane_id);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn command pane",
        )
    }

    fn spawn_command_builder(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
        cmd: CommandBuilder,
        spawn_error_message: &'static str,
    ) -> std::io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // --- Writer channel ---
        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(32);

        info!(
            pane = pane_id.raw(),
            rows, cols, scrollback_limit_bytes, "spawning pane terminal"
        );

        let terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let pane_terminal = GhosttyPaneTerminal::new(terminal, input_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let terminal = Arc::new(PaneTerminal {
            ghostty: pane_terminal,
        });
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // --- Child watcher task ---
        let child_pid = Arc::new(AtomicU32::new(0));
        {
            let child_pid = child_pid.clone();
            let slave = pair.slave;
            let events = events.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                match slave.spawn_command(cmd) {
                    Ok(mut child) => {
                        if let Some(pid) = child.process_id() {
                            child_pid.store(pid, Ordering::Release);
                            info!(pane = pane_id.raw(), pid, "child spawned");
                        }
                        match child.wait() {
                            Ok(status) => info!(pane = pane_id.raw(), ?status, "child exited"),
                            Err(e) => error!(pane = pane_id.raw(), err = %e, "child wait failed"),
                        }
                    }
                    Err(e) => error!(pane = pane_id.raw(), err = %e, "{spawn_error_message}"),
                }
                // Use blocking send — PaneDied is critical, must not be dropped
                if let Err(e) = rt.block_on(events.send(AppEvent::PaneDied { pane_id })) {
                    error!(pane = pane_id.raw(), err = %e, "failed to send PaneDied event");
                }
            });
        }

        // --- Reader task: PTY → terminal backend + screen snapshot + terminal query responses ---
        {
            let mut reader = reader;
            let terminal = terminal.clone();
            let response_writer = input_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Err(e) => {
                            debug!(pane = pane_id.raw(), err = %e, "pty reader closed");
                            break;
                        }
                        Ok(n) => {
                            let shell_pid = child_pid.load(Ordering::Acquire);
                            let result = terminal.process_pty_bytes(
                                pane_id,
                                shell_pid,
                                &buf[..n],
                                &response_writer,
                            );
                            if result.request_render && !render_dirty.swap(true, Ordering::AcqRel) {
                                render_notify.notify_one();
                            }
                            for content in result.clipboard_writes {
                                if let Err(err) =
                                    rt.block_on(events.send(AppEvent::ClipboardWrite { content }))
                                {
                                    warn!(
                                        pane = pane_id.raw(),
                                        err = %err,
                                        "failed to send OSC 52 clipboard write"
                                    );
                                }
                            }
                        }
                    }
                }
                debug!(pane = pane_id.raw(), "reader task exiting");
            });
        }

        // --- Detection task ---
        let (detect_handle, detect_reset_notify, pending_release) = {
            use crate::detect;
            use std::time::{Duration, Instant};

            const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
            const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
            const TICK_PENDING_RELEASE: Duration = Duration::from_millis(50);
            const PROCESS_RECHECK: Duration = Duration::from_secs(5);

            let child_pid = child_pid.clone();
            let terminal = terminal.clone();
            let state_events = events.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detect_reset_notify = Arc::new(Notify::new());
            let detect_reset = detect_reset_notify.clone();
            let pending_release = Arc::new(Mutex::new(None));
            let pending_release_for_task = pending_release.clone();

            let handle = tokio::spawn(async move {
                let mut agent_presence = AgentDetectionPresence::from_agent(None);
                let mut state = AgentState::Unknown;
                let mut last_process_check = Instant::now();
                let mut last_claude_working_at = None;

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let tick = if active_pending_release(&pending_release_for_task, Instant::now())
                        .is_some()
                        || terminal.has_transient_default_color_override()
                    {
                        TICK_PENDING_RELEASE
                    } else if agent_presence.current_agent().is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(tick) => {}
                        _ = detect_reset.notified() => {
                            agent_presence = AgentDetectionPresence::from_agent(None);
                            state = AgentState::Unknown;
                            last_claude_working_at = None;
                        }
                    }

                    let now = Instant::now();
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    let should_check_process = suppressed_agent.is_some()
                        || agent_presence.current_agent().is_none()
                        || now.duration_since(last_process_check) >= PROCESS_RECHECK;

                    let mut agent_changed = false;
                    let mut agent = agent_presence.current_agent();
                    if should_check_process {
                        last_process_check = now;
                        let pid = child_pid.load(Ordering::Acquire);
                        if pid > 0 {
                            let mut process_name = None;
                            let mut process_group_id = None;
                            let mut new_agent = None;

                            if let Some(job) = detect::foreground_job(pid) {
                                process_group_id = Some(job.process_group_id);
                                let identified = detect::identify_agent_in_job(&job);
                                process_name = identified
                                    .as_ref()
                                    .map(|(_, process_name)| process_name.clone());
                                new_agent = identified.as_ref().map(|(agent, _)| *agent);
                            }

                            if let Some(suppressed_agent) = suppressed_agent {
                                if new_agent == Some(suppressed_agent) {
                                    new_agent = None;
                                } else if let Ok(mut pending_release) =
                                    pending_release_for_task.lock()
                                {
                                    *pending_release = None;
                                }
                            }

                            let previous_agent = agent_presence.current_agent();
                            if agent_presence.observe_process_probe(new_agent) {
                                agent = agent_presence.current_agent();
                                if let Some(process_name) = process_name {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        process = %process_name,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                } else {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                }
                                agent_changed = true;
                            }
                        }
                    }

                    let pid = child_pid.load(Ordering::Acquire);
                    if pid > 0 && terminal.maybe_restore_host_terminal_theme(pane_id, pid) {
                        if !render_dirty.swap(true, Ordering::AcqRel) {
                            render_notify.notify_one();
                        }
                    }

                    let content = terminal.detection_text();
                    let raw_state = detect::detect_state(agent, &content);
                    let new_state = stabilize_agent_state(
                        agent,
                        state,
                        raw_state,
                        now,
                        &mut last_claude_working_at,
                    );

                    if new_state != state || agent_changed {
                        debug!(
                            pane = pane_id.raw(),
                            ?state,
                            ?raw_state,
                            ?new_state,
                            ?agent,
                            "state changed"
                        );
                        state = new_state;
                        publish_state_changed_event(
                            state_events.clone(),
                            pane_id,
                            agent,
                            new_state,
                        )
                        .await;
                    }
                }
            });
            (handle.abort_handle(), detect_reset_notify, pending_release)
        };

        // --- Writer task: channel → PTY ---
        {
            let mut writer = BufWriter::new(writer);
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some(bytes) = rt.block_on(input_rx.recv()) {
                    if let Err(e) = writer.write_all(&bytes) {
                        warn!(pane = pane_id.raw(), err = %e, "pty write failed");
                        break;
                    }
                    if let Err(e) = writer.flush() {
                        warn!(pane = pane_id.raw(), err = %e, "pty flush failed");
                        break;
                    }
                }
                debug!(pane = pane_id.raw(), "writer task exiting");
            });
        }

        // --- Resize task ---
        let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(4);
        {
            let master = pair.master;
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some((rows, cols)) = rt.block_on(resize_rx.recv()) {
                    if let Err(e) = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    }) {
                        warn!(pane = pane_id.raw(), err = %e, rows, cols, "pty resize failed");
                    }
                }
            });
        }

        Ok(Self {
            terminal,
            sender: input_tx,
            resize_tx,
            current_size: Cell::new((rows, cols)),
            child_pid,
            kitty_keyboard_flags,
            detect_reset_notify,
            pending_release,
            detect_handle,
        })
    }

    pub fn begin_graceful_release(&self, agent: Agent) {
        if let Ok(mut pending_release) = self.pending_release.lock() {
            *pending_release = Some(PendingAgentRelease {
                agent,
                until: std::time::Instant::now() + RELEASE_REACQUIRE_SUPPRESSION,
            });
        }
        self.detect_reset_notify.notify_one();
    }

    /// Resize if the dimensions actually changed.
    pub fn resize(&self, rows: u16, cols: u16) {
        let rows = rows.max(2);
        let cols = cols.max(4);
        if self.current_size.get() == (rows, cols) {
            return;
        }
        self.current_size.set((rows, cols));
        self.terminal.resize(rows, cols);
        let _ = self.resize_tx.try_send((rows, cols));
    }

    /// Scroll up by N lines (into scrollback history).
    pub fn scroll_up(&self, lines: usize) {
        self.terminal.scroll_up(lines);
    }

    /// Scroll down by N lines (toward live output).
    pub fn scroll_down(&self, lines: usize) {
        self.terminal.scroll_down(lines);
    }

    /// Reset scroll to live view (offset = 0).
    pub fn scroll_reset(&self) {
        self.terminal.scroll_reset();
    }

    /// Set scrollback offset measured from the live bottom of the terminal.
    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.terminal.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.terminal.scroll_metrics()
    }

    pub fn input_state(&self) -> Option<InputState> {
        self.terminal.input_state()
    }

    pub fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.terminal.recent_text(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_text(lines)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.terminal.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.terminal.render(frame, area, show_cursor);
    }

    pub fn keyboard_protocol(&self) -> crate::input::KeyboardProtocol {
        let fallback = crate::input::KeyboardProtocol::from_kitty_flags(
            self.kitty_keyboard_flags.load(Ordering::Relaxed),
        );
        self.terminal.keyboard_protocol(fallback)
    }

    pub fn encode_terminal_key(&self, key: crate::input::TerminalKey) -> Vec<u8> {
        self.terminal
            .encode_terminal_key(key, self.keyboard_protocol())
    }

    pub async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.sender.send(bytes).await
    }

    pub fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        self.sender.try_send(bytes)
    }

    pub async fn send_paste(&self, text: String) -> Result<(), mpsc::error::SendError<Bytes>> {
        let bracketed = self
            .input_state()
            .map(|state| state.bracketed_paste)
            .unwrap_or(false);
        let payload = if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        self.send_bytes(Bytes::from(payload)).await
    }

    pub fn try_send_focus_event(&self, event: crate::ghostty::FocusEvent) -> bool {
        if !self
            .input_state()
            .map(|state| state.focus_reporting)
            .unwrap_or(false)
        {
            return false;
        }

        let Ok(bytes) = crate::ghostty::encode_focus(event) else {
            return false;
        };
        if let Err(err) = self.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, ?event, "failed to forward pane focus event");
        }
        true
    }

    pub fn wheel_routing(&self) -> Option<WheelRouting> {
        let input_state = self.input_state()?;
        Some(if input_state.mouse_reporting_enabled() {
            WheelRouting::MouseReport
        } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
            WheelRouting::AlternateScroll
        } else {
            WheelRouting::HostScroll
        })
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if !self.input_state()?.mouse_protocol_mode.reporting_enabled() {
            return None;
        }
        self.terminal
            .encode_mouse_button(kind, column, row, modifiers)
    }

    pub fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::MouseReport {
            return None;
        }
        self.terminal
            .encode_mouse_wheel(kind, column, row, modifiers)
    }

    pub fn encode_alternate_scroll(
        &self,
        kind: crossterm::event::MouseEventKind,
    ) -> Option<Vec<u8>> {
        self.input_state()?;
        if self.wheel_routing()? != WheelRouting::AlternateScroll {
            return None;
        }
        let key = match kind {
            crossterm::event::MouseEventKind::ScrollUp => crossterm::event::KeyCode::Up,
            crossterm::event::MouseEventKind::ScrollDown => crossterm::event::KeyCode::Down,
            _ => return None,
        };
        Some(self.encode_terminal_key(crate::input::TerminalKey::new(
            key,
            crossterm::event::KeyModifiers::empty(),
        )))
    }

    /// Get the current working directory of the child shell process.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child_pid.load(Ordering::Relaxed);
        crate::platform::process_cwd(pid)
    }
}

#[cfg(test)]
impl PaneRuntime {
    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[])
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self::test_with_scrollback_bytes(cols, rows, 0, bytes)
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, scrollback_limit_bytes, bytes).0
    }

    fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let mut terminal =
            crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes).unwrap();
        terminal.write(bytes);

        (
            Self {
                terminal: Arc::new(PaneTerminal {
                    ghostty: GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
                }),
                sender: tx,
                resize_tx,
                current_size: Cell::new((rows, cols)),
                child_pid: Arc::new(AtomicU32::new(0)),
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                detect_reset_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                detect_handle: tokio::spawn(async {}).abort_handle(),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_numbered_lines(terminal: &mut crate::ghostty::Terminal, count: usize) {
        for i in 0..count {
            terminal.write(format!("{i:06}\r\n").as_bytes());
        }
    }

    fn pane_default_theme(pane: &GhosttyPaneTerminal) -> crate::terminal_theme::TerminalTheme {
        let mut core = pane.core.lock().unwrap();
        let GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        render_state.update(terminal).unwrap();
        let colors = render_state.colors().unwrap();
        crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: colors.foreground.r,
                g: colors.foreground.g,
                b: colors.foreground.b,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: colors.background.r,
                g: colors.background.g,
                b: colors.background.b,
            }),
        }
    }

    fn shell_job(shell_pid: u32) -> crate::platform::ForegroundJob {
        crate::platform::ForegroundJob {
            process_group_id: shell_pid,
            processes: vec![crate::platform::ForegroundProcess {
                pid: shell_pid,
                name: "zsh".to_string(),
                argv0: Some("zsh".to_string()),
                cmdline: Some("zsh".to_string()),
            }],
        }
    }

    #[test]
    fn ghostty_render_can_suppress_cursor_position() {
        let (tx, _rx) = mpsc::channel(4);
        let mut first_terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        first_terminal.write(b"left");
        let first = GhosttyPaneTerminal::new(first_terminal, tx.clone()).unwrap();

        let mut second_terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        second_terminal.write(b"r\r\nb");
        let second = GhosttyPaneTerminal::new(second_terminal, tx).unwrap();

        let backend = ratatui::backend::TestBackend::new(40, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                first.render(frame, Rect::new(0, 0, 20, 5), true);
                second.render(frame, Rect::new(20, 0, 20, 5), false);
            })
            .unwrap();

        terminal.backend_mut().assert_cursor_position((4, 0));
    }

    #[test]
    fn ghostty_keyboard_protocol_tracks_live_terminal_flags() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[>3u");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        assert_eq!(
            pane.keyboard_protocol(),
            Some(crate::input::KeyboardProtocol::Kitty { flags: 3 })
        );
    }

    #[test]
    fn ghostty_plain_text_chars_still_encode_as_text() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );

        assert_eq!(encoded, b"a");
    }

    #[test]
    fn ghostty_char_keys_still_use_herdr_encoding() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[>1u");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
            ),
            crate::input::KeyboardProtocol::Legacy,
        );

        assert_eq!(encoded, vec![1]);
    }

    #[test]
    fn ghostty_key_encoding_honors_application_cursor_mode() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_APPLICATION_CURSOR_KEYS, true)
            .unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );

        assert_eq!(encoded, b"\x1bOA");
    }

    #[test]
    fn ghostty_key_encoder_updates_after_terminal_mode_changes() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let before = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );
        assert_eq!(before, b"\x1b[A");

        pane.process_pty_bytes(pane_id, 0, b"\x1b[?1h", &tx);

        let after = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );
        assert_eq!(after, b"\x1bOA");
    }

    #[test]
    fn ghostty_key_encoder_updates_after_kitty_flag_changes() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
        );

        let before = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);
        pane.process_pty_bytes(pane_id, 0, b"\x1b[>1u", &tx);
        let after = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

        assert_ne!(before, after);
        assert_eq!(after, b"\x1b[13;6u");
    }

    #[test]
    fn ghostty_key_encoders_are_isolated_per_pane() {
        let (tx, _rx) = mpsc::channel(4);
        let first = GhosttyPaneTerminal::new(
            crate::ghostty::Terminal::new(80, 24, 0).unwrap(),
            tx.clone(),
        )
        .unwrap();
        let second = GhosttyPaneTerminal::new(
            crate::ghostty::Terminal::new(80, 24, 0).unwrap(),
            tx.clone(),
        )
        .unwrap();

        first.process_pty_bytes(PaneId::from_raw(1), 0, b"\x1b[?1h", &tx);

        let first_encoded = first.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );
        let second_encoded = second.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );

        assert_eq!(first_encoded, b"\x1bOA");
        assert_eq!(second_encoded, b"\x1b[A");
    }

    #[test]
    fn ghostty_mouse_button_encoding_uses_live_terminal_state() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1000h\x1b[?1006h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_mouse_button(
            crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            11,
            9,
            crossterm::event::KeyModifiers::empty(),
        );

        assert_eq!(encoded.as_deref(), Some(&b"\x1b[<0;12;10m"[..]));
    }

    #[test]
    fn ghostty_mouse_drag_encoding_uses_motion_reporting_state() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1002h\x1b[?1006h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_mouse_button(
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            4,
            6,
            crossterm::event::KeyModifiers::SHIFT,
        );

        assert_eq!(encoded.as_deref(), Some(&b"\x1b[<36;5;7M"[..]));
    }

    #[test]
    fn ghostty_normalize_buffer_symbol_prefers_grapheme_width_when_metadata_disagrees() {
        const WIDE_GRAPHEME: &str = "🙂";
        const VS16_GRAPHEME: &str = "⚠️";
        const EMOJI_GRAPHEME: &str = "💳";

        assert_eq!(
            ghostty_normalize_buffer_symbol(WIDE_GRAPHEME, crate::ghostty::CellWide::Wide),
            WIDE_GRAPHEME
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol("a", crate::ghostty::CellWide::Wide),
            "  "
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol("⌨️", crate::ghostty::CellWide::Narrow),
            "⌨️"
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol(VS16_GRAPHEME, crate::ghostty::CellWide::Narrow),
            VS16_GRAPHEME
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol(EMOJI_GRAPHEME, crate::ghostty::CellWide::Narrow),
            EMOJI_GRAPHEME
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol(" ", crate::ghostty::CellWide::SpacerTail),
            ""
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol("xx", crate::ghostty::CellWide::SpacerHead),
            " "
        );
    }

    #[test]
    fn pane_scrollback_controls_reach_top_without_ui_interference() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 3, 100).unwrap();
        write_numbered_lines(&mut terminal, 1000);
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let before = pane.scroll_metrics().expect("scroll metrics before scroll");
        assert!(before.max_offset_from_bottom > 0);
        assert_eq!(before.offset_from_bottom, 0);

        pane.set_scroll_offset_from_bottom(before.max_offset_from_bottom);

        let after = pane.scroll_metrics().expect("scroll metrics after scroll");
        assert_eq!(after.offset_from_bottom, after.max_offset_from_bottom);
        assert!(pane.visible_text().contains("000000"));
    }

    #[test]
    fn detection_text_stays_at_bottom_when_viewport_is_scrolled() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 3, 100).unwrap();
        write_numbered_lines(&mut terminal, 10);
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let bottom_snapshot = pane.detection_text();
        assert_eq!(bottom_snapshot, pane.recent_text(3));
        assert!(bottom_snapshot.contains("000009"));

        let before = pane.scroll_metrics().expect("scroll metrics before scroll");
        pane.set_scroll_offset_from_bottom(before.max_offset_from_bottom);

        assert!(pane.visible_text().contains("000000"));
        assert_eq!(pane.detection_text(), bottom_snapshot);
    }

    #[test]
    fn extract_selection_reads_screen_rows_not_current_viewport() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(8, 3, 1024).unwrap();
        write_numbered_lines(&mut terminal, 8);
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        pane.set_scroll_offset_from_bottom(3);
        let metrics = pane
            .scroll_metrics()
            .expect("scroll metrics after initial scroll");
        let mut selection =
            crate::selection::Selection::anchor(PaneId::from_raw(1), 0, 0, Some(metrics));
        selection.drag(5, 2, Rect::new(0, 0, 8, 3), Some(metrics));

        pane.scroll_reset();

        let text = pane
            .extract_selection(&selection)
            .expect("selection should extract text");
        assert_eq!(text, "000003\n000004\n000005");
    }

    #[test]
    fn recent_unwrapped_text_ignores_soft_wraps() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(5, 3, 100).unwrap();
        terminal.write(b"ABCDEFGHIJ");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        assert_eq!(pane.recent_text(3), "ABCDE\nFGHIJ\n");
        assert_eq!(pane.recent_unwrapped_text(3), "ABCDEFGHIJ");
    }

    #[test]
    fn synchronized_output_suppresses_intermediate_render_requests_until_batch_ends() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane_terminal = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let begin = pane_terminal.process_pty_bytes(pane_id, 0, b"\x1b[?2026h", &tx);
        assert!(!begin.request_render);

        let body = pane_terminal.process_pty_bytes(pane_id, 0, b"hello", &tx);
        assert!(!body.request_render);

        let end = pane_terminal.process_pty_bytes(pane_id, 0, b"\x1b[?2026l", &tx);
        assert!(end.request_render);
    }

    #[test]
    fn default_color_tracker_detects_split_osc_11_sequences() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]11;rgb:11/22"));
        assert!(tracker.observe(b"/33\x1b\\"));
    }

    #[test]
    fn default_color_tracker_ignores_osc_queries() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]10;?\x1b\\"));
        assert!(!tracker.observe(b"\x1b]11;?\x07"));
    }

    #[test]
    fn osc52_forwarder_detects_write_with_bel() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVsbG8=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_detects_write_with_st() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVsbG8=\x1b\\");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_detects_empty_selector_form() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;;aGVsbG8=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_accepts_clear_clipboard() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![Vec::<u8>::new()]);
    }

    #[test]
    fn osc52_forwarder_ignores_query() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;?\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_empty_selector_query() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;;?\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_other_kinds() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;p;aGk=\x07");
        fw.observe(b"\x1b]52;s;aGk=\x07");
        fw.observe(b"\x1b]52;q;aGk=\x07");
        fw.observe(b"\x1b]52;0;aGk=\x07");
        fw.observe(b"\x1b]52;7;aGk=\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_invalid_base64() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;%%%\x07");
        fw.observe(b"\x1b]52;c;aGVs\x1b[bG8=\x07");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_ignores_non_osc52() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]11;?\x07");
        fw.observe(b"\x1b]0;title\x07");
        fw.observe(b"\x1b]8;;https://example.com\x1b\\");
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn osc52_forwarder_handles_split_sequence_mid_payload() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGVs");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"bG8gd29y");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"bGQ=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hello world".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_handles_split_before_bel() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_handles_split_between_esc_and_backslash() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x1b");
        assert!(fw.drain_pending().is_empty());
        fw.observe(b"\\");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_payload_size_limit() {
        let mut fw = Osc52Forwarder::default();
        let mut huge = Vec::with_capacity(OSC52_MAX_PAYLOAD_BYTES + 32);
        huge.extend_from_slice(b"\x1b]52;c;");
        huge.extend(std::iter::repeat_n(b'A', OSC52_MAX_PAYLOAD_BYTES + 16));
        huge.push(0x07);
        fw.observe(&huge);
        assert!(fw.drain_pending().is_empty());

        fw.observe(b"\x1b]52;c;aGk=\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_recovers_after_garbage() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x01\x02random\x7fbytes\x1b]52;c;aGk=\x07tail");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_multiple_in_one_chunk() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x07\x1b]52;c;Ynll\x07");
        let pending = fw.drain_pending();
        assert_eq!(pending, vec![b"hi".to_vec(), b"bye".to_vec()]);
    }

    #[test]
    fn osc52_forwarder_drain_clears_pending() {
        let mut fw = Osc52Forwarder::default();
        fw.observe(b"\x1b]52;c;aGk=\x07");
        assert_eq!(fw.drain_pending(), vec![b"hi".to_vec()]);
        assert!(fw.drain_pending().is_empty());
    }

    #[test]
    fn droid_scrollback_compat_matches_process_name_and_cmdline() {
        let name_only = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "droid".to_string(),
                argv0: None,
                cmdline: Some("/opt/factory/droid --resume".to_string()),
            }],
        };
        assert!(foreground_job_uses_droid_scrollback_compat(&name_only));

        let cmdline_only = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "bun".to_string(),
                argv0: Some("bun".to_string()),
                cmdline: Some("/home/can/.local/bin/droid --resume".to_string()),
            }],
        };
        assert!(foreground_job_uses_droid_scrollback_compat(&cmdline_only));

        let shell = shell_job(7);
        assert!(!foreground_job_uses_droid_scrollback_compat(&shell));
    }

    #[test]
    fn strip_scrollback_clear_sequences_removes_ed3_only() {
        let filtered = strip_scrollback_clear_sequences(b"a\x1b[3Jb\x1b[?3Jc\x1b[2Jd");
        assert_eq!(filtered.as_ref(), b"abc\x1b[2Jd");
    }

    #[test]
    fn primary_screen_droid_compat_ignores_scrollback_clear_only_for_droid() {
        let droid_job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![crate::platform::ForegroundProcess {
                pid: 42,
                name: "droid".to_string(),
                argv0: Some("droid".to_string()),
                cmdline: Some("droid".to_string()),
            }],
        };

        let filtered = maybe_filter_primary_screen_scrollback_clear(
            b"\x1b[3J\x1b[2J",
            false,
            Some(&droid_job),
        );
        assert_eq!(filtered.as_ref(), b"\x1b[2J");

        let shell = maybe_filter_primary_screen_scrollback_clear(
            b"\x1b[3J\x1b[2J",
            false,
            Some(&shell_job(7)),
        );
        assert_eq!(shell.as_ref(), b"\x1b[3J\x1b[2J");

        let alternate =
            maybe_filter_primary_screen_scrollback_clear(b"\x1b[3J\x1b[2J", true, Some(&droid_job));
        assert_eq!(alternate.as_ref(), b"\x1b[3J\x1b[2J");
    }

    #[test]
    fn host_theme_restore_waits_for_shell_and_non_alternate_screen() {
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            true,
            Some(&shell_job(7)),
        ));
        assert!(!should_restore_host_terminal_theme(42, 7, false, None));
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&crate::platform::ForegroundJob {
                process_group_id: 42,
                processes: vec![crate::platform::ForegroundProcess {
                    pid: 42,
                    name: "droid".to_string(),
                    argv0: Some("droid".to_string()),
                    cmdline: Some("droid".to_string()),
                }],
            }),
        ));
        assert!(should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(target_os = "macos")]
        assert!(should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(not(target_os = "macos"))]
        assert!(!should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));
    }

    #[test]
    fn restore_host_terminal_theme_reapplies_cached_colors() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let pane_id = PaneId::from_raw(1);
        let shell_pid = 7;
        let host_theme = crate::terminal_theme::TerminalTheme {
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
        };

        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.terminal.write(b"\x1b]11;rgb:dd/ee/ff\x1b\\");
        }
        assert_eq!(
            pane_default_theme(&pane).background,
            Some(crate::terminal_theme::RgbColor {
                r: 0xdd,
                g: 0xee,
                b: 0xff,
            })
        );

        {
            let mut core = pane.core.lock().unwrap();
            assert!(restore_host_terminal_theme_if_needed(
                &mut core,
                pane_id,
                shell_pid,
                false,
                Some(&shell_job(shell_pid)),
            ));
        }

        assert_eq!(pane_default_theme(&pane).background, host_theme.background);
        assert_eq!(pane_default_theme(&pane).foreground, host_theme.foreground);
    }

    #[test]
    fn render_leaves_host_default_background_transparent() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let host_theme = crate::terminal_theme::TerminalTheme {
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
        };
        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"hi");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "h");
        assert_eq!(buffer[(0, 0)].style().bg, Some(Color::Reset));
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().bg, Some(Color::Reset));
    }

    #[test]
    fn render_keeps_explicit_default_background_when_it_differs_from_host() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let host_theme = crate::terminal_theme::TerminalTheme {
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
        };
        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b]11;rgb:44/55/66\x1b\\hi");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let expected_bg = Some(Color::Rgb(0x44, 0x55, 0x66));
        assert_eq!(buffer[(0, 0)].symbol(), "h");
        assert_eq!(buffer[(0, 0)].style().bg, expected_bg);
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().bg, expected_bg);
    }

    #[tokio::test]
    async fn focus_events_are_forwarded_when_enabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
            .unwrap();
        let runtime = PaneRuntime {
            terminal: Arc::new(PaneTerminal {
                ghostty: GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            }),
            sender: tx,
            resize_tx,
            current_size: Cell::new((80, 24)),
            child_pid: Arc::new(AtomicU32::new(0)),
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"\x1b[I"));
    }

    #[tokio::test]
    async fn focus_events_are_suppressed_when_disabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let runtime = PaneRuntime {
            terminal: Arc::new(PaneTerminal {
                ghostty: GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            }),
            sender: tx,
            resize_tx,
            current_size: Cell::new((80, 24)),
            child_pid: Arc::new(AtomicU32::new(0)),
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(!runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
                .await
                .is_err()
        );
    }

    #[test]
    fn trim_trailing_blank_rows_drops_empty_viewport_tail() {
        let mut rows = vec!["hello".to_string(), "".to_string(), "   ".to_string()];
        trim_trailing_blank_rows(&mut rows);
        assert_eq!(rows, vec!["hello".to_string()]);
    }

    #[test]
    fn transient_process_miss_keeps_current_agent_detected() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        let changed = presence.observe_process_probe(None);

        assert!(!changed, "one miss should not clear the detected agent");
        assert_eq!(presence.current_agent(), Some(Agent::Pi));
    }

    #[test]
    fn agent_only_clears_after_confirmation_misses() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        for attempt in 1..AGENT_MISS_CONFIRMATION_ATTEMPTS {
            let changed = presence.observe_process_probe(None);
            assert!(
                !changed,
                "miss {attempt} should stay in the confirmation window"
            );
            assert_eq!(presence.current_agent(), Some(Agent::Pi));
        }

        let changed = presence.observe_process_probe(None);
        assert!(changed, "last confirmation miss should clear the agent");
        assert_eq!(presence.current_agent(), None);
    }
    #[tokio::test]
    async fn state_changed_event_waits_for_queue_space_instead_of_dropping() {
        let (tx, mut rx) = mpsc::channel(1);
        let pane_id = PaneId::from_raw(42);

        tx.try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
        })
        .unwrap();

        let publish =
            publish_state_changed_event(tx.clone(), pane_id, Some(Agent::Pi), AgentState::Idle);
        tokio::pin!(publish);

        let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), async {
            (&mut publish).await;
        })
        .await;
        assert!(
            blocked.is_err(),
            "publisher should wait for queue space instead of dropping StateChanged"
        );

        let first = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield first event")
            .expect("sender still alive");
        assert!(matches!(first, AppEvent::UpdateReady { .. }));

        tokio::time::timeout(std::time::Duration::from_millis(50), async {
            (&mut publish).await;
        })
        .await
        .expect("publisher should complete once queue space is available");

        let second = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield second event")
            .expect("sender still alive");
        assert!(matches!(
            second,
            AppEvent::StateChanged {
                pane_id: delivered_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
            } if delivered_pane == pane_id
        ));
    }
}
