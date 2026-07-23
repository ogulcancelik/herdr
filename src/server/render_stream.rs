//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::AppState;
use crate::app::Mode;
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{CursorState, FrameData, RenderEncoding, ServerMessage, TerminalFrame};
use crate::terminal::TerminalRuntimeRegistry;

/// Per-client render baseline for the negotiated render encoding.
pub(crate) enum ClientRenderState {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic { last_frame: Option<FrameData> },
    /// Terminal-ANSI clients keep a terminal diff encoder and sequence number.
    TerminalAnsi { blit_encoder: BlitEncoder, seq: u64 },
}

impl ClientRenderState {
    pub(crate) fn new(render_encoding: RenderEncoding) -> Self {
        match render_encoding {
            RenderEncoding::SemanticFrame => Self::Semantic { last_frame: None },
            RenderEncoding::TerminalAnsi => Self::TerminalAnsi {
                blit_encoder: BlitEncoder::new(),
                seq: 0,
            },
        }
    }

    pub(crate) fn reset_baseline(&mut self) {
        match self {
            Self::Semantic { last_frame } => *last_frame = None,
            Self::TerminalAnsi { blit_encoder, .. } => *blit_encoder = BlitEncoder::new(),
        }
    }

    pub(crate) fn reset_semantic_input_baseline(&mut self) {
        if let Self::Semantic { last_frame } = self {
            *last_frame = None;
        }
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        match self {
            Self::Semantic { last_frame } => {
                if last_frame.as_ref() == Some(&frame) {
                    crate::render_prof::event("prepare_frame.semantic.skip_current");
                    return None;
                }
                crate::render_prof::event("prepare_frame.semantic.changed");
                Some(PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                })
            }
            Self::TerminalAnsi { blit_encoder, seq } => {
                if blit_encoder.is_current(&frame) {
                    crate::render_prof::event("prepare_frame.ansi.skip_current");
                    return None;
                }
                let mut encoded = blit_encoder.encode(&frame, false);
                crate::render_prof::event("prepare_frame.ansi.changed");
                crate::render_prof::counter("prepare_frame.ansi.bytes", encoded.bytes.len() as u64);
                if encoded.full {
                    crate::render_prof::event("prepare_frame.ansi.full");
                } else {
                    crate::render_prof::event("prepare_frame.ansi.partial");
                }
                insert_graphics_before_sync_end(&mut encoded.bytes, &frame.graphics);
                crate::render_prof::counter(
                    "prepare_frame.graphics.bytes",
                    frame.graphics.len() as u64,
                );
                Some(PreparedRender::TerminalAnsi {
                    message: ServerMessage::Terminal(TerminalFrame {
                        seq: *seq + 1,
                        width: frame.width,
                        height: frame.height,
                        full: encoded.full,
                        bytes: encoded.bytes.clone(),
                    }),
                    frame,
                    encoded: Some(encoded),
                })
            }
        }
    }

    pub(crate) fn last_frame(&self) -> Option<&FrameData> {
        match self {
            Self::Semantic { last_frame } => last_frame.as_ref(),
            Self::TerminalAnsi { blit_encoder, .. } => blit_encoder.last_frame(),
        }
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        match (self, prepared) {
            (
                Self::Semantic { last_frame },
                PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                },
            ) => *last_frame = Some(frame),
            (
                Self::TerminalAnsi { blit_encoder, seq },
                PreparedRender::TerminalAnsi {
                    frame,
                    encoded: Some(encoded),
                    ..
                },
            ) => {
                blit_encoder.commit(frame, encoded);
                *seq += 1;
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal_seq(&self) -> Option<u64> {
        match self {
            Self::Semantic { .. } => None,
            Self::TerminalAnsi { seq, .. } => Some(*seq),
        }
    }
}

const SYNC_OUTPUT_END: &[u8] = b"\x1b[?2026l";

fn insert_graphics_before_sync_end(encoded: &mut Vec<u8>, graphics: &[u8]) {
    if graphics.is_empty() {
        return;
    }

    if let Some(sync_end) = rfind_subslice(encoded, SYNC_OUTPUT_END) {
        encoded.splice(sync_end..sync_end, graphics.iter().copied());
    } else {
        encoded.extend_from_slice(graphics);
    }
}

fn rfind_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }

    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
    },
    TerminalAnsi {
        message: ServerMessage,
        frame: FrameData,
        encoded: Option<EncodedBlit>,
    },
}

impl PreparedRender {
    pub(crate) fn message(&self) -> &ServerMessage {
        match self {
            Self::Semantic { message } | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn into_frame(self) -> Option<FrameData> {
        match self {
            Self::Semantic {
                message: ServerMessage::Frame(frame),
            } => Some(frame),
            Self::TerminalAnsi { frame, .. } => Some(frame),
            _ => None,
        }
    }
}

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
            shape: 0,
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
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_virtual(
    app_state: &mut AppState,
    area: Rect,
    resize_panes: bool,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_virtual_with_runtime_registry(
        app_state,
        &terminal_runtimes,
        area,
        resize_panes,
        crate::kitty_graphics::HostCellSize::default(),
    )
}

pub(crate) fn render_virtual_with_runtime_registry(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let popup_visible = app_state.popup_pane.is_some();
    let pre_compute_suppresses_focused_terminal_cursor =
        !popup_visible && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes);
    if resize_panes {
        crate::ui::compute_view_with_cell_size(app_state, terminal_runtimes, area, cell_size);
    } else {
        crate::ui::compute_view_without_resizing_panes(app_state, terminal_runtimes, area);
    }
    let suppress_focused_terminal_cursor = pre_compute_suppresses_focused_terminal_cursor
        || (!popup_visible
            && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes));

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            crate::ui::render_with_runtime_registry(app_state, terminal_runtimes, frame);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor = if popup_visible {
        popup_terminal_cursor(app_state, terminal_runtimes)
    } else if suppress_focused_terminal_cursor {
        None
    } else {
        focused_terminal_cursor(app_state, terminal_runtimes).or_else(|| {
            (!focused_terminal_owns_host_cursor(app_state, terminal_runtimes))
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        })
    };

    (buffer, cursor)
}

fn popup_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    let popup = app_state.popup_pane.as_ref()?;
    let runtime = terminal_runtimes.get(&popup.terminal_id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let (_, inner) = crate::ui::popup_pane_rects(app_state, app_state.view.terminal_area)?;
    let cursor = runtime.cursor_state(inner, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
        shape: cursor.shape,
    })
}

/// Renders one server-owned terminal directly for `terminal attach` clients.
pub(crate) fn render_terminal_virtual(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let suppress_cursor = runtime.synchronized_output_active();
    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            runtime.render(frame, area, true);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor = (!suppress_cursor)
        .then(|| runtime.cursor_state(area, true))
        .flatten()
        .map(|cursor| CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
        .or_else(|| {
            (!suppress_cursor)
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        });

    (buffer, cursor)
}

pub(crate) fn visible_hyperlinks(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<((u16, u16), String, String)> {
    crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, app_state.view.tab_surface())
}

pub(crate) fn focused_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    crate::ui::tab_surface_cursor(app_state, terminal_runtimes, app_state.view.tab_surface())
}

fn focused_terminal_owns_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some()
}

fn focused_terminal_suppresses_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some_and(crate::terminal::TerminalRuntime::synchronized_output_active)
}
