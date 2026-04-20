//! Input handling — translates crossterm key/mouse events into state mutations.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::input::TerminalKey;
use ratatui::layout::{Direction, Rect};

use crate::selection::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

const WORKSPACE_DRAG_THRESHOLD: u16 = 1;
const TAB_DRAG_THRESHOLD: u16 = 1;

mod modal;
mod mouse;
mod navigate;
mod overlays;
mod selection;
mod settings;
mod sidebar;
mod terminal;

#[cfg(test)]
use self::{
    modal::open_rename_active_tab,
    mouse::wheel_routing,
    navigate::{execute_navigate_action, handle_navigate_reserved_key, NavigateAction},
    settings::{open_settings, update_settings_state},
};
use self::{
    modal::{
        apply_context_menu_action, apply_global_menu_action, apply_rename_action,
        confirm_close_accept, confirm_close_cancel, global_menu_actions, leave_modal,
        modal_action_from_buttons, modal_action_from_key, open_global_menu, open_new_tab_dialog,
        ModalAction, ONBOARDING_NOTIFICATION_ACTIONS, ONBOARDING_WELCOME_ACTIONS,
        RELEASE_NOTES_ACTIONS,
    },
    settings::SettingsAction,
};
pub(crate) use self::{
    modal::{
        handle_confirm_close_key, handle_context_menu_key, handle_global_menu_key,
        handle_keybind_help_key, handle_rename_key, handle_resize_key,
    },
    navigate::{handle_navigate_key, terminal_direct_navigation_action},
};
use super::state::{
    AgentPanelScope, AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget,
    MenuListState, Mode, TabPressState, WorkspacePressState,
};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

impl App {
    pub(super) async fn handle_key(&mut self, key: TerminalKey) {
        match self.state.mode {
            Mode::Terminal => self.handle_terminal_key(key).await,
            _ => {
                let key = key.as_key_event();
                match self.state.mode {
                    Mode::Onboarding => self.handle_onboarding_key(key),
                    Mode::ReleaseNotes => self.handle_release_notes_key(key),
                    Mode::Navigate => self.handle_navigate_key(key),
                    Mode::RenameWorkspace | Mode::RenameTab => {
                        handle_rename_key(&mut self.state, key)
                    }
                    Mode::Resize => handle_resize_key(&mut self.state, key),
                    Mode::ConfirmClose => handle_confirm_close_key(&mut self.state, key),
                    Mode::ContextMenu => handle_context_menu_key(&mut self.state, key),
                    Mode::Settings => self.handle_settings_key(key),
                    Mode::GlobalMenu => handle_global_menu_key(&mut self.state, key),
                    Mode::KeybindHelp => handle_keybind_help_key(&mut self.state, key),
                    Mode::Terminal => unreachable!(),
                }
            }
        }
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.mode != Mode::Terminal {
            return;
        }
        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                let _ = rt.send_paste(text).await;
            }
        }
    }

    pub(crate) fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match self.state.onboarding_step {
            0 => match key.code {
                KeyCode::Right | KeyCode::Char('l') => {
                    self.state.onboarding_step = 1;
                }
                _ => match modal_action_from_key(&key, ONBOARDING_WELCOME_ACTIONS) {
                    Some(ModalAction::Continue) => self.state.onboarding_step = 1,
                    _ => {}
                },
            },
            _ => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.state.onboarding_list.move_prev(),
                KeyCode::Down | KeyCode::Char('j') => self.state.onboarding_list.move_next(4),
                KeyCode::Left | KeyCode::Char('h') => {
                    self.state.onboarding_step = 0;
                }
                KeyCode::Char(c) if ('1'..='4').contains(&c) => {
                    self.state
                        .onboarding_list
                        .select((c as usize) - ('1' as usize));
                }
                _ => match modal_action_from_key(&key, ONBOARDING_NOTIFICATION_ACTIONS) {
                    Some(ModalAction::Back) => self.state.onboarding_step = 0,
                    Some(ModalAction::Save) => self.complete_onboarding(),
                    _ => {}
                },
            },
        }
    }

    pub(crate) fn handle_release_notes_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_release_notes(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_release_notes(1),
            KeyCode::PageUp => self.scroll_release_notes(-8),
            KeyCode::PageDown => self.scroll_release_notes(8),
            KeyCode::Home => {
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = 0;
                }
            }
            KeyCode::End => {
                let max_scroll = self.state.release_notes_max_scroll();
                if let Some(notes) = &mut self.state.release_notes {
                    notes.scroll = max_scroll;
                }
            }
            _ => match modal_action_from_key(&key, RELEASE_NOTES_ACTIONS) {
                Some(ModalAction::Close) => self.dismiss_release_notes(),
                _ => {}
            },
        }
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.handle_overlay_mouse(mouse) {
            return;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width = self.state.default_sidebar_width;
                self.state.sidebar_width_auto = false;
                self.state.mark_session_dirty();
                self.state.drag = None;
                return;
            }
        }

        if let Some(action) = self.state.handle_mouse(mouse) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToast(enabled) => self.save_toast(enabled),
            }
        }

        if let Some(content) = self.state.request_clipboard_write.take() {
            if self
                .event_tx
                .try_send(crate::events::AppEvent::ClipboardWrite { content })
                .is_err()
            {
                tracing::warn!("failed to queue clipboard write event");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

impl AppState {
    fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
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

                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    if self.forward_pane_mouse_button(&info, mouse) {
                        self.selection = None;
                        return None;
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

                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    if self.forward_pane_mouse_button(&info, mouse) {
                        self.selection = None;
                        self.workspace_press = None;
                        self.tab_press = None;
                        self.drag = None;
                        return None;
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
}

// Note: split_pane needs runtime (event_tx for PTY spawn), so it lives on App
impl AppState {
    pub(crate) fn split_pane(&mut self, direction: Direction) {
        // Actual PTY spawning happens in Workspace::split_focused
        // which needs events channel — this is called from navigate_key
        // where we don't have async context, so the workspace handles it
        let (rows, cols) = self.estimate_pane_size();
        let new_rows = (rows / 2).max(4);
        let new_cols = (cols / 2).max(10);

        let cwd = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .and_then(|ws| ws.focused_runtime())
            .and_then(|rt| rt.cwd());

        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if let Ok(new_id) = ws.split_focused(
                direction,
                new_rows,
                new_cols,
                cwd,
                self.pane_scrollback_limit_bytes,
                self.host_terminal_theme,
            ) {
                ws.layout.focus_pane(new_id);
                self.mark_session_dirty();
                self.mode = Mode::Terminal;
            }
        }
    }
}

#[cfg(test)]
mod tests;
