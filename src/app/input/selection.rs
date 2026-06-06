use crossterm::event::{MouseEvent, MouseEventKind};

use crate::{
    app::state::{AppState, SelectionAutoscroll, SelectionAutoscrollDirection},
    terminal::TerminalRuntimeRegistry,
};

impl AppState {
    pub(crate) fn update_selection_cursor(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        screen_col: u16,
        screen_row: u16,
    ) {
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        let metrics = self.pane_scroll_metrics(terminal_runtimes, pane_id);
        if let Some(selection) = self.selection.as_mut() {
            selection.drag(screen_col, screen_row, info.inner_rect, metrics);
        }
    }

    fn selection_edge_scroll_lines(distance: u16) -> usize {
        usize::from(distance).saturating_mul(3).clamp(3, 15)
    }

    pub(super) fn update_selection_drag(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        screen_col: u16,
        screen_row: u16,
    ) {
        let Some(pane_id) = self.selection.as_ref().map(|selection| selection.pane_id) else {
            return;
        };
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };

        let top = info.inner_rect.y;
        let bottom = info.inner_rect.y + info.inner_rect.height.saturating_sub(1);

        // Only activate autoscroll when the user is actively dragging.
        // An anchored click in the hot zone should not start the timer.
        // Check before advancing the cursor: if already Dragging from a prior
        // event, it stays true. If Anchored, the mouse must have moved away
        // from the anchor cell for this to count as a real drag.
        let was_dragging = self.selection.as_ref().is_some_and(|s| s.is_dragging());
        let anchor_differs_from_mouse = self.selection.as_ref().is_some_and(|s| {
            // Convert anchor to screen coords for comparison.
            // Anchor is stored in absolute row; for a simple screen
            // comparison, check whether the mouse is on a different
            // cell than the anchor's screen position.
            let (ar, ac) = s.anchor_screen_pos(
                info.inner_rect,
                self.pane_scroll_metrics(terminal_runtimes, s.pane_id),
            );
            ar != screen_row || ac != screen_col
        });
        let is_dragging = was_dragging || anchor_differs_from_mouse;

        // Advance the selection cursor.
        self.update_selection_cursor(terminal_runtimes, pane_id, screen_col, screen_row);

        // If the mouse is on a different cell than the anchor but drag()
        // didn't transition (cursor clamped to edge == anchor), force
        // Dragging so the selection becomes visible and autoscroll can run.
        if is_dragging {
            if let Some(sel) = self.selection.as_mut() {
                if sel.is_just_click() {
                    sel.force_dragging();
                }
            }
        }

        if screen_row < top {
            // Cursor above pane — immediate scroll + set autoscroll state
            if is_dragging {
                self.scroll_pane_up(
                    terminal_runtimes,
                    pane_id,
                    Self::selection_edge_scroll_lines(top - screen_row),
                );
                // Re-advance cursor after scroll so it reflects the new viewport position
                self.update_selection_cursor(terminal_runtimes, pane_id, screen_col, screen_row);
                self.selection_autoscroll = Some(SelectionAutoscroll {
                    direction: SelectionAutoscrollDirection::Up,
                    last_mouse_screen_col: screen_col,
                    last_mouse_screen_row: screen_row,
                    inner_rect: info.inner_rect,
                });
            }
        } else if screen_row > bottom {
            // Cursor below pane — immediate scroll + set autoscroll state
            if is_dragging {
                self.scroll_pane_down(
                    terminal_runtimes,
                    pane_id,
                    Self::selection_edge_scroll_lines(screen_row - bottom),
                );
                // Re-advance cursor after scroll so it reflects the new viewport position
                self.update_selection_cursor(terminal_runtimes, pane_id, screen_col, screen_row);
                self.selection_autoscroll = Some(SelectionAutoscroll {
                    direction: SelectionAutoscrollDirection::Down,
                    last_mouse_screen_col: screen_col,
                    last_mouse_screen_row: screen_row,
                    inner_rect: info.inner_rect,
                });
            }
        } else if screen_row == top {
            // Hot zone: top edge row — no immediate scroll, set autoscroll state
            if is_dragging {
                self.selection_autoscroll = Some(SelectionAutoscroll {
                    direction: SelectionAutoscrollDirection::Up,
                    last_mouse_screen_col: screen_col,
                    last_mouse_screen_row: screen_row,
                    inner_rect: info.inner_rect,
                });
            } else {
                self.selection_autoscroll = None;
            }
        } else if screen_row == bottom {
            // Hot zone: bottom edge row — no immediate scroll, set autoscroll state
            if is_dragging {
                self.selection_autoscroll = Some(SelectionAutoscroll {
                    direction: SelectionAutoscrollDirection::Down,
                    last_mouse_screen_col: screen_col,
                    last_mouse_screen_row: screen_row,
                    inner_rect: info.inner_rect,
                });
            } else {
                self.selection_autoscroll = None;
            }
        } else {
            // Safe zone: inside pane, not on edge rows — clear autoscroll
            self.selection_autoscroll = None;
        }
    }

    pub(super) fn scroll_selection_with_wheel(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) -> bool {
        let lines_per_notch = self.mouse_scroll_lines;

        let Some(selection) = self.selection.as_ref() else {
            return false;
        };
        if !selection.is_in_progress() {
            return false;
        }
        let pane_id = selection.pane_id;
        self.focus_pane(pane_id);
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_pane_up(terminal_runtimes, pane_id, lines_per_notch)
            }
            MouseEventKind::ScrollDown => {
                self.scroll_pane_down(terminal_runtimes, pane_id, lines_per_notch)
            }
            _ => return false,
        }
        self.update_selection_cursor(terminal_runtimes, pane_id, mouse.column, mouse.row);
        true
    }
}

#[cfg(test)]
mod autoscroll_tests {
    use super::*;
    use crate::layout::PaneInfo;
    use crate::terminal::TerminalRuntimeRegistry;
    use crate::workspace::Workspace;
    use ratatui::layout::Rect;

    /// Build an AppState with one workspace/pane and pane_infos populated
    /// so pane_info_by_id works. Returns (state, pane_id).
    fn make_state_with_pane() -> (AppState, crate::layout::PaneId) {
        let mut state = AppState::test_new();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        state.workspaces.push(ws);
        state.active = Some(0);
        state.view.pane_infos.push(PaneInfo {
            id: pane_id,
            rect: Rect::new(0, 0, 80, 24),
            inner_rect: Rect::new(0, 0, 80, 24),
            scrollbar_rect: None,
            header_rect: None,
            is_focused: true,
        });
        (state, pane_id)
    }

    #[test]
    fn above_pane_sets_autoscroll_up() {
        // Build state with pane starting at row 5 so we can drag above it
        let mut state = AppState::test_new();
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        state.workspaces.push(ws);
        state.active = Some(0);
        state.view.pane_infos.push(PaneInfo {
            id: pane_id,
            rect: Rect::new(0, 5, 80, 24),
            inner_rect: Rect::new(0, 5, 80, 24),
            scrollbar_rect: None,
            header_rect: None,
            is_focused: true,
        });
        // Anchor at (5, 10), drag to different cell above pane
        let mut sel = crate::selection::Selection::anchor(pane_id, 5, 10, None);
        sel.drag(4, 5, Rect::new(0, 5, 80, 24), None);
        state.selection = Some(sel);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 5, 4);
        let autoscroll = state.selection_autoscroll.as_ref().unwrap();
        assert_eq!(autoscroll.direction, SelectionAutoscrollDirection::Up);
    }

    #[test]
    fn top_hot_zone_sets_autoscroll_up_on_drag() {
        let (mut state, pane_id) = make_state_with_pane();
        // Anchor at (5, 10), drag to top edge row (row 0) — different cell
        let mut sel = crate::selection::Selection::anchor(pane_id, 5, 10, None);
        sel.drag(0, 0, Rect::new(0, 0, 80, 24), None);
        state.selection = Some(sel);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 0, 0);
        let autoscroll = state.selection_autoscroll.as_ref().unwrap();
        assert_eq!(autoscroll.direction, SelectionAutoscrollDirection::Up);
    }

    #[test]
    fn top_hot_zone_clears_autoscroll_on_click() {
        // An anchored click on the top edge row should NOT start autoscroll.
        let (mut state, pane_id) = make_state_with_pane();
        state.selection = Some(crate::selection::Selection::anchor(pane_id, 0, 0, None));
        // Same-cell drag on top edge row — still anchored
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 0, 0);
        assert!(state.selection_autoscroll.is_none());
    }

    #[test]
    fn bottom_hot_zone_sets_autoscroll_down_on_drag() {
        let (mut state, pane_id) = make_state_with_pane();
        // Anchor at (0, 0), drag to bottom edge row (row 23) — different cell
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(23, 0, Rect::new(0, 0, 80, 24), None);
        state.selection = Some(sel);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 0, 23);
        let autoscroll = state.selection_autoscroll.as_ref().unwrap();
        assert_eq!(autoscroll.direction, SelectionAutoscrollDirection::Down);
    }

    #[test]
    fn bottom_hot_zone_clears_autoscroll_on_click() {
        // An anchored click on the bottom edge row should NOT start autoscroll.
        let (mut state, pane_id) = make_state_with_pane();
        // Anchor at bottom edge row
        state.selection = Some(crate::selection::Selection::anchor(pane_id, 23, 0, None));
        // Same-cell drag — still anchored
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 0, 23);
        assert!(state.selection_autoscroll.is_none());
    }

    #[test]
    fn below_pane_sets_autoscroll_down_on_drag() {
        let (mut state, pane_id) = make_state_with_pane();
        // Anchor at (0, 0), drag to different cell below pane
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(5, 5, Rect::new(0, 0, 80, 24), None);
        state.selection = Some(sel);
        // Drag cursor one row below the pane bottom
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 0, 24);
        let autoscroll = state.selection_autoscroll.as_ref().unwrap();
        assert_eq!(autoscroll.direction, SelectionAutoscrollDirection::Down);
    }

    #[test]
    fn safe_zone_clears_autoscroll() {
        let (mut state, pane_id) = make_state_with_pane();
        // Anchor at (0, 0), drag to (5, 5) so it's truly dragging
        let mut sel = crate::selection::Selection::anchor(pane_id, 0, 0, None);
        sel.drag(5, 5, Rect::new(0, 0, 80, 24), None);
        state.selection = Some(sel);
        // Set autoscroll first
        state.selection_autoscroll = Some(SelectionAutoscroll {
            direction: SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 5,
            last_mouse_screen_row: 23,
            inner_rect: Rect::new(0, 0, 80, 24),
        });
        // Move cursor into safe zone (middle of pane, not on edge rows)
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        state.update_selection_drag(&terminal_runtimes, 5, 12);
        assert!(state.selection_autoscroll.is_none());
    }
}
