//! Text selection and clipboard support.
//!
//! Selection lifecycle:
//!
//!   MouseDown in pane → Anchor recorded (no visual yet)
//!   MouseDrag         → Selection becomes active, cells highlighted
//!   MouseUp           → Text extracted, copied via OSC 52, highlight stays
//!   Next click / key  → Selection cleared
//!
//! Rows are stored in screen-buffer coordinates instead of viewport-relative
//! coordinates. That keeps selection stable while the pane scrolls.

use ratatui::layout::Rect;
use std::io::Write;

use crate::{layout::PaneId, pane::ScrollMetrics};

/// Current phase of a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Mouse is down but hasn't moved yet. If released without
    /// moving, this was just a click — no selection created.
    Anchored,
    /// Mouse has moved from the anchor point. Cells are being highlighted.
    Dragging,
    /// Mouse released after dragging. Selection is visible and text
    /// has been copied to clipboard. Cleared on next interaction.
    Done,
}

/// A text selection within a terminal pane.
#[derive(Debug, Clone)]
pub struct Selection {
    /// Which pane the selection belongs to.
    pub pane_id: PaneId,
    /// Anchor position in screen-buffer coordinates (row, col).
    anchor: (u32, u16),
    /// Current/final position in screen-buffer coordinates (row, col).
    cursor: (u32, u16),
    /// Selection phase.
    phase: Phase,
}

impl Selection {
    /// Start a potential selection. This records the anchor but doesn't
    /// make anything visible yet — the user might just be clicking.
    pub fn anchor(
        pane_id: PaneId,
        viewport_row: u16,
        col: u16,
        metrics: Option<ScrollMetrics>,
    ) -> Self {
        let anchor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        Self {
            pane_id,
            anchor,
            cursor: anchor,
            phase: Phase::Anchored,
        }
    }

    /// Extend the selection as the mouse drags. Activates highlighting
    /// once the cursor moves to a different cell than the anchor.
    /// Screen coordinates are clamped to the pane boundary.
    pub fn drag(
        &mut self,
        screen_col: u16,
        screen_row: u16,
        pane_inner: Rect,
        metrics: Option<ScrollMetrics>,
    ) {
        let (viewport_row, col) = clamp_to_pane(screen_col, screen_row, pane_inner);
        self.cursor = (absolute_row_for_viewport_row(viewport_row, metrics), col);
        if self.cursor != self.anchor {
            self.phase = Phase::Dragging;
        }
    }

    /// Finalize the selection. Returns the selected range if the user
    /// actually dragged (not just clicked). Returns None for plain clicks.
    pub fn finish(&mut self) -> bool {
        if self.phase == Phase::Dragging {
            self.phase = Phase::Done;
            true
        } else {
            false
        }
    }

    /// Whether this selection should be rendered (highlight visible).
    pub fn is_visible(&self) -> bool {
        self.phase == Phase::Dragging || self.phase == Phase::Done
    }

    /// Whether the user just clicked without dragging (not a selection).
    pub fn was_just_click(&self) -> bool {
        self.phase == Phase::Anchored
    }

    /// Whether the pointer is still down and the selection can keep extending.
    pub fn is_in_progress(&self) -> bool {
        matches!(self.phase, Phase::Anchored | Phase::Dragging)
    }

    /// Extend the selection by `rows` rows toward the scroll direction.
    /// Either endpoint may be mutated, depending on which side is currently
    /// the top.
    pub(crate) fn extend_in_scroll_direction(&mut self, scroll_up: bool, rows: u32) {
        // Endpoint pick must match `ordered()` exactly, including the column
        // tie-breaker — otherwise a single-row right-to-left drag moves the
        // wrong side.
        let anchor_precedes_cursor = self.anchor.0 < self.cursor.0
            || (self.anchor.0 == self.cursor.0 && self.anchor.1 <= self.cursor.1);
        let (top, bottom) = if anchor_precedes_cursor {
            (&mut self.anchor, &mut self.cursor)
        } else {
            (&mut self.cursor, &mut self.anchor)
        };

        if scroll_up {
            top.0 = top.0.saturating_sub(rows);
        } else {
            bottom.0 = bottom.0.saturating_add(rows);
        }

        if self.cursor != self.anchor {
            self.phase = Phase::Dragging;
        }
    }

    /// Returns (start, end) in reading order (top-left to bottom-right).
    fn ordered(&self) -> ((u32, u16), (u32, u16)) {
        let (ar, ac) = self.anchor;
        let (cr, cc) = self.cursor;
        if ar < cr || (ar == cr && ac <= cc) {
            ((ar, ac), (cr, cc))
        } else {
            ((cr, cc), (ar, ac))
        }
    }

    pub(crate) fn ordered_cells(&self) -> ((u32, u16), (u32, u16)) {
        self.ordered()
    }

    /// Check whether a pane-relative cell (row, col) is inside the selection.
    pub fn contains(&self, viewport_row: u16, col: u16, metrics: Option<ScrollMetrics>) -> bool {
        if !self.is_visible() {
            return false;
        }
        let row = absolute_row_for_viewport_row(viewport_row, metrics);
        let ((sr, sc), (er, ec)) = self.ordered();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            col >= sc && col <= ec
        } else if row == sr {
            col >= sc
        } else if row == er {
            col <= ec
        } else {
            true
        }
    }
}

fn viewport_top_row(metrics: Option<ScrollMetrics>) -> u32 {
    metrics
        .map(|metrics| {
            metrics
                .max_offset_from_bottom
                .saturating_sub(metrics.offset_from_bottom)
        })
        .unwrap_or(0) as u32
}

fn absolute_row_for_viewport_row(viewport_row: u16, metrics: Option<ScrollMetrics>) -> u32 {
    viewport_top_row(metrics) + u32::from(viewport_row)
}

fn clamp_to_pane(screen_col: u16, screen_row: u16, pane_inner: Rect) -> (u16, u16) {
    let clamped_col = screen_col.clamp(
        pane_inner.x,
        pane_inner.x + pane_inner.width.saturating_sub(1),
    );
    let clamped_row = screen_row.clamp(
        pane_inner.y,
        pane_inner.y + pane_inner.height.saturating_sub(1),
    );
    (clamped_row - pane_inner.y, clamped_col - pane_inner.x)
}

/// Write clipboard bytes to the system clipboard via OSC 52.
///
/// OSC 52 format: `ESC ] 52 ; c ; <base64> BEL`
///
/// The terminal emulator (Ghostty, kitty, etc.) intercepts this
/// and sets the system clipboard. Ghostty has `clipboard-write = allow`
/// by default.
pub fn write_osc52_bytes(bytes: &[u8]) {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    // Use ST (ESC \) terminator — more widely supported than BEL in some terminals
    let sequence = format!("\x1b]52;c;{encoded}\x1b\\");
    let _ = std::io::stdout().write_all(sequence.as_bytes());
    let _ = std::io::stdout().flush();
}

/// Write text to the system clipboard via OSC 52.
pub fn write_osc52(text: &str) {
    write_osc52_bytes(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sel(sr: u32, sc: u16, er: u32, ec: u16) -> Selection {
        let mut sel = Selection::anchor(PaneId::from_raw(0), sr as u16, sc, None);
        sel.anchor = (sr, sc);
        sel.cursor = (er, ec);
        sel.phase = Phase::Dragging;
        sel
    }

    #[test]
    fn ordering_forward() {
        let sel = make_sel(2, 5, 4, 10);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn ordering_backward() {
        let sel = make_sel(4, 10, 2, 5);
        assert_eq!(sel.ordered(), ((2, 5), (4, 10)));
    }

    #[test]
    fn single_line_contains() {
        let sel = make_sel(2, 5, 2, 15);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 10, None));
        assert!(sel.contains(2, 15, None));
        assert!(!sel.contains(2, 16, None));
        assert!(!sel.contains(1, 10, None));
        assert!(!sel.contains(3, 10, None));
    }

    #[test]
    fn multi_line_contains() {
        let sel = make_sel(2, 5, 4, 10);
        assert!(!sel.contains(2, 4, None));
        assert!(sel.contains(2, 5, None));
        assert!(sel.contains(2, 79, None));
        assert!(sel.contains(3, 0, None));
        assert!(sel.contains(3, 79, None));
        assert!(sel.contains(4, 0, None));
        assert!(sel.contains(4, 10, None));
        assert!(!sel.contains(4, 11, None));
    }

    #[test]
    fn anchored_not_visible() {
        let sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(!sel.is_visible());
        assert!(!sel.contains(5, 10, None));
    }

    #[test]
    fn click_without_drag() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(sel.was_just_click());
        let copied = sel.finish();
        assert!(!copied);
    }

    #[test]
    fn drag_then_finish() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        sel.drag(20, 7, Rect::new(10, 5, 80, 24), None);
        assert!(sel.is_visible());
        assert!(!sel.was_just_click());
        let copied = sel.finish();
        assert!(copied);
    }

    #[test]
    fn drag_uses_buffer_rows_when_scrolled() {
        let mut sel = Selection::anchor(
            PaneId::from_raw(0),
            0,
            10,
            Some(ScrollMetrics {
                offset_from_bottom: 1,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        sel.drag(
            10,
            5,
            Rect::new(10, 5, 80, 4),
            Some(ScrollMetrics {
                offset_from_bottom: 2,
                max_offset_from_bottom: 10,
                viewport_rows: 4,
            }),
        );

        assert_eq!(sel.ordered_cells(), ((8, 0), (9, 10)));
    }

    #[test]
    fn contains_tracks_current_viewport_after_scroll() {
        let sel = make_sel(8, 2, 10, 4);
        let metrics = Some(ScrollMetrics {
            offset_from_bottom: 2,
            max_offset_from_bottom: 10,
            viewport_rows: 4,
        });

        assert!(sel.contains(0, 2, metrics));
        assert!(sel.contains(1, 40, metrics));
        assert!(sel.contains(2, 4, metrics));
        assert!(!sel.contains(3, 4, metrics));
    }

    #[test]
    fn extend_scroll_up_moves_top_endpoint() {
        let mut sel = make_sel(10, 2, 20, 5);
        sel.extend_in_scroll_direction(true, 3);
        assert_eq!(sel.ordered_cells(), ((7, 2), (20, 5)));
    }

    #[test]
    fn extend_scroll_down_moves_bottom_endpoint() {
        let mut sel = make_sel(10, 2, 20, 5);
        sel.extend_in_scroll_direction(false, 3);
        assert_eq!(sel.ordered_cells(), ((10, 2), (23, 5)));
    }

    #[test]
    fn extend_scroll_up_when_cursor_is_top() {
        let mut sel = make_sel(20, 5, 10, 2);
        sel.extend_in_scroll_direction(true, 4);
        assert_eq!(sel.ordered_cells(), ((6, 2), (20, 5)));
    }

    #[test]
    fn extend_scroll_down_when_anchor_is_bottom() {
        let mut sel = make_sel(20, 5, 10, 2);
        sel.extend_in_scroll_direction(false, 4);
        assert_eq!(sel.ordered_cells(), ((10, 2), (24, 5)));
    }

    #[test]
    fn extend_from_anchored_transitions_to_dragging() {
        let mut sel = Selection::anchor(PaneId::from_raw(0), 5, 10, None);
        assert!(sel.was_just_click());
        assert!(!sel.is_visible());

        sel.extend_in_scroll_direction(true, 3);

        assert!(!sel.was_just_click());
        assert!(sel.is_visible());
        assert_eq!(sel.ordered_cells(), ((2, 10), (5, 10)));
    }

    #[test]
    fn extend_scroll_up_saturates_at_zero() {
        let mut sel = make_sel(2, 0, 4, 0);
        sel.extend_in_scroll_direction(true, 10);
        assert_eq!(sel.ordered_cells(), ((0, 0), (4, 0)));
    }

    #[test]
    fn extend_single_row_right_to_left_drag_uses_col_tiebreak() {
        let mut sel = make_sel(5, 20, 5, 5);
        assert_eq!(sel.ordered_cells(), ((5, 5), (5, 20)));

        sel.extend_in_scroll_direction(true, 3);

        assert_eq!(sel.ordered_cells(), ((2, 5), (5, 20)));
    }

    #[test]
    fn clamp_to_pane_bounds() {
        let (row, col) = clamp_to_pane(200, 100, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 23);
        assert_eq!(col, 79);

        let (row, col) = clamp_to_pane(0, 0, Rect::new(10, 5, 80, 24));
        assert_eq!(row, 0);
        assert_eq!(col, 0);
    }
}
