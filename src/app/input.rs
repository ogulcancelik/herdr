//! Input handling — translates crossterm key/mouse events into state mutations.

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::input::TerminalKey;
use ratatui::layout::{Direction, Rect};
use tracing::warn;

use crate::layout::{NavDirection, PaneInfo, SplitBorder};
use crate::selection::Selection;

use super::state::{
    key_matches, AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget, Mode,
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
                    Mode::Navigate => handle_navigate_key(&mut self.state, key),
                    Mode::RenameSession => handle_rename_key(&mut self.state, key),
                    Mode::Resize => handle_resize_key(&mut self.state, key),
                    Mode::ConfirmClose => handle_confirm_close_key(&mut self.state, key),
                    Mode::ContextMenu => handle_context_menu_key(&mut self.state, key),
                    Mode::Settings => self.handle_settings_key(key),
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

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.state.on_sidebar_divider(mouse.column, mouse.row)
        {
            let now = std::time::Instant::now();
            let is_double_click = self
                .last_sidebar_divider_click
                .is_some_and(|last| now.duration_since(last) <= super::SIDEBAR_DOUBLE_CLICK_WINDOW);
            self.last_sidebar_divider_click = Some(now);

            if is_double_click {
                self.state.sidebar_width_auto = true;
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
    }

    async fn handle_terminal_key(&mut self, key: TerminalKey) {
        self.state.clear_selection();
        self.state.update_dismissed = true;

        let key_event = key.as_key_event();

        if self.state.is_prefix(&key_event) {
            self.state.mode = Mode::Navigate;
            return;
        }

        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                rt.scroll_reset();
                let flags = rt
                    .kitty_keyboard_flags
                    .load(std::sync::atomic::Ordering::Relaxed);
                let bytes = crate::input::encode_terminal_key(
                    key,
                    crate::input::KeyboardProtocol::from_kitty_flags(flags),
                );
                if bytes.is_empty() {
                    if key.kind != crossterm::event::KeyEventKind::Release
                        && !matches!(
                            key.code,
                            KeyCode::CapsLock
                                | KeyCode::ScrollLock
                                | KeyCode::NumLock
                                | KeyCode::PrintScreen
                                | KeyCode::Pause
                                | KeyCode::Menu
                                | KeyCode::KeypadBegin
                                | KeyCode::Media(_)
                                | KeyCode::Modifier(_)
                        )
                    {
                        warn!(code = ?key_event.code, mods = ?key_event.modifiers, state = ?key_event.state, "key produced empty encoding");
                    }
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

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        crate::app::state::SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        _ => {
            leave_modal(state);
            None
        }
    }
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
            KeyCode::Enter => return apply_settings(state),
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
                    open_confirm_close(state);
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

fn open_confirm_close(state: &mut AppState) {
    state.confirm_close_selected_confirm = true;
    state.mode = Mode::ConfirmClose;
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
        KeyCode::Left | KeyCode::Char('h') => state.confirm_close_selected_confirm = true,
        KeyCode::Right | KeyCode::Char('l') => state.confirm_close_selected_confirm = false,
        KeyCode::Enter => {
            if state.confirm_close_selected_confirm {
                confirm_close_accept(state);
            } else {
                confirm_close_cancel(state);
            }
        }
        KeyCode::Esc => confirm_close_cancel(state),
        _ => {}
    }
}

fn apply_context_menu_action(state: &mut AppState, menu: ContextMenuState, idx: usize) {
    let item = menu.items().get(idx).copied();
    match (menu.kind, item) {
        (ContextMenuKind::Workspace { ws_idx }, Some("Rename")) => {
            state.selected = ws_idx;
            state.name_input = state.workspaces[ws_idx].display_name();
            state.mode = Mode::RenameSession;
        }
        (ContextMenuKind::Workspace { ws_idx }, Some("Close")) => {
            state.selected = ws_idx;
            if state.confirm_close {
                open_confirm_close(state);
            } else {
                state.close_selected_workspace();
                state.mode = Mode::Navigate;
            }
        }
        (ContextMenuKind::Pane, Some("Split vertical")) => {
            state.split_pane(Direction::Horizontal);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane, Some("Split horizontal")) => {
            state.split_pane(Direction::Vertical);
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane, Some("Fullscreen")) => {
            state.toggle_fullscreen();
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Pane, Some("Close pane")) => {
            state.close_pane();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
        }
        _ => leave_modal(state),
    }
}

fn handle_context_menu_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            state.context_menu = None;
            leave_modal(state);
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
                if menu.selected + 1 < menu.items().len() {
                    menu.selected += 1;
                }
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = state.context_menu.take() {
                let idx = menu.selected;
                apply_context_menu_action(state, menu, idx);
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
        if !matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Moved
        ) {
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
                    if matches!(mouse.kind, MouseEventKind::Moved) {
                        return;
                    }
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

    fn settings_popup_rect(&self) -> Rect {
        let area = self.screen_rect();
        let popup_w = 56u16.min(area.width.saturating_sub(4));
        let popup_h = 20u16.min(area.height.saturating_sub(2));
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        Rect::new(popup_x, popup_y, popup_w, popup_h)
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    fn settings_tab_at(&self, col: u16, row: u16) -> Option<crate::app::state::SettingsSection> {
        use crate::app::state::SettingsSection;

        let inner = self.settings_inner_rect();
        let tab_y = inner.y + 1;
        if row != tab_y {
            return None;
        }
        let mut x = inner.x;
        for section in SettingsSection::ALL {
            let width = section.label().len() as u16 + 2;
            if col >= x && col < x + width {
                return Some(*section);
            }
            x += width + 1;
        }
        None
    }

    fn settings_content_rect(&self) -> Rect {
        let inner = self.settings_inner_rect();
        let y = inner.y + 3;
        Rect::new(inner.x, y, inner.width, inner.y + inner.height - y)
    }

    fn settings_list_index_at(&self, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height {
            return None;
        }

        match self.settings.section {
            crate::app::state::SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.selected >= max_visible {
                    self.settings.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < crate::app::state::THEME_NAMES.len()).then_some(idx)
            }
            crate::app::state::SettingsSection::Sound
            | crate::app::state::SettingsSection::Toast => {
                let list_y = area.y + 2;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
        }
    }

    fn settings_button_at(&self, col: u16, row: u16) -> Option<&'static str> {
        let inner = self.settings_inner_rect();
        let footer_y = inner.y + inner.height.saturating_sub(1);
        if row != footer_y {
            return None;
        }
        let total_w = 7u16 + 2 + 7u16;
        let apply_x = inner.x + inner.width.saturating_sub(total_w) / 2;
        let close_x = apply_x + 9;
        if col >= apply_x && col < apply_x + 7 {
            Some("apply")
        } else if col >= close_x && col < close_x + 7 {
            Some("close")
        } else {
            None
        }
    }

    fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        use crate::app::state::SettingsSection;

        match mouse.kind {
            MouseEventKind::Moved => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    self.settings.section = section;
                    self.settings.selected = match section {
                        SettingsSection::Theme => current_theme_index(&self.theme_name),
                        SettingsSection::Sound => usize::from(!self.sound_enabled()),
                        SettingsSection::Toast => usize::from(!self.toast_config.enabled),
                    };
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.row) {
                    self.settings.selected = idx;
                    if self.settings.section == SettingsSection::Theme {
                        preview_selected_theme(self);
                    }
                }
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    self.settings.section = section;
                    self.settings.selected = match section {
                        SettingsSection::Theme => current_theme_index(&self.theme_name),
                        SettingsSection::Sound => usize::from(!self.sound_enabled()),
                        SettingsSection::Toast => usize::from(!self.toast_config.enabled),
                    };
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.row) {
                    self.settings.selected = idx;
                    return match self.settings.section {
                        SettingsSection::Theme => {
                            preview_selected_theme(self);
                            None
                        }
                        SettingsSection::Sound => {
                            let enabled = idx == 0;
                            self.sound.enabled = enabled;
                            Some(SettingsAction::SaveSound(enabled))
                        }
                        SettingsSection::Toast => {
                            let enabled = idx == 0;
                            self.toast_config.enabled = enabled;
                            Some(SettingsAction::SaveToast(enabled))
                        }
                    };
                }
                match self.settings_button_at(mouse.column, mouse.row) {
                    Some("apply") => apply_settings(self),
                    Some("close") => {
                        cancel_settings(self);
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        if self.mode == Mode::Onboarding {
            self.handle_onboarding_mouse(mouse);
            return None;
        }

        if self.mode == Mode::Settings {
            return self.handle_settings_mouse(mouse);
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
                    match self.confirm_close_button_at(mouse.column, mouse.row) {
                        Some(true) => confirm_close_accept(self),
                        Some(false) => confirm_close_cancel(self),
                        None => confirm_close_cancel(self),
                    }
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
                    self.sidebar_width_auto = false;
                    self.set_manual_sidebar_width(mouse.column);
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

                    if let Some((pane_id, offset_from_bottom)) =
                        self.scrollbar_target_at(mouse.column, mouse.row)
                    {
                        self.focus_pane(pane_id);
                        self.set_pane_scroll_offset(pane_id, offset_from_bottom);
                        self.drag = Some(DragState {
                            target: DragTarget::PaneScrollbar { pane_id },
                        });
                        if self.mode != Mode::Terminal {
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }
                }

                if in_sidebar {
                    if self.sidebar_collapsed {
                        let idx = (mouse.row - sidebar.y) as usize;
                        if idx < self.workspaces.len() {
                            self.switch_workspace(idx);
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }

                    let total_h = sidebar.height as usize;
                    let ws_h = (total_h + 1) / 2;
                    let ws_bottom = sidebar.y + ws_h as u16;
                    let new_row = ws_bottom.saturating_sub(1);
                    if mouse.row == new_row {
                        self.request_new_workspace = true;
                        return None;
                    }

                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.switch_workspace(idx);
                        self.mode = Mode::Terminal;
                        return None;
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
                } else if let Some(info) = self.view.pane_infos.iter().find(|p| {
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

            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &self.drag {
                    match &drag.target {
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
                            }
                        }
                        DragTarget::PaneScrollbar { pane_id } => {
                            if let Some(offset_from_bottom) =
                                self.scrollbar_offset_for_pane_row(*pane_id, mouse.row)
                            {
                                self.set_pane_scroll_offset(*pane_id, offset_from_bottom);
                            }
                        }
                        DragTarget::SidebarDivider => {
                            self.sidebar_width_auto = false;
                            self.set_manual_sidebar_width(mouse.column);
                        }
                    }
                } else if let Some(sel) = &mut self.selection {
                    sel.drag(mouse.column, mouse.row);
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                if self.drag.take().is_some() {
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
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    self.scroll_pane_up(info.id, 3);
                } else if let Some(info) = self.pane_frame_at(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    self.scroll_pane_up(info.id, 3);
                } else if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                    if let Some(rt) = ws.focused_runtime() {
                        rt.scroll_up(3);
                    }
                }
            }
            MouseEventKind::ScrollDown if !in_sidebar => {
                self.selection = None;
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    self.scroll_pane_down(info.id, 3);
                } else if let Some(info) = self.pane_frame_at(mouse.column, mouse.row).cloned() {
                    self.focus_pane(info.id);
                    self.scroll_pane_down(info.id, 3);
                } else if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
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

            MouseEventKind::Moved if self.mode == Mode::ConfirmClose => {
                if let Some(selected_confirm) =
                    self.confirm_close_button_at(mouse.column, mouse.row)
                {
                    self.confirm_close_selected_confirm = selected_confirm;
                }
            }

            MouseEventKind::Moved if self.mode == Mode::ContextMenu => {
                if let Some(idx) = self.context_menu_item_at(mouse.column, mouse.row) {
                    if let Some(menu) = &mut self.context_menu {
                        menu.selected = idx;
                    }
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
                if let Some(idx) = self.workspace_at_row(mouse.row) {
                    self.selected = idx;
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Workspace { ws_idx: idx },
                        x: mouse.column,
                        y: mouse.row,
                        selected: 0,
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right) if !in_sidebar => {
                if self.pane_at(mouse.column, mouse.row).is_some() {
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Pane,
                        x: mouse.column,
                        y: mouse.row,
                        selected: 0,
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            _ => {}
        }

        None
    }

    fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        if self.sidebar_collapsed {
            return false;
        }
        let sidebar = self.view.sidebar_rect;
        sidebar.width > 0
            && col == sidebar.x + sidebar.width.saturating_sub(1)
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
    }

    fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width =
            width.clamp(crate::ui::MIN_SIDEBAR_WIDTH, crate::ui::MAX_SIDEBAR_WIDTH);
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
        let area = self.view.terminal_area;
        let popup_w = 44u16.min(area.width.saturating_sub(4));
        let popup_h = 6u16.min(area.height.max(1));
        let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
        Rect::new(popup_x, popup_y, popup_w, popup_h)
    }

    fn confirm_close_button_at(&self, col: u16, row: u16) -> Option<bool> {
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
        if row != y {
            return None;
        }
        if col >= x && col < x + confirm_w {
            Some(true)
        } else if col >= x + confirm_w + gap && col < x + total_w {
            Some(false)
        } else {
            None
        }
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

    fn pane_frame_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.rect.x
                && col < p.rect.x + p.rect.width
                && row >= p.rect.y
                && row < p.rect.y + p.rect.height
        })
    }

    fn focus_pane(&mut self, pane_id: crate::layout::PaneId) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if ws.layout.focused() != pane_id {
                ws.layout.focus_pane(pane_id);
            }
        }
    }

    fn scroll_pane_up(&self, pane_id: crate::layout::PaneId, lines: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.scroll_up(lines);
            }
        }
    }

    fn scroll_pane_down(&self, pane_id: crate::layout::PaneId, lines: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.scroll_down(lines);
            }
        }
    }

    fn set_pane_scroll_offset(&self, pane_id: crate::layout::PaneId, offset_from_bottom: usize) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if let Some(rt) = ws.runtimes.get(&pane_id) {
                rt.set_scroll_offset_from_bottom(offset_from_bottom);
            }
        }
    }

    fn scrollbar_target_at(&self, col: u16, row: u16) -> Option<(crate::layout::PaneId, usize)> {
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
        Some((
            info.id,
            crate::ui::scrollbar_offset_from_row(metrics, track, row),
        ))
    }

    fn scrollbar_offset_for_pane_row(
        &self,
        pane_id: crate::layout::PaneId,
        row: u16,
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
        Some(crate::ui::scrollbar_offset_from_row(metrics, track, row))
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
    use crate::{config::Config, workspace::Workspace};
    use crossterm::event::{KeyModifiers, MouseEvent};
    use ratatui::layout::Rect;

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

    fn app_for_mouse_test() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.mode = Mode::Terminal;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);
        app
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
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

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert!(!app.state.sidebar_width_auto);
        assert_eq!(app.state.sidebar_width, 31);
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_auto_width() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_width_auto = false;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert!(app.state.sidebar_width_auto);
        assert!(app.state.drag.is_none());
    }
}
