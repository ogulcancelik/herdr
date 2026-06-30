use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use ratatui::style::{Color, Modifier, Style};
use ratatui::{layout::Rect, Frame};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, error};
use unicode_width::UnicodeWidthStr;

use crate::layout::PaneId;
use crate::protocol::CellData;

use super::cursor::{CursorPositionSettleState, DecscusrTracker, CURSOR_POSITION_SETTLE};
use super::{
    input::{
        ghostty_key_event_from_terminal_key, ghostty_mouse_encoder_for_terminal,
        ghostty_mouse_event_from_button_kind, ghostty_mouse_event_from_motion_kind,
        ghostty_mouse_event_from_wheel_kind, ghostty_prefers_herdr_text_encoding,
    },
    kitty_keyboard::KittyKeyboardTracker,
    osc::{
        contains_scrollback_clear_sequence, current_transient_default_color_owner,
        maybe_filter_primary_screen_scrollback_clear, restore_host_terminal_theme_if_needed,
        write_host_terminal_theme_selective, AgentOscStateTracker, CwdOscTracker,
        DefaultColorEvent, DefaultColorEventTracker, DefaultColorOscTracker, DefaultColorQuery,
        DefaultColorTrackedEvent, Osc52Forwarder, OscDebugTracker,
    },
    xtgettcap::{XtgettcapQueryTracker, XtgettcapResponse},
};

const DEFAULT_DETECTION_ROWS: usize = 24;
const KITTY_GRAPHICS_REDRAW_SETTLE: Duration = Duration::from_millis(20);
const CURSOR_POSITION_SETTLE_ENABLED: bool = cfg!(windows);
const MODE_MOUSE_X10: u16 = 9;
const MODE_MOUSE_PRESS_RELEASE: u16 = 1000;
const MODE_MOUSE_BUTTON_MOTION: u16 = 1002;
const MODE_MOUSE_ANY_MOTION: u16 = 1003;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub offset_from_bottom: usize,
    pub max_offset_from_bottom: usize,
    pub viewport_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCursorState {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    /// DECSCUSR parameter (0–6). 0 means terminal default.
    pub shape: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalDirtyPatch {
    pub rows: Vec<(u16, Vec<CellData>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalDirtyPatchOutcome {
    Clean,
    Patch(TerminalDirtyPatch),
    Fallback,
}

fn decscusr_cursor_shape(style: crate::ghostty::CursorVisualStyle, blinking: bool) -> u8 {
    match (style, blinking) {
        (crate::ghostty::CursorVisualStyle::Block, true)
        | (crate::ghostty::CursorVisualStyle::BlockHollow, true) => 1,
        (crate::ghostty::CursorVisualStyle::Block, false)
        | (crate::ghostty::CursorVisualStyle::BlockHollow, false) => 2,
        (crate::ghostty::CursorVisualStyle::Underline, true) => 3,
        (crate::ghostty::CursorVisualStyle::Underline, false) => 4,
        (crate::ghostty::CursorVisualStyle::Bar, true) => 5,
        (crate::ghostty::CursorVisualStyle::Bar, false) => 6,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputState {
    pub alternate_screen: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub focus_reporting: bool,
    pub mouse_protocol_mode: crate::input::MouseProtocolMode,
    pub mouse_protocol_encoding: crate::input::MouseProtocolEncoding,
    pub mouse_alternate_scroll: bool,
    #[serde(default)]
    pub modify_other_keys: bool,
}

impl InputState {
    pub fn mouse_reporting_enabled(self) -> bool {
        self.mouse_protocol_mode.reporting_enabled()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessBytesResult {
    pub request_render: bool,
    pub render_delay: Option<Duration>,
    pub clipboard_writes: Vec<Vec<u8>>,
    pub reported_cwd: Option<std::path::PathBuf>,
    pub terminal_responses: Vec<Bytes>,
}

pub(crate) struct GhosttyPaneTerminal {
    pub core: Mutex<GhosttyPaneCore>,
    key_encoder: Mutex<crate::ghostty::KeyEncoder>,
    pending_pty_responses: Arc<Mutex<Vec<Bytes>>>,
}

pub(crate) struct GhosttyPaneCore {
    pub terminal: crate::ghostty::Terminal,
    pub render_state: crate::ghostty::RenderState,
    pub kitty_keyboard: KittyKeyboardTracker,
    pub initial_default_foreground: Option<crate::ghostty::RgbColor>,
    pub initial_default_background: Option<crate::ghostty::RgbColor>,
    pub host_terminal_theme: crate::terminal_theme::TerminalTheme,
    pub transient_default_color_owner_pgid: Option<u32>,
    pub default_color_tracker: DefaultColorOscTracker,
    pub default_color_event_tracker: DefaultColorEventTracker,
    pub child_default_foreground_changed: bool,
    pub child_default_background_changed: bool,
    pub osc52_forwarder: Osc52Forwarder,
    pub cwd_osc_tracker: CwdOscTracker,
    pub osc_debug_tracker: OscDebugTracker,
    pub agent_osc_state: AgentOscStateTracker,
    pub xtgettcap_query_tracker: XtgettcapQueryTracker,
    decscusr_tracker: DecscusrTracker,
    cursor_settle_state: CursorPositionSettleState,
    windows_powershell_prompt_cwd_reporting: bool,
}

pub(crate) struct PaneTerminal {
    pub(crate) ghostty: GhosttyPaneTerminal,
}

impl PaneTerminal {
    pub(crate) fn new(ghostty: GhosttyPaneTerminal) -> Self {
        Self { ghostty }
    }

    pub fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        shell_pid: u32,
        bytes: &[u8],
        response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        self.ghostty
            .process_pty_bytes(pane_id, shell_pid, bytes, response_writer)
    }

    pub fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Vec<Bytes> {
        self.ghostty
            .resize(rows, cols, cell_width_px, cell_height_px)
    }

    pub fn scroll_up(&self, lines: usize) {
        self.ghostty.scroll_up(lines);
    }

    pub fn scroll_down(&self, lines: usize) {
        self.ghostty.scroll_down(lines);
    }

    pub fn scroll_reset(&self) {
        self.ghostty.scroll_reset();
    }

    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.ghostty.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.ghostty.scroll_metrics()
    }

    pub fn input_state(&self) -> Option<InputState> {
        self.ghostty.input_state()
    }

    pub fn wheel_routing(&self) -> Option<crate::pane::WheelRouting> {
        self.ghostty.wheel_routing()
    }

    pub fn cursor_state(&self) -> Option<TerminalCursorState> {
        self.ghostty.cursor_state()
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.ghostty.synchronized_output_active()
    }

    pub fn visible_text(&self) -> String {
        self.ghostty.visible_text()
    }

    pub fn visible_ansi(&self) -> String {
        self.ghostty.visible_ansi()
    }

    pub fn detection_text(&self) -> String {
        self.ghostty.detection_text()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.ghostty.recent_text(lines)
    }

    pub fn recent_ansi(&self, lines: usize) -> String {
        self.ghostty.recent_ansi(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.ghostty.recent_unwrapped_text(lines)
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.ghostty.recent_unwrapped_ansi(lines)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.ghostty.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.ghostty.render(frame, area, show_cursor);
    }

    pub fn collect_dirty_patch(
        &self,
        area_width: u16,
        area_height: u16,
    ) -> TerminalDirtyPatchOutcome {
        self.ghostty.collect_dirty_patch(area_width, area_height)
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.ghostty.visible_hyperlinks(area)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.ghostty
            .kitty_image_placements_with_data_filter(needs_data)
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.ghostty.apply_host_terminal_theme(theme);
    }

    pub fn has_transient_default_color_override(&self) -> bool {
        self.ghostty.has_transient_default_color_override()
    }

    pub fn maybe_restore_host_terminal_theme(&self, pane_id: PaneId, shell_pid: u32) -> bool {
        self.ghostty
            .maybe_restore_host_terminal_theme(pane_id, shell_pid)
    }

    #[allow(dead_code)] // exposed for Stage C (detection loop wiring)
    pub fn agent_osc_title(&self) -> String {
        self.ghostty.agent_osc_title()
    }

    #[allow(dead_code)] // exposed for Stage C (detection loop wiring)
    pub fn agent_osc_progress(&self) -> String {
        self.ghostty.agent_osc_progress()
    }

    /// Clears retained OSC title/progress evidence on foreground agent change.
    pub fn clear_agent_osc_state(&self) {
        self.ghostty.clear_agent_osc_state()
    }

    pub fn keyboard_protocol(
        &self,
        fallback: crate::input::KeyboardProtocol,
    ) -> crate::input::KeyboardProtocol {
        self.ghostty.keyboard_protocol().unwrap_or(fallback)
    }

    #[cfg(unix)]
    pub fn kitty_keyboard_state_ansi(&self) -> Option<String> {
        self.ghostty
            .kitty_keyboard_state_ansi()
            .filter(|ansi| !ansi.is_empty())
    }

    pub fn encode_terminal_key(
        &self,
        key: crate::input::TerminalKey,
        protocol: crate::input::KeyboardProtocol,
    ) -> Vec<u8> {
        self.ghostty.encode_terminal_key(key, protocol)
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.ghostty
            .encode_mouse_button(kind, column, row, modifiers)
    }

    pub fn encode_mouse_motion(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.ghostty
            .encode_mouse_motion(kind, column, row, modifiers)
    }

    pub fn encode_mouse_wheel(
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
    pub fn new(
        mut terminal: crate::ghostty::Terminal,
        _response_writer: mpsc::Sender<Bytes>,
    ) -> std::io::Result<Self> {
        let pending_pty_responses = Arc::new(Mutex::new(Vec::new()));
        let callback_responses = pending_pty_responses.clone();
        terminal
            .set_write_pty_callback(move |bytes| {
                if let Ok(mut responses) = callback_responses.lock() {
                    responses.push(Bytes::copy_from_slice(bytes));
                }
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut render_state =
            crate::ghostty::RenderState::new().map_err(|e| std::io::Error::other(e.to_string()))?;
        let initial_colors = render_state
            .update(&terminal)
            .ok()
            .and_then(|_| render_state.colors().ok());
        let initial_default_foreground = initial_colors.map(|colors| colors.foreground);
        let initial_default_background = initial_colors.map(|colors| colors.background);
        let mut key_encoder =
            crate::ghostty::KeyEncoder::new().map_err(|e| std::io::Error::other(e.to_string()))?;
        key_encoder.set_from_terminal(&terminal);
        Ok(Self {
            core: Mutex::new(GhosttyPaneCore {
                terminal,
                render_state,
                kitty_keyboard: KittyKeyboardTracker::default(),
                initial_default_foreground,
                initial_default_background,
                host_terminal_theme: crate::terminal_theme::TerminalTheme::default(),
                transient_default_color_owner_pgid: None,
                default_color_tracker: DefaultColorOscTracker::default(),
                default_color_event_tracker: DefaultColorEventTracker::default(),
                child_default_foreground_changed: false,
                child_default_background_changed: false,
                osc52_forwarder: Osc52Forwarder::default(),
                cwd_osc_tracker: CwdOscTracker::default(),
                osc_debug_tracker: OscDebugTracker::default(),
                agent_osc_state: AgentOscStateTracker::default(),
                xtgettcap_query_tracker: XtgettcapQueryTracker::default(),
                decscusr_tracker: DecscusrTracker::default(),
                cursor_settle_state: CursorPositionSettleState::default(),
                windows_powershell_prompt_cwd_reporting: false,
            }),
            key_encoder: Mutex::new(key_encoder),
            pending_pty_responses,
        })
    }

    pub(super) fn set_windows_powershell_prompt_cwd_reporting(&self, enabled: bool) {
        if let Ok(mut core) = self.core.lock() {
            core.windows_powershell_prompt_cwd_reporting = enabled;
        }
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        if let Ok(mut core) = self.core.lock() {
            let foreground_unowned = !core.child_default_foreground_changed;
            let background_unowned = !core.child_default_background_changed;
            core.host_terminal_theme = theme;
            if foreground_unowned && background_unowned {
                core.transient_default_color_owner_pgid = None;
            }
            write_host_terminal_theme_selective(
                &mut core.terminal,
                theme,
                foreground_unowned,
                background_unowned,
            );
        }
    }

    pub fn has_transient_default_color_override(&self) -> bool {
        self.core
            .lock()
            .map(|core| core.transient_default_color_owner_pgid.is_some())
            .unwrap_or(false)
    }

    pub fn maybe_restore_host_terminal_theme(&self, pane_id: PaneId, shell_pid: u32) -> bool {
        {
            let Ok(core) = self.core.lock() else {
                return false;
            };
            if !should_probe_host_terminal_theme_restore(&core) {
                return false;
            }
        }

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

    /// Returns the latest OSC 0/2 title retained for agent detection, or `""`
    /// if no title has been seen or the last update was an empty clear.
    #[allow(dead_code)] // exposed for Stage C (detection loop wiring)
    pub fn agent_osc_title(&self) -> String {
        self.core
            .lock()
            .map(|core| core.agent_osc_state.latest_title().to_owned())
            .unwrap_or_default()
    }

    /// Returns the latest OSC 9 progress payload retained for agent detection,
    /// or `""` if none has been seen.
    #[allow(dead_code)] // exposed for Stage C (detection loop wiring)
    pub fn agent_osc_progress(&self) -> String {
        self.core
            .lock()
            .map(|core| core.agent_osc_state.latest_progress().to_owned())
            .unwrap_or_default()
    }

    /// Clears retained OSC title/progress evidence when the pane's foreground
    /// agent changes, so a new agent process starts from a blank OSC slate.
    pub fn clear_agent_osc_state(&self) {
        if let Ok(mut core) = self.core.lock() {
            core.agent_osc_state.clear_retained();
        }
    }

    pub fn process_pty_bytes(
        &self,
        pane_id: PaneId,
        shell_pid: u32,
        bytes: &[u8],
        _response_writer: &mpsc::Sender<Bytes>,
    ) -> ProcessBytesResult {
        crate::render_prof::counter("pty.bytes", bytes.len() as u64);
        let Ok(mut core) = self.core.lock() else {
            error!(pane = pane_id.raw(), "ghostty core lock poisoned in reader");
            return ProcessBytesResult {
                request_render: false,
                render_delay: None,
                clipboard_writes: Vec::new(),
                reported_cwd: None,
                terminal_responses: Vec::new(),
            };
        };

        let default_color_observation = core.default_color_tracker.observe(bytes);
        if shell_pid > 0 && default_color_observation {
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
        core.cwd_osc_tracker.observe(bytes);
        let reported_cwd = core.cwd_osc_tracker.drain_latest();
        core.osc_debug_tracker.observe(bytes);
        for event in core.osc_debug_tracker.drain_pending() {
            debug!(
                pane = pane_id.raw(),
                osc_command = %event.command,
                osc_payload = ?event.payload,
                "agent OSC evidence observed"
            );
        }
        core.agent_osc_state.observe(bytes);

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

        core.kitty_keyboard.observe(filtered_bytes.as_ref());
        let mut terminal_responses = Vec::new();
        core.default_color_event_tracker
            .observe(filtered_bytes.as_ref());
        core.xtgettcap_query_tracker
            .observe(filtered_bytes.as_ref());
        core.decscusr_tracker.observe(filtered_bytes.as_ref());
        let default_color_events = core.default_color_event_tracker.drain_pending();
        let xtgettcap_responses = core.xtgettcap_query_tracker.drain_pending();
        let write_started = crate::render_prof::timer();
        self.write_pty_bytes_with_ordered_responses(
            &mut core,
            filtered_bytes.as_ref(),
            default_color_events,
            xtgettcap_responses,
            &mut terminal_responses,
        );
        crate::render_prof::duration_since("pty.ghostty_write", write_started);

        let has_kitty_graphics_sequence = crate::kitty_graphics::is_enabled()
            && contains_kitty_graphics_sequence(filtered_bytes.as_ref());
        if has_kitty_graphics_sequence {
            debug!(pane = pane_id.raw(), "processed kitty graphics sequence");
        }
        if let Ok(mut key_encoder) = self.key_encoder.lock() {
            key_encoder.set_from_terminal(&core.terminal);
        }
        let synchronized_output = core
            .terminal
            .mode_get(crate::ghostty::MODE_SYNCHRONIZED_OUTPUT)
            .unwrap_or(false);
        if CURSOR_POSITION_SETTLE_ENABLED {
            let cursor_started = crate::render_prof::timer();
            let cursor_after_write = current_cursor_state(&mut core);
            crate::render_prof::duration_since("pty.cursor_state_update", cursor_started);
            core.cursor_settle_state
                .observe(cursor_after_write, Instant::now());
        }
        #[cfg(windows)]
        let reported_cwd = if core.windows_powershell_prompt_cwd_reporting {
            reported_cwd.or_else(|| windows_powershell_current_prompt_cwd(&mut core))
        } else {
            reported_cwd
        };

        let request_render = !synchronized_output && !has_kitty_graphics_sequence;
        let render_delay = render_delay_after_pty_write(
            synchronized_output,
            has_kitty_graphics_sequence,
            cursor_position_settle_pending(&core),
            CURSOR_POSITION_SETTLE_ENABLED,
        );
        if request_render {
            crate::render_prof::event("pty.request_render");
        }
        if render_delay.is_some() {
            crate::render_prof::event("pty.request_render_delayed");
        }
        if synchronized_output {
            crate::render_prof::event("pty.synchronized_output_suppressed");
        }
        ProcessBytesResult {
            request_render,
            render_delay,
            clipboard_writes,
            reported_cwd,
            terminal_responses,
        }
    }

    fn write_pty_bytes_with_ordered_responses(
        &self,
        core: &mut GhosttyPaneCore,
        bytes: &[u8],
        default_color_events: Vec<DefaultColorTrackedEvent>,
        xtgettcap_responses: Vec<XtgettcapResponse>,
        terminal_responses: &mut Vec<Bytes>,
    ) {
        let mut events = Vec::with_capacity(default_color_events.len() + xtgettcap_responses.len());
        events.extend(
            default_color_events
                .into_iter()
                .map(OrderedPtyResponseEvent::DefaultColor),
        );
        events.extend(
            xtgettcap_responses
                .into_iter()
                .map(OrderedPtyResponseEvent::Xtgettcap),
        );
        events.sort_by_key(OrderedPtyResponseEvent::end_offset);

        let mut written = 0;
        for event in events {
            let end_offset = event.end_offset().min(bytes.len());
            if end_offset > written {
                core.terminal.write(&bytes[written..end_offset]);
                terminal_responses.extend(self.drain_pending_pty_responses());
                written = end_offset;
            }
            match event {
                OrderedPtyResponseEvent::DefaultColor(event) => {
                    respond_to_default_color_event(core, terminal_responses, event.event);
                }
                OrderedPtyResponseEvent::Xtgettcap(response) => {
                    terminal_responses.push(response.bytes);
                }
            }
        }

        if written < bytes.len() {
            core.terminal.write(&bytes[written..]);
            terminal_responses.extend(self.drain_pending_pty_responses());
        }

        if !core.child_default_foreground_changed && !core.child_default_background_changed {
            core.transient_default_color_owner_pgid = None;
        }
    }

    fn drain_pending_pty_responses(&self) -> Vec<Bytes> {
        self.pending_pty_responses
            .lock()
            .map(|mut responses| std::mem::take(&mut *responses))
            .unwrap_or_default()
    }

    pub fn seed_history_ansi(&self, ansi: &str) {
        if ansi.is_empty() {
            return;
        }
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        core.terminal.write(ansi.as_bytes());
        if let Ok(mut key_encoder) = self.key_encoder.lock() {
            key_encoder.set_from_terminal(&core.terminal);
        }
    }

    #[cfg(unix)]
    pub fn seed_handoff_input_state(&self, input_state: InputState) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };

        if input_state.alternate_screen {
            core.terminal.write(b"\x1b[?1049h");
        }
        let _ = core.terminal.mode_set(
            crate::ghostty::MODE_APPLICATION_CURSOR_KEYS,
            input_state.application_cursor,
        );
        let _ = core.terminal.mode_set(
            crate::ghostty::MODE_BRACKETED_PASTE,
            input_state.bracketed_paste,
        );
        let _ = core.terminal.mode_set(
            crate::ghostty::MODE_FOCUS_EVENT,
            input_state.focus_reporting,
        );
        let _ = core.terminal.mode_set(
            crate::ghostty::MODE_MOUSE_ALTERNATE_SCROLL,
            input_state.mouse_alternate_scroll,
        );

        for mode in [
            MODE_MOUSE_X10,
            MODE_MOUSE_PRESS_RELEASE,
            MODE_MOUSE_BUTTON_MOTION,
            MODE_MOUSE_ANY_MOTION,
        ] {
            let _ = core.terminal.mode_set(mode, false);
        }
        let mouse_mode = match input_state.mouse_protocol_mode {
            crate::input::MouseProtocolMode::None => None,
            crate::input::MouseProtocolMode::Press => Some(MODE_MOUSE_X10),
            crate::input::MouseProtocolMode::PressRelease => Some(MODE_MOUSE_PRESS_RELEASE),
            crate::input::MouseProtocolMode::ButtonMotion => Some(MODE_MOUSE_BUTTON_MOTION),
            crate::input::MouseProtocolMode::AnyMotion => Some(MODE_MOUSE_ANY_MOTION),
        };
        if let Some(mode) = mouse_mode {
            let _ = core.terminal.mode_set(mode, true);
        }

        let _ = core
            .terminal
            .mode_set(crate::ghostty::MODE_MOUSE_UTF8, false);
        let _ = core
            .terminal
            .mode_set(crate::ghostty::MODE_MOUSE_SGR, false);
        match input_state.mouse_protocol_encoding {
            crate::input::MouseProtocolEncoding::Default => {}
            crate::input::MouseProtocolEncoding::Utf8 => {
                let _ = core
                    .terminal
                    .mode_set(crate::ghostty::MODE_MOUSE_UTF8, true);
            }
            crate::input::MouseProtocolEncoding::Sgr => {
                let _ = core.terminal.mode_set(crate::ghostty::MODE_MOUSE_SGR, true);
            }
        }

        if input_state.modify_other_keys {
            core.terminal.write(b"\x1b[>4;2m");
        }

        if let Ok(mut key_encoder) = self.key_encoder.lock() {
            key_encoder.set_from_terminal(&core.terminal);
        }
    }

    #[cfg(unix)]
    pub fn seed_keyboard_protocol_flags(&self, flags: u16) {
        if flags == 0 {
            return;
        }
        self.seed_keyboard_protocol_ansi(&format!("\x1b[>{flags}u"));
    }

    #[cfg(unix)]
    pub fn seed_keyboard_protocol_ansi(&self, ansi: &str) {
        if ansi.is_empty() {
            return;
        }
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        core.kitty_keyboard.observe(ansi.as_bytes());
        core.terminal.write(ansi.as_bytes());
        if let Ok(mut key_encoder) = self.key_encoder.lock() {
            key_encoder.set_from_terminal(&core.terminal);
        }
    }

    pub fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> Vec<Bytes> {
        if let Ok(mut core) = self.core.lock() {
            let offset_from_bottom = core
                .terminal
                .scrollbar()
                .ok()
                .map(|scrollbar| {
                    scrollbar
                        .total
                        .saturating_sub(scrollbar.offset + scrollbar.len)
                })
                .unwrap_or(0);
            let bottom_before_resize = ghostty_detection_text(&core)
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false);
            let resize_recovery_probe_lines = usize::from(rows)
                .saturating_mul(8)
                .max(DEFAULT_DETECTION_ROWS);
            let replay_ansi = if core.terminal.active_screen().ok()
                == Some(crate::ghostty::ActiveScreen::Primary)
                && bottom_before_resize
            {
                ghostty_recent_ansi(&core, resize_recovery_probe_lines, true)
                    .ok()
                    .filter(|ansi| !ansi.trim().is_empty())
            } else {
                None
            };

            let _ = core
                .terminal
                .resize(cols, rows, cell_width_px, cell_height_px);
            let terminal_responses = self.drain_pending_pty_responses();

            let bottom_is_blank = ghostty_detection_text(&core)
                .map(|text| text.trim().is_empty())
                .unwrap_or(false);
            if bottom_is_blank {
                if let Some(ansi) = replay_ansi.as_deref() {
                    core.terminal.scroll_viewport_bottom();
                    core.terminal.write(ansi.as_bytes());
                }
            }
            ghostty_restore_scroll_offset_from_bottom(&mut core.terminal, offset_from_bottom);
            if offset_from_bottom > 0 {
                let mut remaining = offset_from_bottom.min(resize_recovery_probe_lines);
                while remaining > 0
                    && ghostty_visible_text(&mut core)
                        .map(|text| text.trim().is_empty())
                        .unwrap_or(false)
                {
                    core.terminal.scroll_viewport_delta(1);
                    remaining -= 1;
                }
            }
            terminal_responses
        } else {
            Vec::new()
        }
    }

    pub fn scroll_up(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_delta(-(lines as isize));
        }
    }

    pub fn scroll_down(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_delta(lines as isize);
        }
    }

    pub fn scroll_reset(&self) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_bottom();
        }
    }

    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        if let Ok(mut core) = self.core.lock() {
            core.terminal.scroll_viewport_bottom();
            if lines > 0 {
                core.terminal.scroll_viewport_delta(-(lines as isize));
            }
        }
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
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

    pub fn keyboard_protocol(&self) -> Option<crate::input::KeyboardProtocol> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        Some(crate::input::KeyboardProtocol::from_kitty_flags(
            core.terminal.kitty_keyboard_flags().ok()? as u16,
        ))
    }

    #[cfg(unix)]
    pub fn kitty_keyboard_state_ansi(&self) -> Option<String> {
        let core = self.core.lock().ok()?;
        core.kitty_keyboard.replay_ansi()
    }

    pub fn input_state(&self) -> Option<InputState> {
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
        let mouse_protocol_mode = if core.terminal.mode_get(MODE_MOUSE_ANY_MOTION).ok()? {
            crate::input::MouseProtocolMode::AnyMotion
        } else if core.terminal.mode_get(MODE_MOUSE_BUTTON_MOTION).ok()? {
            crate::input::MouseProtocolMode::ButtonMotion
        } else if core.terminal.mode_get(MODE_MOUSE_PRESS_RELEASE).ok()? {
            crate::input::MouseProtocolMode::PressRelease
        } else if core.terminal.mode_get(MODE_MOUSE_X10).ok()? {
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
            modify_other_keys: core
                .terminal
                .keyboard_state_ansi()
                .ok()
                .is_some_and(|ansi| !ansi.is_empty()),
        })
    }

    pub fn wheel_routing(&self) -> Option<crate::pane::WheelRouting> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        let alternate_screen =
            core.terminal.active_screen().ok()? == crate::ghostty::ActiveScreen::Alternate;
        let mouse_alternate_scroll = core
            .terminal
            .mode_get(crate::ghostty::MODE_MOUSE_ALTERNATE_SCROLL)
            .ok()?;
        let mouse_reporting = core.terminal.mode_get(MODE_MOUSE_ANY_MOTION).ok()?
            || core.terminal.mode_get(MODE_MOUSE_BUTTON_MOTION).ok()?
            || core.terminal.mode_get(MODE_MOUSE_PRESS_RELEASE).ok()?
            || core.terminal.mode_get(MODE_MOUSE_X10).ok()?;
        Some(if mouse_reporting {
            crate::pane::WheelRouting::MouseReport
        } else if alternate_screen && mouse_alternate_scroll {
            crate::pane::WheelRouting::AlternateScroll
        } else {
            crate::pane::WheelRouting::HostScroll
        })
    }

    pub fn cursor_state(&self) -> Option<TerminalCursorState> {
        let mut core = self.core.lock().ok()?;
        let current = current_cursor_state(&mut core);
        effective_cursor_state(&mut core, current)
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.core
            .lock()
            .ok()
            .and_then(|core| {
                core.terminal
                    .mode_get(crate::ghostty::MODE_SYNCHRONIZED_OUTPUT)
                    .ok()
            })
            .unwrap_or(false)
    }

    pub fn encode_terminal_key(
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
            Ok(bytes)
                if !bytes.is_empty() && encoded_key_preserves_event_kind(&bytes, key, protocol) =>
            {
                bytes
            }
            Ok(_) | Err(_) => crate::input::encode_terminal_key(key, protocol),
        }
    }

    pub fn encode_mouse_button(
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
        encoder
            .encode(&event)
            .ok()
            .filter(|bytes| !bytes.is_empty())
    }

    pub fn encode_mouse_motion(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        let Ok(core) = self.core.lock() else {
            return None;
        };
        if !core.terminal.mode_get(MODE_MOUSE_ANY_MOTION).ok()? {
            return None;
        }
        let mut encoder = ghostty_mouse_encoder_for_terminal(&core.terminal)?;
        let event = ghostty_mouse_event_from_motion_kind(kind, column, row, modifiers)?;
        encoder
            .encode(&event)
            .ok()
            .filter(|bytes| !bytes.is_empty())
    }

    pub fn encode_mouse_wheel(
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
        encoder
            .encode(&event)
            .ok()
            .filter(|bytes| !bytes.is_empty())
    }

    pub fn visible_text(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_visible_text(&mut core).ok())
            .unwrap_or_default()
    }

    pub fn visible_ansi(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_visible_ansi(&core).ok())
            .unwrap_or_default()
    }

    pub fn detection_text(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_detection_text(&core).ok())
            .unwrap_or_default()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text(&core, lines).ok())
            .unwrap_or_default()
    }

    pub fn recent_ansi(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_ansi(&core, lines, false).ok())
            .unwrap_or_default()
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text_unwrapped(&core, lines).ok())
            .unwrap_or_default()
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_ansi(&core, lines, true).ok())
            .unwrap_or_default()
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_extract_selection(&mut core, selection).ok())
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_visible_hyperlinks(&mut core, area).ok())
            .unwrap_or_default()
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.core
            .lock()
            .ok()
            .and_then(|core| {
                core.terminal
                    .kitty_image_placements_with_data_filter(needs_data)
                    .ok()
            })
            .unwrap_or_default()
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let host_theme = core.host_terminal_theme;
        let initial_default_foreground = core.initial_default_foreground;
        let initial_default_background = core.initial_default_background;
        let GhosttyPaneCore {
            terminal,
            render_state,
            decscusr_tracker,
            ..
        } = &mut *core;
        if render_state.update(terminal).is_err() {
            return;
        }
        let colors = render_state.colors().ok();
        let default_bg = colors
            .and_then(|c| ghostty_default_bg(c.background, host_theme, initial_default_background));
        let default_fg = colors
            .and_then(|c| ghostty_default_fg(c.foreground, host_theme, initial_default_foreground));
        let resolved_fg = colors.map(|c| ghostty_color(c.foreground));
        let resolved_bg = colors.map(|c| ghostty_color(c.background));
        let hide_kitty_placeholders = crate::kitty_graphics::is_enabled();

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
            let mut grapheme_codepoints = Vec::new();
            let mut symbol_scratch = String::new();
            let mut y = 0u16;
            while y < area.height && rows.next() {
                let mut cells = match rows.populate_cells(&mut row_cells) {
                    Ok(cells) => cells,
                    Err(_) => break,
                };
                let mut x = 0u16;
                while x < area.width && cells.next() {
                    let basic = cells.basic_data().unwrap_or_default();
                    let style = ghostty_cell_style(
                        &cells,
                        &basic,
                        default_fg,
                        default_bg,
                        resolved_fg,
                        resolved_bg,
                    );
                    let symbol = match ghostty_buffer_symbol_into(
                        &cells,
                        basic.wide,
                        hide_kitty_placeholders,
                        &mut grapheme_codepoints,
                        &mut symbol_scratch,
                    ) {
                        Ok(symbol) => symbol,
                        Err(_) => {
                            symbol_scratch.clear();
                            symbol_scratch.push_str(ghostty_blank_symbol_for_width(basic.wide));
                            symbol_scratch.as_str()
                        }
                    };
                    let cell = &mut buf[(area.x + x, area.y + y)];
                    cell.reset();
                    cell.set_symbol(symbol);
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

        ghostty_clear_render_dirty(render_state, area.height);

        let current_cursor = cursor_state_from_render_state(render_state, decscusr_tracker);
        if show_cursor {
            if let Some(cursor) =
                effective_cursor_state(&mut core, current_cursor).filter(|cursor| cursor.visible)
            {
                if cursor.x < area.width && cursor.y < area.height {
                    frame.set_cursor_position((area.x + cursor.x, area.y + cursor.y));
                }
            }
        }
    }

    pub fn collect_dirty_patch(
        &self,
        area_width: u16,
        area_height: u16,
    ) -> TerminalDirtyPatchOutcome {
        self.core
            .lock()
            .ok()
            .map(|mut core| ghostty_collect_dirty_patch(&mut core, area_width, area_height))
            .unwrap_or(TerminalDirtyPatchOutcome::Fallback)
    }
}

fn encoded_key_preserves_event_kind(
    bytes: &[u8],
    key: crate::input::TerminalKey,
    protocol: crate::input::KeyboardProtocol,
) -> bool {
    if !protocol.reports_event_types() || key.kind == crossterm::event::KeyEventKind::Press {
        return true;
    }

    std::str::from_utf8(bytes)
        .ok()
        .and_then(crate::input::parse_terminal_key_sequence)
        .is_some_and(|parsed| {
            parsed.code == key.code && parsed.modifiers == key.modifiers && parsed.kind == key.kind
        })
}

fn cursor_position_settle_pending(core: &GhosttyPaneCore) -> bool {
    core.cursor_settle_state.pending()
}

fn effective_cursor_state(
    core: &mut GhosttyPaneCore,
    current: Option<TerminalCursorState>,
) -> Option<TerminalCursorState> {
    if !CURSOR_POSITION_SETTLE_ENABLED {
        return current;
    }
    core.cursor_settle_state
        .reported_cursor(current, Instant::now())
}

fn render_delay_after_pty_write(
    synchronized_output: bool,
    has_kitty_graphics_sequence: bool,
    cursor_position_settle_pending: bool,
    cursor_position_settle_enabled: bool,
) -> Option<Duration> {
    if synchronized_output {
        None
    } else if has_kitty_graphics_sequence {
        Some(KITTY_GRAPHICS_REDRAW_SETTLE)
    } else if cursor_position_settle_enabled && cursor_position_settle_pending {
        Some(CURSOR_POSITION_SETTLE)
    } else {
        None
    }
}

fn current_cursor_state(core: &mut GhosttyPaneCore) -> Option<TerminalCursorState> {
    let GhosttyPaneCore {
        terminal,
        render_state,
        decscusr_tracker,
        ..
    } = core;
    render_state.update(terminal).ok()?;
    cursor_state_from_render_state(render_state, decscusr_tracker)
}

fn cursor_state_from_render_state(
    render_state: &mut crate::ghostty::RenderState,
    decscusr_tracker: &DecscusrTracker,
) -> Option<TerminalCursorState> {
    let cursor = render_state.cursor_viewport().ok()??;
    let shape = if decscusr_tracker.cursor_shape_overridden() {
        render_state
            .cursor_visual_style()
            .ok()
            .zip(render_state.cursor_blinking().ok())
            .map(|(style, blinking)| decscusr_cursor_shape(style, blinking))
            .unwrap_or(0)
    } else {
        0
    };
    Some(TerminalCursorState {
        x: cursor.x,
        y: cursor.y,
        visible: render_state.cursor_visible().ok()?,
        shape,
    })
}

type VisibleHyperlinks = Vec<((u16, u16), String, String)>;

fn ghostty_clear_render_dirty(render_state: &mut crate::ghostty::RenderState, area_height: u16) {
    let Ok(mut row_iterator) = crate::ghostty::RowIterator::new() else {
        return;
    };
    let Ok(mut rows) = render_state.populate_row_iterator(&mut row_iterator) else {
        return;
    };
    let mut y = 0u16;
    while y < area_height && rows.next() {
        let _ = rows.clear_dirty();
        y += 1;
    }
    let _ = render_state.set_dirty(crate::ghostty::Dirty::Clean);
}

fn ghostty_collect_dirty_patch(
    core: &mut GhosttyPaneCore,
    area_width: u16,
    area_height: u16,
) -> TerminalDirtyPatchOutcome {
    let prof_started = crate::render_prof::timer();
    macro_rules! finish {
        ($outcome:expr) => {{
            let outcome = $outcome;
            if let Some(started) = prof_started {
                crate::render_prof::duration("dirty_collect.total", started.elapsed());
                match &outcome {
                    TerminalDirtyPatchOutcome::Clean => {
                        crate::render_prof::event("dirty_collect.clean");
                    }
                    TerminalDirtyPatchOutcome::Fallback => {
                        crate::render_prof::event("dirty_collect.fallback");
                    }
                    TerminalDirtyPatchOutcome::Patch(patch) => {
                        crate::render_prof::event("dirty_collect.patch");
                        crate::render_prof::counter("dirty_collect.rows", patch.rows.len() as u64);
                        let cells = patch.rows.iter().map(|(_, cells)| cells.len() as u64).sum();
                        crate::render_prof::counter("dirty_collect.cells", cells);
                    }
                }
            }
            return outcome;
        }};
    }
    macro_rules! fallback {
        ($reason:literal) => {{
            crate::render_prof::event(concat!("dirty_fallback.", $reason));
            finish!(TerminalDirtyPatchOutcome::Fallback);
        }};
    }

    let host_theme = core.host_terminal_theme;
    let initial_default_foreground = core.initial_default_foreground;
    let initial_default_background = core.initial_default_background;
    let GhosttyPaneCore {
        terminal,
        render_state,
        ..
    } = core;
    if render_state.update(terminal).is_err() {
        fallback!("render_state_update_error");
    }
    match render_state.dirty() {
        Ok(crate::ghostty::Dirty::Clean) => finish!(TerminalDirtyPatchOutcome::Clean),
        Ok(crate::ghostty::Dirty::Partial) => {}
        Ok(crate::ghostty::Dirty::Full) => fallback!("dirty_full"),
        Err(_) => fallback!("dirty_read_error"),
    }

    let colors = render_state.colors().ok();
    let default_bg = colors
        .and_then(|c| ghostty_default_bg(c.background, host_theme, initial_default_background));
    let default_fg = colors
        .and_then(|c| ghostty_default_fg(c.foreground, host_theme, initial_default_foreground));
    let resolved_fg = colors.map(|c| ghostty_color(c.foreground));
    let resolved_bg = colors.map(|c| ghostty_color(c.background));
    let hide_kitty_placeholders = crate::kitty_graphics::is_enabled();

    let Ok(mut row_iterator) = crate::ghostty::RowIterator::new() else {
        fallback!("row_iterator_new_error");
    };
    let Ok(mut row_cells) = crate::ghostty::RowCells::new() else {
        fallback!("row_cells_new_error");
    };
    let Ok(mut rows) = render_state.populate_row_iterator(&mut row_iterator) else {
        fallback!("populate_rows_error");
    };
    let mut grapheme_codepoints = Vec::new();
    let mut symbol_scratch = String::new();
    let mut patch_rows = Vec::new();
    let mut y = 0u16;
    while y < area_height && rows.next() {
        let Ok(dirty) = rows.dirty() else {
            fallback!("row_dirty_read_error");
        };
        if dirty {
            match rows.selection() {
                Ok(None) => {}
                Ok(Some(_)) => fallback!("row_selection_present"),
                Err(_) => fallback!("row_selection_error"),
            }
            let Ok(mut cells) = rows.populate_cells(&mut row_cells) else {
                fallback!("populate_cells_error");
            };
            let mut patch_cells = Vec::with_capacity(usize::from(area_width));
            let mut x = 0u16;
            while x < area_width && cells.next() {
                let Ok(basic) = cells.basic_data() else {
                    fallback!("basic_data_error");
                };
                if basic.has_hyperlink {
                    fallback!("hyperlink_present");
                }
                let style = ghostty_cell_style(
                    &cells,
                    &basic,
                    default_fg,
                    default_bg,
                    resolved_fg,
                    resolved_bg,
                );
                let symbol = match ghostty_buffer_symbol_into(
                    &cells,
                    basic.wide,
                    hide_kitty_placeholders,
                    &mut grapheme_codepoints,
                    &mut symbol_scratch,
                ) {
                    Ok(symbol) => symbol.to_owned(),
                    Err(_) => ghostty_blank_symbol_for_width(basic.wide).to_owned(),
                };
                patch_cells.push(cell_data_from_style(symbol, style));
                x += 1;
            }
            while x < area_width {
                patch_cells.push(blank_cell_data(default_fg, default_bg));
                x += 1;
            }
            patch_rows.push((y, patch_cells));
        }
        y += 1;
    }

    let dirty_ys: std::collections::HashSet<u16> = patch_rows.iter().map(|(row, _)| *row).collect();
    if !dirty_ys.is_empty() {
        let Ok(mut clear_row_iterator) = crate::ghostty::RowIterator::new() else {
            fallback!("clear_row_iterator_new_error");
        };
        let Ok(mut clear_rows) = render_state.populate_row_iterator(&mut clear_row_iterator) else {
            fallback!("clear_populate_rows_error");
        };
        let mut clear_y = 0u16;
        while clear_y < area_height && clear_rows.next() {
            if dirty_ys.contains(&clear_y) && clear_rows.clear_dirty().is_err() {
                fallback!("clear_dirty_error");
            }
            clear_y += 1;
        }
    }
    if render_state
        .set_dirty(crate::ghostty::Dirty::Clean)
        .is_err()
    {
        fallback!("set_clean_error");
    }

    finish!(TerminalDirtyPatchOutcome::Patch(TerminalDirtyPatch {
        rows: patch_rows
    }));
}

fn ghostty_visible_hyperlinks(
    core: &mut GhosttyPaneCore,
    area: Rect,
) -> Result<VisibleHyperlinks, crate::ghostty::Error> {
    let GhosttyPaneCore {
        terminal,
        render_state,
        ..
    } = core;
    render_state.update(terminal)?;
    let mut row_iterator = crate::ghostty::RowIterator::new()?;
    let mut row_cells = crate::ghostty::RowCells::new()?;
    let mut rows = render_state.populate_row_iterator(&mut row_iterator)?;
    let mut links = Vec::new();
    let mut y = 0u16;
    while y < area.height && rows.next() {
        let mut cells = rows.populate_cells(&mut row_cells)?;
        let mut x = 0u16;
        while x < area.width && cells.next() {
            if cells.has_hyperlink()? {
                if let Some(uri) = terminal.viewport_hyperlink_uri(x, y.into())? {
                    links.push(((area.x + x, area.y + y), ghostty_cell_symbol(&cells)?, uri));
                }
            }
            x += 1;
        }
        y += 1;
    }
    Ok(links)
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

fn ghostty_visible_ansi(core: &GhosttyPaneCore) -> Result<String, crate::ghostty::Error> {
    let rows = core.terminal.rows()?;
    let cols = core.terminal.cols()?;
    if rows == 0 || cols == 0 {
        return Ok(String::new());
    }
    core.terminal.read_ansi_viewport(
        (0, 0),
        (cols.saturating_sub(1), u32::from(rows.saturating_sub(1))),
        false,
    )
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

#[cfg(windows)]
fn windows_powershell_current_prompt_cwd(core: &mut GhosttyPaneCore) -> Option<std::path::PathBuf> {
    if core.terminal.active_screen().ok()? != crate::ghostty::ActiveScreen::Primary {
        return None;
    }
    let cursor = current_cursor_state(core)?;
    let rows = core.terminal.rows().ok()?;
    let cols = core.terminal.cols().ok()?;
    if rows == 0 || cols == 0 || cursor.y >= rows {
        return None;
    }
    let total_rows = core.terminal.total_rows().ok()?;
    let viewport_start = total_rows.saturating_sub(usize::from(rows));
    let cursor_row = viewport_start + usize::from(cursor.y);
    if core
        .terminal
        .read_text_screen(
            (0, cursor_row as u32),
            (cols.saturating_sub(1), cursor_row as u32),
            false,
        )
        .ok()?
        .trim()
        .is_empty()
    {
        return None;
    }
    let text = core
        .terminal
        .read_text_screen(
            (0, viewport_start as u32),
            (cols.saturating_sub(1), cursor_row as u32),
            false,
        )
        .ok()?;
    windows_powershell_prompt_cwd(&text)
}

#[cfg(windows)]
fn windows_powershell_prompt_cwd(text: &str) -> Option<std::path::PathBuf> {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(windows_powershell_prompt_line_cwd)
}

#[cfg(windows)]
fn windows_powershell_prompt_line_cwd(line: &str) -> Option<std::path::PathBuf> {
    let line = line.trim_end();
    let rest = line.strip_prefix("PS ")?;
    let marker = rest.find('>')?;
    let mut raw_cwd = rest[..marker].trim_end();
    let suffix = rest[marker..].trim_end();
    if !suffix.chars().all(|ch| ch == '>') {
        return None;
    }
    if let Some((_, filesystem_path)) = raw_cwd.rsplit_once("::") {
        raw_cwd = filesystem_path;
    }
    let cwd = std::path::PathBuf::from(raw_cwd);
    (cwd.is_absolute() && cwd.is_dir()).then_some(cwd)
}

fn ghostty_recent_text(
    core: &GhosttyPaneCore,
    lines: usize,
) -> Result<String, crate::ghostty::Error> {
    let total_rows = core.terminal.total_rows()?;
    let cols = core.terminal.cols()?;
    if total_rows == 0 || cols == 0 {
        return Ok(String::new());
    }
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
    let end = total_rows.saturating_sub(1) as u32;
    core.terminal
        .read_text_screen((0, start), (cols.saturating_sub(1), end), false)
}

fn ghostty_recent_ansi(
    core: &GhosttyPaneCore,
    lines: usize,
    unwrap: bool,
) -> Result<String, crate::ghostty::Error> {
    let total_rows = core.terminal.total_rows()?;
    let cols = core.terminal.cols()?;
    if total_rows == 0 || cols == 0 {
        return Ok(String::new());
    }
    let start = total_rows.saturating_sub(lines) as u32;
    let end = total_rows.saturating_sub(1) as u32;
    core.terminal
        .read_ansi_screen((0, start), (cols.saturating_sub(1), end), false, unwrap)
}

fn ghostty_restore_scroll_offset_from_bottom(
    terminal: &mut crate::ghostty::Terminal,
    offset_from_bottom: usize,
) {
    terminal.scroll_viewport_bottom();
    if offset_from_bottom == 0 {
        return;
    }
    let Ok(scrollbar) = terminal.scrollbar() else {
        return;
    };
    let max_offset = scrollbar.total.saturating_sub(scrollbar.len);
    let offset = offset_from_bottom.min(max_offset).min(isize::MAX as usize) as isize;
    if offset > 0 {
        terminal.scroll_viewport_delta(-offset);
    }
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
        let (wide, graphemes) = core.terminal.screen_cell(x, y)?;
        if wide == crate::ghostty::CellWide::SpacerTail {
            continue;
        }
        if graphemes.is_empty()
            || graphemes.first().copied() == Some(crate::ghostty::KITTY_UNICODE_PLACEHOLDER)
        {
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
    if cells.wide()? == crate::ghostty::CellWide::SpacerTail {
        return Ok(String::new());
    }
    let text = cells.grapheme_text()?;
    if text.chars().next().map(u32::from) == Some(crate::ghostty::KITTY_UNICODE_PLACEHOLDER) {
        return Ok(" ".to_string());
    }
    if text.is_empty() {
        return Ok(" ".to_string());
    }
    Ok(text)
}

pub(super) fn ghostty_blank_symbol_for_width(wide: crate::ghostty::CellWide) -> &'static str {
    match wide {
        crate::ghostty::CellWide::Wide => "  ",
        crate::ghostty::CellWide::SpacerTail => "",
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => " ",
    }
}

#[cfg(test)]
pub(super) fn ghostty_normalize_buffer_symbol(
    symbol: &str,
    wide: crate::ghostty::CellWide,
) -> String {
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

fn ghostty_buffer_symbol_into<'a>(
    cells: &crate::ghostty::RowCellIter<'_>,
    wide: crate::ghostty::CellWide,
    hide_kitty_placeholders: bool,
    grapheme_codepoints: &mut Vec<u32>,
    symbol_scratch: &'a mut String,
) -> Result<&'a str, crate::ghostty::Error> {
    symbol_scratch.clear();
    match wide {
        crate::ghostty::CellWide::SpacerTail => {}
        crate::ghostty::CellWide::SpacerHead => symbol_scratch.push(' '),
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::Wide => {
            cells.grapheme_text_into(grapheme_codepoints, symbol_scratch)?;
            let hidden_kitty_placeholder = hide_kitty_placeholders
                && symbol_scratch.chars().next().map(u32::from)
                    == Some(crate::ghostty::KITTY_UNICODE_PLACEHOLDER);
            if hidden_kitty_placeholder || symbol_scratch.is_empty() {
                symbol_scratch.clear();
                symbol_scratch.push(' ');
            }
        }
    }

    let expected_width = match wide {
        crate::ghostty::CellWide::Wide => 2,
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => 1,
        crate::ghostty::CellWide::SpacerTail => 0,
    };
    let actual_width = symbol_scratch.width();
    if actual_width != expected_width
        && !(wide == crate::ghostty::CellWide::Narrow && actual_width == 2)
    {
        symbol_scratch.clear();
        symbol_scratch.push_str(ghostty_blank_symbol_for_width(wide));
    }

    Ok(symbol_scratch.as_str())
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

fn blank_cell_data(default_fg: Option<Color>, default_bg: Option<Color>) -> CellData {
    cell_data_from_style(
        " ".to_string(),
        ghostty_default_style(default_fg, default_bg),
    )
}

fn cell_data_from_style(symbol: String, style: Style) -> CellData {
    CellData {
        symbol: if symbol.is_empty() {
            " ".to_string()
        } else {
            symbol
        },
        fg: crate::protocol::color_to_u32(style.fg.unwrap_or(Color::Reset)),
        bg: crate::protocol::color_to_u32(style.bg.unwrap_or(Color::Reset)),
        modifier: crate::protocol::modifier_to_u16(style.add_modifier),
        skip: false,
        hyperlink: None,
    }
}

fn ghostty_default_style(default_fg: Option<Color>, default_bg: Option<Color>) -> Style {
    let mut style = Style::default();
    if let Some(fg) = default_fg {
        style = style.fg(fg);
    }
    if let Some(bg) = default_bg {
        style = style.bg(bg);
    }
    style
}

fn ghostty_cell_style(
    cells: &crate::ghostty::RowCellIter<'_>,
    basic: &crate::ghostty::CellBasicData,
    default_fg: Option<Color>,
    default_bg: Option<Color>,
    resolved_fg: Option<Color>,
    resolved_bg: Option<Color>,
) -> Style {
    let mut fg = basic
        .style
        .fg_color
        .map(ghostty_cell_color)
        .or_else(|| cells.fg_color().ok().flatten().map(ghostty_color))
        .or(default_fg);
    let mut bg = cells
        .content_bg_color()
        .ok()
        .flatten()
        .or(basic.style.bg_color)
        .map(ghostty_cell_color)
        .or_else(|| cells.bg_color().ok().flatten().map(ghostty_color))
        .or(default_bg);
    if basic.style.invisible {
        fg = bg.or(default_bg);
    }
    if basic.style.inverse {
        // When the background is transparent (None), resolve it to the
        // actual terminal background color before swapping.  Otherwise
        // the swapped fg becomes None (Color::Reset) which the host
        // terminal renders as its default foreground — the same hue as
        // the new bg, making inverse text invisible.
        if bg.is_none() {
            bg = resolved_bg;
        }
        if fg.is_none() {
            fg = resolved_fg;
        }
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = ghostty_default_style(fg, bg);
    if let Some(underline_color) = basic.style.underline_color.map(ghostty_cell_color) {
        style = style.underline_color(underline_color);
    }
    let mut modifiers = Modifier::empty();
    if basic.style.bold {
        modifiers |= Modifier::BOLD;
    }
    if basic.style.italic {
        modifiers |= Modifier::ITALIC;
    }
    if basic.style.faint {
        modifiers |= Modifier::DIM;
    }
    if basic.style.blink {
        modifiers |= Modifier::SLOW_BLINK;
    }
    if basic.style.underlined {
        modifiers |= Modifier::UNDERLINED;
    }
    if basic.style.strikethrough {
        modifiers |= Modifier::CROSSED_OUT;
    }
    modifiers = crate::protocol::modifier_with_underline_style(modifiers, basic.style.underline);
    style.add_modifier(modifiers)
}

#[derive(Debug)]
enum OrderedPtyResponseEvent {
    DefaultColor(DefaultColorTrackedEvent),
    Xtgettcap(XtgettcapResponse),
}

impl OrderedPtyResponseEvent {
    fn end_offset(&self) -> usize {
        match self {
            Self::DefaultColor(event) => event.end_offset,
            Self::Xtgettcap(response) => response.end_offset,
        }
    }
}

fn respond_to_default_color_event(
    core: &mut GhosttyPaneCore,
    terminal_responses: &mut Vec<Bytes>,
    event: DefaultColorEvent,
) {
    match event {
        DefaultColorEvent::Query(query) => {
            if let Some(response) = default_color_query_response(query, core) {
                terminal_responses.push(response);
            }
        }
        DefaultColorEvent::PaletteQuery(index) => {
            if let Some(response) = palette_color_query_response(index, core) {
                terminal_responses.push(response);
            }
        }
        DefaultColorEvent::Set(query) => mark_child_default_color_changed(core, query, true),
        DefaultColorEvent::Reset(query) => {
            mark_child_default_color_changed(core, query, false);
            apply_cached_host_default_color(core, query);
        }
    }
}

fn default_color_query_response(
    query: DefaultColorQuery,
    core: &mut GhosttyPaneCore,
) -> Option<Bytes> {
    let color = match query {
        DefaultColorQuery::Foreground if !core.child_default_foreground_changed => core
            .host_terminal_theme
            .foreground
            .map(host_theme_color_to_ghostty),
        DefaultColorQuery::Background if !core.child_default_background_changed => core
            .host_terminal_theme
            .background
            .map(host_theme_color_to_ghostty),
        DefaultColorQuery::Cursor => cursor_color_query_color(core),
        _ => None,
    }?;
    Some(osc_rgb_response(
        &query.osc_number().to_string(),
        color.r,
        color.g,
        color.b,
    ))
}

fn cursor_color_query_color(core: &mut GhosttyPaneCore) -> Option<crate::ghostty::RgbColor> {
    let host_foreground = core.host_terminal_theme.foreground;
    let child_foreground_changed = core.child_default_foreground_changed;
    core.terminal
        .effective_cursor_color()
        .ok()
        .flatten()
        .or_else(|| {
            if child_foreground_changed {
                core.terminal.effective_foreground_color().ok().flatten()
            } else {
                host_foreground
                    .map(host_theme_color_to_ghostty)
                    .or_else(|| core.terminal.effective_foreground_color().ok().flatten())
            }
        })
}

fn palette_color_query_response(index: u8, core: &mut GhosttyPaneCore) -> Option<Bytes> {
    let GhosttyPaneCore {
        terminal,
        render_state,
        ..
    } = core;
    render_state.update(terminal).ok()?;
    let colors = render_state.colors().ok()?;
    let color = colors.palette[usize::from(index)];
    Some(osc_rgb_response(
        &format!("4;{index}"),
        color.r,
        color.g,
        color.b,
    ))
}

fn osc_rgb_response(command: &str, r: u8, g: u8, b: u8) -> Bytes {
    let r = u16::from(r) * 257;
    let g = u16::from(g) * 257;
    let b = u16::from(b) * 257;
    Bytes::from(format!("\x1b]{command};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\"))
}

fn host_theme_color_to_ghostty(color: crate::terminal_theme::RgbColor) -> crate::ghostty::RgbColor {
    crate::ghostty::RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn apply_cached_host_default_color(core: &mut GhosttyPaneCore, query: DefaultColorQuery) {
    write_host_terminal_theme_selective(
        &mut core.terminal,
        core.host_terminal_theme,
        matches!(query, DefaultColorQuery::Foreground),
        matches!(query, DefaultColorQuery::Background),
    );
}

fn mark_child_default_color_changed(
    core: &mut GhosttyPaneCore,
    query: DefaultColorQuery,
    changed: bool,
) {
    match query {
        DefaultColorQuery::Foreground => core.child_default_foreground_changed = changed,
        DefaultColorQuery::Background => core.child_default_background_changed = changed,
        DefaultColorQuery::Cursor => {}
    }
}

fn ghostty_default_fg(
    color: crate::ghostty::RgbColor,
    host_theme: crate::terminal_theme::TerminalTheme,
    initial_default_foreground: Option<crate::ghostty::RgbColor>,
) -> Option<Color> {
    if let Some(host_foreground) = host_theme.foreground {
        if host_foreground == terminal_theme_color(color) {
            None
        } else {
            Some(ghostty_color(color))
        }
    } else if initial_default_foreground.is_some_and(|initial| initial != color) {
        Some(ghostty_color(color))
    } else {
        None
    }
}

fn ghostty_default_bg(
    color: crate::ghostty::RgbColor,
    host_theme: crate::terminal_theme::TerminalTheme,
    initial_default_background: Option<crate::ghostty::RgbColor>,
) -> Option<Color> {
    if let Some(host_background) = host_theme.background {
        if host_background == terminal_theme_color(color) {
            None
        } else {
            Some(ghostty_color(color))
        }
    } else if initial_default_background.is_some_and(|initial| initial != color) {
        Some(ghostty_color(color))
    } else {
        None
    }
}

fn terminal_theme_color(color: crate::ghostty::RgbColor) -> crate::terminal_theme::RgbColor {
    crate::terminal_theme::RgbColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

fn ghostty_cell_color(color: crate::ghostty::CellColor) -> Color {
    match color {
        crate::ghostty::CellColor::Palette(index) => Color::Indexed(index),
        crate::ghostty::CellColor::Rgb(color) => ghostty_color(color),
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

pub(super) fn trim_trailing_blank_rows(rows: &mut Vec<String>) {
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

fn contains_kitty_graphics_sequence(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

fn should_probe_host_terminal_theme_restore(core: &GhosttyPaneCore) -> bool {
    if core.transient_default_color_owner_pgid.is_none() || core.host_terminal_theme.is_empty() {
        return false;
    }

    !core
        .terminal
        .active_screen()
        .map(|screen| screen == crate::ghostty::ActiveScreen::Alternate)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{layout::Rect, style::Color};
    use tokio::sync::mpsc;

    fn write_numbered_lines(terminal: &mut crate::ghostty::Terminal, count: usize) {
        for i in 0..count {
            terminal.write(format!("{i:06}\r\n").as_bytes());
        }
    }

    fn write_wrapped_contract_lines(terminal: &mut crate::ghostty::Terminal, count: usize) {
        for i in 0..count {
            terminal.write(format!("WRAP-{i:03}-abcdefghijklmnopqrstuvwxyz\r\n").as_bytes());
        }
        terminal.write(b"END");
    }

    fn current_palette_color(pane: &GhosttyPaneTerminal, index: u8) -> crate::ghostty::RgbColor {
        let mut core = pane.core.lock().unwrap();
        let GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        render_state.update(terminal).unwrap();
        render_state.colors().unwrap().palette[usize::from(index)]
    }

    fn expected_osc_rgb_response(command: &str, color: crate::ghostty::RgbColor) -> Bytes {
        let r = u16::from(color.r) * 257;
        let g = u16::from(color.g) * 257;
        let b = u16::from(color.b) * 257;
        Bytes::from(format!("\x1b]{command};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\"))
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_prompt_cwd_uses_latest_default_prompt_path() {
        let cwd = std::env::current_dir().unwrap();
        let text = format!("PS C:\\old> cd {}\nPS {}>", cwd.display(), cwd.display());

        assert_eq!(windows_powershell_prompt_cwd(&text).as_ref(), Some(&cwd));
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_prompt_cwd_requires_current_prompt_line() {
        let cwd = std::env::current_dir().unwrap();
        let text = format!("PS {}>\ncommand output", cwd.display());

        assert_eq!(windows_powershell_prompt_cwd(&text), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_powershell_prompt_cwd_ignores_command_echo() {
        let cwd = std::env::current_dir().unwrap();
        let text = format!("PS {}> echo hi", cwd.display());

        assert_eq!(windows_powershell_prompt_cwd(&text), None);
    }

    #[cfg(windows)]
    fn process_windows_powershell_prompt_bytes(
        bytes: &[u8],
        cols: u16,
        rows: u16,
        enabled: bool,
    ) -> ProcessBytesResult {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(cols, rows, 100).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        pane.set_windows_powershell_prompt_cwd_reporting(enabled);
        pane.process_pty_bytes(PaneId::from_raw(1), 0, bytes, &tx)
    }

    #[cfg(windows)]
    #[test]
    fn process_pty_bytes_reports_windows_powershell_prompt_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let bytes = format!("PS C:\\old> cd {}\r\nPS {}>", cwd.display(), cwd.display());

        let result = process_windows_powershell_prompt_bytes(bytes.as_bytes(), 80, 24, true);

        assert_eq!(result.reported_cwd.as_ref(), Some(&cwd));
    }

    #[cfg(windows)]
    #[test]
    fn process_pty_bytes_reports_wrapped_windows_powershell_prompt_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let bytes = format!("PS {}>", cwd.display());

        let result = process_windows_powershell_prompt_bytes(bytes.as_bytes(), 12, 8, true);

        assert_eq!(result.reported_cwd.as_ref(), Some(&cwd));
    }

    #[cfg(windows)]
    #[test]
    fn process_pty_bytes_ignores_prompt_like_output_on_previous_line() {
        let cwd = std::env::current_dir().unwrap();
        let bytes = format!("PS {}>\r\n", cwd.display());

        let result = process_windows_powershell_prompt_bytes(bytes.as_bytes(), 80, 24, true);

        assert_eq!(result.reported_cwd, None);
    }

    #[cfg(windows)]
    #[test]
    fn process_pty_bytes_ignores_windows_powershell_prompt_cwd_on_alternate_screen() {
        let cwd = std::env::current_dir().unwrap();
        let bytes = format!("\x1b[?1049hPS {}>", cwd.display());

        let result = process_windows_powershell_prompt_bytes(bytes.as_bytes(), 80, 24, true);

        assert_eq!(result.reported_cwd, None);
    }

    #[cfg(windows)]
    #[test]
    fn process_pty_bytes_skips_windows_powershell_prompt_cwd_when_disabled() {
        let cwd = std::env::current_dir().unwrap();
        let bytes = format!("PS {}>", cwd.display());

        let result = process_windows_powershell_prompt_bytes(bytes.as_bytes(), 80, 24, false);

        assert_eq!(result.reported_cwd, None);
    }

    fn expected_xtgettcap_response(cap_hex: &str, value: Option<&[u8]>) -> Bytes {
        let mut response = format!("\x1bP1+r{cap_hex}").into_bytes();
        if let Some(value) = value {
            response.push(b'=');
            append_upper_hex(value, &mut response);
        }
        response.extend_from_slice(b"\x1b\\");
        Bytes::from(response)
    }

    fn append_upper_hex(bytes: &[u8], output: &mut Vec<u8>) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for &byte in bytes {
            output.push(HEX[usize::from(byte >> 4)]);
            output.push(HEX[usize::from(byte & 0x0f)]);
        }
    }

    #[test]
    fn decscusr_cursor_shape_preserves_blinking_variants() {
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Block, true),
            1
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Block, false),
            2
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Underline, true),
            3
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Underline, false),
            4
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Bar, true),
            5
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::Bar, false),
            6
        );
        assert_eq!(
            decscusr_cursor_shape(crate::ghostty::CursorVisualStyle::BlockHollow, false),
            2
        );
    }

    #[test]
    fn cursor_state_uses_terminal_default_until_child_sets_shape() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        assert_eq!(pane.cursor_state().unwrap().shape, 0);

        pane.process_pty_bytes(pane_id, 0, b"\x1b[6 q", &tx);

        assert_eq!(pane.cursor_state().unwrap().shape, 6);
    }

    #[test]
    fn cursor_state_returns_terminal_default_after_decscusr_reset() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"\x1b[2 q", &tx);
        assert_eq!(pane.cursor_state().unwrap().shape, 2);

        pane.process_pty_bytes(pane_id, 0, b"\x1b[0 q", &tx);

        assert_eq!(pane.cursor_state().unwrap().shape, 0);
    }

    #[test]
    fn cursor_shape_tracker_handles_split_decscusr_sequences() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"\x1b[", &tx);
        pane.process_pty_bytes(pane_id, 0, b"5 ", &tx);
        pane.process_pty_bytes(pane_id, 0, b"q", &tx);

        assert_eq!(pane.cursor_state().unwrap().shape, 5);
    }

    #[test]
    #[cfg(windows)]
    fn cursor_state_holds_pty_position_change_until_settle_window() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"x", &tx);
        assert_eq!(
            pane.cursor_state()
                .map(|cursor| (cursor.x, cursor.y, cursor.visible)),
            Some((1, 0, true))
        );

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b[6;21H", &tx);

        assert_eq!(result.render_delay, Some(CURSOR_POSITION_SETTLE));
        assert_eq!(
            pane.cursor_state()
                .map(|cursor| (cursor.x, cursor.y, cursor.visible)),
            Some((1, 0, true))
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn cursor_state_uses_live_position_when_settle_policy_disabled() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"x", &tx);
        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b[6;21H", &tx);

        assert_eq!(result.render_delay, None);
        assert_eq!(
            pane.cursor_state()
                .map(|cursor| (cursor.x, cursor.y, cursor.visible)),
            Some((20, 5, true))
        );
    }

    #[test]
    fn cursor_settle_policy_controls_render_delay() {
        assert_eq!(
            render_delay_after_pty_write(false, false, true, true),
            Some(CURSOR_POSITION_SETTLE)
        );
        assert_eq!(
            render_delay_after_pty_write(false, false, true, false),
            None
        );
        assert_eq!(
            render_delay_after_pty_write(false, true, true, false),
            Some(KITTY_GRAPHICS_REDRAW_SETTLE)
        );
        assert_eq!(render_delay_after_pty_write(true, false, true, true), None);
    }

    #[test]
    fn host_terminal_theme_restore_probe_skips_when_no_transient_override() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let core = pane.core.lock().unwrap();

        assert!(!should_probe_host_terminal_theme_restore(&core));
    }

    #[test]
    fn host_terminal_theme_restore_probe_skips_when_host_theme_unknown() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
        }
        let core = pane.core.lock().unwrap();

        assert!(!should_probe_host_terminal_theme_restore(&core));
    }

    #[test]
    fn host_terminal_theme_restore_probe_skips_on_alternate_screen() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1049h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.host_terminal_theme = crate::terminal_theme::TerminalTheme {
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
        }
        let core = pane.core.lock().unwrap();

        assert!(!should_probe_host_terminal_theme_restore(&core));
    }

    #[test]
    fn host_terminal_theme_restore_probe_runs_when_restore_is_pending() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.host_terminal_theme = crate::terminal_theme::TerminalTheme {
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
        }
        let core = pane.core.lock().unwrap();

        assert!(should_probe_host_terminal_theme_restore(&core));
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
    fn ghostty_enter_backspace_release_in_legacy_pane_emits_nothing() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        for code in [
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyCode::Backspace,
        ] {
            let press = pane.encode_terminal_key(
                crate::input::TerminalKey::new(code, crossterm::event::KeyModifiers::empty()),
                crate::input::KeyboardProtocol::Legacy,
            );
            let release = pane.encode_terminal_key(
                crate::input::TerminalKey::new(code, crossterm::event::KeyModifiers::empty())
                    .with_kind(crossterm::event::KeyEventKind::Release),
                crate::input::KeyboardProtocol::Legacy,
            );
            assert!(!press.is_empty(), "{code:?} press should emit bytes");
            assert!(
                release.is_empty(),
                "{code:?} release should emit nothing in a legacy pane, got {release:?}"
            );
        }
    }

    #[test]
    fn ghostty_release_still_encoded_for_report_event_pane() {
        let (tx, _rx) = mpsc::channel(4);
        // Push kitty flags including REPORT_EVENT_TYPES (0b10) + DISAMBIGUATE (0b1).
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[>3u");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let release = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )
            .with_kind(crossterm::event::KeyEventKind::Release),
            pane.keyboard_protocol().unwrap(),
        );
        let parsed =
            crate::input::parse_terminal_key_sequence(std::str::from_utf8(&release).unwrap())
                .unwrap();
        assert_eq!(parsed.code, crossterm::event::KeyCode::Enter);
        assert_eq!(
            parsed.kind,
            crossterm::event::KeyEventKind::Release,
            "report-event pane should encode a release, got {release:?}"
        );
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

    #[cfg(unix)]
    #[test]
    fn ghostty_seed_handoff_input_state_restores_input_modes() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        pane.seed_handoff_input_state(InputState {
            alternate_screen: true,
            application_cursor: true,
            bracketed_paste: true,
            focus_reporting: true,
            mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
            mouse_alternate_scroll: true,
            modify_other_keys: true,
        });

        assert_eq!(
            pane.input_state(),
            Some(InputState {
                alternate_screen: true,
                application_cursor: true,
                bracketed_paste: true,
                focus_reporting: true,
                mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
                mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
                mouse_alternate_scroll: true,
                modify_other_keys: true,
            })
        );

        let encoded = pane.encode_terminal_key(
            crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::empty(),
            ),
            crate::input::KeyboardProtocol::Legacy,
        );
        assert_eq!(encoded, b"\x1bOA");

        let key = crate::input::parse_terminal_key_sequence("\x1b[13;2u").unwrap();
        let encoded = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);
        assert_eq!(encoded, b"\x1b[27;2;13~");
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
    fn ghostty_kitty_pane_encodes_shift_enter_as_csi_u() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.process_pty_bytes(pane_id, 0, b"\x1b[>5u", &tx);

        let key = crate::input::parse_terminal_key_sequence("\x1b[13;2u").unwrap();
        let encoded = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

        assert_eq!(
            pane.keyboard_protocol(),
            Some(crate::input::KeyboardProtocol::Kitty { flags: 5 })
        );
        assert_eq!(encoded, b"\x1b[13;2u");
    }

    #[cfg(unix)]
    #[test]
    fn ghostty_seed_keyboard_protocol_flags_restores_shift_enter_encoding() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        pane.seed_keyboard_protocol_flags(5);

        let key = crate::input::parse_terminal_key_sequence("\x1b[13;2u").unwrap();
        let encoded = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

        assert_eq!(
            pane.keyboard_protocol(),
            Some(crate::input::KeyboardProtocol::Kitty { flags: 5 })
        );
        assert_eq!(encoded, b"\x1b[13;2u");
    }

    #[cfg(unix)]
    #[test]
    fn ghostty_keyboard_protocol_state_replays_nested_stack() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.process_pty_bytes(pane_id, 0, b"\x1b[>1u\x1b[>5u", &tx);

        let ansi = pane.kitty_keyboard_state_ansi().unwrap();

        let (restored_tx, _restored_rx) = mpsc::channel(4);
        let restored_terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let restored = GhosttyPaneTerminal::new(restored_terminal, restored_tx).unwrap();
        restored.seed_keyboard_protocol_ansi(&ansi);
        assert_eq!(
            restored.keyboard_protocol(),
            Some(crate::input::KeyboardProtocol::Kitty { flags: 5 })
        );

        let (pop_tx, _pop_rx) = mpsc::channel(4);
        restored.process_pty_bytes(pane_id, 0, b"\x1b[<u", &pop_tx);
        assert_eq!(
            restored.keyboard_protocol(),
            Some(crate::input::KeyboardProtocol::Kitty { flags: 1 })
        );
    }

    #[test]
    fn ghostty_modify_other_keys_mode_one_preserves_shift_enter() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.process_pty_bytes(pane_id, 0, b"\x1b[>4;1m", &tx);

        let key = crate::input::parse_terminal_key_sequence("\x1b[13;2u").unwrap();
        let encoded = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

        assert_eq!(encoded, b"\x1b[27;2;13~");
    }

    #[test]
    fn ghostty_kitty_pane_encodes_parsed_legacy_alt_backspace_as_csi_u() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.process_pty_bytes(pane_id, 0, b"\x1b[>1u", &tx);

        let key = crate::input::parse_terminal_key_sequence("\x1b\x7f").unwrap();
        let encoded = pane.encode_terminal_key(key, crate::input::KeyboardProtocol::Legacy);

        assert_eq!(encoded, b"\x1b[127;3u");
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
    fn ghostty_mouse_drag_without_motion_reporting_is_not_forwarded() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1000h\x1b[?1006h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_mouse_button(
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            4,
            6,
            crossterm::event::KeyModifiers::empty(),
        );

        assert_eq!(encoded, None);
    }

    #[test]
    fn ghostty_mouse_moved_encoding_uses_any_motion_state() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1003h\x1b[?1006h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_mouse_motion(
            crossterm::event::MouseEventKind::Moved,
            4,
            6,
            crossterm::event::KeyModifiers::empty(),
        );

        assert_eq!(encoded.as_deref(), Some(&b"\x1b[<35;5;7M"[..]));
    }

    #[test]
    fn ghostty_mouse_sgr_pixels_downgrades_to_cell_coordinates() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.write(b"\x1b[?1003h\x1b[?1006h\x1b[?1016h");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let encoded = pane.encode_mouse_motion(
            crossterm::event::MouseEventKind::Moved,
            4,
            6,
            crossterm::event::KeyModifiers::empty(),
        );

        assert_eq!(encoded.as_deref(), Some(&b"\x1b[<35;5;7M"[..]));
    }

    #[test]
    fn ghostty_normalize_buffer_symbol_prefers_grapheme_width_when_metadata_disagrees() {
        const WIDE_GRAPHEME: &str = "🙂";
        const FLAG_GRAPHEME: &str = "🇧🇷";
        const FAMILY_GRAPHEME: &str = "👨‍👩‍👧";
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
            ghostty_normalize_buffer_symbol(FLAG_GRAPHEME, crate::ghostty::CellWide::Wide),
            FLAG_GRAPHEME
        );
        assert_eq!(
            ghostty_normalize_buffer_symbol(FAMILY_GRAPHEME, crate::ghostty::CellWide::Wide),
            FAMILY_GRAPHEME
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

    fn render_cells_to_symbols(
        terminal: &mut crate::ghostty::Terminal,
    ) -> Vec<(crate::ghostty::CellWide, String)> {
        let mut render_state = crate::ghostty::RenderState::new().unwrap();
        render_state.update(terminal).unwrap();

        let mut row_iterator = crate::ghostty::RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        let mut row_cells = crate::ghostty::RowCells::new().unwrap();
        let mut grapheme_codepoints = Vec::new();
        let mut symbol_scratch = String::new();
        let mut out = Vec::new();

        if rows.next() {
            let mut cells = rows.populate_cells(&mut row_cells).unwrap();
            while cells.next() {
                let wide = cells.wide().unwrap_or(crate::ghostty::CellWide::Narrow);
                let symbol = ghostty_buffer_symbol_into(
                    &cells,
                    wide,
                    false,
                    &mut grapheme_codepoints,
                    &mut symbol_scratch,
                )
                .unwrap()
                .to_string();
                out.push((wide, symbol));
            }
        }

        out
    }

    #[test]
    fn grapheme_cluster_mode_renders_flag_emoji_in_single_wide_cell() {
        let mut terminal = crate::ghostty::Terminal::new(40, 1, 0).unwrap();
        terminal.enable_grapheme_cluster_mode().unwrap();
        terminal.write("🇧🇷".as_bytes());

        let cells = render_cells_to_symbols(&mut terminal);

        assert!(
            cells
                .iter()
                .any(|(wide, symbol)| *wide == crate::ghostty::CellWide::Wide && symbol == "🇧🇷"),
            "expected a wide cell containing the full flag grapheme, got {cells:?}"
        );
    }

    #[test]
    fn grapheme_cluster_mode_renders_zwj_family_in_single_wide_cell() {
        let mut terminal = crate::ghostty::Terminal::new(40, 1, 0).unwrap();
        terminal.enable_grapheme_cluster_mode().unwrap();
        terminal.write("👨\u{200d}👩\u{200d}👧".as_bytes());

        let cells = render_cells_to_symbols(&mut terminal);

        assert!(
            cells
                .iter()
                .any(|(wide, symbol)| *wide == crate::ghostty::CellWide::Wide
                    && symbol == "👨\u{200d}👩\u{200d}👧"),
            "expected a wide cell containing the full ZWJ grapheme, got {cells:?}"
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
    fn plain_text_reads_skip_wide_character_spacer_cells() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(40, 3, 100).unwrap();
        terminal.write("日本語テスト ABC 123".as_bytes());
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        assert_eq!(pane.visible_text(), "日本語テスト ABC 123\n");
        assert_eq!(pane.recent_text(3), "日本語テスト ABC 123\n");
        assert_eq!(pane.recent_unwrapped_text(3), "日本語テスト ABC 123");
        assert_eq!(pane.detection_text(), "日本語テスト ABC 123\n");
    }

    #[test]
    fn visible_ansi_preserves_cell_style_sequences() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(20, 3, 100).unwrap();
        terminal.write(b"\x1b[31;1mred\x1b[0m plain");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let ansi = pane.visible_ansi();
        assert!(ansi.contains("red"));
        assert!(ansi.contains("plain"));
        assert!(ansi.contains("\x1b["));
    }

    #[test]
    fn recent_ansi_can_read_styled_scrollback() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(20, 3, 100).unwrap();
        terminal.write(b"\x1b[34mblue\x1b[0m\r\nline2\r\nline3\r\nline4");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let ansi = pane.recent_ansi(4);
        assert!(ansi.contains("blue"));
        assert!(ansi.contains("line4"));
        assert!(ansi.contains("\x1b["));
    }

    #[test]
    fn resize_reflow_keeps_scrolled_viewport_and_bottom_detection_sane() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(12, 4, 10_000).unwrap();
        write_wrapped_contract_lines(&mut terminal, 40);
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let bottom_snapshot = pane.detection_text();
        assert!(bottom_snapshot.contains("END"));

        let initial = pane.scroll_metrics().expect("initial scroll metrics");
        assert!(initial.max_offset_from_bottom > 0);
        pane.set_scroll_offset_from_bottom(initial.max_offset_from_bottom / 2);
        assert!(!pane.visible_text().trim().is_empty());

        for (rows, cols) in [(4, 10), (4, 7), (6, 18), (3, 9), (5, 12)] {
            pane.resize(rows, cols, 0, 0);

            let metrics = pane.scroll_metrics().expect("scroll metrics after resize");
            assert_eq!(metrics.viewport_rows, rows as usize);
            assert!(metrics.offset_from_bottom <= metrics.max_offset_from_bottom);
            assert!(
                metrics.offset_from_bottom > 0,
                "resize should preserve a scrolled viewport instead of jumping to bottom"
            );
            assert!(metrics.max_offset_from_bottom > 0);
            let visible = pane.visible_text();
            assert!(
                !visible.trim().is_empty(),
                "visible text should not be empty after resize to {rows}x{cols}; metrics={metrics:?}; detection={:?}; recent={:?}",
                pane.detection_text(),
                pane.recent_text(6)
            );
            assert!(
                pane.detection_text().contains("END"),
                "bottom detection should remain independent from the scrolled viewport after resize"
            );
        }
    }

    #[test]
    fn resize_recovery_does_not_replay_history_when_visible_screen_was_blank() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(20, 3, 10_000).unwrap();
        terminal.write(b"old history\r\n\x1b[2J\x1b[H");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        assert!(pane.visible_text().trim().is_empty());
        assert!(pane.detection_text().trim().is_empty());

        pane.resize(3, 20, 0, 0);

        assert!(pane.visible_text().trim().is_empty());
        assert!(pane.detection_text().trim().is_empty());
        assert!(pane.recent_text(3).trim().is_empty());
    }

    #[test]
    fn resize_recovery_does_not_replay_scrolled_history_over_blank_bottom() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(20, 3, 10_000).unwrap();
        write_numbered_lines(&mut terminal, 20);
        terminal.write(b"\x1b[2J\x1b[H");
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        assert!(pane.detection_text().trim().is_empty());
        let metrics = pane.scroll_metrics().expect("scroll metrics");
        pane.set_scroll_offset_from_bottom(metrics.max_offset_from_bottom);
        assert!(!pane.visible_text().trim().is_empty());

        pane.resize(3, 20, 0, 0);

        assert!(pane.detection_text().trim().is_empty());
        assert!(pane.recent_text(3).trim().is_empty());
    }

    #[test]
    fn resize_returns_in_band_size_report_response() {
        let (tx, _rx) = mpsc::channel(4);
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal.mode_set(2048, true).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();

        let responses = pane.resize(40, 100, 9, 18);

        assert_eq!(
            responses,
            vec![Bytes::from_static(b"\x1B[48;40;100;720;900t")]
        );
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
    fn seeded_history_is_rendered_on_next_draw() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 100).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        pane.seed_history_ansi("restored history");

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..16).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert_eq!(row, "restored history");
    }

    #[test]
    fn render_leaves_unknown_host_default_background_transparent() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
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
        assert_eq!(buffer[(0, 0)].style().fg, Some(Color::Reset));
        assert_eq!(buffer[(0, 0)].style().bg, Some(Color::Reset));
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().fg, Some(Color::Reset));
        assert_eq!(buffer[(2, 0)].style().bg, Some(Color::Reset));
    }

    #[test]
    fn render_blanks_kitty_unicode_placeholders_when_graphics_enabled() {
        crate::kitty_graphics::set_enabled(true);
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal
                .write("before\u{10eeee}\u{0305}\u{0305}after".as_bytes());
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();
        crate::kitty_graphics::set_enabled(false);

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "b");
        assert_eq!(buffer[(6, 0)].symbol(), " ");
        assert_eq!(buffer[(7, 0)].symbol(), "a");
        assert_eq!(pane.visible_text().lines().next(), Some("before after"));
        assert_eq!(pane.recent_text(5), "before after\n");
    }

    #[test]
    fn render_keeps_explicit_cell_foreground_when_host_is_unknown() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[38;2;68;85;102mhi\x1b[0m");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let expected_fg = Some(Color::Rgb(0x44, 0x55, 0x66));
        assert_eq!(buffer[(0, 0)].symbol(), "h");
        assert_eq!(buffer[(0, 0)].style().fg, expected_fg);
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().fg, Some(Color::Reset));
    }

    #[test]
    fn render_keeps_explicit_cell_background_when_host_is_unknown() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[48;2;68;85;102mhi\x1b[0m");
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
        assert_eq!(buffer[(2, 0)].style().bg, Some(Color::Reset));
    }

    #[test]
    fn render_preserves_palette_colors_instead_of_flattening_to_rgb() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(
                b"\x1b[31mR\x1b[0m \x1b[38;5;171mI\x1b[0m \x1b[48;5;4mB\x1b[0m \x1b[38;2;1;2;3mT",
            );
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "R");
        assert_eq!(buffer[(0, 0)].style().fg, Some(Color::Indexed(1)));
        assert_eq!(buffer[(2, 0)].symbol(), "I");
        assert_eq!(buffer[(2, 0)].style().fg, Some(Color::Indexed(171)));
        assert_eq!(buffer[(4, 0)].symbol(), "B");
        assert_eq!(buffer[(4, 0)].style().bg, Some(Color::Indexed(4)));
        assert_eq!(buffer[(6, 0)].symbol(), "T");
        assert_eq!(buffer[(6, 0)].style().fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn render_preserves_palette_background_fill_cells() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[48;5;4m\x1b[K");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        for x in 0..20 {
            assert_eq!(buffer[(x, 0)].symbol(), " ");
            assert_eq!(buffer[(x, 0)].style().bg, Some(Color::Indexed(4)));
        }
    }

    #[test]
    fn render_preserves_rgb_background_fill_cells() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[48;2;17;34;51m\x1b[K");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        for x in 0..20 {
            assert_eq!(buffer[(x, 0)].symbol(), " ");
            assert_eq!(buffer[(x, 0)].style().bg, Some(Color::Rgb(17, 34, 51)));
        }
    }

    #[test]
    fn process_pty_bytes_returns_libghostty_query_responses_without_queuing_input() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b[6n", &tx);

        assert_eq!(result.terminal_responses.len(), 1);
        assert!(String::from_utf8_lossy(&result.terminal_responses[0]).contains('R'));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_xtgettcap_truecolor_query_responses_without_queuing_input() {
        let (tx, mut rx) = mpsc::channel(8);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1bP+q5463;524742;73657472676266;73657472676262\x1b\\",
            &tx,
        );

        assert_eq!(
            result.terminal_responses,
            vec![
                expected_xtgettcap_response("5463", None),
                expected_xtgettcap_response("524742", Some(b"8")),
                expected_xtgettcap_response("73657472676266", Some(b"\\E[38:2:%p1%d:%p2%d:%p3%dm")),
                expected_xtgettcap_response("73657472676262", Some(b"\\E[48:2:%p1%d:%p2%d:%p3%dm")),
            ]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_split_xtgettcap_query_response() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1bP+q4", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"D73\x1b", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"\\", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![expected_xtgettcap_response(
                "4D73",
                Some(b"\\E]52;%p1%s;%p2%s\\007")
            )]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_orders_device_attribute_reply_before_following_xtgettcap_reply() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b[c\x1bP+q5463\x1b\\", &tx);

        assert_eq!(result.terminal_responses.len(), 2);
        assert!(String::from_utf8_lossy(&result.terminal_responses[0]).contains('c'));
        assert_eq!(
            result.terminal_responses[1],
            expected_xtgettcap_response("5463", None)
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_orders_xtgettcap_reply_before_following_device_attribute_reply() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1bP+q5463\x1b\\\x1b[c", &tx);

        assert_eq!(result.terminal_responses.len(), 2);
        assert_eq!(
            result.terminal_responses[0],
            expected_xtgettcap_response("5463", None)
        );
        assert!(String::from_utf8_lossy(&result.terminal_responses[1]).contains('c'));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_orders_xtgettcap_reply_before_following_default_color_reply() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x00,
                g: 0x2b,
                b: 0x36,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1bP+q5463\x1b\\\x1b]11;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![
                expected_xtgettcap_response("5463", None),
                Bytes::from_static(b"\x1b]11;rgb:0000/2b2b/3636\x1b\\"),
            ]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn host_theme_update_preserves_child_default_color_override() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11;#112233\x07", &tx);
        assert!(result.terminal_responses.is_empty());

        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11;?\x07", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn child_default_color_reset_restores_cached_host_color() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        pane.process_pty_bytes(pane_id, 0, b"\x1b]11;#112233\x07", &tx);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            }),
        });
        pane.process_pty_bytes(pane_id, 0, b"\x1b]111\x07", &tx);
        assert!(!pane.has_transient_default_color_override());

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11;?\x07", &tx);
        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_recovers_xtgettcap_after_osc_bel_terminator() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]0;title\x07\x1bP+q5463\x1b\\", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![expected_xtgettcap_response("5463", None)]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_ignores_unknown_and_unsupported_xtgettcap_queries() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1bP+q6E6F7065;4D7\x1b\\", &tx);

        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_underline_color_xtgettcap_query_responses() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1bP+q5375;536D756C78;536574756C63\x1b\\",
            &tx,
        );

        assert_eq!(
            result.terminal_responses,
            vec![
                expected_xtgettcap_response("5375", None),
                expected_xtgettcap_response("536D756C78", Some(b"\\E[4:%p1%dm")),
                expected_xtgettcap_response(
                    "536574756C63",
                    Some(b"\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m")
                ),
            ]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn render_preserves_underline_color() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[4m\x1b[58:2::17:34:51mU");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let style = terminal.backend().buffer()[(0, 0)].style();
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(style.underline_color, Some(Color::Rgb(17, 34, 51)));
    }

    #[test]
    fn dirty_patch_preserves_curly_underline_style() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[4:3mU");
        }

        let patch = match pane.collect_dirty_patch(20, 5) {
            TerminalDirtyPatchOutcome::Patch(patch) => patch,
            other => panic!("expected dirty patch, got {other:?}"),
        };

        let cell = &patch.rows[0].1[0];
        assert_eq!(cell.symbol, "U");
        assert_eq!(
            crate::protocol::underline_style_from_modifier(cell.modifier),
            3
        );
    }

    #[test]
    fn full_frame_preserves_curly_underline_style() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx).unwrap();
        {
            let mut core = pane.core.lock().unwrap();
            core.terminal.write(b"\x1b[4:3mU");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let frame =
            crate::protocol::FrameData::from_ratatui_buffer(terminal.backend().buffer(), None);
        assert_eq!(frame.cells[0].symbol, "U");
        assert_eq!(
            crate::protocol::underline_style_from_modifier(frame.cells[0].modifier),
            3
        );
    }

    #[test]
    fn process_pty_bytes_orders_default_color_reply_before_following_device_attribute_reply() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x00,
                g: 0x2b,
                b: 0x36,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11;?\x07\x1b[c", &tx);

        assert_eq!(result.terminal_responses.len(), 2);
        assert_eq!(
            result.terminal_responses[0],
            Bytes::from_static(b"\x1b]11;rgb:0000/2b2b/3636\x1b\\")
        );
        assert!(String::from_utf8_lossy(&result.terminal_responses[1]).contains('c'));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_palette_color_query_response_without_queuing_input() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        let color = current_palette_color(&pane, 0);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]4;0;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![expected_osc_rgb_response("4;0", color)]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_split_palette_color_query_response() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        let color = current_palette_color(&pane, 255);

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]4;25", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"5;?\x1b", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"\\", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![expected_osc_rgb_response("4;255", color)]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_ignores_malformed_palette_color_queries() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);

        let result = pane.process_pty_bytes(
            pane_id,
            0,
            b"\x1b]4;;?\x07\x1b]4;-1;?\x07\x1b]4;256;?\x07\x1b]4;0;?;1;?\x07\x1b]4;0;rgb:1111/2222/3333\x07",
            &tx,
        );

        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_orders_palette_reply_before_following_terminal_replies() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        let color = current_palette_color(&pane, 0);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x00,
                g: 0x2b,
                b: 0x36,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]4;0;?\x07\x1b]11;?\x07\x1b[c", &tx);

        assert_eq!(result.terminal_responses.len(), 3);
        assert_eq!(
            result.terminal_responses[0],
            expected_osc_rgb_response("4;0", color)
        );
        assert_eq!(
            result.terminal_responses[1],
            Bytes::from_static(b"\x1b]11;rgb:0000/2b2b/3636\x1b\\")
        );
        assert!(String::from_utf8_lossy(&result.terminal_responses[2]).contains('c'));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_default_color_query_responses_without_queuing_input() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x00,
                g: 0x2b,
                b: 0x36,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]11;rgb:0000/2b2b/3636\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_cursor_color_query_response_from_foreground_fallback() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x65,
                g: 0x7b,
                b: 0x83,
            }),
            background: None,
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]12;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]12;rgb:6565/7b7b/8383\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_cursor_color_query_response_from_child_foreground() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x65,
                g: 0x7b,
                b: 0x83,
            }),
            background: None,
        });

        pane.process_pty_bytes(pane_id, 0, b"\x1b]10;rgb:11/22/33\x07", &tx);
        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]12;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]12;rgb:1111/2222/3333\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_explicit_cursor_color_query_response() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x65,
                g: 0x7b,
                b: 0x83,
            }),
            background: None,
        });

        pane.process_pty_bytes(pane_id, 0, b"\x1b]12;rgb:11/22/33\x07", &tx);
        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]12;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]12;rgb:1111/2222/3333\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_default_color_query_responses_in_order() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0x65,
                g: 0x7b,
                b: 0x83,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0xfd,
                g: 0xf6,
                b: 0xe3,
            }),
        });

        let result =
            pane.process_pty_bytes(pane_id, 0, b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![
                Bytes::from_static(b"\x1b]10;rgb:6565/7b7b/8383\x1b\\"),
                Bytes::from_static(b"\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\"),
                Bytes::from_static(b"\x1b]12;rgb:6565/7b7b/8383\x1b\\"),
            ]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_split_default_color_query_response() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0xfd,
                g: 0xf6,
                b: 0xe3,
            }),
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]11", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b";?\x1b", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"\\", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_returns_split_cursor_color_query_response() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0xfd,
                g: 0xf6,
                b: 0xe3,
            }),
            background: None,
        });

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]12", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b";?\x1b", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());
        let result = pane.process_pty_bytes(pane_id, 0, b"\\", &tx);

        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]12;rgb:fdfd/f6f6/e3e3\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn process_pty_bytes_tracks_default_color_set_and_reset_before_replying() {
        let (tx, mut rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(20, 5, 0).unwrap();
        let pane = GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap();
        let pane_id = PaneId::from_raw(1);
        pane.apply_host_terminal_theme(crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 0xfd,
                g: 0xf6,
                b: 0xe3,
            }),
        });

        let result =
            pane.process_pty_bytes(pane_id, 0, b"\x1b]11;rgb:11/22/33\x07\x1b]11;?\x07", &tx);
        assert!(result.terminal_responses.is_empty());
        assert!(rx.try_recv().is_err());

        let result = pane.process_pty_bytes(pane_id, 0, b"\x1b]111\x07\x1b]11;?\x07", &tx);
        assert_eq!(
            result.terminal_responses,
            vec![Bytes::from_static(b"\x1b]11;rgb:fdfd/f6f6/e3e3\x1b\\")]
        );
        assert!(rx.try_recv().is_err());
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
        assert_eq!(buffer[(0, 0)].style().fg, Some(Color::Reset));
        assert_eq!(buffer[(0, 0)].style().bg, Some(Color::Reset));
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().fg, Some(Color::Reset));
        assert_eq!(buffer[(2, 0)].style().bg, Some(Color::Reset));
    }

    #[test]
    fn render_keeps_explicit_default_foreground_when_it_differs_from_host() {
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
            core.terminal.write(b"\x1b]10;rgb:44/55/66\x1b\\hi");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let expected_fg = Some(Color::Rgb(0x44, 0x55, 0x66));
        assert_eq!(buffer[(0, 0)].symbol(), "h");
        assert_eq!(buffer[(0, 0)].style().fg, expected_fg);
        assert_eq!(buffer[(2, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 0)].style().fg, expected_fg);
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

    #[test]
    fn render_inverse_text_swaps_fg_and_resolved_bg_when_bg_is_transparent() {
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
            // SGR 7 enables inverse/reverse video
            core.terminal.write(b"\x1b[7mhi\x1b[27m");
        }

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| pane.render(frame, Rect::new(0, 0, 20, 5), false))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let cell = &buffer[(0, 0)];
        assert_eq!(cell.symbol(), "h");
        // After inverse: fg should be the resolved bg, bg should be the original fg.
        // fg must NOT be Color::Reset (which would be the same hue as bg).
        assert_eq!(cell.style().fg, Some(Color::Rgb(0x11, 0x22, 0x33)));
        assert_eq!(cell.style().bg, Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn trim_trailing_blank_rows_drops_empty_viewport_tail() {
        let mut rows = vec!["hello".to_string(), "".to_string(), "   ".to_string()];
        trim_trailing_blank_rows(&mut rows);
        assert_eq!(rows, vec!["hello".to_string()]);
    }
}
