use crossterm::event::{MouseEvent, MouseEventKind};

use crate::app::state::AppState;

impl AppState {
    fn update_selection_cursor(
        &mut self,
        pane_id: crate::layout::PaneId,
        screen_col: u16,
        screen_row: u16,
    ) {
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        let metrics = self.pane_scroll_metrics(pane_id);
        if let Some(selection) = self.selection.as_mut() {
            selection.drag(screen_col, screen_row, info.inner_rect, metrics);
        }
    }

    fn selection_edge_scroll_lines(distance: u16) -> usize {
        usize::from(distance).saturating_mul(3).clamp(3, 15)
    }

    pub(super) fn update_selection_drag(&mut self, screen_col: u16, screen_row: u16) {
        let Some(pane_id) = self.selection.as_ref().map(|selection| selection.pane_id) else {
            return;
        };
        let Some(info) = self.pane_info_by_id(pane_id).cloned() else {
            return;
        };

        let bottom = info.inner_rect.y + info.inner_rect.height.saturating_sub(1);
        if screen_row < info.inner_rect.y {
            self.scroll_pane_up(
                pane_id,
                Self::selection_edge_scroll_lines(info.inner_rect.y - screen_row),
            );
        } else if screen_row > bottom {
            self.scroll_pane_down(
                pane_id,
                Self::selection_edge_scroll_lines(screen_row - bottom),
            );
        }

        self.update_selection_cursor(pane_id, screen_col, screen_row);
    }

    pub(super) fn scroll_selection_with_wheel(&mut self, mouse: MouseEvent) -> bool {
        const LINES_PER_NOTCH: usize = 3;

        let Some(selection) = self.selection.as_ref() else {
            return false;
        };
        if !selection.is_in_progress() {
            return false;
        }
        let pane_id = selection.pane_id;
        self.focus_pane(pane_id);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_pane_up(pane_id, LINES_PER_NOTCH),
            MouseEventKind::ScrollDown => self.scroll_pane_down(pane_id, LINES_PER_NOTCH),
            _ => return false,
        }
        self.update_selection_cursor(pane_id, mouse.column, mouse.row);
        true
    }
}
