use bytes::Bytes;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Direction, Rect};
use tracing::warn;

use crate::{
    app::state::{
        AgentPanelScope, AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget,
        MenuListState, Mode, TabPressState, WorkspacePressState,
    },
    layout::{PaneInfo, SplitBorder},
    selection::Selection,
};

#[cfg(test)]
use super::WheelRouting;
use super::{
    modal::{
        apply_context_menu_action, apply_global_menu_action, apply_rename_action,
        confirm_close_accept, confirm_close_cancel, global_menu_actions, leave_modal,
        modal_action_from_buttons, open_global_menu, open_new_tab_dialog, ModalAction,
    },
    settings::SettingsAction,
    ScrollbarClickTarget, TAB_DRAG_THRESHOLD, WORKSPACE_DRAG_THRESHOLD,
};

impl AppState {
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        if self.mode == Mode::Onboarding {
            self.handle_onboarding_mouse(mouse);
            return None;
        }

        if self.mode == Mode::Settings {
            return self.handle_settings_mouse(mouse);
        }

        let launcher_enabled = !self.sidebar_collapsed
            && matches!(
                self.mode,
                Mode::Terminal
                    | Mode::Navigate
                    | Mode::Resize
                    | Mode::GlobalMenu
                    | Mode::KeybindHelp
            );
        let launcher = self.global_launcher_rect();
        let launcher_hit = launcher_enabled
            && mouse.column >= launcher.x
            && mouse.column < launcher.x + launcher.width
            && mouse.row >= launcher.y
            && mouse.row < launcher.y + launcher.height;

        if matches!(mouse.kind, MouseEventKind::Moved) && self.mode == Mode::GlobalMenu {
            let actions = global_menu_actions(self);
            let hovered = self
                .global_menu_item_at(mouse.column, mouse.row)
                .and_then(|action| actions.iter().position(|item| *item == action));
            self.global_menu.hover(hovered);
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && launcher_hit {
            if self.mode == Mode::GlobalMenu {
                leave_modal(self);
            } else {
                open_global_menu(self);
            }
            return None;
        }

        if self.mode == Mode::GlobalMenu {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(action) = self.global_menu_item_at(mouse.column, mouse.row) {
                    apply_global_menu_action(self, action);
                } else {
                    leave_modal(self);
                }
            }
            return None;
        }

        if self.mode == Mode::KeybindHelp {
            return None;
        }

        let sidebar = self.view.sidebar_rect;
        let in_sidebar = mouse.column >= sidebar.x
            && mouse.column < sidebar.x + sidebar.width
            && mouse.row >= sidebar.y
            && mouse.row < sidebar.y + sidebar.height;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.workspace_press = None;

                if self.mode == Mode::ConfirmClose {
                    let popup = self.confirm_close_rect();
                    let inner = Rect::new(
                        popup.x + 1,
                        popup.y + 1,
                        popup.width.saturating_sub(2),
                        popup.height.saturating_sub(2),
                    );
                    let (confirm, cancel) = crate::ui::confirm_close_button_rects(inner);
                    match modal_action_from_buttons(
                        mouse.column,
                        mouse.row,
                        &[
                            (confirm, ModalAction::Confirm),
                            (cancel, ModalAction::Cancel),
                        ],
                    ) {
                        Some(ModalAction::Confirm) => confirm_close_accept(self),
                        Some(ModalAction::Cancel) | None => confirm_close_cancel(self),
                        _ => {}
                    }
                    return None;
                }

                if matches!(self.mode, Mode::RenameWorkspace | Mode::RenameTab) {
                    let action = self
                        .rename_modal_inner()
                        .map(crate::ui::rename_button_rects)
                        .and_then(|(save, clear, cancel)| {
                            modal_action_from_buttons(
                                mouse.column,
                                mouse.row,
                                &[
                                    (save, ModalAction::Save),
                                    (clear, ModalAction::Clear),
                                    (cancel, ModalAction::Cancel),
                                ],
                            )
                        })
                        .unwrap_or(ModalAction::Cancel);
                    apply_rename_action(self, action);
                    return None;
                }

                if self.mode == Mode::ContextMenu {
                    let item_idx = self.context_menu_item_at(mouse.column, mouse.row);
                    if let Some(menu) = self.context_menu.take() {
                        if let Some(idx) = item_idx {
                            apply_context_menu_action(self, menu, idx);
                        } else {
                            leave_modal(self);
                        }
                    }
                    return None;
                }

                if self.on_sidebar_divider(mouse.column, mouse.row) {
                    self.drag = Some(DragState {
                        target: DragTarget::SidebarDivider,
                    });
                    self.set_manual_sidebar_width(mouse.column);
                    return None;
                }

                if self.on_sidebar_section_divider(mouse.column, mouse.row) {
                    self.drag = Some(DragState {
                        target: DragTarget::SidebarSectionDivider,
                    });
                    self.set_sidebar_section_split(mouse.row);
                    return None;
                }

                if !in_sidebar {
                    if let Some(border) = self.find_border_at(mouse.column, mouse.row) {
                        self.drag = Some(DragState {
                            target: DragTarget::PaneSplit {
                                path: border.path.clone(),
                                direction: border.direction,
                                area: border.area,
                            },
                        });
                        return None;
                    }

                    if let Some((pane_id, target)) =
                        self.scrollbar_target_at(mouse.column, mouse.row)
                    {
                        self.focus_pane(pane_id);
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::PaneScrollbar {
                                        pane_id,
                                        grab_row_offset,
                                    },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_pane_scroll_offset(pane_id, offset_from_bottom);
                            }
                        }
                        if self.mode != Mode::Terminal {
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }
                }

                if self.on_tab_scroll_left_button(mouse.column, mouse.row) {
                    self.scroll_tabs_left();
                    return None;
                }
                if self.on_tab_scroll_right_button(mouse.column, mouse.row) {
                    self.scroll_tabs_right();
                    return None;
                }
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.tab_press = Some(TabPressState {
                        ws_idx,
                        tab_idx,
                        start_col: mouse.column,
                        start_row: mouse.row,
                    });
                    return None;
                }
                if self.on_new_tab_button(mouse.column, mouse.row) {
                    open_new_tab_dialog(self);
                    return None;
                }

                if in_sidebar {
                    if self.sidebar_collapsed {
                        if self.on_collapsed_sidebar_toggle(mouse.column, mouse.row) {
                            self.sidebar_collapsed = false;
                            return None;
                        }

                        if let Some(idx) = self.collapsed_workspace_at_row(mouse.row) {
                            self.switch_workspace(idx);
                            self.mode = Mode::Terminal;
                            return None;
                        }

                        if let Some((ws_idx, tab_idx, pane_id)) =
                            self.collapsed_agent_detail_target_at(mouse.row)
                        {
                            self.switch_workspace(ws_idx);
                            self.switch_tab(tab_idx);
                            self.focus_pane(pane_id);
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }

                    let new_button = self.sidebar_new_button_rect();
                    let on_new_button = mouse.row >= new_button.y
                        && mouse.row < new_button.y + new_button.height
                        && mouse.column >= new_button.x
                        && mouse.column < new_button.x + new_button.width;
                    if on_new_button {
                        self.request_new_workspace = true;
                        return None;
                    }

                    if let Some(target) =
                        self.workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::WorkspaceListScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        return None;
                    }

                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.workspace_press = Some(WorkspacePressState {
                            ws_idx: idx,
                            start_col: mouse.column,
                            start_row: mouse.row,
                        });
                        return None;
                    }

                    if self.on_agent_panel_scope_toggle(mouse.column, mouse.row) {
                        self.agent_panel_scope = match self.agent_panel_scope {
                            AgentPanelScope::CurrentWorkspace => AgentPanelScope::AllWorkspaces,
                            AgentPanelScope::AllWorkspaces => AgentPanelScope::CurrentWorkspace,
                        };
                        self.agent_panel_scroll = 0;
                        self.mark_session_dirty();
                        return None;
                    }

                    if let Some(target) =
                        self.agent_panel_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::AgentPanelScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_agent_panel_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        return None;
                    }

                    if let Some((ws_idx, tab_idx, pane_id)) = self.agent_detail_target_at(mouse.row)
                    {
                        self.switch_workspace(ws_idx);
                        self.switch_tab(tab_idx);
                        self.focus_pane(pane_id);
                        self.mode = Mode::Terminal;
                        return None;
                    }
                } else if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }

                    if self.forward_pane_mouse_button(&info, mouse) {
                        self.selection = None;
                        return None;
                    }

                    let (row, col) = (
                        mouse.row - info.inner_rect.y,
                        mouse.column - info.inner_rect.x,
                    );
                    self.selection = Some(Selection::anchor(
                        info.id,
                        row,
                        col,
                        self.pane_scroll_metrics(info.id),
                    ));
                } else if let Some(info) = self.view.pane_infos.iter().find(|p| {
                    mouse.column >= p.rect.x
                        && mouse.column < p.rect.x + p.rect.width
                        && mouse.row >= p.rect.y
                        && mouse.row < p.rect.y + p.rect.height
                }) {
                    let id = info.id;
                    self.focus_pane(id);
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }
                }
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_some() {
                    self.update_selection_drag(mouse.column, mouse.row);
                    return None;
                }

                if self.drag.is_none() {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(&info, mouse) {
                            self.selection = None;
                            return None;
                        }
                    }
                }

                let workspace_drop_index = self.workspace_drop_index_at_row(mouse.row);
                let tab_drop_index = self.tab_drop_index_at(mouse.column, mouse.row);
                if self.drag.is_none() {
                    if let Some(press) = &self.workspace_press {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        if delta_col.max(delta_row) >= WORKSPACE_DRAG_THRESHOLD {
                            self.drag = Some(DragState {
                                target: DragTarget::WorkspaceReorder {
                                    source_ws_idx: press.ws_idx,
                                    insert_idx: workspace_drop_index,
                                },
                            });
                        }
                    } else if let Some(press) = &self.tab_press {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        if delta_col.max(delta_row) >= TAB_DRAG_THRESHOLD {
                            self.drag = Some(DragState {
                                target: DragTarget::TabReorder {
                                    ws_idx: press.ws_idx,
                                    source_tab_idx: press.tab_idx,
                                    insert_idx: tab_drop_index,
                                },
                            });
                        }
                    }
                }

                if let Some(DragState {
                    target: DragTarget::WorkspaceReorder { insert_idx, .. },
                }) = &mut self.drag
                {
                    *insert_idx = workspace_drop_index;
                } else if let Some(DragState {
                    target:
                        DragTarget::TabReorder {
                            ws_idx, insert_idx, ..
                        },
                }) = &mut self.drag
                {
                    if self.active == Some(*ws_idx) {
                        *insert_idx = tab_drop_index;
                    }
                } else if let Some(drag) = &self.drag {
                    match &drag.target {
                        DragTarget::WorkspaceReorder { .. } | DragTarget::TabReorder { .. } => {}
                        DragTarget::WorkspaceListScrollbar { grab_row_offset } => {
                            if let Some(offset_from_bottom) =
                                self.workspace_list_offset_for_drag_row(mouse.row, *grab_row_offset)
                            {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        DragTarget::AgentPanelScrollbar { grab_row_offset } => {
                            if let Some(offset_from_bottom) =
                                self.agent_panel_offset_for_drag_row(mouse.row, *grab_row_offset)
                            {
                                self.set_agent_panel_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        DragTarget::PaneSplit {
                            path,
                            direction,
                            area,
                        } => {
                            let ratio = match direction {
                                Direction::Horizontal => {
                                    (mouse.column.saturating_sub(area.x)) as f32
                                        / area.width.max(1) as f32
                                }
                                Direction::Vertical => {
                                    (mouse.row.saturating_sub(area.y)) as f32
                                        / area.height.max(1) as f32
                                }
                            };
                            let ratio = ratio.clamp(0.1, 0.9);
                            let path = path.clone();
                            if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                                ws.layout.set_ratio_at(&path, ratio);
                                self.mark_session_dirty();
                            }
                        }
                        DragTarget::PaneScrollbar {
                            pane_id,
                            grab_row_offset,
                        } => {
                            if let Some(offset_from_bottom) = self.scrollbar_offset_for_pane_row(
                                *pane_id,
                                mouse.row,
                                *grab_row_offset,
                            ) {
                                self.set_pane_scroll_offset(*pane_id, offset_from_bottom);
                            }
                        }
                        DragTarget::SidebarDivider => {
                            self.set_manual_sidebar_width(mouse.column);
                        }
                        DragTarget::SidebarSectionDivider => {
                            self.set_sidebar_section_split(mouse.row);
                        }
                        DragTarget::ReleaseNotesScrollbar { .. }
                        | DragTarget::KeybindHelpScrollbar { .. } => {}
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                if self.selection.is_some() {
                    self.workspace_press = None;
                    self.tab_press = None;
                    self.drag = None;
                    let was_click = self.selection.as_ref().is_some_and(|s| s.was_just_click());
                    if was_click {
                        self.selection = None;
                    } else {
                        self.copy_selection();
                    }
                    return None;
                }

                if self.drag.is_none() {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(&info, mouse) {
                            self.selection = None;
                            self.workspace_press = None;
                            self.tab_press = None;
                            self.drag = None;
                            return None;
                        }
                    }
                }

                let workspace_press = self.workspace_press.take();
                let tab_press = self.tab_press.take();
                match self.drag.take() {
                    Some(DragState {
                        target:
                            DragTarget::WorkspaceReorder {
                                source_ws_idx,
                                insert_idx: Some(insert_idx),
                            },
                    }) => {
                        self.move_workspace(source_ws_idx, insert_idx);
                    }
                    Some(DragState {
                        target:
                            DragTarget::TabReorder {
                                ws_idx,
                                source_tab_idx,
                                insert_idx: Some(insert_idx),
                            },
                    }) => {
                        if self.active == Some(ws_idx) {
                            self.move_tab(source_tab_idx, insert_idx);
                            self.mode = Mode::Terminal;
                        }
                    }
                    Some(_) => {}
                    None => {
                        if let Some(press) = workspace_press {
                            self.switch_workspace(press.ws_idx);
                            self.mode = Mode::Terminal;
                            return None;
                        }
                        if let Some(press) = tab_press {
                            if self.active == Some(press.ws_idx) {
                                self.switch_tab(press.tab_idx);
                                self.mode = Mode::Terminal;
                                return None;
                            }
                        }
                        let was_click = self.selection.as_ref().is_some_and(|s| s.was_just_click());
                        if was_click {
                            self.selection = None;
                        } else {
                            self.copy_selection();
                        }
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Middle) | MouseEventKind::Drag(MouseButton::Middle)
                if !in_sidebar =>
            {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    let _ = self.forward_pane_mouse_button(&info, mouse);
                }
            }

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !in_sidebar => {
                if self.on_tab_bar(mouse.column, mouse.row) {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => self.scroll_tabs_left(),
                        MouseEventKind::ScrollDown => self.scroll_tabs_right(),
                        _ => {}
                    }
                } else if !self.scroll_selection_with_wheel(mouse) {
                    self.selection = None;
                    self.handle_terminal_wheel(mouse);
                }
            }

            MouseEventKind::ScrollUp if in_sidebar => {
                let agent_area = self.agent_panel_rect();
                let over_agent_panel = agent_area != Rect::default()
                    && mouse.row >= agent_area.y
                    && mouse.row < agent_area.y + agent_area.height;
                if over_agent_panel {
                    if crate::ui::should_show_scrollbar(crate::ui::agent_panel_scroll_metrics(
                        self, agent_area,
                    )) {
                        self.scroll_agent_panel(-1);
                    }
                } else if crate::ui::should_show_scrollbar(
                    crate::ui::workspace_list_scroll_metrics(self, self.workspace_list_rect()),
                ) {
                    self.scroll_workspace_list(-1);
                } else if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_workspace_visible(self.selected);
                }
            }
            MouseEventKind::ScrollDown if in_sidebar => {
                let agent_area = self.agent_panel_rect();
                let over_agent_panel = agent_area != Rect::default()
                    && mouse.row >= agent_area.y
                    && mouse.row < agent_area.y + agent_area.height;
                if over_agent_panel {
                    if crate::ui::should_show_scrollbar(crate::ui::agent_panel_scroll_metrics(
                        self, agent_area,
                    )) {
                        self.scroll_agent_panel(1);
                    }
                } else if crate::ui::should_show_scrollbar(
                    crate::ui::workspace_list_scroll_metrics(self, self.workspace_list_rect()),
                ) {
                    self.scroll_workspace_list(1);
                } else if !self.workspaces.is_empty() && self.selected < self.workspaces.len() - 1 {
                    self.selected += 1;
                    self.ensure_workspace_visible(self.selected);
                }
            }

            MouseEventKind::Moved if self.mode == Mode::ContextMenu => {
                let hovered = self.context_menu_item_at(mouse.column, mouse.row);
                if let Some(menu) = &mut self.context_menu {
                    menu.list.hover(hovered);
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
                if self
                    .workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    .is_some()
                {
                    return None;
                }
                if let Some(idx) = self.workspace_at_row(mouse.row) {
                    self.selected = idx;
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Workspace { ws_idx: idx },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right)
                if self.tab_at(mouse.column, mouse.row).is_some() =>
            {
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.switch_tab(tab_idx);
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Tab { ws_idx, tab_idx },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right) if !in_sidebar => {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Pane,
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            _ => {}
        }

        None
    }

    pub(super) fn screen_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        let terminal = self.view.terminal_area;
        let x = sidebar.x.min(terminal.x);
        let y = sidebar.y.min(terminal.y);
        let right = (sidebar.x + sidebar.width).max(terminal.x + terminal.width);
        let bottom = (sidebar.y + sidebar.height).max(terminal.y + terminal.height);
        Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    pub(crate) fn context_menu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        let screen = self.screen_rect();
        let max_item_w = menu
            .items()
            .iter()
            .map(|item| item.len() as u16)
            .max()
            .unwrap_or(0);
        let menu_w = (max_item_w + 4).max(14).min(screen.width.max(1));
        let menu_h = (menu.items().len() as u16 + 2).min(screen.height.max(1));
        let x = menu.x.min(screen.x + screen.width.saturating_sub(menu_w));
        let y = menu.y.min(screen.y + screen.height.saturating_sub(menu_h));
        Some(Rect::new(x, y, menu_w, menu_h))
    }

    pub(crate) fn confirm_close_rect(&self) -> Rect {
        crate::ui::confirm_close_popup_rect(self.view.terminal_area).unwrap_or_default()
    }

    fn context_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let menu_rect = self.context_menu_rect()?;
        let inner_x = menu_rect.x + 1;
        let inner_y = menu_rect.y + 1;
        let inner_w = menu_rect.width.saturating_sub(2);
        let inner_h = menu_rect.height.saturating_sub(2);
        let item_count = self
            .context_menu
            .as_ref()
            .map(|menu| menu.items().len() as u16)
            .unwrap_or(0);
        if col >= inner_x
            && col < inner_x + inner_w
            && row >= inner_y
            && row < inner_y + inner_h.min(item_count)
        {
            Some((row - inner_y) as usize)
        } else {
            None
        }
    }

    pub(super) fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        self.view
            .tab_hit_areas
            .iter()
            .enumerate()
            .find_map(|(idx, area)| {
                (area.width > 0
                    && row >= area.y
                    && row < area.y + area.height
                    && col >= area.x
                    && col < area.x + area.width)
                    .then_some(idx)
            })
    }

    pub(super) fn on_tab_bar(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_bar_rect;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_tab_scroll_left_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_left_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_tab_scroll_right_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_right_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn tab_drop_index_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.on_tab_bar(col, row) {
            return None;
        }

        let visible_tabs: Vec<_> = self
            .view
            .tab_hit_areas
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.width > 0)
            .collect();
        let (first_idx, first_rect) = *visible_tabs.first()?;
        let (last_idx, last_rect) = *visible_tabs.last()?;

        if self.on_tab_scroll_left_button(col, row) {
            return Some(0);
        }
        if self.on_tab_scroll_right_button(col, row) {
            return self
                .active
                .and_then(|idx| self.workspaces.get(idx))
                .map(|ws| ws.tabs.len());
        }

        let left_edge = if first_idx == 0 {
            first_rect.x
        } else {
            self.view.tab_scroll_left_hit_area.x + self.view.tab_scroll_left_hit_area.width
        };
        let right_edge = if self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .is_some_and(|ws| last_idx + 1 >= ws.tabs.len())
        {
            last_rect.x + last_rect.width
        } else {
            self.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        };

        if col <= left_edge {
            return Some(first_idx);
        }
        if col >= right_edge {
            return Some(last_idx + 1);
        }

        for (idx, rect) in visible_tabs {
            let midpoint = rect.x + rect.width / 2;
            if col < midpoint {
                return Some(idx);
            }
            if col < rect.x + rect.width {
                return Some(idx + 1);
            }
        }

        Some(last_idx + 1)
    }

    pub(super) fn on_new_tab_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.new_tab_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn find_border_at(&self, col: u16, row: u16) -> Option<&SplitBorder> {
        self.view.split_borders.iter().find(|b| match b.direction {
            Direction::Horizontal => {
                (col as i32 - b.pos as i32).unsigned_abs() <= 1
                    && row >= b.area.y
                    && row < b.area.y + b.area.height
            }
            Direction::Vertical => {
                (row as i32 - b.pos as i32).unsigned_abs() <= 1
                    && col >= b.area.x
                    && col < b.area.x + b.area.width
            }
        })
    }

    pub(super) fn pane_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.inner_rect.x
                && col < p.inner_rect.x + p.inner_rect.width
                && row >= p.inner_rect.y
                && row < p.inner_rect.y + p.inner_rect.height
        })
    }

    pub(super) fn pane_mouse_target(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.pane_at(col, row)
            .or_else(|| self.pane_frame_at(col, row))
    }

    pub(super) fn pane_info_by_id(&self, pane_id: crate::layout::PaneId) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|info| info.id == pane_id)
    }

    pub(super) fn pane_frame_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.rect.x
                && col < p.rect.x + p.rect.width
                && row >= p.rect.y
                && row < p.rect.y + p.rect.height
        })
    }

    pub(super) fn focus_pane(&mut self, pane_id: crate::layout::PaneId) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if ws.layout.focused() != pane_id {
                ws.layout.focus_pane(pane_id);
                self.mark_session_dirty();
            }
        }
    }

    pub(super) fn scroll_pane_up(&self, pane_id: crate::layout::PaneId, lines: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.scroll_up(lines);
            }
        }
    }

    pub(super) fn scroll_pane_down(&self, pane_id: crate::layout::PaneId, lines: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.scroll_down(lines);
            }
        }
    }

    pub(super) fn pane_scroll_metrics(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::pane::ScrollMetrics> {
        self.active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| ws.runtime(pane_id))
            .and_then(crate::pane::PaneRuntime::scroll_metrics)
    }

    pub(super) fn handle_terminal_wheel(&mut self, mouse: MouseEvent) {
        const LINES_PER_NOTCH: usize = 3;

        if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            if self.forward_pane_wheel(&info, mouse) {
                return;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_pane_up(info.id, LINES_PER_NOTCH),
                MouseEventKind::ScrollDown => self.scroll_pane_down(info.id, LINES_PER_NOTCH),
                _ => {}
            }
            return;
        }

        if let Some(info) = self.pane_frame_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            match mouse.kind {
                MouseEventKind::ScrollUp => self.scroll_pane_up(info.id, LINES_PER_NOTCH),
                MouseEventKind::ScrollDown => self.scroll_pane_down(info.id, LINES_PER_NOTCH),
                _ => {}
            }
            return;
        }

        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                match mouse.kind {
                    MouseEventKind::ScrollUp => rt.scroll_up(LINES_PER_NOTCH),
                    MouseEventKind::ScrollDown => rt.scroll_down(LINES_PER_NOTCH),
                    _ => {}
                }
            }
        }
    }

    pub(super) fn forward_pane_mouse_button(&self, info: &PaneInfo, mouse: MouseEvent) -> bool {
        let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) else {
            return false;
        };
        let Some(rt) = ws.runtimes.get(&info.id) else {
            return false;
        };
        let column = mouse.column.saturating_sub(info.inner_rect.x);
        let row = mouse.row.saturating_sub(info.inner_rect.y);
        let Some(bytes) = rt.encode_mouse_button(mouse.kind, column, row, mouse.modifiers) else {
            return false;
        };
        rt.scroll_reset();
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, kind = ?mouse.kind, "failed to forward mouse button event");
        }
        true
    }

    pub(super) fn forward_pane_wheel(&self, info: &PaneInfo, mouse: MouseEvent) -> bool {
        let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) else {
            return false;
        };
        let Some(rt) = ws.runtimes.get(&info.id) else {
            return false;
        };
        match rt.wheel_routing() {
            Some(crate::pane::WheelRouting::HostScroll) | None => false,
            Some(crate::pane::WheelRouting::MouseReport) => {
                rt.scroll_reset();
                let column = mouse.column.saturating_sub(info.inner_rect.x);
                let row = mouse.row.saturating_sub(info.inner_rect.y);
                let Some(bytes) = rt.encode_mouse_wheel(mouse.kind, column, row, mouse.modifiers)
                else {
                    warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
                }
                true
            }
            Some(crate::pane::WheelRouting::AlternateScroll) => {
                rt.scroll_reset();
                let Some(bytes) = rt.encode_alternate_scroll(mouse.kind) else {
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward alternate-scroll key");
                }
                true
            }
        }
    }

    pub(super) fn set_pane_scroll_offset(
        &self,
        pane_id: crate::layout::PaneId,
        offset_from_bottom: usize,
    ) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.set_scroll_offset_from_bottom(offset_from_bottom);
            }
        }
    }

    pub(super) fn scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<(crate::layout::PaneId, ScrollbarClickTarget)> {
        let ws = self.active.and_then(|i| self.workspaces.get(i))?;
        let info = self.view.pane_infos.iter().find(|info| {
            crate::ui::pane_scrollbar_rect(info).is_some_and(|track| {
                col >= track.x
                    && col < track.x + track.width
                    && row >= track.y
                    && row < track.y + track.height
            })
        })?;
        let rt = ws.runtimes.get(&info.id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        let track = crate::ui::pane_scrollbar_rect(info)?;
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some((info.id, ScrollbarClickTarget::Thumb { grab_row_offset }))
        } else {
            Some((
                info.id,
                ScrollbarClickTarget::Track {
                    offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
                },
            ))
        }
    }

    pub(super) fn scrollbar_offset_for_pane_row(
        &self,
        pane_id: crate::layout::PaneId,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let ws = self.active.and_then(|i| self.workspaces.get(i))?;
        let info = self
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)?;
        let track = crate::ui::pane_scrollbar_rect(info)?;
        let rt = ws.runtimes.get(&pane_id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }
}

#[cfg(test)]
pub(super) fn wheel_routing(input_state: crate::pane::InputState) -> WheelRouting {
    if input_state.mouse_protocol_mode.reporting_enabled() {
        WheelRouting::MouseReport
    } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
        WheelRouting::AlternateScroll
    } else {
        WheelRouting::HostScroll
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::{Direction, Rect};

    use super::super::{
        app_for_mouse_test, capture_snapshot, handle_context_menu_key, mouse, root_layout_ratio,
    };
    use super::*;
    use crate::{
        app::state::{ContextMenuKind, ContextMenuState, MenuListState, Mode},
        workspace::Workspace,
    };

    #[test]
    fn hovering_context_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 2,
            y: 2,
            list: MenuListState::new(0),
        });
        app.state.mode = Mode::ContextMenu;

        let menu = app.state.context_menu_rect().unwrap();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.context_menu.unwrap().list.highlighted, 1);
    }

    #[test]
    fn clicking_confirm_close_accepts_workspace_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::ConfirmClose;

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_confirm_close_accepts_after_workspace_context_menu_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;
        handle_context_menu_key(
            &mut app.state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.selected, 1);

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x + 1,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
    }

    #[test]
    fn dragging_pane_split_updates_captured_layout_ratio() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn dragging_vertical_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Vertical);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.workspaces[0].tabs[0].runtimes.insert(
            first_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.workspaces[0].tabs[0].runtimes.insert(
            second_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Vertical)
            .expect("vertical split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_col = border.area.x.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            drag_col,
            border.pos,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            border.pos.saturating_add(4),
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn dragging_horizontal_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Horizontal);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.workspaces[0].tabs[0].runtimes.insert(
            first_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.workspaces[0].tabs[0].runtimes.insert(
            second_pane,
            crate::pane::PaneRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Horizontal)
            .expect("horizontal split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn wheel_routing_prefers_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::MouseReport);
    }

    #[test]
    fn wheel_routing_uses_alternate_scroll_in_fullscreen_without_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::AlternateScroll);
    }

    #[test]
    fn wheel_routing_falls_back_to_host_scrollback() {
        let input_state = crate::pane::InputState {
            alternate_screen: false,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::HostScroll);
    }
}
