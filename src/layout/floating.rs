//! Floating pane layout: workspace-level panes that overlay tiled content.

use std::collections::HashMap;

use ratatui::{layout::Rect, widgets::Borders};

use super::{PaneId, PaneInfo};

/// A floating pane's absolute position within the terminal area, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatingPanePosition {
    /// Top-left x offset relative to the terminal area.
    pub x: u16,
    /// Top-left y offset relative to the terminal area.
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl FloatingPanePosition {
    pub fn clamp_to_area(self, area: Rect) -> Self {
        let width = self.width.min(area.width);
        let height = self.height.min(area.height);
        let max_x = area.width.saturating_sub(width);
        let max_y = area.height.saturating_sub(height);
        let x = self.x.min(max_x);
        let y = self.y.min(max_y);
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn to_rect(self, area: Rect) -> Rect {
        let clamped = self.clamp_to_area(area);
        Rect::new(
            area.x + clamped.x,
            area.y + clamped.y,
            clamped.width,
            clamped.height,
        )
    }

    pub fn default_in_area(area: Rect) -> Self {
        let w = ((area.width as f32) * 0.6) as u16;
        let h = ((area.height as f32) * 0.6) as u16;
        let width = w.max(Self::MIN_WIDTH).min(area.width);
        let height = h.max(Self::MIN_HEIGHT).min(area.height);
        let x = area.width.saturating_sub(width) / 2;
        let y = area.height.saturating_sub(height) / 2;
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const MIN_WIDTH: u16 = 10;
    pub const MIN_HEIGHT: u16 = 5;
}

/// Collection of floating panes at the workspace level.
#[derive(Debug, Clone, Default)]
pub struct FloatingPanes {
    /// Rendering order (back to front). Last pane in the vec is on top.
    pub order: Vec<PaneId>,
    /// Absolute positions in terminal-area cell coordinates.
    pub positions: HashMap<PaneId, FloatingPanePosition>,
    /// Currently focused floating pane, if any.
    pub focused: Option<PaneId>,
    /// Whether floating panes are currently visible.
    pub visible: bool,
}

impl FloatingPanes {
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Add a new floating pane at a default centered position.
    pub fn add(&mut self, pane_id: PaneId, area: Rect) {
        let pos = FloatingPanePosition::default_in_area(area);
        self.positions.insert(pane_id, pos);
        self.order.push(pane_id);
        self.focused = Some(pane_id);
        if !self.visible {
            self.visible = true;
        }
    }

    /// Remove a floating pane by id.
    pub fn remove(&mut self, pane_id: PaneId) -> bool {
        if !self.order.contains(&pane_id) {
            return false;
        }
        self.order.retain(|id| *id != pane_id);
        self.positions.remove(&pane_id);
        if self.focused == Some(pane_id) {
            self.focused = self.order.last().copied();
        }
        true
    }

    /// Focus a specific floating pane, bringing it to the top.
    pub fn focus(&mut self, pane_id: PaneId) {
        if !self.order.contains(&pane_id) {
            return;
        }
        self.focused = Some(pane_id);
        self.order.retain(|id| *id != pane_id);
        self.order.push(pane_id);
    }

    /// Clear focus on floating panes (return focus to tiled layout).
    #[cfg(test)]
    pub fn clear_focus(&mut self) {
        self.focused = None;
    }

    /// Hide all floating panes and return focus to tiled layout.
    pub fn hide(&mut self) {
        self.visible = false;
        self.focused = None;
    }

    /// Show floating panes and focus the top-most pane.
    pub fn show(&mut self) {
        if let Some(pane_id) = self.order.last().copied() {
            self.visible = true;
            self.focus(pane_id);
        }
    }

    /// Move the focused floating pane by a delta in cells.
    #[cfg(test)]
    pub fn move_focused(&mut self, dx: i16, dy: i16, area: Rect) -> bool {
        let Some(focused_id) = self.focused else {
            return false;
        };
        let Some(pos) = self.positions.get_mut(&focused_id) else {
            return false;
        };
        let new_x = (pos.x as i32 + dx as i32).clamp(0, area.width.saturating_sub(1) as i32) as u16;
        let new_y =
            (pos.y as i32 + dy as i32).clamp(0, area.height.saturating_sub(1) as i32) as u16;
        if new_x != pos.x || new_y != pos.y {
            pos.x = new_x;
            pos.y = new_y;
            true
        } else {
            false
        }
    }

    /// Resize the focused floating pane by a delta.
    #[cfg(test)]
    pub fn resize_focused(&mut self, dw: i16, dh: i16, area: Rect) -> bool {
        let Some(focused_id) = self.focused else {
            return false;
        };
        let Some(pos) = self.positions.get_mut(&focused_id) else {
            return false;
        };
        let new_w = (pos.width as i32 + dw as i32).clamp(
            FloatingPanePosition::MIN_WIDTH as i32,
            area.width.saturating_sub(pos.x) as i32,
        ) as u16;
        let new_h = (pos.height as i32 + dh as i32).clamp(
            FloatingPanePosition::MIN_HEIGHT as i32,
            area.height.saturating_sub(pos.y) as i32,
        ) as u16;
        if new_w != pos.width || new_h != pos.height {
            pos.width = new_w;
            pos.height = new_h;
            true
        } else {
            false
        }
    }

    /// Set the position of a specific floating pane.
    pub fn set_position(&mut self, pane_id: PaneId, pos: FloatingPanePosition) -> bool {
        if !self.order.contains(&pane_id) {
            return false;
        }
        self.positions.insert(pane_id, pos);
        true
    }

    /// Set a pane position while keeping the whole pane inside the terminal
    /// area. This is for user-driven move/resize operations; render-time
    /// clamping remains non-mutating so stored geometry survives temporary
    /// small client sizes.
    pub fn set_position_clamped(
        &mut self,
        pane_id: PaneId,
        pos: FloatingPanePosition,
        area: Rect,
    ) -> bool {
        self.set_position(pane_id, pos.clamp_to_area(area))
    }

    /// Compute PaneInfo for each floating pane within the terminal area.
    pub fn pane_infos(&self, area: Rect) -> Vec<PaneInfo> {
        if !self.visible {
            return Vec::new();
        }
        self.order
            .iter()
            .filter_map(|&id| {
                let pos = self.positions.get(&id)?;
                let rect = pos.to_rect(area);
                if rect.width < FloatingPanePosition::MIN_WIDTH
                    || rect.height < FloatingPanePosition::MIN_HEIGHT
                {
                    return None;
                }
                let borders = Borders::ALL;
                let inner_rect = crate::ui::panes::pane_inner_rect(rect, borders);
                Some(PaneInfo {
                    id,
                    rect,
                    inner_rect,
                    scrollbar_rect: None,
                    borders,
                    is_focused: self.focused == Some(id),
                })
            })
            .collect()
    }

    /// Check whether any floating pane has focus.
    #[cfg(test)]
    pub fn has_focus(&self) -> bool {
        self.focused.is_some()
    }

    /// Resize all floating pane terminals to their computed inner rects.
    pub fn resize_terminals(
        &self,
        area: Rect,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        app: &crate::app::AppState,
        ws_idx: usize,
        cell_size: crate::kitty_graphics::HostCellSize,
    ) {
        for info in self.pane_infos(area) {
            let pane_inner = crate::ui::panes::pane_inner_rect(info.rect, info.borders);
            if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
            {
                let terminal_id = app
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.pane_state(info.id))
                    .map(|ps| &ps.attached_terminal_id);
                let locked = terminal_id
                    .as_ref()
                    .is_some_and(|tid| app.direct_attach_resize_locks.contains(tid));
                if !locked {
                    rt.resize(
                        pane_inner.height,
                        pane_inner.width,
                        cell_size.width_px,
                        cell_size.height_px,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn pid(id: u32) -> PaneId {
        PaneId::from_raw(id)
    }

    fn test_area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    #[test]
    fn default_position_centers_in_area() {
        let pos = FloatingPanePosition::default_in_area(test_area());
        assert_eq!(pos.width, 60);
        assert_eq!(pos.height, 24);
        assert_eq!(pos.x, 20);
        assert_eq!(pos.y, 8);
    }

    #[test]
    fn default_position_clamps_small_area() {
        let small = Rect::new(0, 0, 15, 8);
        let pos = FloatingPanePosition::default_in_area(small);
        assert!(pos.width >= FloatingPanePosition::MIN_WIDTH);
        assert!(pos.height >= FloatingPanePosition::MIN_HEIGHT);
    }

    #[test]
    fn add_pane_sets_focus_and_visibility() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        assert_eq!(fp.order, vec![pid(1)]);
        assert_eq!(fp.focused, Some(pid(1)));
        assert!(fp.visible);
        assert!(fp.positions.contains_key(&pid(1)));
    }

    #[test]
    fn remove_pane_updates_focus() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.add(pid(2), test_area());
        fp.focus(pid(1));
        assert_eq!(fp.focused, Some(pid(1)));
        fp.remove(pid(1));
        assert_eq!(fp.focused, Some(pid(2)));
        assert_eq!(fp.order, vec![pid(2)]);
    }

    #[test]
    fn remove_last_pane_clears_focus() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.remove(pid(1));
        assert!(fp.order.is_empty());
        assert_eq!(fp.focused, None);
    }

    #[test]
    fn focus_brings_to_top() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.add(pid(2), test_area());
        fp.add(pid(3), test_area());
        assert_eq!(fp.order, vec![pid(1), pid(2), pid(3)]);
        fp.focus(pid(1));
        assert_eq!(fp.order, vec![pid(2), pid(3), pid(1)]);
        assert_eq!(fp.focused, Some(pid(1)));
    }

    #[test]
    fn move_focused_clamps_to_area() {
        let mut fp = FloatingPanes::default();
        let area = test_area();
        fp.add(pid(1), area);
        fp.move_focused(-100, -100, area);
        let pos = fp.positions[&pid(1)];
        assert_eq!(pos.x, 0);
        assert_eq!(pos.y, 0);
    }

    #[test]
    fn resize_focused_respects_min_size() {
        let mut fp = FloatingPanes::default();
        let area = test_area();
        fp.add(pid(1), area);
        fp.resize_focused(-100, -100, area);
        let pos = fp.positions[&pid(1)];
        assert_eq!(pos.width, FloatingPanePosition::MIN_WIDTH);
        assert_eq!(pos.height, FloatingPanePosition::MIN_HEIGHT);
    }

    #[test]
    fn pane_infos_filters_when_not_visible() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.visible = false;
        assert!(fp.pane_infos(test_area()).is_empty());
    }

    #[test]
    fn pane_infos_clamps_to_area() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.positions.insert(
            pid(1),
            FloatingPanePosition {
                x: 0,
                y: 0,
                width: 200,
                height: 200,
            },
        );
        let infos = fp.pane_infos(test_area());
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].rect, test_area());
    }

    #[test]
    fn clamp_moves_oversized_pane_inside_area() {
        let pos = FloatingPanePosition {
            x: 64,
            y: 20,
            width: 193,
            height: 59,
        };
        let clamped = pos.clamp_to_area(Rect::new(0, 0, 80, 24));

        assert_eq!(clamped.x, 0);
        assert_eq!(clamped.y, 0);
        assert_eq!(clamped.width, 80);
        assert_eq!(clamped.height, 24);
    }

    #[test]
    fn clamp_preserves_size_when_moving_near_edge() {
        let pos = FloatingPanePosition {
            x: 95,
            y: 39,
            width: 60,
            height: 24,
        };
        let clamped = pos.clamp_to_area(test_area());

        assert_eq!(
            clamped,
            FloatingPanePosition {
                x: 40,
                y: 16,
                width: 60,
                height: 24,
            }
        );
    }

    #[test]
    fn set_position_is_absolute_and_idempotent() {
        // Drag-move computes origin + total delta and calls set_position each
        // event. Re-applying the same target must not accumulate (no drift).
        let mut fp = FloatingPanes::default();
        let area = test_area();
        fp.add(pid(1), area);
        let origin = fp.positions[&pid(1)];

        let target = FloatingPanePosition {
            x: origin.x + 5,
            y: origin.y + 3,
            ..origin
        };
        fp.set_position(pid(1), target);
        let after_first = fp.positions[&pid(1)];
        fp.set_position(pid(1), target);
        let after_second = fp.positions[&pid(1)];

        assert_eq!(after_first, target);
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn clear_focus_returns_none() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        assert!(fp.has_focus());
        fp.clear_focus();
        assert!(!fp.has_focus());
    }

    #[test]
    fn hide_clears_focus_and_visibility() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());

        fp.hide();

        assert!(!fp.visible);
        assert_eq!(fp.focused, None);
    }

    #[test]
    fn show_focuses_topmost_pane() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.add(pid(2), test_area());
        fp.focus(pid(1));
        fp.hide();

        fp.show();

        assert!(fp.visible);
        assert_eq!(fp.focused, Some(pid(1)));
    }

    #[test]
    fn has_focus_is_false_when_focused_is_none() {
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        fp.focused = None;
        assert!(!fp.has_focus());
    }

    #[test]
    fn is_empty_when_no_panes() {
        assert!(FloatingPanes::default().is_empty());
        let mut fp = FloatingPanes::default();
        fp.add(pid(1), test_area());
        assert!(!fp.is_empty());
    }
}
