use ratatui::layout::Rect;

use crate::app::state::{AppState, Mode};

use super::ScrollbarClickTarget;

impl AppState {
    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect(sidebar, self.sidebar_section_split)
    }

    pub(super) fn agent_panel_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        let (_, detail_area) =
            crate::ui::expanded_sidebar_sections(sidebar, self.sidebar_section_split);
        detail_area
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            return;
        }

        for _ in 0..delta as usize {
            let cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
            let Some(last) = cards.last() else {
                break;
            };
            if last.ws_idx + 1 >= self.workspaces.len() {
                break;
            }
            self.workspace_scroll = self.workspace_scroll.saturating_add(1);
        }
    }

    pub(super) fn agent_panel_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn agent_panel_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        let track = crate::ui::agent_panel_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_agent_panel_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, area);
        self.agent_panel_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
    }

    pub(super) fn scroll_agent_panel(&mut self, delta: i16) {
        let area = self.agent_panel_rect();
        let max_scroll = crate::ui::agent_panel_scroll_metrics(self, area).max_offset_from_bottom;
        if delta.is_negative() {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.agent_panel_scroll = self
                .agent_panel_scroll
                .saturating_add(delta as usize)
                .min(max_scroll);
        }
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let ws_area = self.workspace_list_rect();
        if ws_area == Rect::default() {
            return Rect::default();
        }
        let y = ws_area.y + ws_area.height.saturating_sub(1);
        Rect::new(ws_area.x, y, ws_area.width, 1)
    }

    pub(crate) fn sidebar_new_button_rect(&self) -> Rect {
        let footer = self.sidebar_footer_rect();
        let width = 5u16.min(footer.width.max(1));
        Rect::new(footer.x, footer.y, width, footer.height)
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        let footer = self.sidebar_footer_rect();
        let width = if self.update_available.is_some() {
            8
        } else {
            6
        }
        .min(footer.width.max(1));
        let x = footer.x + footer.width.saturating_sub(width);
        Rect::new(x, footer.y, width, footer.height)
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload keybinds"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push(if self.quit_detaches { "detach" } else { "quit" });
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let extra = if *label == "update ready" { 2 } else { 0 };
                label.chars().count() as u16 + extra
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher.y.saturating_sub(menu_h);
        Rect::new(x, y, menu_w, menu_h)
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        sidebar.width > 0
            && col == sidebar.x + sidebar.width.saturating_sub(1)
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    pub(super) fn on_collapsed_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        if !self.sidebar_collapsed {
            return false;
        }
        let rect = crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect);
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width =
            width.clamp(crate::ui::MIN_SIDEBAR_WIDTH, crate::ui::MAX_SIDEBAR_WIDTH);
        self.mark_session_dirty();
    }

    pub(super) fn on_sidebar_section_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let rect = crate::ui::sidebar_section_divider_rect(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_sidebar_section_split(&mut self, row: u16) {
        let sidebar = self.view.sidebar_rect;
        let content_height = sidebar.height;
        if content_height < 6 {
            return;
        }
        let relative_y = row.saturating_sub(sidebar.y);
        let ratio = (relative_y as f32) / (content_height as f32);
        self.sidebar_section_split = ratio.clamp(0.1, 0.9);
        self.mark_session_dirty();
    }

    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };

        cards.iter().find_map(|card| {
            (row >= card.rect.y && row < card.rect.y + card.rect.height).then_some(card.ws_idx)
        })
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (ws_area, _, _) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx = (row - ws_area.y) as usize;
        (idx < self.workspaces.len()).then_some(idx)
    }

    fn collapsed_detail_workspace_idx(&self) -> Option<usize> {
        if matches!(
            self.mode,
            Mode::Navigate
                | Mode::RenameWorkspace
                | Mode::Resize
                | Mode::ConfirmClose
                | Mode::ContextMenu
                | Mode::Settings
                | Mode::GlobalMenu
                | Mode::KeybindHelp
        ) {
            Some(self.selected)
        } else {
            self.active
        }
    }

    pub(super) fn collapsed_agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if !self.sidebar_collapsed {
            return None;
        }

        let (_, _, detail_area) = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        let detail_content_area = Rect::new(
            detail_area.x,
            detail_area.y,
            detail_area.width,
            detail_area.height.saturating_sub(1),
        );
        if detail_content_area == Rect::default()
            || row < detail_content_area.y
            || row >= detail_content_area.y + detail_content_area.height
        {
            return None;
        }

        let ws_idx = self.collapsed_detail_workspace_idx()?;
        let ws = self.workspaces.get(ws_idx)?;
        let detail_idx = (row - detail_content_area.y) as usize;
        let details = ws.pane_details();
        let detail = details.get(detail_idx)?;
        Some((ws_idx, detail.tab_idx, detail.pane_id))
    }

    pub(super) fn workspace_drop_index_at_row(&self, row: u16) -> Option<usize> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        if cards.is_empty() {
            return Some(0);
        }

        let mut insert_indices = Vec::with_capacity(cards.len() + 1);
        insert_indices.push(cards[0].ws_idx);
        insert_indices.extend(cards.iter().skip(1).map(|card| card.ws_idx));
        insert_indices.push(cards.last().map(|card| card.ws_idx + 1).unwrap_or(0));

        let mut best: Option<(usize, u16)> = None;
        for insert_idx in insert_indices {
            let Some(slot_row) = crate::ui::workspace_drop_indicator_row(&cards, area, insert_idx)
            else {
                continue;
            };
            let distance = row.abs_diff(slot_row);
            match best {
                Some((best_idx, best_distance))
                    if distance > best_distance
                        || (distance == best_distance && insert_idx < best_idx) => {}
                _ => best = Some((insert_idx, distance)),
            }
        }

        best.map(|(insert_idx, _)| insert_idx)
    }

    pub(super) fn on_agent_panel_scope_toggle(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        let rect = crate::ui::agent_panel_toggle_rect(detail_area, self.agent_panel_scope);
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn agent_detail_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, usize, crate::layout::PaneId)> {
        if self.sidebar_collapsed {
            return None;
        }

        let detail_area = self.agent_panel_rect();
        let metrics = crate::ui::agent_panel_scroll_metrics(self, detail_area);
        let body = crate::ui::agent_panel_body_rect(
            detail_area,
            crate::ui::should_show_scrollbar(metrics),
        );
        if body.height < 2 || row < body.y || row >= body.y + body.height {
            return None;
        }

        let mut row_y = body.y;
        for detail in crate::ui::agent_panel_entries(self)
            .into_iter()
            .skip(self.agent_panel_scroll)
        {
            if row_y.saturating_add(1) >= body.y + body.height {
                break;
            }
            if row == row_y || row == row_y + 1 {
                return Some((detail.ws_idx, detail.tab_idx, detail.pane_id));
            }
            row_y = row_y.saturating_add(2);
            if row_y < body.y + body.height {
                row_y = row_y.saturating_add(1);
            }
        }
        None
    }
}
