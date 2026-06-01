use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
};

use crate::app::{
    state::{AppState, DragState, DragTarget, Mode, NavigatorTarget},
    App,
};

use super::{
    modal::{leave_modal, modal_action_from_buttons, ModalAction},
    ScrollbarClickTarget,
};

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

impl App {
    pub(super) fn handle_overlay_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode == Mode::ReleaseNotes {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .release_notes_close_button_at(mouse.column, mouse.row) =>
                {
                    self.dismiss_release_notes();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .release_notes_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::ReleaseNotesScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.state
                                    .set_release_notes_offset_from_bottom(offset_from_bottom);
                            }
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::ReleaseNotesScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .release_notes_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_release_notes_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.scroll_release_notes(-3),
                MouseEventKind::ScrollDown => self.scroll_release_notes(3),
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::ProductAnnouncement {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .product_announcement_close_button_at(mouse.column, mouse.row) =>
                {
                    self.dismiss_product_announcement();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .product_announcement_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::ProductAnnouncementScrollbar {
                                        grab_row_offset,
                                    },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => self
                                .state
                                .set_product_announcement_offset_from_bottom(offset_from_bottom),
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::ProductAnnouncementScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .product_announcement_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_product_announcement_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.scroll_product_announcement(-3),
                MouseEventKind::ScrollDown => self.scroll_product_announcement(3),
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::Navigator {
            match mouse.kind {
                MouseEventKind::Moved => {
                    if let Some(idx) = self.state.navigator_row_index_at(mouse.column, mouse.row) {
                        self.state.navigator.selected = idx;
                        self.state.ensure_navigator_selection_visible();
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if self
                        .state
                        .navigator_search_contains(mouse.column, mouse.row)
                    {
                        self.state.navigator.search_focused = true;
                    } else if let Some(idx) =
                        self.state.navigator_row_index_at(mouse.column, mouse.row)
                    {
                        self.state.navigator.selected = idx;
                        let target = self
                            .state
                            .navigator_rows()
                            .get(idx)
                            .map(|row| (row.target.clone(), row.is_workspace));
                        if let Some((NavigatorTarget::Workspace { .. }, true)) = target {
                            if self.state.navigator_row_caret_at(mouse.column) {
                                self.state.toggle_selected_navigator_workspace();
                            } else {
                                self.state.accept_navigator_selection();
                            }
                        } else {
                            self.state.accept_navigator_selection();
                        }
                    } else if !self.state.navigator_popup_contains(mouse.column, mouse.row) {
                        leave_modal(&mut self.state);
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.state.navigator.scroll = self.state.navigator.scroll.saturating_sub(3);
                    self.state.navigator.selected = self.state.navigator.scroll;
                    self.state.clamp_navigator_selection();
                }
                MouseEventKind::ScrollDown => {
                    let viewport = self.state.navigator_body_rect().height as usize;
                    let max = self.state.navigator_max_scroll(viewport);
                    self.state.navigator.scroll =
                        self.state.navigator.scroll.saturating_add(3).min(max);
                    self.state.navigator.selected = self.state.navigator.scroll;
                    self.state.clamp_navigator_selection();
                }
                _ => {}
            }
            return true;
        }

        if self.state.mode == Mode::KeybindHelp {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .state
                        .keybind_help_close_button_at(mouse.column, mouse.row) =>
                {
                    leave_modal(&mut self.state);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(target) = self
                        .state
                        .keybind_help_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.state.drag = Some(DragState {
                                    target: DragTarget::KeybindHelpScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.state
                                    .set_keybind_help_offset_from_bottom(offset_from_bottom);
                            }
                        }
                    } else {
                        let rect = self.state.keybind_help_popup_rect();
                        let inside = mouse.column >= rect.x
                            && mouse.column < rect.x + rect.width
                            && mouse.row >= rect.y
                            && mouse.row < rect.y + rect.height;
                        if !inside {
                            leave_modal(&mut self.state);
                        }
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(DragState {
                        target: DragTarget::KeybindHelpScrollbar { grab_row_offset },
                    }) = &self.state.drag
                    {
                        if let Some(offset_from_bottom) = self
                            .state
                            .keybind_help_offset_for_drag_row(mouse.row, *grab_row_offset)
                        {
                            self.state
                                .set_keybind_help_offset_from_bottom(offset_from_bottom);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.state.drag = None;
                }
                MouseEventKind::ScrollUp => self.state.scroll_keybind_help(-3),
                MouseEventKind::ScrollDown => self.state.scroll_keybind_help(3),
                _ => {}
            }
            return true;
        }

        false
    }
}

impl AppState {
    pub(super) fn onboarding_full_area(&self) -> Rect {
        self.view.sidebar_rect.union(self.view.terminal_area)
    }

    pub(crate) fn navigator_popup_rect(&self) -> Rect {
        let area = self.onboarding_full_area();
        let margin_x = (area.width / 16).max(2);
        let margin_y = (area.height / 10).max(1);
        let width = area.width.saturating_sub(margin_x.saturating_mul(2));
        let height = area.height.saturating_sub(margin_y.saturating_mul(2));
        Rect::new(
            area.x + margin_x,
            area.y + margin_y,
            width.max(4),
            height.max(4),
        )
    }

    pub(crate) fn navigator_inner_rect(&self) -> Rect {
        Block::default()
            .borders(Borders::ALL)
            .inner(self.navigator_popup_rect())
    }

    pub(crate) fn navigator_search_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(inner.x, inner.y, inner.width, inner.height.min(1))
    }

    pub(crate) fn navigator_body_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        if inner.height <= 4 {
            return Rect::default();
        }
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(4),
        )
    }

    pub(crate) fn navigator_detail_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(2),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn navigator_footer_rect(&self) -> Rect {
        let inner = self.navigator_inner_rect();
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            inner.height.min(1),
        )
    }

    pub(crate) fn navigator_popup_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.navigator_popup_rect(), col, row)
    }

    pub(crate) fn navigator_search_contains(&self, col: u16, row: u16) -> bool {
        rect_contains(self.navigator_search_rect(), col, row)
    }

    pub(crate) fn navigator_row_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let body = self.navigator_body_rect();
        if !rect_contains(body, col, row) {
            return None;
        }
        let idx = self
            .navigator
            .scroll
            .saturating_add(row.saturating_sub(body.y) as usize);
        (idx < self.navigator_rows().len()).then_some(idx)
    }

    pub(crate) fn navigator_row_caret_at(&self, col: u16) -> bool {
        let body = self.navigator_body_rect();
        col <= body.x.saturating_add(3)
    }

    pub(super) fn onboarding_modal_inner(&self, popup_w: u16, popup_h: u16) -> Option<Rect> {
        let area = self.onboarding_full_area();
        let popup_w = popup_w.min(area.width.saturating_sub(4));
        let popup_h = popup_h.min(area.height.saturating_sub(2));
        if popup_w < 4 || popup_h < 4 {
            return None;
        }
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);
        Some(Block::default().borders(Borders::ALL).inner(popup))
    }

    fn release_notes_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
            crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
        )
    }

    fn product_announcement_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
            crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
        )
    }

    fn release_notes_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.release_notes_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    pub(super) fn rename_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(56, 7)
    }

    fn release_notes_body_rect(&self) -> Option<Rect> {
        let inner = self.release_notes_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn release_notes_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let notes = self.release_notes.as_ref()?;
        let body = self.release_notes_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::release_notes_display_lines(
            notes,
            &self.update_install_command,
            &self.palette,
        );

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(notes.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn release_notes_max_scroll(&self) -> u16 {
        self.release_notes_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn release_notes_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let body = self.release_notes_body_rect()?;
        let metrics = self.release_notes_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
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

    fn release_notes_offset_for_drag_row(&self, row: u16, grab_row_offset: u16) -> Option<usize> {
        let body = self.release_notes_body_rect()?;
        let metrics = self.release_notes_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    fn set_release_notes_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.release_notes_max_scroll() as usize;
        if let Some(notes) = &mut self.release_notes {
            notes.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
        }
    }

    fn product_announcement_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.product_announcement_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    fn product_announcement_body_rect(&self) -> Option<Rect> {
        let inner = self.product_announcement_modal_inner()?;
        if inner.height < 8 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn product_announcement_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let announcement = self.product_announcement.as_ref()?;
        let body = self.product_announcement_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let lines = crate::ui::product_announcement_display_lines(announcement, &self.palette);

        let rows_for_width = |wrap_width: u16| {
            crate::ui::release_notes_wrapped_line_count(&lines, wrap_width.max(1))
        };

        let full_width = body.width.max(1);
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1)
        } else {
            full_width
        };
        total_rows = rows_for_width(wrap_width);

        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom.saturating_sub(announcement.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    pub(crate) fn product_announcement_max_scroll(&self) -> u16 {
        self.product_announcement_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn product_announcement_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let body = self.product_announcement_body_rect()?;
        let metrics = self.product_announcement_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
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

    fn product_announcement_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let body = self.product_announcement_body_rect()?;
        let metrics = self.product_announcement_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    fn set_product_announcement_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.product_announcement_max_scroll() as usize;
        if let Some(announcement) = &mut self.product_announcement {
            announcement.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
        }
    }

    pub(super) fn handle_onboarding_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }

        let Some(inner) = self.onboarding_modal_inner(64, 16) else {
            return;
        };
        let actions = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1)
            .actions
            .unwrap_or_default();
        let button = crate::ui::onboarding_welcome_continue_rect(actions);
        if modal_action_from_buttons(mouse.column, mouse.row, &[(button, ModalAction::Continue)])
            == Some(ModalAction::Continue)
        {
            self.request_complete_onboarding = true;
        }
    }

    pub(super) fn keybind_help_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(self.screen_rect(), 76, 22).unwrap_or_default()
    }

    fn keybind_help_modal_inner(&self) -> Option<Rect> {
        self.onboarding_modal_inner(76, 22)
    }

    fn keybind_help_close_button_at(&self, col: u16, row: u16) -> bool {
        let Some(inner) = self.keybind_help_modal_inner() else {
            return false;
        };
        if inner.height < 4 || inner.width < 12 {
            return false;
        }
        let button =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        col >= button.x
            && col < button.x + button.width
            && row >= button.y
            && row < button.y + button.height
    }

    fn keybind_help_body_rect(&self) -> Option<Rect> {
        let inner = self.keybind_help_modal_inner()?;
        if inner.height < 6 || inner.width < 4 {
            return None;
        }
        Some(crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content)
    }

    fn keybind_help_scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        let body = self.keybind_help_body_rect()?;
        let viewport_rows = body.height.max(1) as usize;
        let wrap_width = body.width.max(1) as usize;
        let total_rows = crate::ui::keybind_help_lines(self)
            .into_iter()
            .map(|(width, _)| width.max(1).div_ceil(wrap_width))
            .sum::<usize>();
        let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
        Some(crate::pane::ScrollMetrics {
            offset_from_bottom: max_offset_from_bottom
                .saturating_sub(self.keybind_help.scroll as usize),
            max_offset_from_bottom,
            viewport_rows,
        })
    }

    fn keybind_help_scrollbar_target_at(&self, col: u16, row: u16) -> Option<ScrollbarClickTarget> {
        let body = self.keybind_help_body_rect()?;
        let metrics = self.keybind_help_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        if !(col >= track.x
            && col < track.x + track.width
            && row >= track.y
            && row < track.y + track.height)
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

    fn keybind_help_offset_for_drag_row(&self, row: u16, grab_row_offset: u16) -> Option<usize> {
        let body = self.keybind_help_body_rect()?;
        let metrics = self.keybind_help_scroll_metrics()?;
        let track = crate::ui::release_notes_scrollbar_rect(body, metrics)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(crate) fn keybind_help_max_scroll(&self) -> u16 {
        self.keybind_help_scroll_metrics()
            .map(|metrics| metrics.max_offset_from_bottom as u16)
            .unwrap_or(0)
    }

    fn set_keybind_help_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let max_scroll = self.keybind_help_max_scroll() as usize;
        self.keybind_help.scroll = max_scroll.saturating_sub(offset_from_bottom) as u16;
    }

    pub(super) fn scroll_keybind_help(&mut self, delta: i16) {
        let max_scroll = self.keybind_help_max_scroll();
        let current = self.keybind_help.scroll as i16;
        self.keybind_help.scroll = current.saturating_add(delta).clamp(0, max_scroll as i16) as u16;
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, mouse};
    use super::*;

    #[test]
    fn clicking_keybind_help_close_button_closes_overlay() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::KeybindHelp;

        let rect = app.state.keybind_help_popup_rect();
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        let close =
            crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x,
            close.y,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn onboarding_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;

        let inner = app.state.onboarding_modal_inner(64, 16).unwrap();
        let content = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1).content;
        app.handle_mouse(mouse(MouseEventKind::Moved, content.x + 2, content.y));

        assert!(!app.state.request_complete_onboarding);
    }

    #[test]
    fn onboarding_click_continue_requests_completion() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;

        let inner = app.state.onboarding_modal_inner(64, 16).unwrap();
        let actions = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1)
            .actions
            .unwrap();
        let continue_rect = crate::ui::onboarding_welcome_continue_rect(actions);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            continue_rect.x,
            continue_rect.y,
        ));

        assert!(app.state.request_complete_onboarding);
    }

    #[test]
    fn release_notes_preview_scrollbar_uses_full_content_body() {
        let mut app = app_for_mouse_test();
        app.state.view.sidebar_rect = Rect::new(0, 0, 24, 16);
        app.state.view.terminal_area = Rect::new(24, 0, 96, 16);
        app.state.release_notes = Some(crate::app::state::ReleaseNotesState {
            version: "9.9.9".into(),
            body: "### Added\n- Custom command keybindings now accept an optional description field.\n\n### Fixed\n- Sidebar Git status refresh now deduplicates workspaces.\n- Large restored sessions no longer leave panes without shells after startup.\n- Pane shutdown no longer warns after the direct child has already exited.\n- Closing the last pane or tab in a parent worktree workspace now shows the existing confirmation before closing the whole worktree group.\n- Update prompts, toasts, and docs now distinguish installing a new binary from stopping or reattaching a running Herdr session to use it."
                .into(),
            scroll: 0,
            preview: true,
        });
        app.state.update_install_command = "brew update && brew upgrade herdr".into();

        let inner = app.state.release_notes_modal_inner().unwrap();
        let expected_body = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1).content;
        let body = app.state.release_notes_body_rect().unwrap();

        assert_eq!(body, expected_body);

        let metrics = app.state.release_notes_scroll_metrics().unwrap();
        assert_eq!(metrics.viewport_rows, body.height as usize);
        assert!(metrics.max_offset_from_bottom > 0);

        let track = crate::ui::release_notes_scrollbar_rect(body, metrics).unwrap();
        assert_eq!(track.y, body.y);
        assert!(matches!(
            app.state
                .release_notes_scrollbar_target_at(track.x, track.y),
            Some(ScrollbarClickTarget::Thumb { .. } | ScrollbarClickTarget::Track { .. })
        ));
    }
}
