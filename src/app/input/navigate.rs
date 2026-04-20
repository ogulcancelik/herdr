use std::process::{Command, Stdio};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Direction;

use crate::{
    app::{
        state::{key_matches, AppState, Mode},
        App,
    },
    layout::NavDirection,
};

pub(crate) fn terminal_direct_navigation_action(
    state: &AppState,
    key: &KeyEvent,
) -> Option<NavigateAction> {
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
    pub(super) fn handle_navigate_key(&mut self, key: KeyEvent) {
        self.state.update_dismissed = true;

        if self.state.is_prefix(&key) || key.code == KeyCode::Esc {
            leave_navigate_mode(&mut self.state);
            return;
        }

        if let Some(action) = navigate_action_for_key(&self.state, &key) {
            execute_navigate_action(&mut self.state, action);
            return;
        }

        if handle_navigate_reserved_key(&mut self.state, key) {
            return;
        }

        if let Some(binding) = navigate_custom_command_for_key(&self.state, &key) {
            self.launch_custom_command(binding);
        }
    }

    fn launch_custom_command(&mut self, binding: crate::config::CustomCommandKeybind) {
        let previous_toast = self.state.toast.clone();
        let result = match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(&binding),
            crate::config::CustomCommandAction::Pane => self.spawn_pane_command(&binding.command),
        };
        match result {
            Ok(()) => leave_navigate_mode(&mut self.state),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "custom command failed".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
            }
        }
    }

    fn custom_command_env(&self) -> (Vec<(String, String)>, Option<std::path::PathBuf>) {
        let mut env = vec![(
            crate::api::SOCKET_PATH_ENV_VAR.to_string(),
            crate::api::socket_path().display().to_string(),
        )];
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }

        let mut cwd = None;
        if let Some(ws_idx) = self.state.active {
            env.push((
                "HERDR_ACTIVE_WORKSPACE_ID".to_string(),
                self.public_workspace_id(ws_idx),
            ));
            if let Some(workspace) = self.state.workspaces.get(ws_idx) {
                let tab_idx = workspace.active_tab_index();
                if let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) {
                    env.push(("HERDR_ACTIVE_TAB_ID".to_string(), tab_id));
                }
                if let Some(pane_id) = workspace.focused_pane_id() {
                    if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                        env.push(("HERDR_ACTIVE_PANE_ID".to_string(), public_pane_id));
                    }
                    if let Some(pane_cwd) = workspace
                        .active_tab()
                        .and_then(|tab| tab.cwd_for_pane(pane_id))
                    {
                        env.push((
                            "HERDR_ACTIVE_PANE_CWD".to_string(),
                            pane_cwd.display().to_string(),
                        ));
                        if pane_cwd.is_dir() {
                            cwd = Some(pane_cwd);
                        }
                    }
                }
            }
        }
        (env, cwd)
    }

    fn spawn_custom_command(
        &self,
        binding: &crate::config::CustomCommandKeybind,
    ) -> std::io::Result<()> {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-lc")
            .arg(&binding.command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (env, cwd) = self.custom_command_env();
        command.envs(env);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command.spawn()?;
        Ok(())
    }

    fn spawn_pane_command(&mut self, command: &str) -> std::io::Result<()> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);
        let (env, _) = self.custom_command_env();

        let ws = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let tab_idx = ws.active_tab_index();
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
        let cwd = ws
            .active_tab()
            .and_then(|tab| tab.cwd_for_pane(previous_focus));
        let new_pane_id = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            &env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        )?;
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .layout
            .focus_pane(new_pane_id);
        ws.active_tab_mut()
            .expect("workspace must have an active tab")
            .zoomed = true;
        self.overlay_panes.insert(
            new_pane_id,
            super::super::OverlayPaneState {
                ws_idx,
                tab_idx,
                previous_focus,
                previous_zoomed,
            },
        );
        self.state.mode = Mode::Terminal;
        Ok(())
    }
}

fn navigate_custom_command_for_key(
    state: &AppState,
    key: &KeyEvent,
) -> Option<crate::config::CustomCommandKeybind> {
    state
        .keybinds
        .custom_commands
        .iter()
        .find(|binding| key_matches(key, binding.key.0, binding.key.1))
        .cloned()
}

pub(super) fn handle_navigate_reserved_key(state: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') => {
            super::request_quit_or_detach(state);
            leave_navigate_mode(state);
            true
        }
        KeyCode::Enter => {
            if !state.workspaces.is_empty() {
                state.switch_workspace(state.selected);
                leave_navigate_mode(state);
            }
            true
        }
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as usize) - ('1' as usize);
            if idx < state.workspaces.len() {
                state.switch_workspace(idx);
                leave_navigate_mode(state);
            }
            true
        }
        KeyCode::Char('s') => {
            super::open_settings(state);
            true
        }
        KeyCode::Char('?') => {
            super::open_keybind_help(state);
            true
        }
        KeyCode::Up => {
            if state.selected > 0 {
                state.selected -= 1;
                state.ensure_workspace_visible(state.selected);
            }
            true
        }
        KeyCode::Down => {
            if !state.workspaces.is_empty() && state.selected < state.workspaces.len() - 1 {
                state.selected += 1;
                state.ensure_workspace_visible(state.selected);
            }
            true
        }
        KeyCode::Char('h') | KeyCode::Left => {
            state.navigate_pane(NavDirection::Left);
            true
        }
        KeyCode::Char('j') => {
            state.navigate_pane(NavDirection::Down);
            true
        }
        KeyCode::Char('k') => {
            state.navigate_pane(NavDirection::Up);
            true
        }
        KeyCode::Char('l') | KeyCode::Right => {
            state.navigate_pane(NavDirection::Right);
            true
        }
        KeyCode::Tab => {
            state.cycle_pane(false);
            true
        }
        KeyCode::BackTab => {
            state.cycle_pane(true);
            true
        }
        _ => false,
    }
}

#[allow(dead_code)] // exercised in input unit tests; production uses App::handle_navigate_key
pub(crate) fn handle_navigate_key(state: &mut AppState, key: KeyEvent) {
    state.update_dismissed = true;

    if state.is_prefix(&key) || key.code == KeyCode::Esc {
        leave_navigate_mode(state);
        return;
    }

    if let Some(action) = navigate_action_for_key(state, &key) {
        execute_navigate_action(state, action);
        return;
    }

    let _ = handle_navigate_reserved_key(state, key);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigateAction {
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
    Detach,
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
    if kb
        .detach
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::Detach);
    }
    None
}

pub(super) fn execute_navigate_action(state: &mut AppState, action: NavigateAction) {
    match action {
        NavigateAction::NewWorkspace => {
            state.request_new_workspace = true;
            leave_navigate_mode(state);
        }
        NavigateAction::RenameWorkspace => {
            if !state.workspaces.is_empty() {
                super::open_rename_workspace(state, state.selected);
            }
        }
        NavigateAction::CloseWorkspace => {
            if !state.workspaces.is_empty() {
                if state.confirm_close {
                    super::open_confirm_close(state);
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
        NavigateAction::NewTab => super::open_new_tab_dialog(state),
        NavigateAction::RenameTab => super::open_rename_active_tab(state, false),
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
        NavigateAction::Detach => {
            state.detach_requested = true;
            leave_navigate_mode(state);
        }
    }
}

fn leave_navigate_mode(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    }
}
