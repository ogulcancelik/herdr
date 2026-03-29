//! Input handling — translates crossterm key/mouse events into state mutations.

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Direction, Rect};
use tracing::warn;

use crate::layout::{NavDirection, PaneInfo, SplitBorder};
use crate::selection::Selection;

use super::state::{key_matches, AppState, ContextMenuState, DragState, Mode, CONTEXT_MENU_ITEMS};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

impl App {
    pub(super) async fn handle_key(&mut self, key: KeyEvent) {
        match self.state.mode {
            Mode::Onboarding => self.handle_onboarding_key(key),
            Mode::Navigate => handle_navigate_key(&mut self.state, key),
            Mode::Terminal => self.handle_terminal_key(key).await,
            Mode::RenameSession => handle_rename_key(&mut self.state, key),
            Mode::Resize => handle_resize_key(&mut self.state, key),
            Mode::ConfirmClose => handle_confirm_close_key(&mut self.state, key),
            Mode::ContextMenu => handle_context_menu_key(&mut self.state, key),
            Mode::Settings => self.handle_settings_key(key),
        }
    }

    pub(super) async fn handle_paste(&mut self, text: String) {
        if self.state.mode != Mode::Terminal {
            return;
        }
        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                let bracketed = rt
                    .parser
                    .read()
                    .map(|p| p.screen().bracketed_paste())
                    .unwrap_or(false);

                let payload = if bracketed {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text
                };
                let _ = rt.sender.send(Bytes::from(payload)).await;
            }
        }
    }

    fn handle_onboarding_key(&mut self, key: KeyEvent) {
        match self.state.onboarding_step {
            0 => match key.code {
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    self.state.onboarding_step = 1;
                }
                KeyCode::Char('q') => self.state.should_quit = true,
                _ => {}
            },
            _ => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.state.onboarding_selected > 0 {
                        self.state.onboarding_selected -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.state.onboarding_selected < 3 {
                        self.state.onboarding_selected += 1;
                    }
                }
                KeyCode::Left | KeyCode::Esc | KeyCode::Char('h') => {
                    self.state.onboarding_step = 0;
                }
                KeyCode::Char(c) if ('1'..='4').contains(&c) => {
                    self.state.onboarding_selected = (c as usize) - ('1' as usize);
                }
                KeyCode::Enter => self.complete_onboarding(),
                KeyCode::Char('q') => self.state.should_quit = true,
                _ => {}
            },
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        if let Some(action) = update_settings_state(&mut self.state, key) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToast(enabled) => self.save_toast(enabled),
            }
        }
    }

    async fn handle_terminal_key(&mut self, key: KeyEvent) {
        self.state.clear_selection();
        self.state.update_dismissed = true;

        if self.state.is_prefix(&key) {
            self.state.mode = Mode::Navigate;
            return;
        }

        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                rt.scroll_reset();
                let kitty = rt.kitty_keyboard.load(std::sync::atomic::Ordering::Relaxed);
                let bytes = crate::input::encode_key(key, kitty);
                if bytes.is_empty() {
                    warn!(code = ?key.code, mods = ?key.modifiers, state = ?key.state, "key produced empty encoding");
                } else {
                    let _ = rt.sender.send(Bytes::from(bytes)).await;
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingsAction {
    SaveTheme(String),
    SaveSound(bool),
    SaveToast(bool),
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    use crate::app::state::THEME_NAMES;

    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::{Palette, THEME_NAMES};

    let name = THEME_NAMES[state.settings.selected];
    if let Some(palette) = Palette::from_name(name) {
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    leave_modal(state);
}

fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    use crate::app::state::SettingsSection;

    match state.settings.section {
        SettingsSection::Theme => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if state.settings.selected > 0 {
                    state.settings.selected -= 1;
                    preview_selected_theme(state);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if state.settings.selected + 1 < crate::app::state::THEME_NAMES.len() {
                    state.settings.selected += 1;
                    preview_selected_theme(state);
                }
            }
            KeyCode::Enter => {
                let theme_name = state.theme_name.clone();
                state.settings.original_palette = None;
                state.settings.original_theme = None;
                leave_modal(state);
                return Some(SettingsAction::SaveTheme(theme_name));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Esc | KeyCode::Char('q') => cancel_settings(state),
            _ => {}
        },
        SettingsSection::Sound => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.selected = 1 - state.settings.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.selected == 0;
                state.sound.enabled = enabled;
                return Some(SettingsAction::SaveSound(enabled));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.selected = usize::from(!state.toast_config.enabled);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.selected = current_theme_index(&state.theme_name);
            }
            KeyCode::Esc | KeyCode::Char('q') => cancel_settings(state),
            _ => {}
        },
        SettingsSection::Toast => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.selected = 1 - state.settings.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.selected == 0;
                state.toast_config.enabled = enabled;
                return Some(SettingsAction::SaveToast(enabled));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.selected = current_theme_index(&state.theme_name);
            }
            KeyCode::Esc | KeyCode::Char('q') => cancel_settings(state),
            _ => {}
        },
    }

    None
}

fn handle_navigate_key(state: &mut AppState, key: KeyEvent) {
    state.update_dismissed = true;

    if state.is_prefix(&key) || key.code == KeyCode::Esc {
        leave_navigate_mode(state);
        return;
    }

    if let Some(action) = navigate_action_for_key(state, &key) {
        execute_navigate_action(state, action);
        return;
    }

    match key.code {
        KeyCode::Char('q') => state.should_quit = true,
        KeyCode::Enter => {
            if !state.workspaces.is_empty() {
                state.switch_workspace(state.selected);
                leave_navigate_mode(state);
            }
        }
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < state.workspaces.len() {
                state.switch_workspace(idx);
                leave_navigate_mode(state);
            }
        }
        KeyCode::Char('s') => {
            open_settings(state);
        }
        KeyCode::Up => {
            if state.selected > 0 {
                state.selected -= 1;
            }
        }
        KeyCode::Down => {
            if !state.workspaces.is_empty() && state.selected < state.workspaces.len() - 1 {
                state.selected += 1;
            }
        }
        KeyCode::Char('h') | KeyCode::Left => state.navigate_pane(NavDirection::Left),
        KeyCode::Char('j') => state.navigate_pane(NavDirection::Down),
        KeyCode::Char('k') => state.navigate_pane(NavDirection::Up),
        KeyCode::Char('l') | KeyCode::Right => state.navigate_pane(NavDirection::Right),
        KeyCode::Tab => state.cycle_pane(false),
        KeyCode::BackTab => state.cycle_pane(true),
        _ => {}
    }
}

#[derive(Debug, Clone, Copy)]
enum NavigateAction {
    NewWorkspace,
    RenameWorkspace,
    CloseWorkspace,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    Fullscreen,
    EnterResizeMode,
    ToggleSidebar,
}

fn navigate_action_for_key(state: &AppState, key: &KeyEvent) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    if key_matches(key, kb.new_workspace.0, kb.new_workspace.1) {
        return Some(NavigateAction::NewWorkspace);
    }
    if key_matches(key, kb.rename_workspace.0, kb.rename_workspace.1) {
        return Some(NavigateAction::RenameWorkspace);
    }
    if key_matches(key, kb.close_workspace.0, kb.close_workspace.1) {
        return Some(NavigateAction::CloseWorkspace);
    }
    if key_matches(key, kb.split_vertical.0, kb.split_vertical.1) {
        return Some(NavigateAction::SplitVertical);
    }
    if key_matches(key, kb.split_horizontal.0, kb.split_horizontal.1) {
        return Some(NavigateAction::SplitHorizontal);
    }
    if key_matches(key, kb.close_pane.0, kb.close_pane.1) {
        return Some(NavigateAction::ClosePane);
    }
    if key_matches(key, kb.fullscreen.0, kb.fullscreen.1) {
        return Some(NavigateAction::Fullscreen);
    }
    if key_matches(key, kb.resize_mode.0, kb.resize_mode.1) {
        return Some(NavigateAction::EnterResizeMode);
    }
    if key_matches(key, kb.toggle_sidebar.0, kb.toggle_sidebar.1) {
        return Some(NavigateAction::ToggleSidebar);
    }
    None
}

fn execute_navigate_action(state: &mut AppState, action: NavigateAction) {
    match action {
        NavigateAction::NewWorkspace => {
            state.request_new_workspace = true;
            leave_navigate_mode(state);
        }
        NavigateAction::RenameWorkspace => {
            if !state.workspaces.is_empty() {
                state.name_input = state.workspaces[state.selected].display_name();
                state.mode = Mode::RenameSession;
            }
        }
        NavigateAction::CloseWorkspace => {
            if !state.workspaces.is_empty() {
                if state.confirm_close {
                    state.mode = Mode::ConfirmClose;
                } else {
                    state.close_selected_workspace();
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::SplitVertical => {
            state.split_pane(Direction::Horizontal);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitHorizontal => {
            state.split_pane(Direction::Vertical);
            leave_navigate_mode(state);
        }
        NavigateAction::ClosePane => {
            state.close_pane();
            leave_navigate_mode(state);
        }
        NavigateAction::Fullscreen => {
            state.toggle_fullscreen();
            leave_navigate_mode(state);
        }
        NavigateAction::EnterResizeMode => state.mode = Mode::Resize,
        NavigateAction::ToggleSidebar => {
            state.sidebar_collapsed = !state.sidebar_collapsed;
            leave_navigate_mode(state);
        }
    }
}

fn leave_navigate_mode(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    }
}

/// Return to the appropriate mode after completing a modal action.
/// Goes to Terminal if a workspace is active, otherwise Navigate.
fn leave_modal(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    } else {
        state.mode = Mode::Navigate;
    }
}

fn open_settings(state: &mut AppState) {
    use crate::app::state::SettingsSection;

    // Save current state for cancel
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.section = SettingsSection::Theme;
    state.settings.selected = current_theme_index(&state.theme_name);
    state.mode = Mode::Settings;
}

fn handle_rename_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let new_name = if state.name_input.trim().is_empty() {
                state.name_input.clone()
            } else {
                state.name_input.trim().to_string()
            };
            if !new_name.is_empty() && !state.workspaces.is_empty() {
                state.workspaces[state.selected].set_custom_name(new_name);
            }
            state.name_input.clear();
            state.mode = Mode::Navigate;
        }
        KeyCode::Esc => {
            state.name_input.clear();
            state.mode = Mode::Navigate;
        }
        KeyCode::Backspace => {
            state.name_input.pop();
        }
        KeyCode::Char(c) => {
            state.name_input.push(c);
        }
        _ => {}
    }
}

fn handle_resize_key(state: &mut AppState, key: KeyEvent) {
    if key.code == KeyCode::Esc
        || key.code == KeyCode::Enter
        || key_matches(
            &key,
            state.keybinds.resize_mode.0,
            state.keybinds.resize_mode.1,
        )
    {
        if state.active.is_some() {
            state.mode = Mode::Terminal;
        } else {
            state.mode = Mode::Navigate;
        }
        return;
    }

    match key.code {
        KeyCode::Char('h') | KeyCode::Left => state.resize_pane(NavDirection::Left),
        KeyCode::Char('l') | KeyCode::Right => state.resize_pane(NavDirection::Right),
        KeyCode::Char('j') | KeyCode::Down => state.resize_pane(NavDirection::Down),
        KeyCode::Char('k') | KeyCode::Up => state.resize_pane(NavDirection::Up),
        _ => {}
    }
}

fn confirm_close_accept(state: &mut AppState) {
    state.close_selected_workspace();
    if state.workspaces.is_empty() {
        state.mode = Mode::Navigate;
    } else {
        state.mode = Mode::Terminal;
    }
}

fn confirm_close_cancel(state: &mut AppState) {
    state.mode = Mode::Navigate;
}

fn handle_confirm_close_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => confirm_close_accept(state),
        _ => confirm_close_cancel(state),
    }
}

fn handle_context_menu_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            state.context_menu = None;
            state.mode = Mode::Navigate;
        }
        KeyCode::Up => {
            if let Some(menu) = &mut state.context_menu {
                if menu.selected > 0 {
                    menu.selected -= 1;
                }
            }
        }
        KeyCode::Down => {
            if let Some(menu) = &mut state.context_menu {
                if menu.selected < CONTEXT_MENU_ITEMS.len() - 1 {
                    menu.selected += 1;
                }
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = state.context_menu.take() {
                match CONTEXT_MENU_ITEMS[menu.selected] {
                    "Rename" => {
                        state.selected = menu.ws_idx;
                        state.name_input = state.workspaces[menu.ws_idx].display_name();
                        state.mode = Mode::RenameSession;
                    }
                    "Close" => {
                        state.selected = menu.ws_idx;
                        if state.confirm_close {
                            state.mode = Mode::ConfirmClose;
                        } else {
                            state.close_selected_workspace();
                            state.mode = Mode::Navigate;
                        }
                    }
                    _ => state.mode = Mode::Navigate,
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Mouse handling
// ---------------------------------------------------------------------------

impl AppState {
    fn onboarding_full_area(&self) -> ratatui::layout::Rect {
        self.view.sidebar_rect.union(self.view.terminal_area)
    }

    fn onboarding_modal_inner(&self, popup_w: u16, popup_h: u16) -> Option<ratatui::layout::Rect> {
        let area = self.onboarding_full_area();
        let popup_w = popup_w.min(area.width.saturating_sub(4));
        let popup_h = popup_h.min(area.height.saturating_sub(2));
        if popup_w < 4 || popup_h < 4 {
            return None;
        }
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        let popup = ratatui::layout::Rect::new(popup_x, popup_y, popup_w, popup_h);
        let block = ratatui::widgets::Block::default().borders(ratatui::widgets::Borders::ALL);
        Some(block.inner(popup))
    }

    fn handle_onboarding_mouse(&mut self, mouse: MouseEvent) {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }

        match self.onboarding_step {
            0 => {
                let Some(inner) = self.onboarding_modal_inner(64, 15) else {
                    return;
                };
                let footer_y = inner.y + 9;
                let button_x = inner.x;
                let button_w = 14;
                if mouse.row == footer_y
                    && mouse.column >= button_x
                    && mouse.column < button_x + button_w
                {
                    self.onboarding_step = 1;
                }
            }
            _ => {
                let Some(inner) = self.onboarding_modal_inner(52, 10) else {
                    return;
                };
                let options_start_y = inner.y + 2;
                if mouse.row >= options_start_y && mouse.row < options_start_y + 4 {
                    self.onboarding_selected = (mouse.row - options_start_y) as usize;
                    return;
                }

                let footer_y = inner.y + 6;
                let back_x = inner.x;
                let back_w = 10;
                let save_x = inner.x + 12;
                let save_w = 10;
                if mouse.row == footer_y {
                    if mouse.column >= back_x && mouse.column < back_x + back_w {
                        self.onboarding_step = 0;
                    } else if mouse.column >= save_x && mouse.column < save_x + save_w {
                        self.request_complete_onboarding = true;
                    }
                }
            }
        }
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.mode == Mode::Onboarding {
            self.handle_onboarding_mouse(mouse);
            return;
        }

        let sidebar = self.view.sidebar_rect;
        let in_sidebar = mouse.column >= sidebar.x
            && mouse.column < sidebar.x + sidebar.width
            && mouse.row >= sidebar.y
            && mouse.row < sidebar.y + sidebar.height;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;

                if self.mode == Mode::ConfirmClose {
                    if self.confirm_close_confirm_button_at(mouse.column, mouse.row) {
                        confirm_close_accept(self);
                    } else {
                        confirm_close_cancel(self);
                    }
                    return;
                }

                if self.mode == Mode::ContextMenu {
                    if let Some(menu) = &self.context_menu {
                        let item_idx = self.context_menu_item_at(mouse.column, mouse.row);
                        if let Some(idx) = item_idx {
                            let ws_idx = menu.ws_idx;
                            self.context_menu = None;
                            match CONTEXT_MENU_ITEMS[idx] {
                                "Rename" => {
                                    self.selected = ws_idx;
                                    self.name_input = self.workspaces[ws_idx].display_name();
                                    self.mode = Mode::RenameSession;
                                }
                                "Close" => {
                                    self.selected = ws_idx;
                                    if self.confirm_close {
                                        self.mode = Mode::ConfirmClose;
                                    } else {
                                        self.close_selected_workspace();
                                        self.mode = Mode::Navigate;
                                    }
                                }
                                _ => self.mode = Mode::Navigate,
                            }
                        } else {
                            self.context_menu = None;
                            self.mode = Mode::Navigate;
                        }
                    }
                    return;
                }

                if !in_sidebar {
                    if let Some(border) = self.find_border_at(mouse.column, mouse.row) {
                        self.drag = Some(DragState {
                            path: border.path.clone(),
                            direction: border.direction,
                            area: border.area,
                        });
                        return;
                    }
                }

                if in_sidebar {
                    if self.sidebar_collapsed {
                        // Collapsed: each workspace is 1 row
                        let idx = (mouse.row - sidebar.y) as usize;
                        if idx < self.workspaces.len() {
                            self.switch_workspace(idx);
                            self.mode = Mode::Terminal;
                        }
                        return;
                    }

                    // Two-section layout: top half is workspaces
                    let total_h = sidebar.height as usize;
                    let ws_h = (total_h + 1) / 2;
                    let ws_bottom = sidebar.y + ws_h as u16;

                    // "new" button is at the last row of workspace section
                    let new_row = ws_bottom.saturating_sub(1);
                    if mouse.row == new_row {
                        self.request_new_workspace = true;
                        return;
                    }

                    // Workspace clicks in top section
                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.switch_workspace(idx);
                        self.mode = Mode::Terminal;
                        return;
                    }
                } else if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    let (row, col) = (
                        mouse.row - info.inner_rect.y,
                        mouse.column - info.inner_rect.x,
                    );
                    self.selection = Some(Selection::anchor(info.id, row, col, info.inner_rect));

                    if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                        if ws.layout.focused() != info.id {
                            ws.layout.focus_pane(info.id);
                        }
                    }
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }
                } else {
                    if let Some(info) = self.view.pane_infos.iter().find(|p| {
                        mouse.column >= p.rect.x
                            && mouse.column < p.rect.x + p.rect.width
                            && mouse.row >= p.rect.y
                            && mouse.row < p.rect.y + p.rect.height
                    }) {
                        let id = info.id;
                        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                            if ws.layout.focused() != id {
                                ws.layout.focus_pane(id);
                            }
                        }
                        if self.mode != Mode::Terminal {
                            self.mode = Mode::Terminal;
                        }
                    }
                }
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &self.drag {
                    let ratio = match drag.direction {
                        Direction::Horizontal => {
                            (mouse.column.saturating_sub(drag.area.x)) as f32
                                / drag.area.width.max(1) as f32
                        }
                        Direction::Vertical => {
                            (mouse.row.saturating_sub(drag.area.y)) as f32
                                / drag.area.height.max(1) as f32
                        }
                    };
                    let ratio = ratio.clamp(0.1, 0.9);
                    let path = drag.path.clone();
                    if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                        ws.layout.set_ratio_at(&path, ratio);
                    }
                } else if let Some(sel) = &mut self.selection {
                    sel.drag(mouse.column, mouse.row);
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
                    // Drag ended
                } else {
                    let was_click = self.selection.as_ref().is_some_and(|s| s.was_just_click());
                    if was_click {
                        self.selection = None;
                    } else {
                        self.copy_selection();
                    }
                }
            }

            MouseEventKind::ScrollUp if !in_sidebar => {
                self.selection = None;
                if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                    if let Some(rt) = ws.focused_runtime() {
                        rt.scroll_up(3);
                    }
                }
            }
            MouseEventKind::ScrollDown if !in_sidebar => {
                self.selection = None;
                if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                    if let Some(rt) = ws.focused_runtime() {
                        rt.scroll_down(3);
                    }
                }
            }

            MouseEventKind::ScrollUp if in_sidebar => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            MouseEventKind::ScrollDown if in_sidebar => {
                if !self.workspaces.is_empty() && self.selected < self.workspaces.len() - 1 {
                    self.selected += 1;
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
                if let Some(idx) = self.workspace_at_row(mouse.row) {
                    self.selected = idx;
                    self.context_menu = Some(ContextMenuState {
                        ws_idx: idx,
                        x: mouse.column,
                        y: mouse.row,
                        selected: 0,
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            _ => {}
        }
    }

    /// Find which workspace index a sidebar row belongs to (two-section layout).
    fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let sidebar = self.view.sidebar_rect;
        let total_h = sidebar.height as usize;
        let ws_h = (total_h + 1) / 2;
        let ws_bottom = sidebar.y + ws_h as u16;
        let new_row = ws_bottom.saturating_sub(1);

        if row < sidebar.y || row >= new_row {
            return None;
        }

        let mut row_y = sidebar.y;
        for (i, ws) in self.workspaces.iter().enumerate() {
            let has_branch = ws.branch().is_some();
            let card_h: u16 = if has_branch { 2 } else { 1 };
            if row >= row_y && row < row_y + card_h {
                return Some(i);
            }
            row_y += card_h + 1; // +1 for gap
            if row_y >= new_row {
                break;
            }
        }
        None
    }

    fn screen_rect(&self) -> Rect {
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
        let menu_w = 14u16.min(screen.width.max(1));
        let menu_h = (CONTEXT_MENU_ITEMS.len() as u16 + 2).min(screen.height.max(1));
        let x = menu.x.min(screen.x + screen.width.saturating_sub(menu_w));
        let y = menu.y.min(screen.y + screen.height.saturating_sub(menu_h));
        Some(Rect::new(x, y, menu_w, menu_h))
    }

    pub(crate) fn confirm_close_rect(&self) -> Rect {
        let area = self.view.terminal_area;
        let popup_w = 44u16.min(area.width.saturating_sub(4));
        let popup_h = 6u16.min(area.height.max(1));
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        Rect::new(popup_x, popup_y, popup_w, popup_h)
    }

    fn confirm_close_confirm_button_at(&self, col: u16, row: u16) -> bool {
        let popup = self.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let confirm_w = 9u16;
        let cancel_w = 8u16;
        let gap = 2u16;
        let total_w = confirm_w + gap + cancel_w;
        let x = inner.x + inner.width.saturating_sub(total_w) / 2;
        let y = inner.y + 2.min(inner.height.saturating_sub(1));
        col >= x && col < x + confirm_w && row == y
    }

    fn context_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let menu_rect = self.context_menu_rect()?;
        let inner_x = menu_rect.x + 1;
        let inner_y = menu_rect.y + 1;
        let inner_w = menu_rect.width.saturating_sub(2);
        let inner_h = menu_rect.height.saturating_sub(2);
        if col >= inner_x
            && col < inner_x + inner_w
            && row >= inner_y
            && row < inner_y + inner_h.min(CONTEXT_MENU_ITEMS.len() as u16)
        {
            Some((row - inner_y) as usize)
        } else {
            None
        }
    }

    fn find_border_at(&self, col: u16, row: u16) -> Option<&SplitBorder> {
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

    fn pane_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.inner_rect.x
                && col < p.inner_rect.x + p.inner_rect.width
                && row >= p.inner_rect.y
                && row < p.inner_rect.y + p.inner_rect.height
        })
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
            if let Ok(new_id) = ws.split_focused(direction, new_rows, new_cols, cwd) {
                ws.layout.focus_pane(new_id);
                self.mode = Mode::Terminal;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use crossterm::event::KeyModifiers;

    fn state_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.selected = 0;
            state.mode = Mode::Navigate;
        }
        state
    }

    #[test]
    fn custom_rename_key_enters_rename_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.rename_workspace = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.rename_workspace_label = "g".into();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::RenameSession);
        assert_eq!(state.name_input, "test");
    }

    #[test]
    fn custom_new_workspace_key_requests_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.new_workspace = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.new_workspace_label = "g".into();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_new_workspace);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_sidebar_toggle_key_toggles_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.toggle_sidebar_label = "g".into();
        assert!(!state.sidebar_collapsed);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.sidebar_collapsed);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_resize_key_enters_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.resize_mode = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.resize_mode_label = "g".into();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Resize);
    }

    #[test]
    fn movement_action_stays_in_navigate_mode() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn fullscreen_action_exits_navigate_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.keybinds.fullscreen = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.fullscreen_label = "g".into();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_resize_key_exits_resize_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::Resize;
        state.keybinds.resize_mode = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.resize_mode_label = "g".into();

        handle_resize_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings(&mut state);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Sound
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }
}
