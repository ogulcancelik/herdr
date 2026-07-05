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
    let pre_compute_suppresses_focused_terminal_cursor =
        focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes);
    if resize_panes {
        crate::ui::compute_view_with_cell_size(app_state, terminal_runtimes, area, cell_size);
    } else {
        crate::ui::compute_view_without_resizing_panes(app_state, terminal_runtimes, area);
    }
    let suppress_focused_terminal_cursor = pre_compute_suppresses_focused_terminal_cursor
        || focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes);

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            crate::ui::render_with_runtime_registry(app_state, terminal_runtimes, frame);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor = if suppress_focused_terminal_cursor {
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

/// Overlays a focused remote pane's live grid onto an already-rendered App
/// frame's content region (Task 9b, VIEW-ONLY: a focused remote pane takes
/// over `content_rect` -- where the workspace panes would otherwise draw --
/// while the sidebar/chrome around it is untouched).
///
/// Reuses `render_terminal_virtual` (no bespoke grid renderer): it always
/// renders into a fresh `(0,0)`-origin backend, so its returned buffer is
/// repositioned onto `content_rect` via `Buffer::resize` before merging --
/// since the buffer's cell count already matches `content_rect`'s area,
/// `resize` only moves the buffer's area rect in place (see its doc
/// comment), it does not reallocate or drop any rendered cell. `Buffer::
/// merge` then composites at each buffer's own absolute area, so the
/// workspace panes' cells at `content_rect` are replaced and everything
/// outside it (sidebar, tab bar, ...) is left alone. The cursor, if any, is
/// translated by the same offset. A no-op if `content_rect` is degenerate
/// (a client that hasn't been laid out to a nonzero size yet).
///
/// This function is pure: it only reads `runtime` and rewrites the caller's
/// already-computed `buffer`/`cursor`, the same "render never mutates
/// server/app state" contract every other function in this module holds.
pub(crate) fn overlay_focused_remote_pane(
    buffer: &mut ratatui::buffer::Buffer,
    cursor: &mut Option<CursorState>,
    runtime: &crate::terminal::TerminalRuntime,
    content_rect: Rect,
) {
    if content_rect.width == 0 || content_rect.height == 0 {
        return;
    }

    let (mut remote_buffer, remote_cursor) = render_terminal_virtual(
        runtime,
        Rect::new(0, 0, content_rect.width, content_rect.height),
    );
    remote_buffer.resize(content_rect);
    buffer.merge(&remote_buffer);
    *cursor = remote_cursor.map(|c| CursorState {
        x: c.x.saturating_add(content_rect.x),
        y: c.y.saturating_add(content_rect.y),
        visible: c.visible,
        shape: c.shape,
    });
}

pub(crate) fn visible_hyperlinks(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<((u16, u16), String, String)> {
    let Some(ws_idx) = app_state.active else {
        return Vec::new();
    };
    let Some(tab) = app_state
        .workspaces
        .get(ws_idx)
        .and_then(crate::workspace::Workspace::active_tab)
    else {
        return Vec::new();
    };

    let mut links = Vec::new();
    for info in &app_state.view.pane_infos {
        if let Some(runtime) = tab
            .terminal_id(info.id)
            .and_then(|terminal_id| terminal_runtimes.get(terminal_id))
        {
            links.extend(runtime.visible_hyperlinks(info.inner_rect));
        }
    }
    links
}

pub(crate) fn focused_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    if app_state.mode != Mode::Terminal {
        return None;
    }

    let ws_idx = app_state.active?;
    let info = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)?;
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return None;
    }
    let rt = app_state.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
    if rt.synchronized_output_active() {
        return None;
    }
    let scrolled_back = crate::ui::pane_is_scrolled_back(rt);
    // Determine whether the IME-anchor reveal applies to this focused pane.
    // The master switch must be on, and either no agent filter is configured
    // (apply to any pane) or the focused pane's detected agent matches the
    // allow-list. A configured list with no valid entries reveals nothing.
    let reveal = app_state.reveal_hidden_cursor_for_cjk_ime
        && (!app_state.cjk_ime_agent_filter_configured || {
            let detected = app_state
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.terminal_id(info.id))
                .and_then(|tid| app_state.terminals.get(tid))
                .and_then(|t| t.detected_agent);
            detected.is_some_and(|agent| app_state.cjk_ime_agents.contains(&agent))
        });

    if let Some(cursor) = rt.cursor_state(info.inner_rect, true) {
        // When the reveal applies, expose the cursor anchor regardless of the
        // pane's `?25l` request so macOS IMEs keep tracking the candidate
        // window when TUIs paint their own cursor. Scrollback suppression
        // still applies.
        let visible = if reveal {
            !scrolled_back
        } else {
            cursor.visible && !scrolled_back
        };
        Some(CursorState {
            x: cursor.x,
            y: cursor.y,
            visible,
            shape: if reveal && visible {
                app_state.cjk_ime_cursor_shape
            } else {
                cursor.shape
            },
        })
    } else if reveal && !scrolled_back {
        // cursor_state() returned None — the viewport has no cursor position
        // (can happen with complex TUIs). Fall back to the pane's top-left so
        // the outer terminal still exposes a cursor anchor for IME tracking.
        Some(CursorState {
            x: info.inner_rect.x,
            y: info.inner_rect.y,
            visible: true,
            shape: app_state.cjk_ime_cursor_shape,
        })
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use ratatui::buffer::Buffer;
    use ratatui::style::Style;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::Notify;

    /// Task 9b commit 4's render branch (`HeadlessServer`'s App-mode render
    /// arm in `headless.rs`) reuses this function verbatim to substitute a
    /// focused remote pane's live grid for the workspace panes in the
    /// content rect. This test proves the merge itself is correct and pure
    /// in isolation, without any of that plumbing: cells strictly outside
    /// `content_rect` (sidebar/chrome) are left untouched, cells inside it
    /// show the remote runtime's own rendered content translated into
    /// place, and the cursor (if any) is translated by the same offset.
    #[tokio::test]
    async fn overlay_focused_remote_pane_replaces_only_the_content_rect_and_translates_cursor() {
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let runtime = crate::terminal::TerminalRuntime::spawn_remote_fed(
            PaneId::from_raw(1),
            5,
            20,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            render_notify,
            render_dirty,
        )
        .expect("remote-fed runtime should construct without a PTY");
        runtime.feed_remote_bytes(b"remote output");

        let outer = Rect::new(0, 0, 40, 10);
        let content_rect = Rect::new(10, 2, 20, 5);
        let mut buffer = Buffer::empty(outer);
        for y in 0..outer.height {
            buffer.set_string(0, y, "S".repeat(usize::from(outer.width)), Style::default());
        }
        let mut cursor: Option<CursorState> = None;

        // Independently compute what render_terminal_virtual alone would
        // produce for the remote runtime, so the cursor-translation
        // assertion below doesn't have to hard-code whether the ghostty
        // grid happens to report a visible cursor.
        let (_, expected_cursor) = render_terminal_virtual(
            &runtime,
            Rect::new(0, 0, content_rect.width, content_rect.height),
        );

        overlay_focused_remote_pane(&mut buffer, &mut cursor, &runtime, content_rect);

        // Outside content_rect: untouched (still the 'S' sentinel).
        assert_eq!(
            buffer[(0, 0)].symbol(),
            "S",
            "top-left chrome corner must be untouched"
        );
        assert_eq!(
            buffer[(outer.width - 1, outer.height - 1)].symbol(),
            "S",
            "bottom-right chrome corner must be untouched"
        );
        assert_eq!(
            buffer[(content_rect.x - 1, content_rect.y)].symbol(),
            "S",
            "column just left of content_rect must be untouched"
        );

        // Inside content_rect: the fed text appears at the translated
        // position (row 0 of the remote grid == content_rect.y).
        let text: String = (0..13)
            .map(|i| {
                buffer[(content_rect.x + i, content_rect.y)]
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_eq!(text, "remote output");

        // Cursor translated by the same (x, y) offset as the content rect,
        // or still None if the remote grid itself reported none.
        let translated_expected = expected_cursor.map(|c| CursorState {
            x: c.x.saturating_add(content_rect.x),
            y: c.y.saturating_add(content_rect.y),
            visible: c.visible,
            shape: c.shape,
        });
        assert_eq!(cursor, translated_expected);

        runtime.shutdown();
    }

    /// A degenerate (zero-area) content rect -- a client that hasn't been
    /// laid out to a nonzero size yet -- must be a no-op, not a panic from
    /// indexing an empty buffer.
    #[tokio::test]
    async fn overlay_focused_remote_pane_is_a_no_op_for_a_degenerate_content_rect() {
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let runtime = crate::terminal::TerminalRuntime::spawn_remote_fed(
            PaneId::from_raw(2),
            5,
            20,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            render_notify,
            render_dirty,
        )
        .expect("remote-fed runtime should construct without a PTY");

        let outer = Rect::new(0, 0, 10, 10);
        let mut buffer = Buffer::empty(outer);
        buffer.set_string(0, 0, "S", Style::default());
        let mut cursor: Option<CursorState> = None;

        overlay_focused_remote_pane(&mut buffer, &mut cursor, &runtime, Rect::new(0, 0, 0, 0));

        assert_eq!(buffer[(0, 0)].symbol(), "S");
        assert_eq!(cursor, None);

        runtime.shutdown();
    }
}
