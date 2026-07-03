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
        state::{AppState, Mode},
        App,
    },
    input::TerminalKey,
    layout::NavDirection,
    terminal::TerminalRuntimeRegistry,
};

pub(crate) fn terminal_direct_navigation_action(
    state: &AppState,
    key: TerminalKey,
) -> Option<NavigateAction> {
    action_for_key(state, key, BindingDispatch::Direct)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionContext {
    Direct,
    Prefix,
    Navigate,
}

impl App {
    pub(crate) fn handle_prefix_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if self.state.is_prefix_key(raw_key) {
            if !self.pass_through_key_to_focused_pane(raw_key) {
                leave_command_mode(&mut self.state);
            }
            return;
        }

        if key.code == KeyCode::Esc {
            leave_command_mode(&mut self.state);
            return;
        }

        if let Some(action) = action_for_key(&self.state, raw_key, BindingDispatch::Prefix) {
            if action == NavigateAction::EditScrollback {
                let previous_mode = self.state.mode;
                self.launch_focused_scrollback_editor();
                finish_action_context(&mut self.state, ActionContext::Prefix, previous_mode);
            } else {
                self.execute_tui_navigate_action(action, ActionContext::Prefix);
            }
            self.selection_autoscroll_deadline = None;
            return;
        }

        if let Some(binding) = command_for_key(&self.state, raw_key, BindingDispatch::Prefix) {
            self.launch_custom_command(binding, ActionContext::Prefix);
            return;
        }

        leave_command_mode(&mut self.state);
    }

    pub(crate) fn handle_navigate_key(&mut self, raw_key: TerminalKey) {
        let key = raw_key.as_key_event();
        self.state.update_dismissed = true;

        if key.code == KeyCode::Esc || self.state.is_prefix_key(raw_key) {
            leave_navigate_mode(&mut self.state);
            return;
        }

        if let Some(action) = navigate_reserved_action_for_key(&self.state, raw_key) {
            self.execute_tui_navigate_action(action, ActionContext::Navigate);
            return;
        }

        if let Some(action) = navigate_mode_action_for_key(&self.state, raw_key) {
            if action == NavigateAction::EditScrollback {
                self.launch_focused_scrollback_editor();
            } else {
                self.execute_tui_navigate_action(action, ActionContext::Navigate);
            }
            self.selection_autoscroll_deadline = None;
            return;
        }

        if let Some(binding) = command_for_key(&self.state, raw_key, BindingDispatch::Prefix) {
            self.launch_custom_command(binding, ActionContext::Navigate);
        }
    }

    pub(super) fn execute_tui_navigate_action(
        &mut self,
        action: NavigateAction,
        context: ActionContext,
    ) {
        let previous_mode = self.state.mode;
        match action {
            NavigateAction::NewWorkspace => {
                self.dispatch_runtime_mutation(
                    "tui.key.workspace.create",
                    crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                            env: Default::default(),
                        },
                    ),
                );
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::NewWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_new_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::OpenWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context).filter(|idx| {
                    workspace_can_start_worktree_action(&self.state, &self.terminal_runtimes, *idx)
                }) {
                    self.state.request_open_existing_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RemoveWorktree => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.request_remove_linked_worktree = Some(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::RenameWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    super::modal::open_rename_workspace(
                        &mut self.state,
                        &self.terminal_runtimes,
                        ws_idx,
                    );
                }
            }
            NavigateAction::CloseWorkspace => {
                if let Some(ws_idx) = workspace_action_target(&self.state, context) {
                    self.state.selected = ws_idx;
                    if self.state.confirm_close {
                        super::modal::open_confirm_close(&mut self.state);
                    } else {
                        self.close_workspace_idx_via_api(ws_idx);
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::SwitchWorkspace(idx) => {
                if let Some(ws_idx) = self.state.workspace_at_visible_position(idx) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::SwitchTab(idx) => {
                if self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .is_some_and(|ws| idx < ws.tabs.len())
                {
                    self.focus_tab_idx_via_api(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::FocusAgent(idx) => {
                if let Some((ws_idx, pane_id)) = self.agent_entry_target(idx) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    self.state.ensure_agent_panel_entry_visible(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::WorkspacePicker => {
                self.state.mobile_switcher_scroll = 0;
                self.state.mode = Mode::Navigate;
            }
            NavigateAction::PreviousWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(-1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextWorkspace => {
                if let Some(ws_idx) = self.relative_visible_workspace(1) {
                    self.focus_workspace_idx_via_api(ws_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::PreviousAgent => {
                if let Some((idx, ws_idx, pane_id)) = self.relative_agent_entry(false) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    self.state.ensure_agent_panel_entry_visible(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextAgent => {
                if let Some((idx, ws_idx, pane_id)) = self.relative_agent_entry(true) {
                    self.focus_pane_internal_via_api(ws_idx, pane_id);
                    self.state.ensure_agent_panel_entry_visible(idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NewTab => {
                if self.state.active.is_some() {
                    if self.state.prompt_new_tab_name {
                        super::modal::open_new_tab_dialog(&mut self.state);
                    } else {
                        self.dispatch_runtime_mutation(
                            "tui.key.tab.create",
                            crate::api::schema::Method::TabCreate(
                                crate::api::schema::TabCreateParams {
                                    workspace_id: None,
                                    cwd: None,
                                    focus: true,
                                    label: None,
                                    env: Default::default(),
                                },
                            ),
                        );
                        leave_navigate_mode(&mut self.state);
                    }
                }
            }
            NavigateAction::RenameTab => {
                super::modal::open_rename_active_tab(&mut self.state, false)
            }
            NavigateAction::PreviousTab => {
                if let Some(tab_idx) = self.relative_tab(-1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::NextTab => {
                if let Some(tab_idx) = self.relative_tab(1) {
                    self.focus_tab_idx_via_api(tab_idx);
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::CloseTab => {
                self.close_active_tab_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::RenamePane => {
                if let Some(pane_id) = self
                    .state
                    .active
                    .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
                    .and_then(|ws| ws.focused_pane_id())
                {
                    super::modal::open_rename_pane(&mut self.state, pane_id);
                }
            }
            NavigateAction::FocusPaneLeft => self.focus_pane_direction_via_api(NavDirection::Left),
            NavigateAction::FocusPaneDown => self.focus_pane_direction_via_api(NavDirection::Down),
            NavigateAction::FocusPaneUp => self.focus_pane_direction_via_api(NavDirection::Up),
            NavigateAction::FocusPaneRight => {
                self.focus_pane_direction_via_api(NavDirection::Right)
            }
            NavigateAction::SwapPaneLeft => {
                self.swap_pane_direction_via_api(NavDirection::Left);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneDown => {
                self.swap_pane_direction_via_api(NavDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneUp => {
                self.swap_pane_direction_via_api(NavDirection::Up);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SwapPaneRight => {
                self.swap_pane_direction_via_api(NavDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitVertical => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Right);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::SplitHorizontal => {
                self.split_focused_pane_via_api(crate::api::schema::SplitDirection::Down);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::ClosePane => {
                self.close_focused_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::EditScrollback => {}
            NavigateAction::CopyMode => self.state.enter_copy_mode(&self.terminal_runtimes),
            NavigateAction::Zoom => {
                self.zoom_focused_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::EnterResizeMode => self.state.mode = Mode::Resize,
            NavigateAction::ToggleSidebar => {
                self.state.sidebar_collapsed = !self.state.sidebar_collapsed;
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePaneNext => {
                self.cycle_pane_via_api(false);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::CyclePanePrevious => {
                self.cycle_pane_via_api(true);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::LastPane => {
                self.last_pane_via_api();
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::Help => super::modal::open_keybind_help(&mut self.state),
            NavigateAction::Settings => super::settings::open_settings(&mut self.state),
            NavigateAction::ReloadConfig => {
                self.dispatch_runtime_mutation(
                    "tui.server.reload_config",
                    crate::api::schema::Method::ServerReloadConfig(
                        crate::api::schema::EmptyParams::default(),
                    ),
                );
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNotificationTarget => {
                self.focus_toast_target_via_api();
                if self.state.mode == Mode::Navigate {
                    leave_navigate_mode(&mut self.state);
                }
            }
            NavigateAction::Detach => {
                super::modal::request_detach(&mut self.state);
                leave_navigate_mode(&mut self.state);
            }
            NavigateAction::OpenNavigator => {
                self.state.open_navigator_from(&self.terminal_runtimes)
            }
        }

        finish_action_context(&mut self.state, context, previous_mode);
    }

    pub(crate) fn focus_workspace_idx_via_api(&mut self, ws_idx: usize) {
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_focus("tui.workspace.focus", workspace_id);
    }

    pub(crate) fn close_workspace_idx_via_api(&mut self, ws_idx: usize) {
        let workspace_id = self.public_workspace_id(ws_idx);
        self.runtime_workspace_close("tui.workspace.close", workspace_id);
    }

    pub(crate) fn move_workspace_via_api(&mut self, source_ws_idx: usize, insert_idx: usize) {
        let workspace_id = self.public_workspace_id(source_ws_idx);
        self.dispatch_runtime_mutation(
            "tui.workspace.move",
            crate::api::schema::Method::WorkspaceMove(crate::api::schema::WorkspaceMoveParams {
                workspace_id,
                insert_index: insert_idx,
            }),
        );
    }

    pub(crate) fn focus_tab_idx_via_api(&mut self, tab_idx: usize) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_focus("tui.tab.focus", tab_id);
    }

    pub(crate) fn close_active_tab_via_api(&mut self) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let tab_idx = self.state.workspaces[ws_idx].active_tab_index();
        let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
            return;
        };
        self.runtime_tab_close("tui.tab.close", tab_id);
    }

    pub(crate) fn move_tab_via_api(
        &mut self,
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: usize,
    ) {
        let Some(tab_id) = self.public_tab_id(ws_idx, source_tab_idx) else {
            return;
        };
        self.dispatch_runtime_mutation(
            "tui.tab.move",
            crate::api::schema::Method::TabMove(crate::api::schema::TabMoveParams {
                tab_id,
                insert_index: insert_idx,
            }),
        );
    }

    pub(crate) fn focus_pane_internal_via_api(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) {
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        self.runtime_pane_focus("tui.pane.focus", pane_id);
    }

    pub(crate) fn focus_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, target)) = self.directional_pane_target_from_view(direction) {
            self.focus_pane_internal_via_api(ws_idx, target);
            return;
        }
        self.dispatch_runtime_mutation(
            "tui.pane.focus_direction",
            crate::api::schema::Method::PaneFocusDirection(
                crate::api::schema::PaneFocusDirectionParams {
                    pane_id: None,
                    direction: api_pane_direction(direction),
                },
            ),
        );
    }

    pub(crate) fn swap_pane_direction_via_api(&mut self, direction: NavDirection) {
        if let Some((ws_idx, source, target)) = self.directional_pane_swap_from_view(direction) {
            let source_pane_id = self.public_pane_id(ws_idx, source);
            let target_pane_id = self.public_pane_id(ws_idx, target);
            if let (Some(source_pane_id), Some(target_pane_id)) = (source_pane_id, target_pane_id) {
                self.dispatch_runtime_mutation(
                    "tui.pane.swap_exact",
                    crate::api::schema::Method::PaneSwap(crate::api::schema::PaneSwapParams {
                        pane_id: None,
                        direction: None,
                        source_pane_id: Some(source_pane_id),
                        target_pane_id: Some(target_pane_id),
                    }),
                );
                return;
            }
        }
        self.dispatch_runtime_mutation(
            "tui.pane.swap",
            crate::api::schema::Method::PaneSwap(crate::api::schema::PaneSwapParams {
                pane_id: None,
                direction: Some(api_pane_direction(direction)),
                source_pane_id: None,
                target_pane_id: None,
            }),
        );
    }

    pub(crate) fn split_focused_pane_via_api(
        &mut self,
        direction: crate::api::schema::SplitDirection,
    ) {
        self.dispatch_runtime_mutation(
            "tui.pane.split",
            crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: None,
                target_pane_id: None,
                direction,
                ratio: None,
                cwd: None,
                focus: true,
                env: Default::default(),
            }),
        );
    }

    pub(crate) fn close_focused_pane_via_api(&mut self) {
        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return;
        };
        let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        self.runtime_pane_close("tui.pane.close", pane_id);
    }

    pub(crate) fn zoom_focused_pane_via_api(&mut self) {
        self.dispatch_runtime_mutation(
            "tui.pane.zoom",
            crate::api::schema::Method::PaneZoom(crate::api::schema::PaneZoomParams {
                pane_id: None,
                mode: crate::api::schema::PaneZoomMode::Toggle,
            }),
        );
    }

    pub(crate) fn set_split_ratio_via_api(&mut self, path: Vec<bool>, ratio: f32) {
        self.dispatch_runtime_mutation(
            "tui.layout.set_split_ratio",
            crate::api::schema::Method::LayoutSetSplitRatio(
                crate::api::schema::LayoutSetSplitRatioParams {
                    tab_id: None,
                    pane_id: None,
                    path,
                    ratio,
                },
            ),
        );
    }

    pub(crate) fn cycle_pane_via_api(&mut self, reverse: bool) {
        let Some((ws_idx, pane_id)) = self.focused_pane_target() else {
            return;
        };
        let Some(tab) = self.state.workspaces[ws_idx].active_tab() else {
            return;
        };
        let ids = tab.layout.pane_ids();
        let Some(pos) = ids.iter().position(|id| *id == pane_id) else {
            return;
        };
        let target = if reverse {
            ids[(pos + ids.len() - 1) % ids.len()]
        } else {
            ids[(pos + 1) % ids.len()]
        };
        self.focus_pane_internal_via_api(ws_idx, target);
    }

    pub(crate) fn last_pane_via_api(&mut self) {
        let Some(target) = self.state.previous_pane_focus.clone() else {
            return;
        };
        let Some((ws_idx, _tab_idx)) = self.state.pane_focus_target_indices(&target) else {
            self.state.previous_pane_focus = None;
            return;
        };
        if self.state.current_pane_focus_target().as_ref() == Some(&target) {
            self.state.previous_pane_focus = None;
            return;
        }
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
    }

    pub(crate) fn focus_toast_target_via_api(&mut self) {
        let Some(target) = self
            .state
            .toast
            .as_ref()
            .and_then(|toast| toast.target.clone())
        else {
            return;
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        self.focus_pane_internal_via_api(ws_idx, target.pane_id);
        self.state.toast = None;
        self.state.mode = Mode::Terminal;
    }

    fn focused_pane_target(&self) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        Some((ws_idx, pane_id))
    }

    fn directional_pane_target_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, target))
    }

    fn directional_pane_swap_from_view(
        &self,
        direction: NavDirection,
    ) -> Option<(usize, crate::layout::PaneId, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let focused = self
            .state
            .view
            .pane_infos
            .iter()
            .find(|pane| pane.is_focused)?;
        let target =
            crate::layout::find_in_direction(focused, direction, &self.state.view.pane_infos)?;
        Some((ws_idx, focused.id, target))
    }

    fn relative_visible_workspace(&self, delta: isize) -> Option<usize> {
        let order = self.state.visible_workspace_order();
        if order.is_empty() {
            return None;
        }
        let current = self.state.active.unwrap_or(self.state.selected);
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let next = (current_pos as isize + delta).rem_euclid(order.len() as isize) as usize;
        order.get(next).copied()
    }

    fn relative_tab(&self, delta: isize) -> Option<usize> {
        let ws = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))?;
        if ws.tabs.is_empty() {
            return None;
        }
        Some((ws.active_tab as isize + delta).rem_euclid(ws.tabs.len() as isize) as usize)
    }

    fn agent_entry_target(&self, idx: usize) -> Option<(usize, crate::layout::PaneId)> {
        let entries = crate::ui::agent_panel_entries(&self.state);
        let target = entries.get(idx)?;
        Some((target.ws_idx, target.pane_id))
    }

    fn relative_agent_entry(&self, forward: bool) -> Option<(usize, usize, crate::layout::PaneId)> {
        let entries = crate::ui::agent_panel_entries(&self.state);
        if entries.is_empty() {
            return None;
        }
        let focused = self
            .state
            .active
            .and_then(|idx| self.state.workspaces.get(idx))
            .and_then(crate::workspace::Workspace::focused_pane_id);
        let current_idx = entries
            .iter()
            .position(|entry| Some(entry.pane_id) == focused)
            .unwrap_or(0);
        let next_idx = if forward {
            (current_idx + 1) % entries.len()
        } else if current_idx == 0 {
            entries.len() - 1
        } else {
            current_idx - 1
        };
        let target = entries.get(next_idx)?;
        Some((next_idx, target.ws_idx, target.pane_id))
    }

    fn pass_through_key_to_focused_pane(&mut self, key: TerminalKey) -> bool {
        let Some(ws_idx) = self.state.active else {
            return false;
        };
        let Some(rt) = self
            .state
            .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
        else {
            return false;
        };

        let bytes = rt.encode_terminal_key(key);
        if bytes.is_empty() || rt.try_send_bytes(Bytes::from(bytes)).is_err() {
            return false;
        }

        self.state.mode = Mode::Terminal;
        true
    }

    pub(super) fn launch_custom_command(
        &mut self,
        binding: crate::config::CustomCommandKeybind,
        context: ActionContext,
    ) {
        let previous_mode = self.state.mode;
        let previous_toast = self.state.toast.clone();
        let result = match binding.action {
            crate::config::CustomCommandAction::Shell => self.spawn_custom_command(&binding),
            crate::config::CustomCommandAction::Pane => {
                self.spawn_pane_command(&binding.command, Vec::new())
            }
            crate::config::CustomCommandAction::PluginAction => self
                .invoke_plugin_action_from_keybind(binding.command.clone())
                .map_err(std::io::Error::other),
        };
        match result {
            Ok(()) => finish_custom_command_context(&mut self.state, context, previous_mode),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "custom command failed".to_string(),
                    context: err.to_string(),
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
                finish_custom_command_context(&mut self.state, context, previous_mode);
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
                        tab.cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
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

    pub(super) fn launch_focused_scrollback_editor(&mut self) {
        let previous_toast = self.state.toast.clone();
        match self.open_focused_scrollback_in_editor() {
            Ok(()) => self.sync_toast_deadline(previous_toast),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "edit scrollback failed".to_string(),
                    context: err.to_string(),
                    position: None,
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
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .ok_or_else(|| std::io::Error::other("focused pane has no scrollback runtime"))?
            .recent_text(usize::MAX);

        let path = write_scrollback_temp_file(&scrollback)?;

        let argv = match crate::platform::scrollback_editor_argv(&path) {
            Ok(argv) => argv,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let (env, _) = self.custom_command_env();
        let new_pane = match self.spawn_overlay_argv_command(&argv, None, env, vec![path.clone()]) {
            Ok((_, new_pane)) => new_pane,
            Err(err) => {
                let _ = fs::remove_file(&path);
                return Err(err);
            }
        };
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state.terminals.insert(terminal_id, new_pane.terminal);

        if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::Finished,
                title: "opened scrollback".to_string(),
                context: format!("focused pane {public_pane_id}"),
                position: None,
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
        let previous_focus_target = self.state.current_pane_focus_target();
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
                &self.terminal_runtimes,
            )
        });
        let new_pane = ws.split_focused_command(
            Direction::Horizontal,
            new_rows,
            new_cols,
            cwd,
            command,
            env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        )?;
        let new_pane_id = new_pane.pane_id;
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id: new_pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
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
        self.state.remove_alias_shadowed_by_new_pane(new_pane_id);
        self.state.mode = Mode::Terminal;
        Ok(())
    }

    pub(crate) fn spawn_overlay_argv_command(
        &mut self,
        argv: &[String],
        cwd: Option<std::path::PathBuf>,
        extra_env: Vec<(String, String)>,
        temp_files: Vec<std::path::PathBuf>,
    ) -> std::io::Result<(usize, crate::workspace::NewPane)> {
        let Some(ws_idx) = self.state.active else {
            return Err(std::io::Error::other("no active workspace"));
        };
        let previous_focus_target = self.state.current_pane_focus_target();
        let (rows, cols) = self.state.estimate_pane_size();
        let new_rows = rows.max(4);
        let new_cols = cols.max(10);

        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let previous_focus = ws
            .focused_pane_id()
            .ok_or_else(|| std::io::Error::other("no focused pane"))?;
        let cwd = cwd.or_else(|| {
            ws.active_tab().and_then(|tab| {
                tab.cwd_for_pane(
                    previous_focus,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            })
        });

        let (tab_idx, new_pane, workspace_id) = {
            let ws = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
            let previous_zoomed = ws.active_tab().map(|tab| tab.zoomed).unwrap_or(false);
            let result = ws.split_pane_argv_command(
                previous_focus,
                Direction::Horizontal,
                new_rows,
                new_cols,
                cwd,
                argv,
                extra_env,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
                true,
            );
            let (tab_idx, new_pane) = match result {
                Some(Ok(result)) => result,
                Some(Err(err)) => return Err(err),
                None => return Err(std::io::Error::other("focused pane disappeared")),
            };
            ws.tabs
                .get_mut(tab_idx)
                .ok_or_else(|| std::io::Error::other("plugin overlay tab disappeared"))?
                .zoomed = true;
            self.overlay_panes.insert(
                new_pane.pane_id,
                super::super::OverlayPaneState {
                    ws_idx,
                    tab_idx,
                    previous_focus,
                    previous_zoomed,
                    temp_files,
                },
            );
            (tab_idx, new_pane, ws.id.clone())
        };

        let new_focus_target = crate::app::state::PaneFocusTarget {
            workspace_id,
            pane_id: new_pane.pane_id,
        };
        if previous_focus_target.as_ref() != Some(&new_focus_target) {
            self.state.previous_pane_focus = previous_focus_target;
        }
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        self.state.mode = Mode::Terminal;
        Ok((ws_idx, new_pane))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingDispatch {
    Direct,
    Prefix,
}

pub(crate) fn command_for_key(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<crate::config::CustomCommandKeybind> {
    state
        .keybinds
        .custom_commands
        .iter()
        .find(|binding| match dispatch {
            BindingDispatch::Direct => binding.bindings.matches_direct_key(key),
            BindingDispatch::Prefix => binding.bindings.matches_prefix_key(key),
        })
        .cloned()
}

pub(super) fn handle_navigate_reserved_key(state: &mut AppState, key: TerminalKey) -> bool {
    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                if !state.workspaces.is_empty() {
                    state.switch_workspace(state.selected);
                    leave_navigate_mode(state);
                }
                return true;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
                    state.switch_workspace(ws_idx);
                    leave_navigate_mode(state);
                }
                return true;
            }
            KeyCode::Tab => {
                state.cycle_pane(false);
                return true;
            }
            KeyCode::BackTab => {
                state.cycle_pane(true);
                return true;
            }
            KeyCode::Left => {
                state.navigate_pane(NavDirection::Left);
                return true;
            }
            KeyCode::Right => {
                state.navigate_pane(NavDirection::Right);
                return true;
            }
            _ => {}
        }
    }

    if state.keybinds.navigate.workspace_up.matches_direct_key(key) {
        state.move_selected_workspace_by_visible_delta(-1);
        return true;
    }
    if state
        .keybinds
        .navigate
        .workspace_down
        .matches_direct_key(key)
    {
        state.move_selected_workspace_by_visible_delta(1);
        return true;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(key) {
        state.navigate_pane(NavDirection::Left);
        return true;
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(key) {
        state.navigate_pane(NavDirection::Down);
        return true;
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(key) {
        state.navigate_pane(NavDirection::Up);
        return true;
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(key) {
        state.navigate_pane(NavDirection::Right);
        return true;
    }

    false
}

fn navigate_reserved_action_for_key(state: &AppState, key: TerminalKey) -> Option<NavigateAction> {
    let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
    if modifiers.is_empty() {
        match code {
            KeyCode::Enter => {
                return (!state.workspaces.is_empty()).then_some(NavigateAction::SwitchWorkspace(
                    state
                        .visible_workspace_order()
                        .iter()
                        .position(|idx| *idx == state.selected)
                        .unwrap_or(state.selected),
                ));
            }
            KeyCode::Char(c @ '1'..='9') => {
                return Some(NavigateAction::SwitchWorkspace(
                    (c as usize) - ('1' as usize),
                ));
            }
            KeyCode::Tab => return Some(NavigateAction::CyclePaneNext),
            KeyCode::BackTab => return Some(NavigateAction::CyclePanePrevious),
            KeyCode::Left => return Some(NavigateAction::FocusPaneLeft),
            KeyCode::Right => return Some(NavigateAction::FocusPaneRight),
            _ => {}
        }
    }

    if state.keybinds.navigate.workspace_up.matches_direct_key(key)
        || state
            .keybinds
            .navigate
            .workspace_down
            .matches_direct_key(key)
    {
        return None;
    }
    if state.keybinds.navigate.pane_left.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneLeft);
    }
    if state.keybinds.navigate.pane_down.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneDown);
    }
    if state.keybinds.navigate.pane_up.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneUp);
    }
    if state.keybinds.navigate.pane_right.matches_direct_key(key) {
        return Some(NavigateAction::FocusPaneRight);
    }

    None
}

pub(super) fn api_pane_direction(direction: NavDirection) -> crate::api::schema::PaneDirection {
    match direction {
        NavDirection::Left => crate::api::schema::PaneDirection::Left,
        NavDirection::Right => crate::api::schema::PaneDirection::Right,
        NavDirection::Up => crate::api::schema::PaneDirection::Up,
        NavDirection::Down => crate::api::schema::PaneDirection::Down,
    }
}

#[allow(dead_code)] // exercised in input unit tests; production uses App::handle_navigate_key
pub(crate) fn handle_navigate_key(state: &mut AppState, key: KeyEvent) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    state.update_dismissed = true;
    let terminal_key = TerminalKey::from(key);

    if state.is_prefix_key(terminal_key) || key.code == KeyCode::Esc {
        leave_navigate_mode(state);
        return;
    }

    if handle_navigate_reserved_key(state, terminal_key) {
        return;
    }

    if let Some(action) = navigate_mode_action_for_key(state, terminal_key) {
        execute_navigate_action_in_context(
            state,
            &mut terminal_runtimes,
            action,
            ActionContext::Navigate,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigateAction {
    NewWorkspace,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    RenameWorkspace,
    CloseWorkspace,
    SwitchWorkspace(usize),
    SwitchTab(usize),
    FocusAgent(usize),
    WorkspacePicker,
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
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    EditScrollback,
    CopyMode,
    Zoom,
    EnterResizeMode,
    ToggleSidebar,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    Help,
    Settings,
    ReloadConfig,
    OpenNotificationTarget,
    Detach,
    OpenNavigator,
}

fn indexed_navigation_action(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    let kb = &state.keybinds;
    let trigger_matches = |binding: &crate::config::IndexedKeybind| match dispatch {
        BindingDispatch::Direct => binding.trigger.is_direct(),
        BindingDispatch::Prefix => binding.trigger.is_prefix(),
    };

    for binding in &kb.switch_tab {
        if trigger_matches(binding) {
            if let Some(idx) = binding.matched_index(key) {
                return Some(NavigateAction::SwitchTab(idx));
            }
        }
    }
    for binding in &kb.switch_workspace {
        if trigger_matches(binding) {
            if let Some(idx) = binding.matched_index(key) {
                return Some(NavigateAction::SwitchWorkspace(idx));
            }
        }
    }
    for binding in &kb.focus_agent {
        if trigger_matches(binding) {
            if let Some(idx) = binding.matched_index(key) {
                return Some(NavigateAction::FocusAgent(idx));
            }
        }
    }

    None
}

fn action_matches(
    bindings: &crate::config::ActionKeybinds,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> bool {
    match dispatch {
        BindingDispatch::Direct => bindings.matches_direct_key(key),
        BindingDispatch::Prefix => bindings.matches_prefix_key(key),
    }
}

fn action_for_key(
    state: &AppState,
    key: TerminalKey,
    dispatch: BindingDispatch,
) -> Option<NavigateAction> {
    if let Some(action) = indexed_navigation_action(state, key, dispatch) {
        return Some(action);
    }

    let kb = &state.keybinds;
    for (bindings, action) in [
        (&kb.help, NavigateAction::Help),
        (&kb.settings, NavigateAction::Settings),
        (&kb.workspace_picker, NavigateAction::WorkspacePicker),
        (&kb.new_workspace, NavigateAction::NewWorkspace),
        (&kb.new_worktree, NavigateAction::NewWorktree),
        (&kb.open_worktree, NavigateAction::OpenWorktree),
        (&kb.remove_worktree, NavigateAction::RemoveWorktree),
        (&kb.rename_workspace, NavigateAction::RenameWorkspace),
        (&kb.close_workspace, NavigateAction::CloseWorkspace),
        (&kb.previous_workspace, NavigateAction::PreviousWorkspace),
        (&kb.next_workspace, NavigateAction::NextWorkspace),
        (&kb.previous_agent, NavigateAction::PreviousAgent),
        (&kb.next_agent, NavigateAction::NextAgent),
        (&kb.new_tab, NavigateAction::NewTab),
        (&kb.rename_tab, NavigateAction::RenameTab),
        (&kb.previous_tab, NavigateAction::PreviousTab),
        (&kb.next_tab, NavigateAction::NextTab),
        (&kb.close_tab, NavigateAction::CloseTab),
        (&kb.rename_pane, NavigateAction::RenamePane),
        (&kb.edit_scrollback, NavigateAction::EditScrollback),
        (&kb.copy_mode, NavigateAction::CopyMode),
        (&kb.focus_pane_left, NavigateAction::FocusPaneLeft),
        (&kb.focus_pane_down, NavigateAction::FocusPaneDown),
        (&kb.focus_pane_up, NavigateAction::FocusPaneUp),
        (&kb.focus_pane_right, NavigateAction::FocusPaneRight),
        (&kb.swap_pane_left, NavigateAction::SwapPaneLeft),
        (&kb.swap_pane_down, NavigateAction::SwapPaneDown),
        (&kb.swap_pane_up, NavigateAction::SwapPaneUp),
        (&kb.swap_pane_right, NavigateAction::SwapPaneRight),
        (&kb.last_pane, NavigateAction::LastPane),
        (&kb.cycle_pane_next, NavigateAction::CyclePaneNext),
        (&kb.cycle_pane_previous, NavigateAction::CyclePanePrevious),
        (&kb.split_vertical, NavigateAction::SplitVertical),
        (&kb.split_horizontal, NavigateAction::SplitHorizontal),
        (&kb.close_pane, NavigateAction::ClosePane),
        (&kb.zoom, NavigateAction::Zoom),
        (&kb.resize_mode, NavigateAction::EnterResizeMode),
        (&kb.toggle_sidebar, NavigateAction::ToggleSidebar),
        (&kb.reload_config, NavigateAction::ReloadConfig),
        (
            &kb.open_notification_target,
            NavigateAction::OpenNotificationTarget,
        ),
        (&kb.detach, NavigateAction::Detach),
        (&kb.goto, NavigateAction::OpenNavigator),
    ] {
        if action_matches(bindings, key, dispatch) {
            return Some(action);
        }
    }
    None
}

fn navigate_mode_action_for_key(state: &AppState, key: TerminalKey) -> Option<NavigateAction> {
    let action = action_for_key(state, key, BindingDispatch::Prefix)?;
    if matches!(
        action,
        NavigateAction::FocusPaneLeft
            | NavigateAction::FocusPaneDown
            | NavigateAction::FocusPaneUp
            | NavigateAction::FocusPaneRight
    ) {
        return None;
    }
    Some(action)
}

#[cfg(test)]
pub(super) fn execute_navigate_action(state: &mut AppState, action: NavigateAction) {
    let mut terminal_runtimes = TerminalRuntimeRegistry::new();
    execute_navigate_action_in_context(
        state,
        &mut terminal_runtimes,
        action,
        ActionContext::Navigate,
    );
}

pub(super) fn execute_navigate_action_in_context(
    state: &mut AppState,
    terminal_runtimes: &mut TerminalRuntimeRegistry,
    action: NavigateAction,
    context: ActionContext,
) {
    let previous_mode = state.mode;
    match action {
        NavigateAction::NewWorkspace => {
            state.request_new_workspace = true;
            leave_navigate_mode(state);
        }
        NavigateAction::NewWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_new_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::OpenWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context)
                .filter(|idx| workspace_can_start_worktree_action(state, terminal_runtimes, *idx))
            {
                state.request_open_existing_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RemoveWorktree => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.request_remove_linked_worktree = Some(ws_idx);
                leave_navigate_mode(state);
            }
        }
        NavigateAction::RenameWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                super::modal::open_rename_workspace(state, terminal_runtimes, ws_idx);
            }
        }
        NavigateAction::CloseWorkspace => {
            if let Some(ws_idx) = workspace_action_target(state, context) {
                state.selected = ws_idx;
                if state.confirm_close {
                    super::modal::open_confirm_close(state);
                } else {
                    state.close_selected_workspace();
                    leave_navigate_mode(state);
                }
            }
        }
        NavigateAction::SwitchWorkspace(idx) => {
            if let Some(ws_idx) = state.workspace_at_visible_position(idx) {
                state.switch_workspace(ws_idx);
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
        NavigateAction::WorkspacePicker => {
            state.mobile_switcher_scroll = 0;
            state.mode = Mode::Navigate;
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
            if state.active.is_some() {
                if state.prompt_new_tab_name {
                    super::modal::open_new_tab_dialog(state);
                } else {
                    state.request_new_tab = true;
                    leave_navigate_mode(state);
                }
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
            if !state.close_tab() {
                leave_navigate_mode(state);
            }
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
        NavigateAction::SwapPaneLeft => {
            state.swap_pane(NavDirection::Left);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneDown => {
            state.swap_pane(NavDirection::Down);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneUp => {
            state.swap_pane(NavDirection::Up);
            leave_navigate_mode(state);
        }
        NavigateAction::SwapPaneRight => {
            state.swap_pane(NavDirection::Right);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitVertical => {
            state.split_pane(terminal_runtimes, Direction::Horizontal);
            leave_navigate_mode(state);
        }
        NavigateAction::SplitHorizontal => {
            state.split_pane(terminal_runtimes, Direction::Vertical);
            leave_navigate_mode(state);
        }
        NavigateAction::ClosePane => {
            if !state.close_pane() {
                leave_navigate_mode(state);
            }
        }
        NavigateAction::EditScrollback => {}
        NavigateAction::CopyMode => state.enter_copy_mode(terminal_runtimes),
        NavigateAction::Zoom => {
            state.toggle_zoom();
            leave_navigate_mode(state);
        }
        NavigateAction::EnterResizeMode => state.mode = Mode::Resize,
        NavigateAction::ToggleSidebar => {
            state.sidebar_collapsed = !state.sidebar_collapsed;
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePaneNext => {
            state.cycle_pane(false);
            leave_navigate_mode(state);
        }
        NavigateAction::CyclePanePrevious => {
            state.cycle_pane(true);
            leave_navigate_mode(state);
        }
        NavigateAction::LastPane => {
            state.last_pane();
            leave_navigate_mode(state);
        }
        NavigateAction::Help => super::modal::open_keybind_help(state),
        NavigateAction::Settings => super::settings::open_settings(state),
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
            super::modal::request_detach(state);
            leave_navigate_mode(state);
        }
        NavigateAction::OpenNavigator => state.open_navigator_from(terminal_runtimes),
    }

    finish_action_context(state, context, previous_mode);
}

fn workspace_action_target(state: &AppState, context: ActionContext) -> Option<usize> {
    let idx = match context {
        ActionContext::Direct | ActionContext::Prefix => state.active.unwrap_or(state.selected),
        ActionContext::Navigate => state.selected,
    };
    (idx < state.workspaces.len()).then_some(idx)
}

fn workspace_can_start_worktree_action(
    state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) -> bool {
    let Some(ws) = state.workspaces.get(ws_idx) else {
        return false;
    };
    if ws
        .worktree_space()
        .is_some_and(|space| space.is_linked_worktree)
    {
        return false;
    }
    let git_space = ws.git_space().cloned().or_else(|| {
        ws.resolved_identity_cwd_from(&state.terminals, terminal_runtimes)
            .as_deref()
            .and_then(crate::workspace::git_space_metadata)
    });
    !git_space.is_some_and(|space| space.is_linked_worktree)
}

fn leave_navigate_mode(state: &mut AppState) {
    if state.active.is_some() {
        state.mode = Mode::Terminal;
    }
}

fn finish_action_context(state: &mut AppState, context: ActionContext, previous_mode: Mode) {
    if matches!(context, ActionContext::Direct | ActionContext::Prefix)
        && state.mode == previous_mode
    {
        leave_command_mode(state);
    }
}

fn finish_custom_command_context(
    state: &mut AppState,
    context: ActionContext,
    previous_mode: Mode,
) {
    if context == ActionContext::Navigate {
        leave_navigate_mode(state);
    } else {
        finish_action_context(state, context, previous_mode);
    }
}

fn leave_command_mode(state: &mut AppState) {
    state.mode = if state.active.is_some() {
        Mode::Terminal
    } else {
        Mode::Navigate
    };
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Direction;

    #[cfg(unix)]
    use super::super::wait_for_file;
    use super::super::{state_with_workspaces, unique_temp_path};
    use super::*;
    use crate::{
        app::App, config::Config, input::TerminalKey, terminal::TerminalState, workspace::Workspace,
    };

    fn mark_worktree_space_member(state: &mut AppState, ws_idx: usize, key: &str) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: ws_idx != 0,
        });
    }

    #[test]
    fn default_goto_key_opens_navigator() {
        let mut state = state_with_workspaces(&["test"]);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Navigator);
    }

    #[test]
    fn custom_rename_key_enters_rename_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.name_input, "test");
    }

    #[test]
    fn rename_workspace_prefills_live_terminal_cwd_label() {
        let mut state = state_with_workspaces(&["stale"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].panes[&root]
            .attached_terminal_id
            .clone();
        state.workspaces[0].custom_name = None;
        state.workspaces[0].identity_cwd = "/__herdr_original__".into();
        state.terminals.insert(
            terminal_id.clone(),
            TerminalState::new(terminal_id, "/__herdr_projects__".into()),
        );
        state.keybinds.rename_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.name_input, "__herdr_projects__");
        assert_eq!(state.workspaces[0].display_name(), "__herdr_original__");
    }

    #[test]
    fn prefix_rename_workspace_targets_active_workspace_not_stale_selection() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.mode = Mode::Prefix;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::RenameWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.mode, Mode::RenameWorkspace);
        assert_eq!(state.selected, 1);
        assert_eq!(state.name_input, "issue");
    }

    #[test]
    fn prefix_close_workspace_targets_active_linked_worktree_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.active = Some(1);
        state.selected = 0;
        state.mode = Mode::Prefix;
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::CloseWorkspace,
            ActionContext::Prefix,
        );

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_new_workspace_key_requests_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.new_workspace = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert!(state.request_new_workspace);
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn custom_new_worktree_key_requests_selected_workspace() {
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[1].identity_cwd = unique_temp_path("navigate-new-worktree-selected");
        state.mode = Mode::Navigate;
        state.selected = 1;
        state.active = Some(0);
        state.keybinds.new_worktree = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.request_new_linked_worktree, Some(1));
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn worktree_actions_do_not_start_from_linked_child_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.mode = Mode::Navigate;
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_new_linked_worktree, None);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::OpenWorktree,
            ActionContext::Navigate,
        );
        assert_eq!(state.request_open_existing_worktree, None);
    }

    #[test]
    fn direct_new_worktree_action_targets_active_workspace() {
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        let mut state = state_with_workspaces(&["main", "scratch"]);
        state.workspaces[0].identity_cwd = unique_temp_path("navigate-new-worktree-active");
        state.mode = Mode::Terminal;
        state.selected = 1;
        state.active = Some(0);

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewWorktree,
            ActionContext::Direct,
        );

        assert_eq!(state.request_new_linked_worktree, Some(0));
    }

    #[test]
    fn navigate_down_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Navigate;
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 2);
    }

    #[test]
    fn navigate_number_keys_follow_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Navigate;
        state.active = Some(0);
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty()),
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn indexed_switch_workspace_keybind_follows_grouped_sidebar_visual_order() {
        let mut state = state_with_workspaces(&["main", "normal", "issue"]);
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 2, "repo-key");
        state.mode = Mode::Prefix;
        state.active = Some(0);
        state.selected = 0;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::SwitchWorkspace(1),
            ActionContext::Prefix,
        );

        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn custom_sidebar_toggle_key_toggles_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("g");
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
        state.keybinds.resize_mode = crate::config::ActionKeybinds::prefix("g");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Resize);
    }

    #[test]
    fn custom_reload_config_key_requests_reload_and_exits_navigate() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.reload_config = crate::config::ActionKeybinds::prefix("g");

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
        state.keybinds.open_notification_target = crate::config::ActionKeybinds::prefix("g");
        let target_workspace_id = state.workspaces[1].id.clone();
        let target_pane = state.workspaces[1].tabs[0].root_pane;
        state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::NeedsAttention,
            title: "pi needs attention".into(),
            context: "two".into(),
            position: None,
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
    fn navigate_workspace_keys_are_configurable() {
        let mut state = state_with_workspaces(&["a", "b"]);
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();
        state.selected = 0;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );

        assert_eq!(state.selected, 1);
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn navigate_pane_keys_are_configurable() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn focus_pane_prefix_rhs_does_not_create_navigate_mode_pane_shortcut() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
focus_pane_down = "prefix+f"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn customized_navigate_pane_key_disables_matching_prefix_rhs_fallback() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let below = state.workspaces[0].test_split(Direction::Vertical);
        state.workspaces[0].layout.focus_pane(root);
        state.view.pane_infos = state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(below));
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn left_and_right_arrows_remain_permanent_navigate_pane_aliases() {
        let mut state = state_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_left = "ctrl+h"
navigate_pane_right = "ctrl+l"
"#,
        )
        .unwrap();
        state.keybinds = config.keybinds();

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 24));

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
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
        state.keybinds.next_agent = crate::config::ActionKeybinds::direct("alt+a");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('a'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::NextAgent));
    }

    #[test]
    fn terminal_direct_focus_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.focus_pane_left = crate::config::ActionKeybinds::direct("alt+left");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Left, KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::FocusPaneLeft));
    }

    #[test]
    fn terminal_direct_swap_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.swap_pane_right = crate::config::ActionKeybinds::direct("alt+shift+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT | KeyModifiers::SHIFT),
        );

        assert_eq!(action, Some(NavigateAction::SwapPaneRight));
    }

    #[test]
    fn terminal_direct_last_pane_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.last_pane = crate::config::ActionKeybinds::direct("alt+l");

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('l'), KeyModifiers::ALT),
        );

        assert_eq!(action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn prefix_tab_override_can_map_to_last_pane() {
        let config: Config = toml::from_str(
            r#"
[keys]
last_pane = "prefix+tab"
"#,
        )
        .unwrap();
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds = config.keybinds();

        let pane_action = action_for_key(
            &state,
            TerminalKey::new(KeyCode::Tab, KeyModifiers::empty()),
            BindingDispatch::Prefix,
        );

        assert_eq!(pane_action, Some(NavigateAction::LastPane));
    }

    #[test]
    fn terminal_direct_indexed_tab_shortcut_maps_to_navigation_action() {
        let mut state = state_with_workspaces(&["test"]);
        let config: Config = toml::from_str("[keys]\nswitch_tab = \"ctrl+3\"\n").unwrap();
        state.keybinds.switch_tab = config.keybinds().switch_tab;

        let action = terminal_direct_navigation_action(
            &state,
            TerminalKey::new(KeyCode::Char('3'), KeyModifiers::CONTROL),
        );

        assert_eq!(action, Some(NavigateAction::SwitchTab(2)));
    }

    #[tokio::test]
    async fn navigate_mode_runs_prefix_action_rhs_without_pressing_prefix_again() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::SHIFT));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn navigate_mode_matches_legacy_uppercase_shifted_letter() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty()));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_workspace_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('W'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_reload_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('R'), KeyModifiers::empty()));

        assert!(!app.state.request_reload_config);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn legacy_uppercase_prefers_shifted_pane_binding_over_unshifted() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;

        app.handle_navigate_key(TerminalKey::new(KeyCode::Char('P'), KeyModifiers::empty()));

        assert_eq!(app.state.mode, Mode::RenamePane);
    }

    #[tokio::test]
    async fn prefix_focus_pane_is_one_shot_and_returns_to_terminal() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].layout.focus_pane(right);
        app.state.view.pane_infos = app.state.workspaces[0]
            .active_tab()
            .unwrap()
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 80, 24));

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn no_op_prefix_action_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('o'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn unmatched_prefix_rhs_exits_prefix_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::F(12), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[tokio::test]
    async fn prefix_help_matches_enhanced_shifted_question_mark() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32),
        )
        .await;

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn navigate_mode_help_is_binding_driven() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.help = crate::config::ActionKeybinds::prefix("f");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.mode, Mode::Navigate);

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::empty()),
        );
        assert_eq!(state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn modified_navigate_local_key_can_be_bound_as_prefix_rhs() {
        let mut state = state_with_workspaces(&["test"]);
        state.keybinds.toggle_sidebar = crate::config::ActionKeybinds::prefix("shift+u");

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT),
        );

        assert!(state.sidebar_collapsed);
    }

    #[test]
    fn empty_state_new_tab_is_no_op() {
        let mut state = crate::app::state::AppState::test_new();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        state.mode = Mode::Prefix;

        execute_navigate_action_in_context(
            &mut state,
            &mut terminal_runtimes,
            NavigateAction::NewTab,
            ActionContext::Prefix,
        );

        assert_eq!(state.mode, Mode::Navigate);
        assert!(!state.creating_new_tab);
        assert!(!state.request_new_tab);
        assert!(state.workspaces.is_empty());
    }

    #[test]
    fn closing_linked_worktree_closes_workspace_without_removing_checkout() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        state.selected = 1;
        state.active = Some(1);
        state.mode = Mode::Navigate;
        state.confirm_close = false;
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        execute_navigate_action(&mut state, NavigateAction::CloseWorkspace);

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "main");
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn prefix_close_pane_last_parent_group_pane_opens_confirmation() {
        let mut state = state_with_workspaces(&["main", "issue"]);
        mark_worktree_space_member(&mut state, 0, "repo-key");
        mark_worktree_space_member(&mut state, 1, "repo-key");
        state.selected = 1;
        state.active = Some(0);
        state.mode = Mode::Navigate;

        execute_navigate_action(&mut state, NavigateAction::ClosePane);

        assert_eq!(state.selected, 0);
        assert_eq!(state.mode, Mode::ConfirmClose);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn custom_command_runs_from_prefix_key_in_navigate_mode() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
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
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Shell,
            description: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        assert_eq!(app.state.mode, Mode::Prefix);

        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;

        let content = wait_for_file(&output_path);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], app.state.workspaces[0].id);
        assert_eq!(lines[1], format!("{}:t1", app.state.workspaces[0].id));
        assert_eq!(lines[2], format!("{}:p1", app.state.workspaces[0].id));
        assert_eq!(app.state.mode, Mode::Terminal);

        let _ = std::fs::remove_file(output_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_overlay_command_opens_and_closes_after_exit() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
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
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        let root_pane = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("custom-pane-command");
        let command = format!("printf done > '{}'", output_path.display());
        app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("m"),
            label: "prefix+m".into(),
            command,
            action: crate::config::CustomCommandAction::Pane,
            description: None,
        }];

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('m'), KeyModifiers::empty()))
            .await;

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.terminal_runtimes.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].zoomed);
        let overlay_pane = app.state.workspaces[0].focused_pane_id().unwrap();
        assert_ne!(overlay_pane, root_pane);

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root_pane));

        app.state.last_pane();

        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(overlay_pane)
        );

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

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_scrollback_key_opens_focused_runtime_scrollback_in_editor_pane() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                5,
                4096,
                b"alpha\nbeta\n",
            ),
        );
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        let output_path = unique_temp_path("edit-scrollback");
        let previous_editor = std::env::var_os("EDITOR");
        std::env::set_var(
            "EDITOR",
            format!("sh -c 'cp \"$1\" {}' sh", output_path.display()),
        );
        app.state.keybinds.edit_scrollback = crate::config::ActionKeybinds::prefix("g");

        app.handle_key(TerminalKey::new(
            app.state.prefix_code,
            app.state.prefix_mods,
        ))
        .await;
        app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
            .await;

        match previous_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }

        let content = wait_for_file(&output_path);
        assert!(content.contains("alpha"));
        assert!(content.contains("beta"));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(
            app.state.terminals.values().any(|terminal| terminal
                .launch_argv
                .as_ref()
                .is_some_and(|argv| argv.first().is_some_and(|program| program == "/bin/sh"))),
            "scrollback editor should launch through argv overlay path"
        );

        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn zoom_action_exits_navigate_mode() {
        let mut state = state_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        state.keybinds.zoom = crate::config::ActionKeybinds::prefix("g");

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
    fn navigate_q_detaches_in_persistence_mode() {
        let mut state = crate::app::state::AppState::test_new();
        state.detach_exits = false;

        handle_navigate_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty()),
        );

        assert!(state.detach_requested);
        assert!(!state.should_quit);
    }
}
