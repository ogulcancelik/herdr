use std::cell::Cell;
use std::io::{BufWriter, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex, RwLock,
};

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
#[cfg(feature = "ghostty-vt")]
use ratatui::style::{Color, Modifier, Style};
use ratatui::{layout::Rect, Frame};
use tokio::sync::{mpsc, Notify};
use tracing::{debug, error, info, warn};
use tui_term::widget::PseudoTerminal;
#[cfg(feature = "ghostty-vt")]
use unicode_width::UnicodeWidthStr;

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::pty_callbacks::PtyResponses;

const CLAUDE_WORKING_HOLD: std::time::Duration = std::time::Duration::from_millis(1200);
const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);

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

fn stabilize_agent_state(
    agent: Option<Agent>,
    previous: AgentState,
    raw: AgentState,
    now: std::time::Instant,
    last_claude_working_at: &mut Option<std::time::Instant>,
) -> AgentState {
    if agent != Some(Agent::Claude) {
        return raw;
    }

    match raw {
        AgentState::Working => {
            *last_claude_working_at = Some(now);
            AgentState::Working
        }
        AgentState::Blocked => AgentState::Blocked,
        AgentState::Idle if previous == AgentState::Working => {
            if last_claude_working_at
                .is_some_and(|last_working| now.duration_since(last_working) < CLAUDE_WORKING_HOLD)
            {
                AgentState::Working
            } else {
                AgentState::Idle
            }
        }
        _ => raw,
    }
}

// ---------------------------------------------------------------------------
// PaneState — pure data, constructable without PTYs, testable
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent: Agent,
    pub state: AgentState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub agent: Option<Agent>,
    pub state: AgentState,
}

/// Observable state for a single pane.
/// This is the only part of a pane that workspace logic and tests need.
pub struct PaneState {
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    pub hook_authority: Option<HookAuthority>,
    pub state: AgentState,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
}

impl PaneState {
    pub fn new() -> Self {
        Self {
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            hook_authority: None,
            state: AgentState::Unknown,
            seen: true,
        }
    }

    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        let previous_agent = self.detected_agent;
        let previous_state = self.state;
        self.detected_agent = agent;
        self.fallback_state = fallback_state;
        if self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| Some(authority.agent) != self.detected_agent)
        {
            self.hook_authority = None;
        }
        self.recompute_effective_state(previous_agent, previous_state)
    }

    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent: Agent,
        state: AgentState,
        message: Option<String>,
    ) -> Option<EffectiveStateChange> {
        let previous_agent = self.detected_agent;
        let previous_state = self.state;
        self.hook_authority = Some(HookAuthority {
            source,
            agent,
            state,
            message,
        });
        self.recompute_effective_state(previous_agent, previous_state)
    }

    pub fn clear_hook_authority(&mut self, source: Option<&str>) -> Option<EffectiveStateChange> {
        let previous_agent = self.detected_agent;
        let previous_state = self.state;
        let should_clear = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        if !should_clear {
            return None;
        }
        self.hook_authority = None;
        self.recompute_effective_state(previous_agent, previous_state)
    }

    pub fn release_agent(&mut self, source: &str, agent: Agent) -> Option<EffectiveStateChange> {
        if self.detected_agent != Some(agent) {
            return None;
        }

        if self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| authority.agent != agent || authority.source != source)
        {
            return None;
        }

        let previous_agent = self.detected_agent;
        let previous_state = self.state;
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.hook_authority = None;
        self.recompute_effective_state(previous_agent, previous_state)
    }

    fn recompute_effective_state(
        &mut self,
        previous_agent: Option<Agent>,
        previous_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        let state = self
            .hook_authority
            .as_ref()
            .filter(|authority| Some(authority.agent) == self.detected_agent)
            .map(|authority| authority.state)
            .unwrap_or(self.fallback_state);

        if previous_agent == self.detected_agent && previous_state == state {
            return None;
        }

        self.state = state;
        Some(EffectiveStateChange {
            previous_agent,
            previous_state,
            agent: self.detected_agent,
            state,
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessBytesResult {
    request_render: bool,
}

#[cfg_attr(feature = "ghostty-vt", allow(dead_code))]
struct VtPaneTerminal {
    parser: Arc<RwLock<vt100::Parser<PtyResponses>>>,
    screen_content: Arc<RwLock<String>>,
    mouse_alternate_scroll: Arc<AtomicBool>,
}

#[cfg(feature = "ghostty-vt")]
struct GhosttyPaneTerminal {
    core: Mutex<GhosttyPaneCore>,
}

#[cfg(feature = "ghostty-vt")]
struct GhosttyPaneCore {
    terminal: crate::ghostty::Terminal,
    render_state: crate::ghostty::RenderState,
}

enum PaneTerminal {
    #[cfg_attr(feature = "ghostty-vt", allow(dead_code))]
    Vt(VtPaneTerminal),
    #[cfg(feature = "ghostty-vt")]
    Ghostty(GhosttyPaneTerminal),
}

impl PaneTerminal {
    fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        match self {
            Self::Vt(vt) => vt.process_pty_bytes(pane_id, bytes, response_writer),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.process_pty_bytes(pane_id, bytes, response_writer),
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        match self {
            Self::Vt(vt) => vt.resize(rows, cols),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.resize(rows, cols),
        }
    }

    fn scroll_up(&self, lines: usize) {
        match self {
            Self::Vt(vt) => vt.scroll_up(lines),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.scroll_up(lines),
        }
    }

    fn scroll_down(&self, lines: usize) {
        match self {
            Self::Vt(vt) => vt.scroll_down(lines),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.scroll_down(lines),
        }
    }

    fn scroll_reset(&self) {
        match self {
            Self::Vt(vt) => vt.scroll_reset(),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.scroll_reset(),
        }
    }

    fn set_scroll_offset_from_bottom(&self, lines: usize) {
        match self {
            Self::Vt(vt) => vt.set_scroll_offset_from_bottom(lines),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.set_scroll_offset_from_bottom(lines),
        }
    }

    fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        match self {
            Self::Vt(vt) => vt.scroll_metrics(),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.scroll_metrics(),
        }
    }

    fn input_state(&self) -> Option<InputState> {
        match self {
            Self::Vt(vt) => vt.input_state(),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.input_state(),
        }
    }

    fn visible_text(&self) -> String {
        match self {
            Self::Vt(vt) => vt.visible_text(),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.visible_text(),
        }
    }

    fn recent_text(&self, lines: usize) -> String {
        match self {
            Self::Vt(vt) => vt.recent_text(lines),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.recent_text(lines),
        }
    }

    fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        match self {
            Self::Vt(vt) => vt.extract_selection(selection),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.extract_selection(selection),
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self {
            Self::Vt(vt) => vt.render(frame, area),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.render(frame, area),
        }
    }

    fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        let _ = theme;
        match self {
            Self::Vt(_) => {}
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.apply_host_terminal_theme(theme),
        }
    }

    fn keyboard_protocol(
        &self,
        fallback: crate::input::KeyboardProtocol,
    ) -> crate::input::KeyboardProtocol {
        match self {
            Self::Vt(_) => fallback,
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.keyboard_protocol().unwrap_or(fallback),
        }
    }

    fn encode_terminal_key(
        &self,
        key: crate::input::TerminalKey,
        protocol: crate::input::KeyboardProtocol,
    ) -> Vec<u8> {
        match self {
            Self::Vt(vt) => vt.encode_terminal_key(key, protocol),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.encode_terminal_key(key, protocol),
        }
    }

    fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        match self {
            Self::Vt(vt) => vt.input_state().and_then(|input_state| {
                crate::input::encode_mouse_button(
                    kind,
                    column,
                    row,
                    modifiers,
                    input_state.mouse_protocol_encoding,
                )
            }),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.encode_mouse_button(kind, column, row, modifiers),
        }
    }

    fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        match self {
            Self::Vt(vt) => vt.input_state().and_then(|input_state| {
                crate::input::encode_mouse_scroll(
                    kind,
                    column,
                    row,
                    modifiers,
                    input_state.mouse_protocol_encoding,
                )
            }),
            #[cfg(feature = "ghostty-vt")]
            Self::Ghostty(ghostty) => ghostty.encode_mouse_wheel(kind, column, row, modifiers),
        }
    }
}

#[cfg_attr(feature = "ghostty-vt", allow(dead_code))]
impl VtPaneTerminal {
    fn new(
        parser: Arc<RwLock<vt100::Parser<PtyResponses>>>,
        screen_content: Arc<RwLock<String>>,
        mouse_alternate_scroll: Arc<AtomicBool>,
    ) -> Self {
        Self {
            parser,
            screen_content,
            mouse_alternate_scroll,
        }
    }

    fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        let resp = if let Ok(mut parser) = self.parser.write() {
            parser.process(bytes);

            let scrollback = parser.screen().scrollback();
            if scrollback > 0 {
                parser.screen_mut().set_scrollback(0);
            }
            let content = parser.screen().contents();
            if scrollback > 0 {
                parser.screen_mut().set_scrollback(scrollback);
            }
            if let Ok(mut screen_content) = self.screen_content.write() {
                *screen_content = content;
            }
            parser.callbacks_mut().take()
        } else {
            error!(pane = pane_id.raw(), "parser lock poisoned in reader");
            return ProcessBytesResult {
                request_render: false,
            };
        };

        if !resp.is_empty() {
            if let Err(e) = response_writer.try_send(Bytes::from(resp)) {
                warn!(pane = pane_id.raw(), err = %e, "dropped terminal query response");
            }
        }

        ProcessBytesResult {
            request_render: true,
        }
    }

    fn resize(&self, rows: u16, cols: u16) {
        if let Ok(mut parser) = self.parser.write() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    fn scroll_up(&self, lines: usize) {
        if let Ok(mut parser) = self.parser.write() {
            let current = parser.screen().scrollback();
            parser.screen_mut().set_scrollback(current + lines);
        }
    }

    fn scroll_down(&self, lines: usize) {
        if let Ok(mut parser) = self.parser.write() {
            let current = parser.screen().scrollback();
            parser
                .screen_mut()
                .set_scrollback(current.saturating_sub(lines));
        }
    }

    fn scroll_reset(&self) {
        if let Ok(mut parser) = self.parser.write() {
            parser.screen_mut().set_scrollback(0);
        }
    }

    fn set_scroll_offset_from_bottom(&self, lines: usize) {
        if let Ok(mut parser) = self.parser.write() {
            parser.screen_mut().set_scrollback(lines);
        }
    }

    fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        let Ok(mut parser) = self.parser.write() else {
            return None;
        };
        let max_offset_from_bottom = max_scrollback(&mut parser);
        let screen = parser.screen();
        let (viewport_rows, _) = screen.size();
        Some(ScrollMetrics {
            offset_from_bottom: screen.scrollback(),
            max_offset_from_bottom,
            viewport_rows: viewport_rows as usize,
        })
    }

    fn input_state(&self) -> Option<InputState> {
        let Ok(parser) = self.parser.read() else {
            return None;
        };
        let screen = parser.screen();
        Some(InputState {
            alternate_screen: screen.alternate_screen(),
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
            focus_reporting: false,
            mouse_protocol_mode: match screen.mouse_protocol_mode() {
                vt100::MouseProtocolMode::None => crate::input::MouseProtocolMode::None,
                vt100::MouseProtocolMode::Press => crate::input::MouseProtocolMode::Press,
                vt100::MouseProtocolMode::PressRelease => {
                    crate::input::MouseProtocolMode::PressRelease
                }
                vt100::MouseProtocolMode::ButtonMotion => {
                    crate::input::MouseProtocolMode::ButtonMotion
                }
                vt100::MouseProtocolMode::AnyMotion => crate::input::MouseProtocolMode::AnyMotion,
            },
            mouse_protocol_encoding: match screen.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Default => {
                    crate::input::MouseProtocolEncoding::Default
                }
                vt100::MouseProtocolEncoding::Utf8 => crate::input::MouseProtocolEncoding::Utf8,
                vt100::MouseProtocolEncoding::Sgr => crate::input::MouseProtocolEncoding::Sgr,
            },
            mouse_alternate_scroll: self.mouse_alternate_scroll.load(Ordering::Relaxed),
        })
    }

    fn visible_text(&self) -> String {
        let Ok(content) = self.screen_content.read() else {
            return String::new();
        };
        let mut rows: Vec<String> = content
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        trim_trailing_blank_rows(&mut rows);
        let text = rows.join("\n");
        if text.is_empty() {
            text
        } else {
            format!("{text}\n")
        }
    }

    fn recent_text(&self, lines: usize) -> String {
        self.parser
            .write()
            .map(|mut parser| recent_text_from_parser(&mut parser, lines))
            .unwrap_or_default()
    }

    fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.parser
            .read()
            .map(|parser| crate::selection::extract_text(parser.screen(), selection))
            .ok()
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        if let Ok(parser) = self.parser.read() {
            let show_cursor = parser.screen().scrollback() == 0;
            let pt = PseudoTerminal::new(parser.screen())
                .cursor(tui_term::widget::Cursor::default().visibility(show_cursor));
            frame.render_widget(pt, area);
        }
    }

    fn encode_terminal_key(
        &self,
        key: crate::input::TerminalKey,
        protocol: crate::input::KeyboardProtocol,
    ) -> Vec<u8> {
        let application_cursor = self
            .input_state()
            .map(|state| state.application_cursor)
            .unwrap_or(false);
        if matches!(
            key.code,
            crossterm::event::KeyCode::Up
                | crossterm::event::KeyCode::Down
                | crossterm::event::KeyCode::Left
                | crossterm::event::KeyCode::Right
        ) && key.modifiers.is_empty()
        {
            return crate::input::encode_cursor_key(key.code, application_cursor);
        }
        crate::input::encode_terminal_key(key, protocol)
    }
}

#[cfg(feature = "ghostty-vt")]
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
        Ok(Self {
            core: Mutex::new(GhosttyPaneCore {
                terminal,
                render_state,
            }),
        })
    }

    fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        if let Ok(mut core) = self.core.lock() {
            if let Some(color) = theme.foreground {
                let sequence = crate::terminal_theme::osc_set_default_color_sequence(
                    crate::terminal_theme::DefaultColorKind::Foreground,
                    color,
                );
                core.terminal.write(sequence.as_bytes());
            }
            if let Some(color) = theme.background {
                let sequence = crate::terminal_theme::osc_set_default_color_sequence(
                    crate::terminal_theme::DefaultColorKind::Background,
                    color,
                );
                core.terminal.write(sequence.as_bytes());
            }
        }
    }

    fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        let Ok(mut core) = self.core.lock() else {
            error!(pane = pane_id.raw(), "ghostty core lock poisoned in reader");
            return ProcessBytesResult {
                request_render: false,
            };
        };

        core.terminal.write(bytes);
        let _ = response_writer;
        ProcessBytesResult {
            request_render: true,
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

        let Ok(core) = self.core.lock() else {
            return crate::input::encode_terminal_key(key, protocol);
        };

        let Some(event) = ghostty_key_event_from_terminal_key(key) else {
            return crate::input::encode_terminal_key(key, protocol);
        };

        let mut encoder = match crate::ghostty::KeyEncoder::new() {
            Ok(encoder) => encoder,
            Err(_) => return crate::input::encode_terminal_key(key, protocol),
        };
        encoder.set_from_terminal(&core.terminal);
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

    fn recent_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text(&core, lines).ok())
            .unwrap_or_default()
    }

    fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_extract_selection(&mut core, selection).ok())
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let GhosttyPaneCore {
            terminal,
            render_state,
        } = &mut *core;
        if render_state.update(terminal).is_err() {
            return;
        }
        let colors = render_state.colors().ok();
        let default_bg = colors.map(|c| ghostty_color(c.background));
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

        if render_state.cursor_visible().ok() == Some(true) {
            if let Ok(Some(cursor)) = render_state.cursor_viewport() {
                if cursor.x < area.width && cursor.y < area.height {
                    frame.set_cursor_position((area.x + cursor.x, area.y + cursor.y));
                }
            }
        }
    }
}

#[cfg(feature = "ghostty-vt")]
#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_prefers_herdr_text_encoding(key: crate::input::TerminalKey) -> bool {
    matches!(key.code, crossterm::event::KeyCode::Char(_))
}

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_unshifted_codepoint(key: crate::input::TerminalKey) -> Option<u32> {
    match key.code {
        crossterm::event::KeyCode::Char(c) => Some(c as u32),
        _ => None,
    }
}

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_visible_text(core: &mut GhosttyPaneCore) -> Result<String, crate::ghostty::Error> {
    let GhosttyPaneCore {
        terminal,
        render_state,
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_extract_selection(
    core: &mut GhosttyPaneCore,
    selection: &crate::selection::Selection,
) -> Result<String, crate::ghostty::Error> {
    let GhosttyPaneCore {
        terminal,
        render_state,
    } = core;
    render_state.update(terminal)?;
    let mut row_iterator = crate::ghostty::RowIterator::new()?;
    let mut row_cells = crate::ghostty::RowCells::new()?;
    let mut rows = render_state.populate_row_iterator(&mut row_iterator)?;
    let mut lines = Vec::new();
    let mut row_index = 0u16;
    let ((start_row, _), (end_row, _)) = selection.ordered_cells();
    while rows.next() {
        if row_index > end_row {
            break;
        }
        if row_index >= start_row {
            let mut cells = rows.populate_cells(&mut row_cells)?;
            let (start_col, end_col) = selection_cols_for_row(selection, row_index);
            let mut line = String::new();
            for x in start_col..=end_col {
                cells.select(x)?;
                line.push_str(&ghostty_cell_symbol(&cells)?);
            }
            lines.push(line.trim_end().to_string());
        }
        row_index += 1;
    }
    Ok(lines.join("\n"))
}

#[cfg(feature = "ghostty-vt")]
fn selection_cols_for_row(selection: &crate::selection::Selection, row: u16) -> (u16, u16) {
    let ((start_row, start_col), (end_row, end_col)) = selection.ordered_cells();
    if start_row == end_row {
        (start_col, end_col)
    } else if row == start_row {
        (start_col, selection.pane_width().saturating_sub(1))
    } else if row == end_row {
        (0, end_col)
    } else {
        (0, selection.pane_width().saturating_sub(1))
    }
}

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_line_from_cells(
    cells: &mut crate::ghostty::RowCellIter<'_>,
) -> Result<String, crate::ghostty::Error> {
    let mut line = String::new();
    while cells.next() {
        line.push_str(&ghostty_cell_symbol(cells)?);
    }
    Ok(line.trim_end().to_string())
}

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
fn ghostty_blank_symbol_for_width(wide: crate::ghostty::CellWide) -> &'static str {
    match wide {
        crate::ghostty::CellWide::Wide => "  ",
        crate::ghostty::CellWide::SpacerTail => "",
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => " ",
    }
}

#[cfg(feature = "ghostty-vt")]
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
    ghostty_blank_symbol_for_width(wide).to_string()
}

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

#[cfg(feature = "ghostty-vt")]
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

fn max_scrollback(parser: &mut vt100::Parser<PtyResponses>) -> usize {
    let screen = parser.screen_mut();
    let original_scrollback = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let max_scrollback = screen.scrollback();
    screen.set_scrollback(original_scrollback);
    max_scrollback
}

fn parser_rows(parser: &mut vt100::Parser<PtyResponses>, lines: usize) -> Vec<String> {
    let max_scrollback = max_scrollback(parser);
    let screen = parser.screen_mut();
    let original_scrollback = screen.scrollback();

    let (_, cols) = screen.size();
    screen.set_scrollback(0);
    let visible_rows: Vec<String> = screen
        .rows(0, cols)
        .map(|row| row.trim_end().to_string())
        .collect();
    let extra_rows = lines.saturating_sub(visible_rows.len()).min(max_scrollback);

    let mut rows = Vec::with_capacity(extra_rows + visible_rows.len());
    if extra_rows > 0 {
        for offset in (1..=extra_rows).rev() {
            screen.set_scrollback(offset);
            if let Some(row) = screen.rows(0, cols).next() {
                rows.push(row.trim_end().to_string());
            }
        }
    }

    screen.set_scrollback(original_scrollback);
    rows.extend(visible_rows);
    trim_trailing_blank_rows(&mut rows);
    rows
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

fn recent_text_from_parser(parser: &mut vt100::Parser<PtyResponses>, lines: usize) -> String {
    let rows = parser_rows(parser, lines);
    recent_text_from_rows(&rows, lines)
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
        _host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
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

        // Live screen snapshot for detection (decoupled from parser scrollback)
        #[cfg_attr(feature = "ghostty-vt", allow(unused_variables))]
        let screen_content = Arc::new(RwLock::new(String::new()));

        #[cfg(not(feature = "ghostty-vt"))]
        let terminal = {
            let responses = PtyResponses::new();
            let kitty_keyboard_flags = responses.kitty_keyboard_flags.clone();
            let mouse_alternate_scroll = responses.mouse_alternate_scroll.clone();
            let parser = Arc::new(RwLock::new(vt100::Parser::new_with_callbacks(
                rows, cols, 10000, responses,
            )));
            (
                Arc::new(PaneTerminal::Vt(VtPaneTerminal::new(
                    parser,
                    screen_content.clone(),
                    mouse_alternate_scroll,
                ))),
                kitty_keyboard_flags,
            )
        };

        #[cfg(feature = "ghostty-vt")]
        let terminal = {
            let terminal = crate::ghostty::Terminal::new(cols, rows, 10000)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            let pane_terminal = GhosttyPaneTerminal::new(terminal, input_tx.clone())?;
            pane_terminal.apply_host_terminal_theme(_host_terminal_theme);
            (
                Arc::new(PaneTerminal::Ghostty(pane_terminal)),
                Arc::new(AtomicU16::new(0)),
            )
        };

        let (terminal, kitty_keyboard_flags) = terminal;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        crate::integration::apply_pane_env(&mut cmd, pane_id);

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
                    Err(e) => error!(pane = pane_id.raw(), err = %e, "failed to spawn shell"),
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
                            let result =
                                terminal.process_pty_bytes(pane_id, &buf[..n], &response_writer);
                            if result.request_render && !render_dirty.swap(true, Ordering::AcqRel) {
                                render_notify.notify_one();
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
            let detect_reset_notify = Arc::new(Notify::new());
            let detect_reset = detect_reset_notify.clone();
            let pending_release = Arc::new(Mutex::new(None));
            let pending_release_for_task = pending_release.clone();

            let handle = tokio::spawn(async move {
                let mut agent: Option<Agent> = None;
                let mut state = AgentState::Unknown;
                let mut last_process_check = Instant::now();
                let mut last_claude_working_at = None;

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let tick = if active_pending_release(&pending_release_for_task, Instant::now())
                        .is_some()
                    {
                        TICK_PENDING_RELEASE
                    } else if agent.is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(tick) => {}
                        _ = detect_reset.notified() => {
                            agent = None;
                            state = AgentState::Unknown;
                            last_claude_working_at = None;
                        }
                    }

                    let now = Instant::now();
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    let should_check_process = suppressed_agent.is_some()
                        || agent.is_none()
                        || now.duration_since(last_process_check) >= PROCESS_RECHECK;

                    let mut agent_changed = false;
                    if should_check_process {
                        last_process_check = now;
                        let pid = child_pid.load(Ordering::Acquire);
                        if pid > 0 {
                            if let Some(job) = detect::foreground_job(pid) {
                                let identified = detect::identify_agent_in_job(&job);
                                let mut new_agent = identified.as_ref().map(|(agent, _)| *agent);

                                if let Some(suppressed_agent) = suppressed_agent {
                                    if new_agent == Some(suppressed_agent) {
                                        new_agent = None;
                                    } else if let Ok(mut pending_release) =
                                        pending_release_for_task.lock()
                                    {
                                        *pending_release = None;
                                    }
                                }

                                if new_agent != agent {
                                    if let Some((_, process_name)) = identified {
                                        info!(
                                            pane = pane_id.raw(),
                                            ?new_agent,
                                            process = %process_name,
                                            pgid = job.process_group_id,
                                            "agent changed"
                                        );
                                    } else {
                                        info!(
                                            pane = pane_id.raw(),
                                            ?new_agent,
                                            pgid = job.process_group_id,
                                            "agent changed"
                                        );
                                    }
                                    agent = new_agent;
                                    agent_changed = true;
                                }
                            }
                        }
                    }

                    let content = terminal.visible_text();
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

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.terminal.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        self.terminal.render(frame, area);
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

    #[cfg(feature = "ghostty-vt")]
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
mod tests {
    use super::*;

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
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

    #[cfg(feature = "ghostty-vt")]
    #[test]
    fn ghostty_normalize_buffer_symbol_uses_ratatui_width_contract() {
        assert_eq!(
            ghostty_normalize_buffer_symbol("🙂", crate::ghostty::CellWide::Wide),
            "🙂"
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol("a", crate::ghostty::CellWide::Wide),
            "  "
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol("⌨️", crate::ghostty::CellWide::Narrow),
            " "
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

    #[cfg(feature = "ghostty-vt")]
    #[tokio::test]
    async fn focus_events_are_forwarded_when_enabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
            .unwrap();
        let runtime = PaneRuntime {
            terminal: Arc::new(PaneTerminal::Ghostty(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
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

    #[cfg(feature = "ghostty-vt")]
    #[tokio::test]
    async fn focus_events_are_suppressed_when_disabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = mpsc::channel(1);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let runtime = PaneRuntime {
            terminal: Arc::new(PaneTerminal::Ghostty(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
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
    fn recent_text_reconstructs_scrollback_tail() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(3, 10, 100, responses);
        parser.process(b"a\r\nb\r\nc\r\nd\r\ne");

        let recent = recent_text_from_parser(&mut parser, 4);
        assert_eq!(recent, "b\nc\nd\ne\n");
    }

    #[test]
    fn max_scrollback_reports_clamped_history_without_changing_position() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(3, 10, 100, responses);
        parser.process(b"a\r\nb\r\nc\r\nd\r\ne");
        parser.screen_mut().set_scrollback(1);

        let max = max_scrollback(&mut parser);

        assert_eq!(max, 2);
        assert_eq!(parser.screen().scrollback(), 1);
    }

    #[test]
    fn trim_trailing_blank_rows_drops_empty_viewport_tail() {
        let mut rows = vec!["hello".to_string(), "".to_string(), "   ".to_string()];
        trim_trailing_blank_rows(&mut rows);
        assert_eq!(rows, vec!["hello".to_string()]);
    }

    #[test]
    fn alternate_screen_does_not_accumulate_host_scrollback() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(3, 10, 100, responses);
        parser.process(b"\x1b[?1049h1\r\n2\r\n3\r\n4");

        assert!(parser.screen().alternate_screen());
        assert_eq!(max_scrollback(&mut parser), 0);
        assert_eq!(recent_text_from_parser(&mut parser, 4), "2\n3\n4\n");
    }

    #[test]
    fn normal_screen_top_anchored_scroll_regions_feed_scrollback() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(5, 10, 100, responses);
        parser.process(b"1\r\n2\r\n3\r\n4\r\n5");
        parser.process(b"\x1b[1;3r\x1b[3;1H\r\nX");

        assert_eq!(max_scrollback(&mut parser), 1);
    }

    #[test]
    fn normal_screen_non_top_anchored_scroll_regions_do_not_feed_scrollback() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(5, 10, 100, responses);
        parser.process(b"1\r\n2\r\n3\r\n4\r\n5");
        parser.process(b"\x1b[2;4r\x1b[4;1H\r\nX");

        assert_eq!(max_scrollback(&mut parser), 0);
    }

    #[test]
    fn alternate_screen_scroll_regions_do_not_create_host_scrollback() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(5, 10, 100, responses);
        parser.process(b"\x1b[?1049h1\r\n2\r\n3\r\n4\r\n5");
        parser.process(b"\x1b[1;3r\x1b[3;1H\r\nX");

        assert!(parser.screen().alternate_screen());
        assert_eq!(max_scrollback(&mut parser), 0);
    }

    #[test]
    fn claude_working_is_sticky_for_short_gap() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Idle,
            AgentState::Working,
            now,
            &mut last_working,
        );
        assert_eq!(working, AgentState::Working);

        let still_working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + std::time::Duration::from_millis(400),
            &mut last_working,
        );
        assert_eq!(still_working, AgentState::Working);
    }

    #[test]
    fn claude_transitions_to_idle_after_hold_expires() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + CLAUDE_WORKING_HOLD + std::time::Duration::from_millis(1),
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn non_claude_states_are_unchanged() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let state = stabilize_agent_state(
            Some(Agent::Codex),
            AgentState::Working,
            AgentState::Idle,
            now,
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut pane = PaneState::new();
        pane.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        pane.set_hook_authority("herdr:pi".into(), Agent::Pi, AgentState::Working, None);

        assert_eq!(pane.detected_agent, Some(Agent::Pi));
        assert_eq!(pane.fallback_state, AgentState::Idle);
        assert_eq!(pane.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_clears_when_detected_agent_changes() {
        let mut pane = PaneState::new();
        pane.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        pane.set_hook_authority("herdr:pi".into(), Agent::Pi, AgentState::Working, None);

        pane.set_detected_state(None, AgentState::Unknown);

        assert!(pane.hook_authority.is_none());
        assert_eq!(pane.detected_agent, None);
        assert_eq!(pane.state, AgentState::Unknown);
    }

    #[test]
    fn release_agent_clears_identity_immediately() {
        let mut pane = PaneState::new();
        pane.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        pane.set_hook_authority("herdr:pi".into(), Agent::Pi, AgentState::Working, None);

        pane.release_agent("herdr:pi", Agent::Pi);

        assert!(pane.hook_authority.is_none());
        assert_eq!(pane.detected_agent, None);
        assert_eq!(pane.fallback_state, AgentState::Unknown);
        assert_eq!(pane.state, AgentState::Unknown);
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
