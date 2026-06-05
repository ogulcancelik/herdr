//! Frame blitting — renders FrameData to the terminal using diff-based updates.
//!
//! The blitting strategy:
//! 1. On the first frame, write the entire buffer (full redraw).
//! 2. On subsequent frames, diff against the last frame and only write
//!    the cells that changed.
//! 3. Wrap each frame in synchronized output so terminals that support it do
//!    not expose intermediate cursor positions while the frame is painted.
//! 4. Before writing any cells, hide the cursor to avoid stray cursor
//!    artifacts on terminals that render the hardware cursor at intermediate
//!    `CUP` positions during the frame stream.
//! 5. After writing all changed cells, restore the final cursor visibility
//!    and position from `frame.cursor`.
//! 6. After ending synchronized output, repeat the final cursor anchor so
//!    external IMEs can place candidate windows at the real input position.
//!
//! Escape sequences used:
//! - `CSI H` (CUP) — move cursor to (row, col)
//! - `CSI m` (SGR) — set graphic rendition (colors, bold, etc.)
//! - `CSI ? 2026 h/l` — begin/end synchronized output
//! - `CSI Ps SP q` — DECSCUSR cursor shape
//! - `ESC ] 52 ; c ; <base64> BEL` — OSC 52 clipboard write
//!
//! The goal is minimal output: skip unchanged cells, batch adjacent changes,
//! and minimize cursor movement.

use std::cmp;
use std::io::Write;

use unicode_width::UnicodeWidthStr;

use crate::protocol::{CellData, FrameData};

/// Bytes produced by a [`BlitEncoder`] for one terminal frame.
pub(crate) struct EncodedBlit {
    /// Terminal escape bytes ready to write to the host terminal.
    pub(crate) bytes: Vec<u8>,
    /// Whether this frame was encoded as a full redraw.
    pub(crate) full: bool,
    next_last_visible_cursor: Option<(u16, u16)>,
    next_last_cursor_shape: u8,
}

/// Stateful encoder that diffs semantic frames into terminal ANSI bytes.
#[derive(Default)]
pub(crate) struct BlitEncoder {
    last_frame: Option<FrameData>,
    last_visible_cursor: Option<(u16, u16)>,
    last_cursor_shape: u8,
}

impl BlitEncoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn encode(&self, frame: &FrameData, force_full: bool) -> EncodedBlit {
        let prev = if force_full {
            None
        } else {
            self.last_frame.as_ref()
        };
        let full = force_full
            || prev.is_none()
            || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);
        let prof_stats =
            crate::render_prof::enabled().then(|| compute_prof_blit_stats(frame, prev, full));
        let prof_started = crate::render_prof::timer();
        let mut bytes = Vec::new();
        let mut next_last_visible_cursor = self.last_visible_cursor;
        let mut next_last_cursor_shape = self.last_cursor_shape;
        blit_frame_to_with_cursor_memory(
            &mut bytes,
            frame,
            prev,
            &mut next_last_visible_cursor,
            &mut next_last_cursor_shape,
        );
        if let Some(stats) = prof_stats {
            crate::render_prof::duration_since("ansi_encode.total", prof_started);
            crate::render_prof::counter("ansi_encode.bytes", bytes.len() as u64);
            crate::render_prof::counter("ansi_encode.scanned_cells", stats.scanned_cells);
            crate::render_prof::counter("ansi_encode.changed_cells", stats.changed_cells);
            crate::render_prof::counter("ansi_encode.changed_runs", stats.changed_runs);
            if full {
                crate::render_prof::event("ansi_encode.full");
            } else {
                crate::render_prof::event("ansi_encode.partial");
            }
        }
        EncodedBlit {
            bytes,
            full,
            next_last_visible_cursor,
            next_last_cursor_shape,
        }
    }

    pub(crate) fn commit(&mut self, frame: FrameData, encoded: EncodedBlit) {
        self.last_visible_cursor = encoded.next_last_visible_cursor;
        self.last_cursor_shape = encoded.next_last_cursor_shape;
        self.last_frame = Some(frame);
    }

    pub(crate) fn is_current(&self, frame: &FrameData) -> bool {
        self.last_frame.as_ref() == Some(frame)
    }

    pub(crate) fn last_frame(&self) -> Option<&FrameData> {
        self.last_frame.as_ref()
    }
}

#[derive(Clone, Copy, Default)]
struct ProfBlitStats {
    scanned_cells: u64,
    changed_cells: u64,
    changed_runs: u64,
}

fn compute_prof_blit_stats(
    frame: &FrameData,
    prev: Option<&FrameData>,
    full: bool,
) -> ProfBlitStats {
    let Some(prev) = prev.filter(|_| !full) else {
        let changed_cells = frame.cells.iter().filter(|cell| !cell.skip).count() as u64;
        return ProfBlitStats {
            scanned_cells: frame.cells.len() as u64,
            changed_cells,
            changed_runs: changed_cells,
        };
    };
    if prev.width != frame.width || prev.height != frame.height {
        let changed_cells = frame.cells.iter().filter(|cell| !cell.skip).count() as u64;
        return ProfBlitStats {
            scanned_cells: frame.cells.len() as u64,
            changed_cells,
            changed_runs: changed_cells,
        };
    }

    let sanitized_hyperlinks = sanitized_frame_hyperlinks(frame);
    let prev_sanitized_hyperlinks = sanitized_frame_hyperlinks(prev);
    let mut stats = ProfBlitStats {
        scanned_cells: frame.cells.len() as u64,
        changed_cells: 0,
        changed_runs: 0,
    };
    for row in 0..frame.height {
        let mut in_run = false;
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;
        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];
            let changed = !cell.skip
                && (!cells_visually_equal(
                    &sanitized_hyperlinks,
                    cell,
                    &prev_sanitized_hyperlinks,
                    prev_cell,
                ) || invalidated > 0)
                && to_skip == 0;
            if changed {
                stats.changed_cells += 1;
                if !in_run {
                    stats.changed_runs += 1;
                    in_run = true;
                }
            } else {
                in_run = false;
            }
            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }
    stats
}

// ---------------------------------------------------------------------------
// Color → escape sequence
// ---------------------------------------------------------------------------

/// Converts a packed u32 color to an SGR escape sequence fragment.
///
/// Returns a string like `38;5;123` (indexed) or `38;2;255;128;64` (RGB)
/// or `39` (reset), without the leading `\x1b[` or trailing `m`.
fn write_color_sgr_fg(writer: &mut impl Write, val: u32) {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => {
                let _ = writer.write_all(b"39");
            } // Reset
            0x01 => {
                let _ = writer.write_all(b"30");
            } // Black
            0x02 => {
                let _ = writer.write_all(b"31");
            } // Red
            0x03 => {
                let _ = writer.write_all(b"32");
            } // Green
            0x04 => {
                let _ = writer.write_all(b"33");
            } // Yellow
            0x05 => {
                let _ = writer.write_all(b"34");
            } // Blue
            0x06 => {
                let _ = writer.write_all(b"35");
            } // Magenta
            0x07 => {
                let _ = writer.write_all(b"36");
            } // Cyan
            0x08 => {
                let _ = writer.write_all(b"37");
            } // Gray (light gray)
            0x09 => {
                let _ = writer.write_all(b"90");
            } // DarkGray
            0x0A => {
                let _ = writer.write_all(b"91");
            } // LightRed
            0x0B => {
                let _ = writer.write_all(b"92");
            } // LightGreen
            0x0C => {
                let _ = writer.write_all(b"93");
            } // LightYellow
            0x0D => {
                let _ = writer.write_all(b"94");
            } // LightBlue
            0x0E => {
                let _ = writer.write_all(b"95");
            } // LightMagenta
            0x0F => {
                let _ = writer.write_all(b"96");
            } // LightCyan
            0x10 => {
                let _ = writer.write_all(b"97");
            } // White
            _ => {
                let _ = writer.write_all(b"39");
            } // Unknown → Reset
        },
        0x01 => {
            let _ = write!(writer, "38;5;{}", val & 0xFF);
        } // Indexed
        0x02 => {
            // RGB
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            let _ = write!(writer, "38;2;{r};{g};{b}");
        }
        _ => {
            let _ = writer.write_all(b"39");
        } // Unknown → Reset
    }
}

#[cfg(test)]
fn color_to_sgr_fg(val: u32) -> String {
    let mut out = Vec::new();
    write_color_sgr_fg(&mut out, val);
    String::from_utf8(out).unwrap()
}

/// Converts a packed u32 color to a background SGR fragment.
fn write_color_sgr_bg(writer: &mut impl Write, val: u32) {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => {
                let _ = writer.write_all(b"49");
            } // Reset
            0x01 => {
                let _ = writer.write_all(b"40");
            } // Black
            0x02 => {
                let _ = writer.write_all(b"41");
            } // Red
            0x03 => {
                let _ = writer.write_all(b"42");
            } // Green
            0x04 => {
                let _ = writer.write_all(b"43");
            } // Yellow
            0x05 => {
                let _ = writer.write_all(b"44");
            } // Blue
            0x06 => {
                let _ = writer.write_all(b"45");
            } // Magenta
            0x07 => {
                let _ = writer.write_all(b"46");
            } // Cyan
            0x08 => {
                let _ = writer.write_all(b"47");
            } // Gray (light gray)
            0x09 => {
                let _ = writer.write_all(b"100");
            } // DarkGray
            0x0A => {
                let _ = writer.write_all(b"101");
            } // LightRed
            0x0B => {
                let _ = writer.write_all(b"102");
            } // LightGreen
            0x0C => {
                let _ = writer.write_all(b"103");
            } // LightYellow
            0x0D => {
                let _ = writer.write_all(b"104");
            } // LightBlue
            0x0E => {
                let _ = writer.write_all(b"105");
            } // LightMagenta
            0x0F => {
                let _ = writer.write_all(b"106");
            } // LightCyan
            0x10 => {
                let _ = writer.write_all(b"107");
            } // White
            _ => {
                let _ = writer.write_all(b"49");
            } // Unknown → Reset
        },
        0x01 => {
            let _ = write!(writer, "48;5;{}", val & 0xFF);
        } // Indexed
        0x02 => {
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            let _ = write!(writer, "48;2;{r};{g};{b}");
        }
        _ => {
            let _ = writer.write_all(b"49");
        }
    }
}

#[cfg(test)]
fn color_to_sgr_bg(val: u32) -> String {
    let mut out = Vec::new();
    write_color_sgr_bg(&mut out, val);
    String::from_utf8(out).unwrap()
}

// ---------------------------------------------------------------------------
// Modifier → SGR
// ---------------------------------------------------------------------------

/// Converts a u16 modifier bitmask to SGR escape sequence fragments.
///
/// Returns a Vec of SGR parameter strings (e.g., "1" for bold, "3" for italic).
#[cfg(test)]
fn modifier_to_sgr_parts(val: u16) -> Vec<&'static str> {
    let mut parts = Vec::new();

    // ratatui::Modifier bits (from bitflags)
    const BOLD: u16 = 1 << 0; // 0x01
    const DIM: u16 = 1 << 1; // 0x02
    const ITALIC: u16 = 1 << 2; // 0x04
    const UNDERLINED: u16 = 1 << 3; // 0x08
    const SLOW_BLINK: u16 = 1 << 4; // 0x10
    const RAPID_BLINK: u16 = 1 << 5; // 0x20
    const REVERSED: u16 = 1 << 6; // 0x40
    const HIDDEN: u16 = 1 << 7; // 0x80
    const CROSSED_OUT: u16 = 1 << 8; // 0x100

    if val & BOLD != 0 {
        parts.push("1");
    }
    if val & DIM != 0 {
        parts.push("2");
    }
    if val & ITALIC != 0 {
        parts.push("3");
    }
    if val & UNDERLINED != 0 {
        parts.push("4");
    }
    if val & SLOW_BLINK != 0 {
        parts.push("5");
    }
    if val & RAPID_BLINK != 0 {
        parts.push("6");
    }
    if val & REVERSED != 0 {
        parts.push("7");
    }
    if val & HIDDEN != 0 {
        parts.push("8");
    }
    if val & CROSSED_OUT != 0 {
        parts.push("9");
    }

    parts
}

fn write_modifier_sgr_parts(writer: &mut impl Write, val: u16) {
    const BOLD: u16 = 1 << 0;
    const DIM: u16 = 1 << 1;
    const ITALIC: u16 = 1 << 2;
    const UNDERLINED: u16 = 1 << 3;
    const SLOW_BLINK: u16 = 1 << 4;
    const RAPID_BLINK: u16 = 1 << 5;
    const REVERSED: u16 = 1 << 6;
    const HIDDEN: u16 = 1 << 7;
    const CROSSED_OUT: u16 = 1 << 8;

    for (bit, part) in [
        (BOLD, b"1" as &[u8]),
        (DIM, b"2"),
        (ITALIC, b"3"),
        (UNDERLINED, b"4"),
        (SLOW_BLINK, b"5"),
        (RAPID_BLINK, b"6"),
        (REVERSED, b"7"),
        (HIDDEN, b"8"),
        (CROSSED_OUT, b"9"),
    ] {
        if val & bit != 0 {
            let _ = writer.write_all(b";");
            let _ = writer.write_all(part);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellStyle {
    fg: u32,
    bg: u32,
    modifier: u16,
}

impl From<&CellData> for CellStyle {
    fn from(cell: &CellData) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            modifier: cell.modifier,
        }
    }
}

fn write_sgr(writer: &mut impl Write, style: CellStyle) {
    let _ = writer.write_all(b"\x1b[0");
    write_modifier_sgr_parts(writer, style.modifier);
    let _ = writer.write_all(b";");
    write_color_sgr_fg(writer, style.fg);
    let _ = writer.write_all(b";");
    write_color_sgr_bg(writer, style.bg);
    let _ = writer.write_all(b"m");
}

/// Builds a complete SGR escape sequence for a cell's style.
#[cfg(test)]
fn build_sgr(fg: u32, bg: u32, modifier: u16) -> String {
    let mut parts = vec!["0".to_owned()];
    parts.extend(
        modifier_to_sgr_parts(modifier)
            .into_iter()
            .map(str::to_owned),
    );
    parts.push(color_to_sgr_fg(fg));
    parts.push(color_to_sgr_bg(bg));
    format!("\x1b[{}m", parts.join(";"))
}

// ---------------------------------------------------------------------------
// Cell comparison
// ---------------------------------------------------------------------------

/// Checks if two cells are visually identical.
#[cfg(test)]
fn cells_equal(a: &CellData, b: &CellData) -> bool {
    a.symbol == b.symbol
        && a.fg == b.fg
        && a.bg == b.bg
        && a.modifier == b.modifier
        && a.hyperlink == b.hyperlink
    // Skip flag is only for ratatui internal use, not visual.
}

// ---------------------------------------------------------------------------
// Blitting
// ---------------------------------------------------------------------------

/// Blits a frame to a writer, diffing against the previous frame.
#[cfg(test)]
fn blit_frame_to(writer: impl Write, frame: &FrameData, prev: Option<&FrameData>) {
    let mut last_visible_cursor = None;
    let mut last_cursor_shape = 0;
    blit_frame_to_with_cursor_memory(
        writer,
        frame,
        prev,
        &mut last_visible_cursor,
        &mut last_cursor_shape,
    );
}

fn blit_frame_to_with_cursor_memory(
    mut writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
) {
    // On first frame or size change, do a full redraw.
    let full_redraw =
        prev.is_none() || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);

    // Ask terminals that support synchronized output to apply the whole frame
    // atomically. This keeps IMEs and cursor trackers from observing the
    // intermediate CUP positions used while painting changed cells.
    let _ = writer.write_all(b"\x1b[?2026h");

    // Hide cursor before any cell writes to avoid stray cursor artifacts
    // on terminals that render the hardware cursor at intermediate CUP positions.
    let _ = writer.write_all(b"\x1b[?25l");

    // Start each frame from a known OSC 8 state. If a previous write was
    // interrupted or the outer terminal had an active hyperlink, unlinked cells
    // must not inherit it.
    let _ = writer.write_all(b"\x1b]8;;\x1b\\");

    if full_redraw {
        // Clear the screen and write all cells.
        let _ = writer.write_all(b"\x1b[2J\x1b[H");
        write_all_cells(&mut writer, frame);
    } else {
        // Diff-based update: only write changed cells.
        let prev = prev.unwrap();
        if !write_scrolled_frame_delta(&mut writer, frame, prev) {
            write_changed_cells(&mut writer, frame, prev);
        }
    }

    // Position the cursor while it is still hidden, then restore visibility.
    // Showing before moving makes slow terminals and IMEs briefly observe the
    // cursor at the last painted cell, which can be an animated sidebar/status
    // cell rather than the focused pane's input position. When the focused pane
    // hides its cursor, still park the host cursor intentionally so IMEs do not
    // anchor to whichever cell happened to be painted last.
    let host_cursor = resolve_host_cursor_state(frame, last_visible_cursor);
    write_host_cursor_state(&mut writer, host_cursor, last_cursor_shape);

    // End the synchronized output block immediately after the final cursor
    // state is emitted so supporting terminals can present the frame atomically.
    let _ = writer.write_all(b"\x1b[?2026l");

    // Some native IMEs track candidate-window placement from normal terminal
    // cursor updates and may not observe cursor moves emitted inside synchronized
    // output. Re-emit only the resolved final cursor anchor after the sync block;
    // intermediate paint cursor positions remain hidden and the focused pane's
    // requested cursor visibility is preserved.
    write_ime_anchor_cursor_state(&mut writer, host_cursor);
    let _ = writer.flush();
}

/// Writes all cells in the frame (full redraw).
fn cell_width(cell: &CellData) -> usize {
    if cell.symbol.len() == 1 && cell.symbol.as_bytes()[0].is_ascii() {
        return 1;
    }
    cell.symbol.width()
}

#[derive(Clone, Copy)]
struct HostCursorState {
    position: (u16, u16),
    visible: bool,
    /// DECSCUSR parameter (0–6). 0 means terminal default.
    shape: u8,
}

fn resolve_host_cursor_state(
    frame: &FrameData,
    last_visible_cursor: &mut Option<(u16, u16)>,
) -> HostCursorState {
    if let Some(cursor) = &frame.cursor {
        if cursor.visible {
            let position = clamp_cursor_position(frame, cursor.x, cursor.y);
            *last_visible_cursor = Some(position);
            return HostCursorState {
                position,
                visible: true,
                shape: normalize_cursor_shape(cursor.shape),
            };
        }

        let position = clamp_cursor_position(frame, cursor.x, cursor.y);
        return HostCursorState {
            position,
            visible: false,
            shape: normalize_cursor_shape(cursor.shape),
        };
    }

    let position = (*last_visible_cursor)
        .map(|(x, y)| clamp_cursor_position(frame, x, y))
        .unwrap_or_else(|| default_hidden_cursor_position(frame));
    HostCursorState {
        position,
        visible: false,
        shape: 0,
    }
}

fn normalize_cursor_shape(shape: u8) -> u8 {
    if shape <= 6 {
        shape
    } else {
        0
    }
}

fn default_hidden_cursor_position(frame: &FrameData) -> (u16, u16) {
    (
        frame.width.saturating_sub(1),
        frame.height.saturating_sub(1),
    )
}

fn clamp_cursor_position(frame: &FrameData, x: u16, y: u16) -> (u16, u16) {
    (
        x.min(frame.width.saturating_sub(1)),
        y.min(frame.height.saturating_sub(1)),
    )
}

fn write_cursor_position(writer: &mut impl Write, (x, y): (u16, u16)) {
    // CUP: move cursor to (row+1, col+1) — 1-based.
    let _ = write!(writer, "\x1b[{};{}H", y + 1, x + 1);
}

fn write_host_cursor_state(writer: &mut impl Write, cursor: HostCursorState, last_shape: &mut u8) {
    write_cursor_position(writer, cursor.position);
    if cursor.shape != *last_shape {
        let _ = write!(writer, "\x1b[{} q", cursor.shape);
        *last_shape = cursor.shape;
    }
    if cursor.visible {
        // Show cursor only after it is already at the final position.
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_ime_anchor_cursor_state(writer: &mut impl Write, cursor: HostCursorState) {
    write_cursor_position(writer, cursor.position);
    if cursor.visible {
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_all_cells(writer: &mut impl Write, frame: &FrameData) {
    let mut active_hyperlink = None;
    let mut last_style = None;
    for row in 0..frame.height {
        let mut to_skip = 0usize;
        for col in 0..frame.width {
            if to_skip > 0 {
                to_skip -= 1;
                continue;
            }

            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];

            if cell.skip {
                continue;
            }

            // Move cursor to position (1-based).
            let _ = write!(writer, "\x1b[{};{}H", row + 1, col + 1);

            let style = CellStyle::from(cell);
            if last_style != Some(style) {
                write_sgr(writer, style);
                last_style = Some(style);
            }

            write_hyperlink_if_changed(
                writer,
                &mut active_hyperlink,
                cell_hyperlink_uri(frame, cell),
            );

            // Write the symbol.
            let _ = writer.write_all(cell.symbol.as_bytes());
            to_skip = cell_width(cell).saturating_sub(1);
        }
    }

    close_hyperlink(writer, &mut active_hyperlink);

    // Reset style at the end.
    let _ = writer.write_all(b"\x1b[0m");
}

fn cell_hyperlink_uri<'a>(frame: &'a FrameData, cell: &CellData) -> Option<&'a str> {
    let index = cell.hyperlink? as usize;
    frame.hyperlinks.get(index).map(String::as_str)
}

fn sanitized_hyperlink_uri(uri: &str) -> Option<String> {
    let sanitized: String = uri
        .chars()
        .filter(|ch| *ch != '\x1b' && *ch != '\x07' && !ch.is_control())
        .collect();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn sanitized_frame_hyperlinks(frame: &FrameData) -> Vec<Option<String>> {
    frame
        .hyperlinks
        .iter()
        .map(|uri| sanitized_hyperlink_uri(uri))
        .collect()
}

fn sanitized_cell_hyperlink_uri<'a>(
    sanitized_hyperlinks: &'a [Option<String>],
    cell: &CellData,
) -> Option<&'a str> {
    let index = cell.hyperlink? as usize;
    sanitized_hyperlinks.get(index)?.as_deref()
}

fn write_hyperlink_if_changed(
    writer: &mut impl Write,
    active: &mut Option<String>,
    requested: Option<&str>,
) {
    let requested = requested.and_then(sanitized_hyperlink_uri);
    if active.as_deref() == requested.as_deref() {
        return;
    }

    if active.is_some() {
        let _ = writer.write_all(b"\x1b]8;;\x1b\\");
    }
    *active = requested;
    if let Some(uri) = active.as_deref() {
        let _ = write!(writer, "\x1b]8;;{uri}\x1b\\");
    }
}

fn close_hyperlink(writer: &mut impl Write, active: &mut Option<String>) {
    if active.take().is_some() {
        let _ = writer.write_all(b"\x1b]8;;\x1b\\");
    }
}

fn write_cell_at_cursor(
    writer: &mut impl Write,
    cell: &CellData,
    last_style: &mut Option<CellStyle>,
    active_hyperlink: &mut Option<String>,
    frame: &FrameData,
) {
    if cell.skip {
        return;
    }

    let style = CellStyle::from(cell);
    if *last_style != Some(style) {
        write_sgr(writer, style);
        *last_style = Some(style);
    }

    write_hyperlink_if_changed(writer, active_hyperlink, cell_hyperlink_uri(frame, cell));
    let _ = writer.write_all(cell.symbol.as_bytes());
}

/// Writes only the cells that changed between the previous and current frame.
fn cells_visually_equal(
    sanitized_hyperlinks: &[Option<String>],
    cell: &CellData,
    prev_sanitized_hyperlinks: &[Option<String>],
    prev_cell: &CellData,
) -> bool {
    cell.symbol == prev_cell.symbol
        && cell.fg == prev_cell.fg
        && cell.bg == prev_cell.bg
        && cell.modifier == prev_cell.modifier
        && sanitized_cell_hyperlink_uri(sanitized_hyperlinks, cell)
            == sanitized_cell_hyperlink_uri(prev_sanitized_hyperlinks, prev_cell)
    // Skip flag is only for ratatui internal use, not visual.
}

fn rows_visually_equal(
    sanitized_hyperlinks: &[Option<String>],
    frame: &FrameData,
    row: u16,
    prev_sanitized_hyperlinks: &[Option<String>],
    prev: &FrameData,
    prev_row: u16,
) -> bool {
    let width = frame.width as usize;
    let start = row as usize * width;
    let prev_start = prev_row as usize * width;
    frame.cells[start..start + width]
        .iter()
        .zip(&prev.cells[prev_start..prev_start + width])
        .all(|(cell, prev_cell)| {
            cells_visually_equal(
                sanitized_hyperlinks,
                cell,
                prev_sanitized_hyperlinks,
                prev_cell,
            )
        })
}

fn detect_vertical_scroll_delta(
    frame: &FrameData,
    prev: &FrameData,
    sanitized_hyperlinks: &[Option<String>],
    prev_sanitized_hyperlinks: &[Option<String>],
) -> Option<i16> {
    if frame.width == 0 || frame.height < 2 || frame.cells.len() != prev.cells.len() {
        return None;
    }

    let max_shift = frame.height.saturating_sub(1);
    for shift in 1..=max_shift {
        if (0..frame.height - shift).all(|row| {
            rows_visually_equal(
                sanitized_hyperlinks,
                frame,
                row,
                prev_sanitized_hyperlinks,
                prev,
                row + shift,
            )
        }) {
            return Some(shift as i16);
        }

        if (shift..frame.height).all(|row| {
            rows_visually_equal(
                sanitized_hyperlinks,
                frame,
                row,
                prev_sanitized_hyperlinks,
                prev,
                row - shift,
            )
        }) {
            return Some(-(shift as i16));
        }
    }

    None
}

fn write_row_cells(
    writer: &mut impl Write,
    frame: &FrameData,
    row: u16,
    last_style: &mut Option<CellStyle>,
    active_hyperlink: &mut Option<String>,
) {
    let mut to_skip = 0usize;
    let mut run_next_col = None;
    for col in 0..frame.width {
        if to_skip > 0 {
            to_skip -= 1;
            continue;
        }

        let idx = (row as usize) * (frame.width as usize) + (col as usize);
        let cell = &frame.cells[idx];
        if cell.skip {
            run_next_col = None;
            continue;
        }

        if run_next_col != Some(col) {
            write_cursor_position(writer, (col, row));
        }
        write_cell_at_cursor(writer, cell, last_style, active_hyperlink, frame);
        let width = cell_width(cell) as u16;
        run_next_col = width.checked_sub(1).and_then(|_| col.checked_add(width));
        to_skip = (width as usize).saturating_sub(1);
    }
}

fn write_scrolled_frame_delta(
    writer: &mut impl Write,
    frame: &FrameData,
    prev: &FrameData,
) -> bool {
    if prev.width != frame.width || prev.height != frame.height {
        return false;
    }

    let sanitized_hyperlinks = sanitized_frame_hyperlinks(frame);
    let prev_sanitized_hyperlinks = sanitized_frame_hyperlinks(prev);
    let Some(delta) = detect_vertical_scroll_delta(
        frame,
        prev,
        &sanitized_hyperlinks,
        &prev_sanitized_hyperlinks,
    ) else {
        return false;
    };

    let amount = delta.unsigned_abs();
    let _ = write!(writer, "\x1b[1;{}r", frame.height);
    if delta > 0 {
        let _ = write!(writer, "\x1b[{amount}S");
    } else {
        let _ = write!(writer, "\x1b[{amount}T");
    }
    let _ = writer.write_all(b"\x1b[r");

    let mut last_style = None;
    let mut active_hyperlink = None;
    if delta > 0 {
        for row in frame.height - amount..frame.height {
            write_row_cells(writer, frame, row, &mut last_style, &mut active_hyperlink);
        }
    } else {
        for row in 0..amount {
            write_row_cells(writer, frame, row, &mut last_style, &mut active_hyperlink);
        }
    }

    close_hyperlink(writer, &mut active_hyperlink);
    if last_style.is_some() {
        let _ = writer.write_all(b"\x1b[0m");
    }
    true
}

fn write_changed_cells(writer: &mut impl Write, frame: &FrameData, prev: &FrameData) {
    let mut last_style = None; // Track last SGR to avoid redundant style changes.
    let mut active_hyperlink = None;
    let sanitized_hyperlinks = sanitized_frame_hyperlinks(frame);
    let prev_sanitized_hyperlinks = sanitized_frame_hyperlinks(prev);

    for row in 0..frame.height {
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;

        let mut run_next_col = None;

        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];

            if !cell.skip
                && (!cells_visually_equal(
                    &sanitized_hyperlinks,
                    cell,
                    &prev_sanitized_hyperlinks,
                    prev_cell,
                ) || invalidated > 0)
                && to_skip == 0
            {
                if run_next_col != Some(col) {
                    write_cursor_position(writer, (col, row));
                }
                write_cell_at_cursor(writer, cell, &mut last_style, &mut active_hyperlink, frame);
                let width = cell_width(cell) as u16;
                run_next_col = width.checked_sub(1).and_then(|_| col.checked_add(width));
            } else if to_skip == 0 {
                run_next_col = None;
            }

            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }

    close_hyperlink(writer, &mut active_hyperlink);

    // Reset style if we wrote anything.
    if last_style.is_some() {
        let _ = writer.write_all(b"\x1b[0m");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CellData, CursorState};

    const WIDE_GRAPHEME: &str = "💡";

    fn make_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg,
            modifier,
            skip: false,
            hyperlink: None,
        }
    }

    fn make_frame(width: u16, height: u16, cells: Vec<CellData>) -> FrameData {
        FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    fn count_cup_sequences(output: &str) -> usize {
        let bytes = output.as_bytes();
        let mut count = 0;
        let mut i = 0;

        while i + 2 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'H' {
                    count += 1;
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }

        count
    }

    fn shifted_scrollback_frames(width: u16, height: u16, shift: u16) -> (FrameData, FrameData) {
        fn frame_for_offset(width: u16, height: u16, offset: u16) -> FrameData {
            let cells = (0..height)
                .flat_map(|row| {
                    (0..width).map(move |col| {
                        let ch = char::from(b'a' + ((row + offset + col) % 26) as u8);
                        make_cell(&ch.to_string(), 0, 0, 0)
                    })
                })
                .collect();

            make_frame(width, height, cells)
        }

        (
            frame_for_offset(width, height, 0),
            frame_for_offset(width, height, shift),
        )
    }

    fn encode_diff_output(curr: &FrameData, prev: &FrameData) -> String {
        let mut output = Vec::new();
        blit_frame_to(&mut output, curr, Some(prev));
        String::from_utf8(output).unwrap()
    }

    fn linked_cell(symbol: &str, index: u32) -> CellData {
        let mut cell = make_cell(symbol, 0, 0, 0);
        cell.hyperlink = Some(index);
        cell
    }

    #[test]
    #[ignore]
    fn scrollback_shift_diff_metrics() {
        use std::time::Instant;

        let (prev, curr) = shifted_scrollback_frames(120, 40, 1);
        let changed_cells = prev
            .cells
            .iter()
            .zip(&curr.cells)
            .filter(|(prev, curr)| !cells_equal(prev, curr))
            .count();
        let output = encode_diff_output(&curr, &prev);
        let cup_count = count_cup_sequences(&output);

        let iterations = 500;
        let started = Instant::now();
        for _ in 0..iterations {
            let _ = encode_diff_output(&curr, &prev);
        }
        let elapsed = started.elapsed();

        eprintln!(
            "scrollback_shift_diff_metrics width={} height={} shift=1 changed_cells={} bytes={} cup_count={} iterations={} elapsed_ms={:.3}",
            curr.width,
            curr.height,
            changed_cells,
            output.len(),
            cup_count,
            iterations,
            elapsed.as_secs_f64() * 1000.0
        );
    }

    #[test]
    fn color_to_sgr_fg_named_colors() {
        assert_eq!(color_to_sgr_fg(0x00_00_00_00), "39"); // Reset
        assert_eq!(color_to_sgr_fg(0x00_00_00_01), "30"); // Black
        assert_eq!(color_to_sgr_fg(0x00_00_00_02), "31"); // Red
        assert_eq!(color_to_sgr_fg(0x00_00_00_10), "97"); // White
    }

    #[test]
    fn color_to_sgr_fg_indexed() {
        assert_eq!(color_to_sgr_fg(0x01_00_00_AB), "38;5;171");
    }

    #[test]
    fn color_to_sgr_fg_rgb() {
        assert_eq!(color_to_sgr_fg(0x02_FF_80_40), "38;2;255;128;64");
    }

    #[test]
    fn color_to_sgr_bg_named_colors() {
        assert_eq!(color_to_sgr_bg(0x00_00_00_00), "49"); // Reset
        assert_eq!(color_to_sgr_bg(0x00_00_00_01), "40"); // Black
        assert_eq!(color_to_sgr_bg(0x00_00_00_10), "107"); // White
    }

    #[test]
    fn color_to_sgr_bg_rgb() {
        assert_eq!(color_to_sgr_bg(0x02_FF_80_40), "48;2;255;128;64");
    }

    #[test]
    fn modifier_to_sgr_parts_bold() {
        let parts = modifier_to_sgr_parts(1); // BOLD
        assert!(parts.contains(&"1"));
    }

    #[test]
    fn modifier_to_sgr_parts_italic() {
        let parts = modifier_to_sgr_parts(4); // ITALIC
        assert!(parts.contains(&"3"));
    }

    #[test]
    fn modifier_to_sgr_parts_empty() {
        let parts = modifier_to_sgr_parts(0);
        assert!(parts.is_empty());
    }

    #[test]
    fn build_sgr_produces_valid_sequence() {
        let sgr = build_sgr(0x00_00_00_02, 0x00_00_00_01, 1); // fg=Red, bg=Black, bold
        assert!(sgr.starts_with("\x1b["));
        assert!(sgr.ends_with("m"));
        assert!(sgr.contains("0")); // reset existing style first
        assert!(sgr.contains("1")); // bold
        assert!(sgr.contains("31")); // fg red
        assert!(sgr.contains("40")); // bg black
    }

    #[test]
    fn build_sgr_resets_previous_modifiers_when_cell_is_plain() {
        assert_eq!(build_sgr(0x00_00_00_00, 0x00_00_00_00, 0), "\x1b[0;39;49m");
    }

    #[test]
    fn cells_equal_identical() {
        let a = make_cell("A", 2, 1, 0);
        let b = make_cell("A", 2, 1, 0);
        assert!(cells_equal(&a, &b));
    }

    #[test]
    fn cells_equal_different_symbol() {
        let a = make_cell("A", 2, 1, 0);
        let b = make_cell("B", 2, 1, 0);
        assert!(!cells_equal(&a, &b));
    }

    #[test]
    fn cells_equal_different_color() {
        let a = make_cell("A", 2, 1, 0);
        let b = make_cell("A", 3, 1, 0);
        assert!(!cells_equal(&a, &b));
    }

    #[test]
    fn blit_frame_hides_cursor_before_full_redraw_writes() {
        let frame = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "should begin synchronized output and hide cursor before any cell writes during full redraw"
        );
    }

    #[test]
    fn blit_frame_hides_cursor_before_diff_writes() {
        let prev = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let curr = make_frame(
            2,
            2,
            vec![
                make_cell("X", 0, 0, 0), // Changed
                make_cell("i", 0, 0, 0), // Same
                make_cell("!", 0, 0, 0), // Same
                make_cell(" ", 0, 0, 0), // Same
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "should begin synchronized output and hide cursor before any cell writes during diff"
        );
    }

    #[test]
    fn blit_frame_wraps_frame_in_synchronized_output() {
        let frame = make_frame(1, 1, vec![make_cell("A", 0, 0, 0)]);

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h"),
            "should begin synchronized output before frame writes"
        );
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output after frame writes");
        assert!(
            sync_end + "\x1b[?2026l".len() < output_str.len(),
            "should end synchronized output before trailing IME cursor update"
        );
    }

    #[test]
    fn blit_frame_repeats_final_cursor_state_after_synchronized_output() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[2;3H\x1b[?25h",
            "should expose only the final cursor state after synchronized output"
        );
    }

    #[test]
    fn blit_frame_emits_cursor_shape_before_visibility_without_touching_ime_anchor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 6,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        let final_cursor = output_str
            .find("\x1b[1;1H\x1b[6 q\x1b[?25h")
            .expect("should set cursor shape before showing cursor");
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        assert!(
            final_cursor < sync_end,
            "shape should be part of the synchronized final cursor state"
        );
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[1;1H\x1b[?25h",
            "IME anchor update should preserve the existing position/visibility-only contract"
        );
    }

    #[test]
    fn blit_frame_repeats_explicit_hidden_cursor_anchor_after_synchronized_output() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(
            &mut output,
            &visible,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
        );
        output.clear();
        blit_frame_to_with_cursor_memory(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
            &mut last_cursor_shape,
        );

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[2;3H\x1b[?25l",
            "should repeat the explicit hidden cursor position while preserving visibility"
        );
    }

    #[test]
    fn blit_frame_emits_osc8_for_linked_cells() {
        let mut frame = make_frame(
            3,
            1,
            vec![
                linked_cell("L", 0),
                linked_cell("i", 0),
                make_cell("!", 0, 0, 0),
            ],
        );
        frame.hyperlinks.push("https://example.com".to_owned());

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("\x1b]8;;https://example.com\x1b\\L"));
        assert!(output_str.contains('i'));
        assert!(output_str.contains("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn blit_frame_sanitizes_hyperlink_uris() {
        let mut frame = make_frame(1, 1, vec![linked_cell("L", 0)]);
        frame
            .hyperlinks
            .push("https://exa\x1b\x07mple.com".to_owned());

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("\x1b]8;;https://example.com\x1b\\L"));
    }

    #[test]
    fn blit_frame_first_frame_produces_output() {
        let frame = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        // Full redraw should start with clear screen.
        assert!(
            output_str.contains("\x1b[2J"),
            "full redraw should clear screen"
        );
        assert!(
            output_str.contains('H') || output_str.contains('i'),
            "should contain cell content"
        );
    }

    #[test]
    fn blit_frame_diff_only_writes_changed_cells() {
        let prev = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        // Only the first cell changed.
        let curr = make_frame(
            2,
            2,
            vec![
                make_cell("X", 0, 0, 0), // Changed
                make_cell("i", 0, 0, 0), // Same
                make_cell("!", 0, 0, 0), // Same
                make_cell(" ", 0, 0, 0), // Same
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let output_str = String::from_utf8(output).unwrap();
        // Diff should NOT clear the screen.
        assert!(
            !output_str.contains("\x1b[2J"),
            "diff should not clear screen"
        );
        // Should contain the changed cell content.
        assert!(output_str.contains('X'), "should contain changed cell 'X'");
    }

    #[test]
    fn diff_redraw_batches_scrollback_shift_by_changed_runs() {
        let (prev, curr) = shifted_scrollback_frames(120, 40, 1);
        let changed_cells = prev
            .cells
            .iter()
            .zip(&curr.cells)
            .filter(|(prev, curr)| !cells_equal(prev, curr))
            .count();

        let output = encode_diff_output(&curr, &prev);
        let cup_count = count_cup_sequences(&output);

        assert_eq!(changed_cells, 120 * 40);
        assert!(
            cup_count <= curr.height as usize + 2,
            "expected one paint CUP per changed row plus final cursor anchors, got {cup_count} CUPs"
        );
        assert!(
            cup_count * 20 < changed_cells,
            "batched output should use far fewer CUPs than changed cells"
        );
        assert!(
            output.len() * 5 < changed_cells * 10,
            "batched output should stay close to payload size instead of per-cell CUP overhead"
        );
    }

    #[test]
    fn diff_redraw_batches_contiguous_cells_with_style_changes() {
        let prev = make_frame(4, 1, vec![make_cell("A", 0, 0, 0); 4]);
        let curr = make_frame(
            4,
            1,
            vec![
                make_cell("B", 0x00_00_00_02, 0, 0),
                make_cell("C", 0x00_00_00_03, 0, 1),
                make_cell("D", 0x00_00_00_04, 0, 0),
                make_cell("E", 0x00_00_00_05, 0, 1),
            ],
        );

        let output = encode_diff_output(&curr, &prev);

        assert_eq!(count_cup_sequences(&output), 3);
        assert!(output.contains('B'));
        assert!(output.contains('C'));
        assert!(output.contains('D'));
        assert!(output.contains('E'));
        assert!(output.contains("\x1b[0;31;49m"));
        assert!(output.contains("\x1b[0;1;32;49m"));
    }

    #[test]
    fn diff_redraw_batches_wide_grapheme_runs_without_tail_cup() {
        let prev = make_frame(
            4,
            1,
            vec![
                make_cell("A", 0, 0, 0),
                make_cell("B", 0, 0, 0),
                make_cell("C", 0, 0, 0),
                make_cell("D", 0, 0, 0),
            ],
        );
        let curr = make_frame(
            4,
            1,
            vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
                make_cell("D", 0, 0, 0),
            ],
        );

        let output = encode_diff_output(&curr, &prev);

        assert_eq!(count_cup_sequences(&output), 3);
        assert!(output.contains(WIDE_GRAPHEME));
        assert!(output.contains('Z'));
        assert!(!output.contains("\x1b[1;2H"));
        assert!(!output.contains("\x1b[1;3H"));
    }

    #[test]
    fn diff_redraw_batches_contiguous_cells_with_hyperlink_changes() {
        let prev = make_frame(3, 1, vec![make_cell("A", 0, 0, 0); 3]);
        let mut curr = make_frame(
            3,
            1,
            vec![
                linked_cell("L", 0),
                linked_cell("i", 0),
                make_cell("!", 0, 0, 0),
            ],
        );
        curr.hyperlinks.push("https://example.com".to_owned());

        let output = encode_diff_output(&curr, &prev);

        assert_eq!(count_cup_sequences(&output), 3);
        assert!(output.contains("\x1b]8;;https://example.com\x1b\\L"));
        assert!(output.contains('i'));
        assert!(output.contains("\x1b]8;;\x1b\\!"));
    }

    #[test]
    fn blit_frame_size_change_triggers_full_redraw() {
        let prev = make_frame(2, 2, vec![make_cell("A", 0, 0, 0); 4]);

        let curr = make_frame(3, 2, vec![make_cell("B", 0, 0, 0); 6]);

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[2J"),
            "size change should trigger full redraw"
        );
    }

    #[test]
    fn blit_frame_positions_cursor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[1;1H"),
            "should position cursor at (1,1)"
        );
    }

    #[test]
    fn blit_frame_hides_cursor_when_invisible() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25l"),
            "should hide cursor when invisible"
        );
    }

    #[test]
    fn blit_frame_no_cursor_hides_cursor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25l"),
            "should hide cursor when no cursor state"
        );
    }

    #[test]
    fn blit_frame_restores_cursor_visibility() {
        // First frame: cursor hidden.
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &prev, None);
        assert!(
            String::from_utf8(output).unwrap().contains("\x1b[?25l"),
            "first frame should hide cursor"
        );

        // Second frame: cursor visible — should restore visibility.
        let curr = FrameData {
            cells: vec![make_cell("B", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25h"),
            "second frame should restore cursor visibility with ?25h"
        );
        assert!(
            output_str.contains("\x1b[1;1H"),
            "should position cursor before showing it"
        );
    }

    #[test]
    fn blit_frame_positions_cursor_before_showing_it() {
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut curr = prev.clone();
        curr.cells[0] = make_cell("B", 0, 0, 0);
        curr.cursor = Some(CursorState {
            x: 2,
            y: 2,
            visible: true,
            shape: 0,
        });

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();
        let final_move = output_str
            .rfind("\x1b[3;3H")
            .expect("should move cursor to final position");
        let show = output_str
            .rfind("\x1b[?25h")
            .expect("should show cursor after positioning it");

        assert!(
            final_move < show,
            "should move cursor to final position before showing it"
        );
    }

    #[test]
    fn blit_frame_parks_hidden_cursor_at_last_visible_position() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 1,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(
            &mut output,
            &visible,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
        );
        output.clear();
        blit_frame_to_with_cursor_memory(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
            &mut last_cursor_shape,
        );

        let output_str = String::from_utf8(output).unwrap();
        let park = output_str
            .rfind("\x1b[2;2H")
            .expect("should park hidden cursor at last visible position");
        let hide = output_str
            .rfind("\x1b[?25l")
            .expect("should keep hidden cursor hidden");
        assert!(park < hide, "should park cursor before hiding it");
    }

    #[test]
    fn blit_frame_parks_hidden_cursor_at_bottom_right_without_history() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 6],
            width: 3,
            height: 2,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(
            &mut output,
            &frame,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
        );

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[2;3H\x1b[?25l"),
            "should park hidden cursor at bottom-right before ending the frame"
        );
    }

    #[test]
    fn blit_frame_hides_previous_visible_cursor_when_next_frame_has_none() {
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![make_cell("B", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        assert!(
            String::from_utf8(output).unwrap().contains("\x1b[?25l"),
            "diff redraw should hide a previously visible cursor when the next frame has none"
        );
    }

    #[test]
    fn full_redraw_skips_trailing_cells_covered_by_wide_graphemes() {
        let frame = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
        assert!(output_str.contains("\x1b[1;3H"));
    }

    #[test]
    fn diff_redraw_reveals_cells_hidden_by_previous_wide_graphemes() {
        let prev = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(
            output_str.contains("A "),
            "cells hidden by a previous wide grapheme must be redrawn when they become visible"
        );
    }

    #[test]
    fn diff_redraw_skips_new_trailing_cells_covered_by_wide_graphemes() {
        let prev = FrameData {
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell("B", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
    }
}
