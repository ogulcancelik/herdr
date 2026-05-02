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
//! - `ESC ] 52 ; c ; <base64> BEL` — OSC 52 clipboard write
//!
//! The goal is minimal output: skip unchanged cells, batch adjacent changes,
//! and minimize cursor movement.

use std::cmp;
use std::io::{self, Write};

use unicode_width::UnicodeWidthStr;

use crate::server::protocol::{CellData, FrameData};

// ---------------------------------------------------------------------------
// Color → escape sequence
// ---------------------------------------------------------------------------

/// Converts a packed u32 color to an SGR escape sequence fragment.
///
/// Returns a string like `38;5;123` (indexed) or `38;2;255;128;64` (RGB)
/// or `39` (reset), without the leading `\x1b[` or trailing `m`.
fn color_to_sgr_fg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "39".to_owned(), // Reset
            0x01 => "30".to_owned(), // Black
            0x02 => "31".to_owned(), // Red
            0x03 => "32".to_owned(), // Green
            0x04 => "33".to_owned(), // Yellow
            0x05 => "34".to_owned(), // Blue
            0x06 => "35".to_owned(), // Magenta
            0x07 => "36".to_owned(), // Cyan
            0x08 => "37".to_owned(), // Gray (light gray)
            0x09 => "90".to_owned(), // DarkGray
            0x0A => "91".to_owned(), // LightRed
            0x0B => "92".to_owned(), // LightGreen
            0x0C => "93".to_owned(), // LightYellow
            0x0D => "94".to_owned(), // LightBlue
            0x0E => "95".to_owned(), // LightMagenta
            0x0F => "96".to_owned(), // LightCyan
            0x10 => "97".to_owned(), // White
            _ => "39".to_owned(),    // Unknown → Reset
        },
        0x01 => format!("38;5;{}", val & 0xFF), // Indexed
        0x02 => {
            // RGB
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("38;2;{r};{g};{b}")
        }
        _ => "39".to_owned(), // Unknown → Reset
    }
}

/// Converts a packed u32 color to a background SGR fragment.
fn color_to_sgr_bg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "49".to_owned(),  // Reset
            0x01 => "40".to_owned(),  // Black
            0x02 => "41".to_owned(),  // Red
            0x03 => "42".to_owned(),  // Green
            0x04 => "43".to_owned(),  // Yellow
            0x05 => "44".to_owned(),  // Blue
            0x06 => "45".to_owned(),  // Magenta
            0x07 => "46".to_owned(),  // Cyan
            0x08 => "47".to_owned(),  // Gray (light gray)
            0x09 => "100".to_owned(), // DarkGray
            0x0A => "101".to_owned(), // LightRed
            0x0B => "102".to_owned(), // LightGreen
            0x0C => "103".to_owned(), // LightYellow
            0x0D => "104".to_owned(), // LightBlue
            0x0E => "105".to_owned(), // LightMagenta
            0x0F => "106".to_owned(), // LightCyan
            0x10 => "107".to_owned(), // White
            _ => "49".to_owned(),     // Unknown → Reset
        },
        0x01 => format!("48;5;{}", val & 0xFF), // Indexed
        0x02 => {
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("48;2;{r};{g};{b}")
        }
        _ => "49".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Modifier → SGR
// ---------------------------------------------------------------------------

/// Converts a u16 modifier bitmask to SGR escape sequence fragments.
///
/// Returns a Vec of SGR parameter strings (e.g., "1" for bold, "3" for italic).
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

/// Builds a complete SGR escape sequence for a cell's style.
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
    a.symbol == b.symbol && a.fg == b.fg && a.bg == b.bg && a.modifier == b.modifier
    // Skip flag is only for ratatui internal use, not visual.
}

// ---------------------------------------------------------------------------
// Blitting
// ---------------------------------------------------------------------------

/// Blits a frame to the terminal, diffing against the previous frame.
///
/// On the first frame (no previous), does a full redraw.
/// On subsequent frames, only writes cells that changed.
/// After writing cells, positions the cursor.
pub fn blit_frame_with_cursor_memory(
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
) {
    let mut stdout = io::stdout();
    blit_frame_to_with_cursor_memory(&mut stdout, frame, prev, last_visible_cursor);
}

/// Blits a frame to a writer, diffing against the previous frame.
#[cfg(test)]
fn blit_frame_to(writer: impl Write, frame: &FrameData, prev: Option<&FrameData>) {
    let mut last_visible_cursor = None;
    blit_frame_to_with_cursor_memory(writer, frame, prev, &mut last_visible_cursor);
}

fn blit_frame_to_with_cursor_memory(
    mut writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
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

    if full_redraw {
        // Clear the screen and write all cells.
        let _ = writer.write_all(b"\x1b[2J\x1b[H");
        write_all_cells(&mut writer, frame);
    } else {
        // Diff-based update: only write changed cells.
        let prev = prev.unwrap();
        write_changed_cells(&mut writer, frame, prev);
    }

    // Position the cursor while it is still hidden, then restore visibility.
    // Showing before moving makes slow terminals and IMEs briefly observe the
    // cursor at the last painted cell, which can be an animated sidebar/status
    // cell rather than the focused pane's input position. When the focused pane
    // hides its cursor, still park the host cursor intentionally so IMEs do not
    // anchor to whichever cell happened to be painted last.
    let host_cursor = resolve_host_cursor_state(frame, last_visible_cursor);
    write_host_cursor_state(&mut writer, host_cursor);

    // End the synchronized output block immediately after the final cursor
    // state is emitted so supporting terminals can present the frame atomically.
    let _ = writer.write_all(b"\x1b[?2026l");

    // Some native IMEs track candidate-window placement from normal terminal
    // cursor updates and may not observe cursor moves emitted inside synchronized
    // output. Re-emit only the resolved final cursor anchor after the sync block;
    // intermediate paint cursor positions remain hidden. When the focused pane
    // explicitly reports a hidden cursor, expose that anchor to the host terminal
    // so IMEs can attach to it instead of an older visible cursor position.
    write_ime_anchor_cursor_state(&mut writer, host_cursor, frame.cursor.is_some());
    let _ = writer.flush();
}

/// Writes all cells in the frame (full redraw).
fn cell_width(cell: &CellData) -> usize {
    cell.symbol.width()
}

#[derive(Clone, Copy)]
struct HostCursorState {
    position: (u16, u16),
    visible: bool,
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
            };
        }

        let position = clamp_cursor_position(frame, cursor.x, cursor.y);
        return HostCursorState {
            position,
            visible: false,
        };
    }

    let position = (*last_visible_cursor)
        .map(|(x, y)| clamp_cursor_position(frame, x, y))
        .unwrap_or_else(|| default_hidden_cursor_position(frame));
    HostCursorState {
        position,
        visible: false,
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

fn write_host_cursor_state(writer: &mut impl Write, cursor: HostCursorState) {
    write_cursor_position(writer, cursor.position);
    if cursor.visible {
        // Show cursor only after it is already at the final position.
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_ime_anchor_cursor_state(
    writer: &mut impl Write,
    cursor: HostCursorState,
    expose_hidden_anchor: bool,
) {
    write_cursor_position(writer, cursor.position);
    if cursor.visible || expose_hidden_anchor {
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_all_cells(writer: &mut impl Write, frame: &FrameData) {
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

            // Set style.
            let sgr = build_sgr(cell.fg, cell.bg, cell.modifier);
            let _ = writer.write_all(sgr.as_bytes());

            // Write the symbol.
            let _ = writer.write_all(cell.symbol.as_bytes());
            to_skip = cell_width(cell).saturating_sub(1);
        }
    }

    // Reset style at the end.
    let _ = writer.write_all(b"\x1b[0m");
}

fn write_cell(writer: &mut impl Write, row: u16, col: u16, cell: &CellData, last_sgr: &mut String) {
    if cell.skip {
        return;
    }

    let _ = write!(writer, "\x1b[{};{}H", row + 1, col + 1);

    let sgr = build_sgr(cell.fg, cell.bg, cell.modifier);
    if sgr != *last_sgr {
        let _ = writer.write_all(sgr.as_bytes());
        *last_sgr = sgr;
    }

    let _ = writer.write_all(cell.symbol.as_bytes());
}

/// Writes only the cells that changed between the previous and current frame.
fn write_changed_cells(writer: &mut impl Write, frame: &FrameData, prev: &FrameData) {
    let mut last_sgr = String::new(); // Track last SGR to avoid redundant style changes.

    for row in 0..frame.height {
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;

        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];

            if !cell.skip && (cell != prev_cell || invalidated > 0) && to_skip == 0 {
                write_cell(writer, row, col, cell, &mut last_sgr);
            }

            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }

    // Reset style if we wrote anything.
    if !last_sgr.is_empty() {
        let _ = writer.write_all(b"\x1b[0m");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::protocol::{CellData, CursorState};

    const WIDE_GRAPHEME: &str = "💡";

    fn make_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg,
            modifier,
            skip: false,
        }
    }

    fn make_frame(width: u16, height: u16, cells: Vec<CellData>) -> FrameData {
        FrameData {
            cells,
            width,
            height,
            cursor: None,
        }
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
            }),
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
    fn blit_frame_exposes_explicit_hidden_cursor_anchor_after_synchronized_output() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
            }),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: false,
            }),
        };
        let mut last_visible_cursor = None;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(&mut output, &visible, None, &mut last_visible_cursor);
        output.clear();
        blit_frame_to_with_cursor_memory(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
        );

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[2;3H\x1b[?25h",
            "should expose the explicit hidden cursor position for IME anchoring"
        );
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
            }),
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
            }),
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
            }),
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
            }),
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
            }),
        };
        let mut curr = prev.clone();
        curr.cells[0] = make_cell("B", 0, 0, 0);
        curr.cursor = Some(CursorState {
            x: 2,
            y: 2,
            visible: true,
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
            }),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: None,
        };
        let mut last_visible_cursor = None;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(&mut output, &visible, None, &mut last_visible_cursor);
        output.clear();
        blit_frame_to_with_cursor_memory(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
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
        };
        let mut last_visible_cursor = None;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(&mut output, &frame, None, &mut last_visible_cursor);

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
            }),
        };
        let curr = FrameData {
            cells: vec![make_cell("B", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: None,
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
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(
            output_str.contains("\x1b[1;2H"),
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
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
    }
}
