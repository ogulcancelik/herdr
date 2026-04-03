//! Input handling — translates crossterm key/mouse events into state mutations.

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};

use crate::input::TerminalKey;
use ratatui::layout::{Direction, Rect};
use tracing::{debug, warn};

use crate::layout::{NavDirection, PaneInfo, SplitBorder};
use crate::selection::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollbarClickTarget {
    Thumb { grab_row_offset: u16 },
    Track { offset_from_bottom: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

use super::state::{
    key_matches, AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget, MenuListState,
    Mode,
};
use super::App;

// ---------------------------------------------------------------------------
// Key handling
// ---------------------------------------------------------------------------

fn is_modifier_only_key(code: &KeyCode) -> bool {
    matches!(code, KeyCode::Modifier(_))
}

fn terminal_direct_navigation_action(state: &AppState, key: &KeyEvent) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    if kb
        .previous_workspace
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousWorkspace);
    }
    if kb
        .next_workspace
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextWorkspace);
    }
    if kb
        .previous_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousTab);
    }
    if kb
        .next_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextTab);
    }
    if kb
        .focus_pane_left
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::FocusPaneLeft);
    }
    if kb
        .focus_pane_down
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::FocusPaneDown);
    }
    if kb
        .focus_pane_up
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::FocusPaneUp);
    }
    if kb
        .focus_pane_right
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::FocusPaneRight);
    }
    None
}

impl App {
    pub(super) async fn handle_key(&mut self, key: TerminalKey) {
        match self.state.mode {
            Mode::Terminal => self.handle_terminal_key(key).await,
            _ => {
                let key = key.as_key_event();
                match self.state.mode {
                    Mode::Onboarding => self.handle_onboarding_key(key),
                    Mode::ReleaseNotes => self.handle_release_notes_key(key),
                    Mode::Navigate => handle_navigate_key(&mut self.state, key),
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
                KeyCode::Right | KeyCode::Char('l') => {
                    self.state.onboarding_step = 1;
                }
                KeyCode::Char('q') => self.state.should_quit = true,
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
                KeyCode::Char('q') => self.state.should_quit = true,
                _ => match modal_action_from_key(&key, ONBOARDING_NOTIFICATION_ACTIONS) {
                    Some(ModalAction::Back) => self.state.onboarding_step = 0,
                    Some(ModalAction::Save) => self.complete_onboarding(),
                    _ => {}
                },
            },
        }
    }

    fn handle_release_notes_key(&mut self, key: KeyEvent) {
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
            return;
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

        if let Some(action) = terminal_direct_navigation_action(&self.state, &key_event) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                action = ?action,
                "intercepted terminal direct navigation key before forwarding to pane"
            );
            execute_navigate_action(&mut self.state, action);
            return;
        }

        if self.state.is_prefix(&key_event) {
            self.state.mode = Mode::Navigate;
            return;
        }

        if is_modifier_only_key(&key_event.code) {
            debug!(
                code = ?key_event.code,
                modifiers = ?key_event.modifiers,
                kind = ?key_event.kind,
                "dropping modifier-only terminal key event instead of forwarding it to pane"
            );
            return;
        }

        if let Some(ws) = self.state.active.and_then(|i| self.state.workspaces.get(i)) {
            if let Some(rt) = ws.focused_runtime() {
                rt.scroll_reset();
                let flags = rt
                    .kitty_keyboard_flags
                    .load(std::sync::atomic::Ordering::Relaxed);
                let protocol = crate::input::KeyboardProtocol::from_kitty_flags(flags);
                let bytes = crate::input::encode_terminal_key(key, protocol);
                if matches!(key_event.code, KeyCode::Esc)
                    || key_event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::ALT)
                {
                    debug!(
                        code = ?key_event.code,
                        modifiers = ?key_event.modifiers,
                        kind = ?key_event.kind,
                        protocol = ?protocol,
                        encoded = ?bytes,
                        "forwarding potentially-ambiguous terminal key to pane"
                    );
                }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalAction {
    Continue,
    Back,
    Save,
    Clear,
    Cancel,
    Confirm,
    Apply,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalKeyBinding {
    Enter,
    Esc,
    EscOrQ,
    CtrlC,
}

impl ModalKeyBinding {
    fn matches(self, key: &KeyEvent) -> bool {
        match self {
            Self::Enter => key.code == KeyCode::Enter,
            Self::Esc => key.code == KeyCode::Esc,
            Self::EscOrQ => key.code == KeyCode::Esc || key.code == KeyCode::Char('q'),
            Self::CtrlC => {
                key.code == KeyCode::Char('c')
                    && key.modifiers == crossterm::event::KeyModifiers::CONTROL
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ModalActionSpec<A> {
    action: A,
    bindings: &'static [ModalKeyBinding],
}

fn modal_action_from_key<A: Copy>(key: &KeyEvent, specs: &[ModalActionSpec<A>]) -> Option<A> {
    specs
        .iter()
        .find(|spec| spec.bindings.iter().any(|binding| binding.matches(key)))
        .map(|spec| spec.action)
}

fn modal_action_from_buttons<A: Copy>(col: u16, row: u16, buttons: &[(Rect, A)]) -> Option<A> {
    buttons.iter().find_map(|(rect, action)| {
        (col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height)
            .then_some(*action)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalMenuAction {
    Keybinds,
    Settings,
}

impl GlobalMenuAction {
    const ALL: [Self; 2] = [Self::Keybinds, Self::Settings];
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

    let name = THEME_NAMES[state.settings.list.selected];
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
                let previous = state.settings.list.selected;
                state.settings.list.move_prev();
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let previous = state.settings.list.selected;
                state
                    .settings
                    .list
                    .move_next(crate::app::state::THEME_NAMES.len());
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            _ => match modal_action_from_key(&key, SETTINGS_ACTIONS) {
                Some(ModalAction::Apply) => return apply_settings(state),
                Some(ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
        SettingsSection::Sound => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                state.sound.enabled = enabled;
                return Some(SettingsAction::SaveSound(enabled));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = usize::from(!state.toast_config.enabled);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            _ => match modal_action_from_key(&key, SETTINGS_ACTIONS) {
                Some(ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
        SettingsSection::Toast => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                state.toast_config.enabled = enabled;
                return Some(SettingsAction::SaveToast(enabled));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            _ => match modal_action_from_key(&key, SETTINGS_ACTIONS) {
                Some(ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
    }

    None
}

fn open_global_menu(state: &mut AppState) {
    state.global_menu = MenuListState::new(0);
    state.mode = Mode::GlobalMenu;
}

fn open_keybind_help(state: &mut AppState) {
    state.keybind_help.scroll = 0;
    state.mode = Mode::KeybindHelp;
}

fn apply_global_menu_action(state: &mut AppState, action: GlobalMenuAction) {
    match action {
        GlobalMenuAction::Keybinds => open_keybind_help(state),
        GlobalMenuAction::Settings => open_settings(state),
    }
}

fn handle_global_menu_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => leave_modal(state),
        KeyCode::Up | KeyCode::Char('k') => state.global_menu.move_prev(),
        KeyCode::Down | KeyCode::Char('j') => {
            state.global_menu.move_next(GlobalMenuAction::ALL.len())
        }
        KeyCode::Enter => {
            let action = GlobalMenuAction::ALL[state.global_menu.highlighted];
            apply_global_menu_action(state, action);
        }
        _ => {}
    }
}

fn handle_keybind_help_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.scroll_keybind_help(-1),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_keybind_help(1),
        KeyCode::PageUp => state.scroll_keybind_help(-8),
        KeyCode::PageDown => state.scroll_keybind_help(8),
        KeyCode::Home => state.keybind_help.scroll = 0,
        KeyCode::End => state.keybind_help.scroll = state.keybind_help_max_scroll(),
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
            leave_modal(state)
        }
        _ => {}
    }
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
        KeyCode::Char('?') => open_keybind_help(state),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigateAction {
    NewWorkspace,
    RenameWorkspace,
    CloseWorkspace,
    PreviousWorkspace,
    NextWorkspace,
    NewTab,
    RenameTab,
    PreviousTab,
    NextTab,
    CloseTab,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
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
    if kb
        .previous_workspace
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousWorkspace);
    }
    if kb
        .next_workspace
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextWorkspace);
    }
    if key_matches(key, kb.new_tab.0, kb.new_tab.1) {
        return Some(NavigateAction::NewTab);
    }
    if kb
        .rename_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::RenameTab);
    }
    if kb
        .previous_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousTab);
    }
    if kb
        .next_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextTab);
    }
    if kb
        .close_tab
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::CloseTab);
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
                state.mode = Mode::RenameWorkspace;
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
        NavigateAction::PreviousWorkspace => {
            state.previous_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::NextWorkspace => {
            state.next_workspace();
            leave_navigate_mode(state);
        }
        NavigateAction::NewTab => {
            state.request_new_tab = true;
            leave_navigate_mode(state);
        }
        NavigateAction::RenameTab => {
            if let Some(ws) = state.active.and_then(|i| state.workspaces.get(i)) {
                if let Some(name) = ws.active_tab_display_name() {
                    state.name_input = name;
                    state.mode = Mode::RenameTab;
                }
            }
        }
        NavigateAction::PreviousTab => {
            state.previous_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::NextTab => {
            state.next_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::CloseTab => {
            state.close_tab();
            leave_navigate_mode(state);
        }
        NavigateAction::FocusPaneLeft => state.navigate_pane(NavDirection::Left),
        NavigateAction::FocusPaneDown => state.navigate_pane(NavDirection::Down),
        NavigateAction::FocusPaneUp => state.navigate_pane(NavDirection::Up),
        NavigateAction::FocusPaneRight => state.navigate_pane(NavDirection::Right),
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

const ONBOARDING_WELCOME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Continue,
    bindings: &[ModalKeyBinding::Enter],
}];

const ONBOARDING_NOTIFICATION_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Back,
        bindings: &[ModalKeyBinding::Esc],
    },
    ModalActionSpec {
        action: ModalAction::Save,
        bindings: &[ModalKeyBinding::Enter],
    },
];

const RELEASE_NOTES_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Close,
    bindings: &[ModalKeyBinding::Enter, ModalKeyBinding::EscOrQ],
}];

const RENAME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Save,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Clear,
        bindings: &[ModalKeyBinding::CtrlC],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::EscOrQ],
    },
];

const CONFIRM_CLOSE_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Confirm,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::EscOrQ],
    },
];

const SETTINGS_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Apply,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Close,
        bindings: &[ModalKeyBinding::EscOrQ],
    },
];

fn open_settings(state: &mut AppState) {
    use crate::app::state::SettingsSection;

    // Save current state for cancel
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.section = SettingsSection::Theme;
    state.settings.list.selected = current_theme_index(&state.theme_name);
    state.mode = Mode::Settings;
}

fn apply_rename_action(state: &mut AppState, action: ModalAction) {
    match action {
        ModalAction::Save => {
            let new_name = if state.name_input.trim().is_empty() {
                state.name_input.clone()
            } else {
                state.name_input.trim().to_string()
            };
            if !new_name.is_empty() {
                match state.mode {
                    Mode::RenameWorkspace if !state.workspaces.is_empty() => {
                        state.workspaces[state.selected].set_custom_name(new_name);
                    }
                    Mode::RenameTab => {
                        if let Some(ws) = state.active.and_then(|i| state.workspaces.get_mut(i)) {
                            if let Some(tab) = ws.active_tab_mut() {
                                tab.set_custom_name(new_name);
                            }
                        }
                    }
                    _ => {}
                }
            }
            state.name_input.clear();
            state.mode = Mode::Navigate;
        }
        ModalAction::Clear => state.name_input.clear(),
        ModalAction::Cancel => {
            state.name_input.clear();
            state.mode = Mode::Navigate;
        }
        _ => {}
    }
}

fn handle_rename_key(state: &mut AppState, key: KeyEvent) {
    if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
        apply_rename_action(state, action);
        return;
    }

    match key.code {
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
    match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
        Some(ModalAction::Confirm) => confirm_close_accept(state),
        Some(ModalAction::Cancel) => confirm_close_cancel(state),
        _ => {}
    }
}

fn apply_context_menu_action(state: &mut AppState, menu: ContextMenuState, idx: usize) {
    let item = menu.items().get(idx).copied();
    match (menu.kind, item) {
        (ContextMenuKind::Workspace { ws_idx }, Some("Rename")) => {
            state.selected = ws_idx;
            state.name_input = state.workspaces[ws_idx].display_name();
            state.mode = Mode::RenameWorkspace;
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
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("New tab")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.request_new_tab = true;
            state.mode = Mode::Terminal;
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("Rename")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            if let Some(ws) = state.workspaces.get(ws_idx) {
                if let Some(name) = ws.active_tab_display_name() {
                    state.name_input = name;
                    state.mode = Mode::RenameTab;
                }
            }
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("Close")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            state.close_tab();
            state.mode = if state.active.is_some() {
                Mode::Terminal
            } else {
                Mode::Navigate
            };
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
                menu.list.move_prev();
            }
        }
        KeyCode::Down => {
            if let Some(menu) = &mut state.context_menu {
                menu.list.move_next(menu.items().len());
            }
        }
        KeyCode::Enter => {
            if let Some(menu) = state.context_menu.take() {
                let idx = menu.list.highlighted;
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

    fn release_notes_modal_inner(&self) -> Option<ratatui::layout::Rect> {
        self.onboarding_modal_inner(76, 20)
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

    fn rename_modal_inner(&self) -> Option<Rect> {
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
        let lines = crate::ui::release_notes_lines(&notes.body, &self.palette);

        let rows_for_width = |wrap_width: usize| {
            lines
                .iter()
                .map(|(width, _)| width.max(&1).div_ceil(wrap_width.max(1)))
                .sum::<usize>()
        };

        let full_width = body.width.max(1) as usize;
        let mut total_rows = rows_for_width(full_width);
        let wrap_width = if total_rows > viewport_rows && full_width > 1 {
            body.width.saturating_sub(1).max(1) as usize
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

    fn handle_onboarding_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }

        match self.onboarding_step {
            0 => {
                let Some(inner) = self.onboarding_modal_inner(64, 16) else {
                    return;
                };
                let actions = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1)
                    .actions
                    .unwrap_or_default();
                let button = crate::ui::onboarding_welcome_continue_rect(actions);
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && modal_action_from_buttons(
                        mouse.column,
                        mouse.row,
                        &[(button, ModalAction::Continue)],
                    ) == Some(ModalAction::Continue)
                {
                    self.onboarding_step = 1;
                }
            }
            _ => {
                let Some(inner) = self.onboarding_modal_inner(56, 14) else {
                    return;
                };
                let stack = crate::ui::modal_stack_areas(inner, 3, 0, 1, 1);
                if mouse.row >= stack.content.y && mouse.row < stack.content.y + 4 {
                    self.onboarding_list
                        .select((mouse.row - stack.content.y) as usize);
                    return;
                }

                let (back, save) = crate::ui::onboarding_notification_button_rects(
                    stack.actions.unwrap_or_default(),
                );
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    match modal_action_from_buttons(
                        mouse.column,
                        mouse.row,
                        &[(back, ModalAction::Back), (save, ModalAction::Save)],
                    ) {
                        Some(ModalAction::Back) => self.onboarding_step = 0,
                        Some(ModalAction::Save) => self.request_complete_onboarding = true,
                        _ => {}
                    }
                }
            }
        }
    }

    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        let content = Rect::new(
            sidebar.x,
            sidebar.y,
            sidebar.width.saturating_sub(1),
            sidebar.height,
        );
        let total_h = content.height as usize;
        let ws_h = (total_h + 1) / 2;
        let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
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
        let width = 6u16.min(footer.width.max(1));
        let x = footer.x + footer.width.saturating_sub(width);
        Rect::new(x, footer.y, width, footer.height)
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let menu_w = 14u16.min(screen.width.max(1));
        let menu_h = (GlobalMenuAction::ALL.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher.y.saturating_sub(menu_h);
        Rect::new(x, y, menu_w, menu_h)
    }

    fn global_menu_item_at(&self, col: u16, row: u16) -> Option<GlobalMenuAction> {
        let rect = self.global_menu_rect();
        if col <= rect.x
            || col >= rect.x + rect.width.saturating_sub(1)
            || row <= rect.y
            || row >= rect.y + rect.height.saturating_sub(1)
        {
            return None;
        }
        let idx = (row - rect.y - 1) as usize;
        GlobalMenuAction::ALL.get(idx).copied()
    }

    pub(crate) fn keybind_help_popup_rect(&self) -> Rect {
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

    fn scroll_keybind_help(&mut self, delta: i16) {
        let max_scroll = self.keybind_help_max_scroll();
        let current = self.keybind_help.scroll as i16;
        self.keybind_help.scroll = current.saturating_add(delta).clamp(0, max_scroll as i16) as u16;
    }

    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(self.screen_rect(), 56, 20).unwrap_or_default()
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
        crate::ui::modal_stack_areas(inner, 3, 2, 0, 1).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            crate::app::state::SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
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

    fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        use crate::app::state::SettingsSection;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    self.settings.section = section;
                    self.settings.list.select(match section {
                        SettingsSection::Theme => current_theme_index(&self.theme_name),
                        SettingsSection::Sound => usize::from(!self.sound_enabled()),
                        SettingsSection::Toast => usize::from(!self.toast_config.enabled),
                    });
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    self.settings.list.select(idx);
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

                let inner = self.settings_inner_rect();
                let (apply, close) = crate::ui::settings_button_rects(inner);
                match modal_action_from_buttons(
                    mouse.column,
                    mouse.row,
                    &[(apply, ModalAction::Apply), (close, ModalAction::Close)],
                ) {
                    Some(ModalAction::Apply) => apply_settings(self),
                    Some(ModalAction::Close) => {
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
            let hovered = self
                .global_menu_item_at(mouse.column, mouse.row)
                .and_then(|action| {
                    GlobalMenuAction::ALL
                        .iter()
                        .position(|item| *item == action)
                });
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

                if let Some(tab_idx) = self.tab_at(mouse.column, mouse.row) {
                    self.switch_tab(tab_idx);
                    self.mode = Mode::Terminal;
                    return None;
                }
                if self.on_new_tab_button(mouse.column, mouse.row) {
                    self.request_new_tab = true;
                    self.mode = Mode::Terminal;
                    return None;
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

                    let new_button = self.sidebar_new_button_rect();
                    let on_new_button = mouse.row >= new_button.y
                        && mouse.row < new_button.y + new_button.height
                        && mouse.column >= new_button.x
                        && mouse.column < new_button.x + new_button.width;
                    if on_new_button {
                        self.request_new_workspace = true;
                        return None;
                    }

                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.switch_workspace(idx);
                        self.mode = Mode::Terminal;
                        return None;
                    }

                    if let Some((ws_idx, tab_idx, pane_id)) = self.agent_detail_target_at(mouse.row)
                    {
                        self.switch_workspace(ws_idx);
                        if let Some(ws) = self.workspaces.get_mut(ws_idx) {
                            ws.switch_tab(tab_idx);
                            ws.layout.focus_pane(pane_id);
                        }
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
                            self.sidebar_width_auto = false;
                            self.set_manual_sidebar_width(mouse.column);
                        }
                        DragTarget::ReleaseNotesScrollbar { .. }
                        | DragTarget::KeybindHelpScrollbar { .. } => {}
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

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !in_sidebar => {
                self.selection = None;
                self.handle_terminal_wheel(mouse);
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

            MouseEventKind::Moved if self.mode == Mode::ContextMenu => {
                let hovered = self.context_menu_item_at(mouse.column, mouse.row);
                if let Some(menu) = &mut self.context_menu {
                    menu.list.hover(hovered);
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
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
                if self.pane_at(mouse.column, mouse.row).is_some() {
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
    fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        self.view
            .tab_hit_areas
            .iter()
            .enumerate()
            .find_map(|(idx, area)| {
                (row >= area.y
                    && row < area.y + area.height
                    && col >= area.x
                    && col < area.x + area.width)
                    .then_some(idx)
            })
    }

    fn on_new_tab_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.new_tab_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let footer = self.sidebar_footer_rect();
        if footer == Rect::default() {
            return None;
        }

        if row < self.view.sidebar_rect.y || row >= footer.y {
            return None;
        }

        let mut row_y = self.view.sidebar_rect.y;
        for (i, ws) in self.workspaces.iter().enumerate() {
            let has_branch = ws.branch().is_some();
            let card_h: u16 = if has_branch { 2 } else { 1 };
            if row >= row_y && row < row_y + card_h {
                return Some(i);
            }
            row_y += card_h + 1; // +1 for gap
            if row_y >= footer.y {
                break;
            }
        }
        None
    }

    fn agent_detail_target_at(&self, row: u16) -> Option<(usize, usize, crate::layout::PaneId)> {
        if self.sidebar_collapsed {
            return None;
        }

        let content = Rect::new(
            self.view.sidebar_rect.x,
            self.view.sidebar_rect.y,
            self.view.sidebar_rect.width.saturating_sub(1),
            self.view.sidebar_rect.height,
        );
        if content.width == 0 || content.height == 0 {
            return None;
        }

        let total_h = content.height as usize;
        let ws_h = (total_h + 1) / 2;
        let detail_area = Rect::new(
            content.x,
            content.y + ws_h as u16,
            content.width,
            total_h.saturating_sub(ws_h) as u16,
        );
        if detail_area.height < 4
            || row < detail_area.y + 3
            || row >= detail_area.y + detail_area.height
        {
            return None;
        }

        let detail_ws_idx = if matches!(
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
            self.selected
        } else {
            self.active?
        };

        let ws = self.workspaces.get(detail_ws_idx)?;
        let relative_row = row - (detail_area.y + 3);
        let entry_height = 3;
        if relative_row % entry_height == 2 {
            return None;
        }
        let detail_idx = (relative_row / entry_height) as usize;
        let details = ws.pane_details();
        let detail = details.get(detail_idx)?;
        Some((detail_ws_idx, detail.tab_idx, detail.pane_id))
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

    fn handle_terminal_wheel(&mut self, mouse: MouseEvent) {
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

    fn forward_pane_wheel(&self, info: &PaneInfo, mouse: MouseEvent) -> bool {
        let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) else {
            return false;
        };
        let Some(rt) = ws.runtimes.get(&info.id) else {
            return false;
        };
        let Some(input_state) = rt.input_state() else {
            return false;
        };

        match wheel_routing(input_state) {
            WheelRouting::HostScroll => false,
            WheelRouting::MouseReport => {
                rt.scroll_reset();
                let column = mouse.column.saturating_sub(info.inner_rect.x);
                let row = mouse.row.saturating_sub(info.inner_rect.y);
                let Some(bytes) = crate::input::encode_mouse_scroll(
                    mouse.kind,
                    column,
                    row,
                    mouse.modifiers,
                    input_state.mouse_protocol_encoding,
                ) else {
                    warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
                    return true;
                };
                if let Err(err) = rt.sender.try_send(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
                }
                true
            }
            WheelRouting::AlternateScroll => {
                rt.scroll_reset();
                let key = match mouse.kind {
                    MouseEventKind::ScrollUp => KeyCode::Up,
                    MouseEventKind::ScrollDown => KeyCode::Down,
                    _ => return true,
                };
                let bytes = crate::input::encode_cursor_key(key, input_state.application_cursor);
                if let Err(err) = rt.sender.try_send(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward alternate-scroll key");
                }
                true
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

    fn scrollbar_target_at(
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

    fn scrollbar_offset_for_pane_row(
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

fn wheel_routing(input_state: crate::pane::InputState) -> WheelRouting {
    if input_state.mouse_protocol_mode != vt100::MouseProtocolMode::None {
        WheelRouting::MouseReport
    } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
        WheelRouting::AlternateScroll
    } else {
        WheelRouting::HostScroll
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
    use crate::{config::Config, detect::Agent, workspace::Workspace};
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

        assert_eq!(state.mode, Mode::RenameWorkspace);
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
    fn terminal_direct_focus_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.focus_pane_left = Some((KeyCode::Left, KeyModifiers::ALT));
        state.keybinds.focus_pane_left_label = Some("alt+left".into());

        let action = terminal_direct_navigation_action(
            &state,
            &KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::FocusPaneLeft));
    }

    #[tokio::test]
    async fn terminal_direct_focus_pane_shortcut_switches_focus_without_leaving_terminal_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(Rect::new(0, 0, 80, 24));
        let focused_before = app.state.workspaces[0].layout.focused();
        app.state.keybinds.focus_pane_left = Some((KeyCode::Char('h'), KeyModifiers::ALT));
        app.state.keybinds.focus_pane_left_label = Some("alt+h".into());

        app.handle_terminal_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::ALT))
            .await;

        assert_ne!(app.state.workspaces[0].layout.focused(), focused_before);
        assert_eq!(app.state.mode, Mode::Terminal);
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
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn question_mark_opens_keybind_help_from_navigate() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );

        assert_eq!(state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn rename_modal_keyboard_and_mouse_share_actions() {
        let mut state = state_with_workspaces(&["test"]);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "hello".into();

        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(state.name_input.is_empty());

        state.name_input = "renamed".into();
        handle_rename_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::Navigate);
        assert_eq!(state.workspaces[0].display_name(), "renamed");

        state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        state.view.terminal_area = Rect::new(26, 0, 80, 20);
        state.mode = Mode::RenameWorkspace;
        state.name_input = "mouse".into();
        let inner = state.rename_modal_inner().unwrap();
        let (save, _, _) = crate::ui::rename_button_rects(inner);
        let action = modal_action_from_buttons(save.x, save.y, &[(save, ModalAction::Save)]);
        assert_eq!(action, Some(ModalAction::Save));
    }

    #[test]
    fn confirm_close_keyboard_actions_are_direct_not_focused() {
        let mut state = state_with_workspaces(&["a", "b"]);
        state.mode = Mode::ConfirmClose;
        state.selected = 1;

        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::Navigate);
        assert_eq!(state.workspaces.len(), 2);

        state.mode = Mode::ConfirmClose;
        handle_confirm_close_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::Settings);
    }

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
    fn onboarding_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;
        app.state.onboarding_step = 1;
        app.state.onboarding_list.select(1);

        let inner = app.state.onboarding_modal_inner(56, 14).unwrap();
        let content = crate::ui::modal_stack_areas(inner, 3, 0, 1, 1).content;
        app.handle_mouse(mouse(MouseEventKind::Moved, content.x + 2, content.y));

        assert_eq!(app.state.onboarding_list.selected, 1);
    }

    #[test]
    fn onboarding_click_selects_notification_option() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::Onboarding;
        app.state.onboarding_step = 1;
        app.state.onboarding_list.select(0);

        let inner = app.state.onboarding_modal_inner(56, 14).unwrap();
        let content = crate::ui::modal_stack_areas(inner, 3, 0, 1, 1).content;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 2,
            content.y + 2,
        ));

        assert_eq!(app.state.onboarding_list.selected, 2);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
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
    fn clicking_agent_detail_row_switches_to_correct_tab_and_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let first_pane = ws.tabs[0].root_pane;
        ws.tabs[0]
            .panes
            .get_mut(&first_pane)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_tab = ws.test_add_tab(Some("logs"));
        let second_pane = ws.tabs[second_tab].root_pane;
        ws.tabs[second_tab]
            .panes
            .get_mut(&second_pane)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 16));

        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert_eq!(
            app.state.workspaces[0].tabs[1].layout.focused(),
            second_pane
        );
        assert_eq!(app.state.mode, Mode::Terminal);
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

    #[test]
    fn wheel_routing_prefers_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            mouse_protocol_mode: vt100::MouseProtocolMode::ButtonMotion,
            mouse_protocol_encoding: vt100::MouseProtocolEncoding::Sgr,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::MouseReport);
    }

    #[test]
    fn wheel_routing_uses_alternate_scroll_in_fullscreen_without_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            mouse_protocol_mode: vt100::MouseProtocolMode::None,
            mouse_protocol_encoding: vt100::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::AlternateScroll);
    }

    #[test]
    fn wheel_routing_falls_back_to_host_scrollback() {
        let input_state = crate::pane::InputState {
            alternate_screen: false,
            application_cursor: false,
            mouse_protocol_mode: vt100::MouseProtocolMode::None,
            mouse_protocol_encoding: vt100::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::HostScroll);
    }
}
