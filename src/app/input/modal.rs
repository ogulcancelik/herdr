use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Direction, Rect};

use crate::{
    app::state::{key_matches, AppState, ContextMenuKind, ContextMenuState, MenuListState, Mode},
    layout::NavDirection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModalAction {
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
pub(super) enum ModalKeyBinding {
    Enter,
    Esc,
    CtrlC,
}

impl ModalKeyBinding {
    fn matches(self, key: &KeyEvent) -> bool {
        match self {
            Self::Enter => key.code == KeyCode::Enter,
            Self::Esc => key.code == KeyCode::Esc,
            Self::CtrlC => {
                key.code == KeyCode::Char('c')
                    && key.modifiers == crossterm::event::KeyModifiers::CONTROL
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ModalActionSpec<A> {
    pub action: A,
    pub bindings: &'static [ModalKeyBinding],
}

pub(super) fn modal_action_from_key<A: Copy>(
    key: &KeyEvent,
    specs: &[ModalActionSpec<A>],
) -> Option<A> {
    specs
        .iter()
        .find(|spec| spec.bindings.iter().any(|binding| binding.matches(key)))
        .map(|spec| spec.action)
}

pub(super) fn modal_action_from_buttons<A: Copy>(
    col: u16,
    row: u16,
    buttons: &[(Rect, A)],
) -> Option<A> {
    buttons.iter().find_map(|(rect, action)| {
        (col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height)
            .then_some(*action)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalMenuAction {
    Quit,
    WhatsNew,
    Keybinds,
    ReloadKeybinds,
    Settings,
}

pub(super) fn global_menu_actions(state: &AppState) -> Vec<GlobalMenuAction> {
    let mut actions = vec![
        GlobalMenuAction::Settings,
        GlobalMenuAction::Keybinds,
        GlobalMenuAction::ReloadKeybinds,
    ];
    if state.update_available.is_some() || state.latest_release_notes_available {
        actions.push(GlobalMenuAction::WhatsNew);
    }
    actions.push(GlobalMenuAction::Quit);
    actions
}

pub(super) fn open_global_menu(state: &mut AppState) {
    state.global_menu = MenuListState::new(0);
    state.mode = Mode::GlobalMenu;
}

pub(super) fn open_keybind_help(state: &mut AppState) {
    state.keybind_help.scroll = 0;
    state.mode = Mode::KeybindHelp;
}

fn open_update_release_notes(state: &mut AppState) {
    let Some(notes) = crate::release_notes::load_latest() else {
        return;
    };

    state.release_notes = Some(crate::app::state::ReleaseNotesState {
        version: notes.version,
        body: notes.body,
        scroll: 0,
        preview: notes.preview,
    });
    state.mode = Mode::ReleaseNotes;
}

pub(super) fn request_quit_or_detach(state: &mut AppState) {
    if state.quit_detaches {
        state.detach_requested = true;
    } else {
        state.should_quit = true;
    }
}

pub(super) fn apply_global_menu_action(state: &mut AppState, action: GlobalMenuAction) {
    match action {
        GlobalMenuAction::Quit => request_quit_or_detach(state),
        GlobalMenuAction::WhatsNew => open_update_release_notes(state),
        GlobalMenuAction::Keybinds => open_keybind_help(state),
        GlobalMenuAction::ReloadKeybinds => {
            state.request_reload_keybinds = true;
            leave_modal(state);
        }
        GlobalMenuAction::Settings => super::settings::open_settings(state),
    }
}

pub(crate) fn handle_global_menu_key(state: &mut AppState, key: KeyEvent) {
    let actions = global_menu_actions(state);
    match key.code {
        KeyCode::Esc => leave_modal(state),
        KeyCode::Up | KeyCode::Char('k') => state.global_menu.move_prev(),
        KeyCode::Down | KeyCode::Char('j') => state.global_menu.move_next(actions.len()),
        KeyCode::Enter => {
            if let Some(action) = actions.get(state.global_menu.highlighted).copied() {
                apply_global_menu_action(state, action);
            }
        }
        _ => {}
    }
}

pub(crate) fn handle_keybind_help_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.scroll_keybind_help(-1),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_keybind_help(1),
        KeyCode::PageUp => state.scroll_keybind_help(-8),
        KeyCode::PageDown => state.scroll_keybind_help(8),
        KeyCode::Home => state.keybind_help.scroll = 0,
        KeyCode::End => state.keybind_help.scroll = state.keybind_help_max_scroll(),
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => leave_modal(state),
        _ => {}
    }
}

pub(super) fn open_rename_workspace(state: &mut AppState, ws_idx: usize) {
    state.selected = ws_idx;
    state.name_input = state.workspaces[ws_idx].display_name();
    state.name_input_replace_on_type = false;
    state.mode = Mode::RenameWorkspace;
}

pub(super) fn open_rename_active_tab(state: &mut AppState, replace_on_type: bool) {
    state.creating_new_tab = false;
    state.requested_new_tab_name = None;
    if let Some(ws) = state.active.and_then(|i| state.workspaces.get(i)) {
        if let Some(name) = ws.active_tab_display_name() {
            state.name_input = name;
            state.name_input_replace_on_type = replace_on_type;
            state.mode = Mode::RenameTab;
        }
    }
}

fn next_new_tab_default_name(state: &AppState) -> String {
    state
        .active
        .and_then(|i| state.workspaces.get(i))
        .map(|ws| (ws.tabs.len() + 1).to_string())
        .unwrap_or_else(|| "1".to_string())
}

pub(super) fn open_new_tab_dialog(state: &mut AppState) {
    state.creating_new_tab = true;
    state.requested_new_tab_name = None;
    state.name_input = next_new_tab_default_name(state);
    state.name_input_replace_on_type = true;
    state.mode = Mode::RenameTab;
}

pub(super) fn leave_modal(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    } else {
        state.mode = Mode::Navigate;
    }
}

pub(super) const ONBOARDING_WELCOME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Continue,
    bindings: &[ModalKeyBinding::Enter],
}];

pub(super) const ONBOARDING_NOTIFICATION_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Back,
        bindings: &[ModalKeyBinding::Esc],
    },
    ModalActionSpec {
        action: ModalAction::Save,
        bindings: &[ModalKeyBinding::Enter],
    },
];

pub(super) const RELEASE_NOTES_ACTIONS: &[ModalActionSpec<ModalAction>] = &[ModalActionSpec {
    action: ModalAction::Close,
    bindings: &[ModalKeyBinding::Enter, ModalKeyBinding::Esc],
}];

pub(super) const RENAME_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
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
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const CONFIRM_CLOSE_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Confirm,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Cancel,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) const SETTINGS_ACTIONS: &[ModalActionSpec<ModalAction>] = &[
    ModalActionSpec {
        action: ModalAction::Apply,
        bindings: &[ModalKeyBinding::Enter],
    },
    ModalActionSpec {
        action: ModalAction::Close,
        bindings: &[ModalKeyBinding::Esc],
    },
];

pub(super) fn apply_rename_action(state: &mut AppState, action: ModalAction) {
    match action {
        ModalAction::Save => {
            let new_name = if state.name_input.trim().is_empty() {
                state.name_input.clone()
            } else {
                state.name_input.trim().to_string()
            };
            match state.mode {
                Mode::RenameWorkspace if !state.workspaces.is_empty() => {
                    if !new_name.is_empty() {
                        state.workspaces[state.selected].set_custom_name(new_name);
                        state.mark_session_dirty();
                    }
                }
                Mode::RenameTab if state.creating_new_tab => {
                    state.request_new_tab = true;
                    let default_name = next_new_tab_default_name(state);
                    state.requested_new_tab_name =
                        if new_name.is_empty() || new_name == default_name {
                            None
                        } else {
                            Some(new_name)
                        };
                }
                Mode::RenameTab => {
                    if let Some(ws) = state.active.and_then(|i| state.workspaces.get_mut(i)) {
                        if let Some(tab) = ws.active_tab_mut() {
                            let keep_auto_name =
                                tab.is_auto_named() && new_name == tab.number.to_string();
                            if !new_name.is_empty() && !keep_auto_name {
                                tab.set_custom_name(new_name);
                                state.mark_session_dirty();
                            }
                        }
                    }
                }
                _ => {}
            }
            state.creating_new_tab = false;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        ModalAction::Clear => {
            state.name_input.clear();
            state.name_input_replace_on_type = false;
        }
        ModalAction::Cancel => {
            state.creating_new_tab = false;
            state.requested_new_tab_name = None;
            state.name_input.clear();
            state.name_input_replace_on_type = false;
            leave_modal(state);
        }
        _ => {}
    }
}

pub(crate) fn handle_rename_key(state: &mut AppState, key: KeyEvent) {
    if let Some(action) = modal_action_from_key(&key, RENAME_ACTIONS) {
        apply_rename_action(state, action);
        return;
    }

    match key.code {
        KeyCode::Backspace => {
            if state.name_input_replace_on_type {
                state.name_input.clear();
                state.name_input_replace_on_type = false;
            } else {
                state.name_input.pop();
            }
        }
        KeyCode::Char(c) => {
            if state.name_input_replace_on_type {
                state.name_input.clear();
                state.name_input_replace_on_type = false;
            }
            state.name_input.push(c);
        }
        _ => {}
    }
}

pub(crate) fn handle_resize_key(state: &mut AppState, key: KeyEvent) {
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

pub(super) fn open_confirm_close(state: &mut AppState) {
    state.mode = Mode::ConfirmClose;
}

pub(super) fn confirm_close_accept(state: &mut AppState) {
    state.close_selected_workspace();
    if state.workspaces.is_empty() {
        state.mode = Mode::Navigate;
    } else {
        state.mode = Mode::Terminal;
    }
}

pub(super) fn confirm_close_cancel(state: &mut AppState) {
    state.mode = Mode::Navigate;
}

pub(crate) fn handle_confirm_close_key(state: &mut AppState, key: KeyEvent) {
    match modal_action_from_key(&key, CONFIRM_CLOSE_ACTIONS) {
        Some(ModalAction::Confirm) => confirm_close_accept(state),
        Some(ModalAction::Cancel) => confirm_close_cancel(state),
        _ => {}
    }
}

pub(super) fn apply_context_menu_action(state: &mut AppState, menu: ContextMenuState, idx: usize) {
    let item = menu.items().get(idx).copied();
    match (menu.kind, item) {
        (ContextMenuKind::Workspace { ws_idx }, Some("Rename")) => {
            open_rename_workspace(state, ws_idx);
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
            open_new_tab_dialog(state);
        }
        (ContextMenuKind::Tab { ws_idx, tab_idx }, Some("Rename")) => {
            state.selected = ws_idx;
            state.active = Some(ws_idx);
            state.switch_tab(tab_idx);
            open_rename_active_tab(state, false);
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

pub(crate) fn handle_context_menu_key(state: &mut AppState, key: KeyEvent) {
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

impl AppState {
    pub(super) fn global_menu_item_at(&self, col: u16, row: u16) -> Option<GlobalMenuAction> {
        let rect = self.global_menu_rect();
        if col <= rect.x
            || col >= rect.x + rect.width.saturating_sub(1)
            || row <= rect.y
            || row >= rect.y + rect.height.saturating_sub(1)
        {
            return None;
        }
        let idx = (row - rect.y - 1) as usize;
        global_menu_actions(self).get(idx).copied()
    }
}
