use std::{
    fs, io,
    io::Write,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Direction;

use crate::{
    app::{
        state::{key_matches, AppState, Mode},
        App,
    },
    input::TerminalKey,
    layout::NavDirection,
};

pub(crate) fn terminal_direct_navigation_action(
    state: &AppState,
    key: &KeyEvent,
) -> Option<NavigateAction> {
    if let Some(action) = indexed_navigation_action(state, key) {
        return Some(action);
    }

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
        .previous_agent
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousAgent);
    }
    if kb
        .next_agent
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextAgent);
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
    pub(crate) fn handle_navigate_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if self.state.is_prefix(&key) {
            if !self.pass_through_key_to_focused_pane(raw_key) {
                leave_navigate_mode(&mut self.state);
            }
            return;
        }

        if key.code == KeyCode::Esc {
            leave_navigate_mode(&mut self.state);
            return;
        }

        if let Some(action) = navigate_action_for_key(&self.state, &key) {
            if action == NavigateAction::EditScrollback {
                self.launch_focused_scrollback_editor();
            } else {
                execute_navigate_action(&mut self.state, action);
            }
            self.selection_autoscroll_deadline = None;
            return;
        }

        if handle_navigate_reserved_key(&mut self.state, key) {
            return;
        }

        if let Some(binding) = navigate_custom_command_for_key(&self.state, &key) {
            self.launch_custom_command(binding);
        }
    }

    fn pass_through_key_to_focused_pane(&mut self, key: TerminalKey) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(rt) = self.state.focused_runtime_in_workspace(ws_idx) else {
            return false;
        };

        let bytes = rt.encode_terminal_key(key);
        if bytes.is_empty() || rt.try_send_bytes(Bytes::from(bytes)).is_err() {
            return false;
        }

        self.state.mode = Mode::Terminal;
        true
    }

    fn launch_custom_command(&mut self, binding: crate::config::CustomCommandKeybind) {
        let previous_toast = self.state.toast.clone();
        let result = match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(&binding),
            crate::config::CustomCommandAction::Pane => {
                self.spawn_pane_command(&binding.command, Vec::new())
            }
        };
        match result {
            Ok(()) => leave_navigate_mode(&mut self.state),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "custom command failed".to_string(),
                    context: err.to_string(),
                    target: None,
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
                    if let Some(pane_cwd) = workspace.active_tab().and_then(|tab| {
                        tab.cwd_for_pane(
                            pane_id,
                            &self.state.terminals,
                            &self.state.terminal_runtimes,
                        )
                    }) {
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

    fn launch_focused_scrollback_editor(&mut self) {
        let previous_toast = self.state.toast.clone();
        match self.open_focused_scrollback_in_editor() {
            Ok(()) => self.sync_toast_deadline(previous_toast),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "edit scrollback failed".to_string(),
                    context: err.to_string(),
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
            }
        }
    }

    fn open_focused_scrollback_in_editor(&mut self) -> std::io::Result<()> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| std::io::Error::other("no active workspace"))?;
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let pane_id = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let scrollback = self
            .state
            .runtime_for_pane_in_workspace(ws_idx, pane_id)
            .ok_or_else(|| std::io::Error::other("focused pane has no scrollback runtime"))?
            .recent_text(usize::MAX);

        let path = write_scrollback_temp_file(&scrollback)?;

        let quoted_path = shell_quote(&path.display().to_string());
        let command = format!(
            r#"scrollback_file={quoted_path}; eval "${{EDITOR:-vi}} \"\$scrollback_file\""; status=$?; rm -f "$scrollback_file"; exit $status"#
        );
        if let Err(err) = self.spawn_pane_command(&command, vec![path.clone()]) {
            let _ = fs::remove_file(&path);
            return Err(err);
        }

        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::Finished,
                title: "opened scrollback".to_string(),
                context: format!("focused pane {public_pane_id}"),
                target: None,
            });
        }
        Ok(())
    }

    fn spawn_pane_command(
        &mut self,
        command: &str,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
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
        let cwd = ws.active_tab().and_then(|tab| {
            tab.cwd_for_pane(
                previous_focus,
                &self.state.terminals,
                &self.state.terminal_runtimes,
            )
        });
        let new_pane = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            &env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        )?;
        let new_pane_id = new_pane.pane_id;
        self.state
            .terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
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
                temp_files,
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
            super::modal::request_quit_or_detach(state);
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
            super::settings::open_settings(state);
            true
        }
        KeyCode::Char('?') => {
            super::modal::open_keybind_help(state);
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
    SwitchWorkspace(usize),
    SwitchTab(usize),
    FocusAgent(usize),
    PreviousWorkspace,
    NextWorkspace,
    PreviousAgent,
    NextAgent,
    NewTab,
    RenameTab,
    PreviousTab,
    NextTab,
    CloseTab,
    RenamePane,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    EditScrollback,
    Zoom,
    EnterResizeMode,
    ToggleSidebar,
    ReloadConfig,
    OpenNotificationTarget,
    Detach,
}

fn indexed_navigation_action(state: &AppState, key: &KeyEvent) -> Option<NavigateAction> {
    let KeyCode::Char(c @ '1'..='9') = key.code else {
        return None;
    };
    let idx = (c as usize) - ('1' as usize);
    let kb = &state.keybinds;

    if kb
        .indexed_tabs
        .is_some_and(|mods| key_matches(key, KeyCode::Char(c), mods))
    {
        return Some(NavigateAction::SwitchTab(idx));
    }
    if kb
        .indexed_workspaces
        .is_some_and(|mods| key_matches(key, KeyCode::Char(c), mods))
    {
        return Some(NavigateAction::SwitchWorkspace(idx));
    }
    if kb
        .indexed_agents
        .is_some_and(|mods| key_matches(key, KeyCode::Char(c), mods))
    {
        return Some(NavigateAction::FocusAgent(idx));
    }

    None
}

fn navigate_action_for_key(state: &AppState, key: &KeyEvent) -> Option<NavigateAction> {
    if let Some(action) = indexed_navigation_action(state, key) {
        return Some(action);
    }

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
    if kb
        .previous_agent
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::PreviousAgent);
    }
    if kb
        .next_agent
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::NextAgent);
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
    if kb
        .rename_pane
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::RenamePane);
    }
    if kb
        .edit_scrollback
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::EditScrollback);
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
    if key_matches(key, kb.zoom.0, kb.zoom.1) {
        return Some(NavigateAction::Zoom);
    }
    if key_matches(key, kb.resize_mode.0, kb.resize_mode.1) {
        return Some(NavigateAction::EnterResizeMode);
    }
    if key_matches(key, kb.toggle_sidebar.0, kb.toggle_sidebar.1) {
        return Some(NavigateAction::ToggleSidebar);
    }
    if kb
        .reload_config
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::ReloadConfig);
    }
    if kb
        .open_notification_target
        .is_some_and(|(code, mods)| key_matches(key, code, mods))
    {
        return Some(NavigateAction::OpenNotificationTarget);
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
                super::modal::open_rename_workspace(state, state.selected);
            }
        }
        NavigateAction::CloseWorkspace => {
            if !state.workspaces.is_empty() {
                if state.confirm_close {
                    super::modal::open_confirm_close(state);
                } else {
                    state.close_selected_workspace();
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::SwitchWorkspace(idx) => {
            if idx < state.workspaces.len() {
                state.switch_workspace(idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::SwitchTab(idx) => {
            let tab_exists = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .is_some_and(|ws| idx < ws.tabs.len());
            if tab_exists {
                state.switch_tab(idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::FocusAgent(idx) => {
            if state.focus_agent_entry(idx) {
                leave_navigate_mode(state);
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
        NavigateAction::PreviousAgent => {
            state.previous_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NextAgent => {
            state.next_agent();
            leave_navigate_mode(state);
        }
        NavigateAction::NewTab => {
            if state.prompt_new_tab_name {
                super::modal::open_new_tab_dialog(state);
            } else {
                state.request_new_tab = true;
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenameTab => super::modal::open_rename_active_tab(state, false),
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
        NavigateAction::RenamePane => {
            if let Some(pane_id) = state
                .active
                .and_then(|ws_idx| state.workspaces.get(ws_idx))
                .and_then(|ws| ws.focused_pane_id())
            {
                super::modal::open_rename_pane(state, pane_id);
            }
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
        NavigateAction::EditScrollback => {}
        NavigateAction::Zoom => {
            state.toggle_zoom();
            leave_navigate_mode(state);
        }
        NavigateAction::EnterResizeMode => state.mode = Mode::Resize,
        NavigateAction::ToggleSidebar => {
            state.sidebar_collapsed = !state.sidebar_collapsed;
            leave_navigate_mode(state);
        }
        NavigateAction::ReloadConfig => {
            state.request_reload_config = true;
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNotificationTarget => {
            state.focus_toast_target();
            if state.mode == Mode::Navigate {
                leave_navigate_mode(state);
            }
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

fn write_scrollback_temp_file(content: &str) -> io::Result<std::path::PathBuf> {
    let mut last_collision = None;
    for attempt in 0..16 {
        let path = unique_scrollback_path(attempt);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(content.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(err);
            }
            Err(err) => return Err(err),
        }
    }

    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create unique scrollback temp file",
        )
    }))
}

fn unique_scrollback_path(attempt: u32) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "herdr-scrollback-{}-{nanos}-{attempt}.txt",
        std::process::id()
    ))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Direction;

    use super::super::{state_with_workspaces, unique_temp_path, wait_for_file};
    use super::*;
    use crate::{app::App, config::Config, input::TerminalKey, workspace::Workspace};

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
    fn custom_reload_config_key_requests_reload_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.reload_config = Some((KeyCode::Char('g'), KeyModifiers::empty()));
        state.keybinds.reload_config_label = Some("g".into());

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_reload_config);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_open_notification_key_focuses_current_toast_target() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
        state.keybinds.open_notification_target = Some((KeyCode::Char('g'), KeyModifiers::empty()));
        state.keybinds.open_notification_target_label = Some("g".into());
        let target_workspace_id = state.workspaces[1].id.clone();
        let target_pane = state.workspaces[1].tabs[0].root_pane;
        state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".into(),
            context: "two".into(),
            target: Some(crate::app::state::ToastTarget {
                workspace_id: target_workspace_id,
                pane_id: target_pane,
            }),
        });

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(1));
        assert_eq!(state.selected, 1);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert!(state.toast.is_none());
        assert_eq!(state.mode, Mode::Terminal);
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
    fn mobile_workspace_keyboard_navigation_keeps_selected_row_visible() {
        let mut state = state_with_workspaces(&["a", "b", "c", "d"]);
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 44, 8));
        assert_eq!(state.mobile_switcher_scroll, 0);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mobile_switcher_scroll, 1);
    }

    #[test]
    fn terminal_direct_agent_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.next_agent = Some((KeyCode::Char('a'), KeyModifiers::ALT));
        state.keybinds.next_agent_label = Some("alt+a".into());

        let action = terminal_direct_navigation_action(
            &state,
            &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::NextAgent));
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

    #[test]
    fn terminal_direct_indexed_tab_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.indexed_tabs = Some(KeyModifiers::CONTROL);
        state.keybinds.indexed_tabs_label = Some("ctrl+1..9".into());

        let action = terminal_direct_navigation_action(
            &state,
            &KeyEvent::new(KeyCode::Char('3'), KeyModifiers::CONTROL),
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(2)));
    }

    #[tokio::test]
    async fn custom_command_runs_from_prefix_key_in_navigate_mode() {
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

        let output_path = unique_temp_path("custom-command-keybind");
        let command = format!(
            "printf '%s\\n%s\\n%s\\n' \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'",
            output_path.display()
        );
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            key: (KeyCode::Char('g'), KeyModifiers::empty()),
            label: "g".into(),
            command,
            action: crate::config::CustomCommandAction::Shell,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        assert_eq!(app.state.mode, Mode::Navigate);

        app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
            .await;

        let content = wait_for_file(&output_path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], app.state.workspaces[0].id);
        assert_eq!(lines[1], format!("{}:1", app.state.workspaces[0].id));
        assert_eq!(lines[2], format!("{}-1", app.state.workspaces[0].id));
        assert_eq!(app.state.mode, Mode::Terminal);

        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn pane_overlay_command_opens_and_closes_after_exit() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.host_terminal_theme,
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        app.state.workspaces = vec![workspace];
        app.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("custom-pane-command");
        let command = format!("printf done > '{}'", output_path.display());
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            key: (KeyCode::Char('g'), KeyModifiers::empty()),
            label: "g".into(),
            command,
            action: crate::config::CustomCommandAction::Pane,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.state.terminal_runtimes.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);

        let _ = wait_for_file(&output_path);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if app.drain_internal_events()
                && app.state.workspaces[0].tabs[0].layout.pane_count() == 1
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
        assert_eq!(app.state.mode, Mode::Terminal);
        let _ = std::fs::remove_file(output_path);
    }

    #[tokio::test]
    async fn edit_scrollback_key_opens_focused_runtime_scrollback_in_editor_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            crate::pane::PaneRuntime::test_with_scrollback_bytes(20, 5, 4096, b"alpha\nbeta\n"),
        );
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        let output_path = unique_temp_path("edit-scrollback");
        let previous_editor = std::env::var_os("EDITOR");
        std::env::set_var(
            "EDITOR",
            format!("sh -c 'cp \"$1\" {}' sh", output_path.display()),
        );
        app.state.keybinds.edit_scrollback = Some((KeyCode::Char('g'), KeyModifiers::empty()));
        app.state.keybinds.edit_scrollback_label = Some("g".into());

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()));

        match previous_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        let content = wait_for_file(&output_path);
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert_eq!(app.state.mode, Mode::Terminal);

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn zoom_action_exits_navigate_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.keybinds.zoom = (KeyCode::Char('g'), KeyModifiers::empty());
        state.keybinds.zoom_label = "g".into();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn focus_pane_action_keeps_zoomed_when_changing_focus() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        execute_navigate_action(&mut state, NavigateAction::FocusPaneRight);

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
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
    fn new_tab_action_opens_dialog_without_creating_tab() {
        let mut state = state_with_workspaces(&["test"]);

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.mode, Mode::RenameTab);
        assert!(state.creating_new_tab);
        assert_eq!(state.name_input, "2");
        assert!(state.name_input_replace_on_type);
        assert!(!state.request_new_tab);
        assert_eq!(state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn new_tab_action_can_skip_rename_dialog() {
        let mut state = state_with_workspaces(&["test"]);
        state.prompt_new_tab_name = false;

        execute_navigate_action(&mut state, NavigateAction::NewTab);

        assert_eq!(state.mode, Mode::Terminal);
        assert!(!state.creating_new_tab);
        assert!(state.request_new_tab);
        assert!(state.requested_new_tab_name.is_none());
    }

    #[test]
    fn persistence_mode_navigate_q_detaches_instead_of_quitting_server() {
        let mut state = crate::app::state::AppState::test_new();
        state.quit_detaches = true;

        assert!(handle_navigate_reserved_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())
        ));
        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }
}
