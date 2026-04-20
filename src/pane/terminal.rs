use std::borrow::Cow;
use std::sync::Mutex;

use bytes::Bytes;
use ratatui::style::{Color, Modifier, Style};
use ratatui::{layout::Rect, Frame};
use tokio::sync::mpsc;
use tracing::{debug, error};
use unicode_width::UnicodeWidthStr;

use crate::layout::PaneId;

use super::{
    input::{
        ghostty_key_event_from_terminal_key, ghostty_mouse_encoder_for_terminal,
        ghostty_mouse_event_from_button_kind, ghostty_mouse_event_from_wheel_kind,
        ghostty_prefers_herdr_text_encoding,
    },
    osc::{
        contains_scrollback_clear_sequence, current_transient_default_color_owner,
        maybe_filter_primary_screen_scrollback_clear, restore_host_terminal_theme_if_needed,
        write_host_terminal_theme, DefaultColorOscTracker, Osc52Forwarder,
    },
};

const DEFAULT_DETECTION_ROWS: usize = 24;

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
pub(crate) struct ProcessBytesResult {
    pub request_render: bool,
    pub clipboard_writes: Vec<Vec<u8>>,
}

pub(crate) struct GhosttyPaneTerminal {
    pub core: Mutex<GhosttyPaneCore>,
    key_encoder: Mutex<crate::ghostty::KeyEncoder>,
}

pub(crate) struct GhosttyPaneCore {
    pub terminal: crate::ghostty::Terminal,
    pub render_state: crate::ghostty::RenderState,
    pub host_terminal_theme: crate::terminal_theme::TerminalTheme,
    pub transient_default_color_owner_pgid: Option<u32>,
    pub default_color_tracker: DefaultColorOscTracker,
    pub osc52_forwarder: Osc52Forwarder,
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

    pub fn resize(&self, rows: u16, cols: u16) {
        self.ghostty.resize(rows, cols);
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

    pub fn visible_text(&self) -> String {
        self.ghostty.visible_text()
    }

    pub fn detection_text(&self) -> String {
        self.ghostty.detection_text()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.ghostty.recent_text(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.ghostty.recent_unwrapped_text(lines)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.ghostty.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.ghostty.render(frame, area, show_cursor);
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

    pub fn keyboard_protocol(
        &self,
        fallback: crate::input::KeyboardProtocol,
    ) -> crate::input::KeyboardProtocol {
        self.ghostty.keyboard_protocol().unwrap_or(fallback)
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

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        if let Ok(mut core) = self.core.lock() {
            core.host_terminal_theme = theme;
            core.transient_default_color_owner_pgid = None;
            write_host_terminal_theme(&mut core.terminal, theme);
        }
    }

    pub fn has_transient_default_color_override(&self) -> bool {
        self.core
            .lock()
            .map(|core| core.transient_default_color_owner_pgid.is_some())
            .unwrap_or(false)
    }

    pub fn maybe_restore_host_terminal_theme(&self, pane_id: PaneId, shell_pid: u32) -> bool {
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

    pub fn process_pty_bytes(
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

    pub fn resize(&self, rows: u16, cols: u16) {
        if let Ok(mut core) = self.core.lock() {
            let _ = core.terminal.resize(cols, rows);
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
            Ok(bytes) if !bytes.is_empty() => bytes,
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
        encoder.encode(&event).ok()
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
        encoder.encode(&event).ok()
    }

    pub fn visible_text(&self) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_visible_text(&mut core).ok())
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

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.core
            .lock()
            .ok()
            .and_then(|core| ghostty_recent_text_unwrapped(&core, lines).ok())
            .unwrap_or_default()
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.core
            .lock()
            .ok()
            .and_then(|mut core| ghostty_extract_selection(&mut core, selection).ok())
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
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

pub(super) fn ghostty_blank_symbol_for_width(wide: crate::ghostty::CellWide) -> &'static str {
    match wide {
        crate::ghostty::CellWide::Wide => "  ",
        crate::ghostty::CellWide::SpacerTail => "",
        crate::ghostty::CellWide::Narrow | crate::ghostty::CellWide::SpacerHead => " ",
    }
}

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
