use std::process::Command;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent};
use tracing::error;

use super::{
    api_helpers::{pane_agent_status, tab_attention_priority},
    state::NewWorkspacePickerState,
    App, Mode,
};
use crate::workspace::Workspace;

/// How long to wait for a remote provider's `list_command` before bailing.
/// Synchronous on purpose for v1 — refactor to async if this proves slow.
const REMOTE_LIST_TIMEOUT: Duration = Duration::from_secs(8);

impl App {
    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<std::path::PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.state.terminal_runtimes)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    /// Create a workspace with a real PTY (needs event_tx).
    ///
    /// If at least one `[[remote]]` provider is configured, this opens the
    /// new-workspace type picker. The user can choose `local` (current
    /// behavior) or a provider, in which case the provider's `list_command`
    /// is run and the user picks a target to spawn under `connect_command`.
    pub(crate) fn create_workspace(&mut self) {
        if !self.state.remote_providers.is_empty() {
            self.open_new_workspace_type_picker();
            return;
        }
        self.create_local_workspace();
    }

    fn create_local_workspace(&mut self) {
        let initial_cwd = self
            .workspace_creation_source()
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        if let Err(e) = self.create_workspace_with_options(initial_cwd, true) {
            error!(err = %e, "failed to create workspace");
            self.state.mode = Mode::Navigate;
        }
    }

    fn open_new_workspace_type_picker(&mut self) {
        let mut entries = vec!["local".to_string()];
        entries.extend(self.state.remote_providers.iter().map(|p| p.name.clone()));
        self.state.new_workspace_picker = Some(NewWorkspacePickerState {
            entries,
            highlighted: 0,
            provider_index: None,
            message: None,
        });
        self.state.mode = Mode::NewWorkspaceTypePicker;
    }

    fn open_new_workspace_remote_picker(&mut self, provider_idx: usize) {
        let Some(provider) = self.state.remote_providers.get(provider_idx).cloned() else {
            self.close_new_workspace_picker();
            return;
        };

        let output = Command::new("sh")
            .arg("-c")
            .arg(&provider.list_command)
            .output();

        let (entries, message) = match output {
            Ok(out) if out.status.success() => {
                let entries: Vec<String> = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if entries.is_empty() {
                    (entries, Some("no entries returned".to_string()))
                } else {
                    (entries, None)
                }
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let trimmed = stderr.trim();
                let msg = if trimmed.is_empty() {
                    format!("list_command exited {}", out.status)
                } else {
                    format!("list_command failed: {trimmed}")
                };
                (Vec::new(), Some(msg))
            }
            Err(err) => (Vec::new(), Some(format!("could not run list_command: {err}"))),
        };

        // We don't enforce REMOTE_LIST_TIMEOUT yet (would need a thread or
        // async wrapper); use it as docs for now and keep the constant alive.
        let _ = REMOTE_LIST_TIMEOUT;

        self.state.new_workspace_picker = Some(NewWorkspacePickerState {
            entries,
            highlighted: 0,
            provider_index: Some(provider_idx),
            message,
        });
        self.state.mode = Mode::NewWorkspaceRemotePicker;
    }

    fn close_new_workspace_picker(&mut self) {
        self.state.new_workspace_picker = None;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    fn confirm_type_picker_selection(&mut self) {
        let Some(picker) = self.state.new_workspace_picker.as_ref() else {
            return;
        };
        let highlighted = picker.highlighted;
        // entries[0] is always "local"; entries[1..] mirror remote_providers.
        if highlighted == 0 {
            self.close_new_workspace_picker();
            self.create_local_workspace();
            return;
        }
        let provider_idx = highlighted - 1;
        self.open_new_workspace_remote_picker(provider_idx);
    }

    fn confirm_remote_picker_selection(&mut self) {
        let Some(picker) = self.state.new_workspace_picker.as_ref() else {
            return;
        };
        if picker.entries.is_empty() {
            return;
        }
        let Some(target) = picker.entries.get(picker.highlighted).cloned() else {
            return;
        };
        let Some(provider_idx) = picker.provider_index else {
            return;
        };
        let Some(provider) = self.state.remote_providers.get(provider_idx).cloned() else {
            self.close_new_workspace_picker();
            return;
        };
        self.close_new_workspace_picker();

        let connect = provider.connect_command.replace("{name}", &target);
        let argv = vec!["sh".to_string(), "-c".to_string(), connect.clone()];
        let initial_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        let label = format!("{}: {}", provider.name, target);

        if let Err(e) = self.create_remote_workspace_with_argv(
            initial_cwd,
            argv,
            provider.name.clone(),
            label,
        ) {
            error!(err = %e, "failed to create remote workspace");
            self.state.mode = Mode::Navigate;
        }
    }

    fn create_remote_workspace_with_argv(
        &mut self,
        initial_cwd: std::path::PathBuf,
        argv: Vec<String>,
        provider_name: String,
        label: String,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (mut ws, terminal, runtime) = Workspace::new_argv_command(
            initial_cwd,
            rows,
            cols,
            &argv,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        ws.custom_name = Some(label);
        ws.remote_argv = Some(argv);
        ws.remote_provider_name = Some(provider_name);
        self.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        self.state.switch_workspace(idx);
        self.state.mode = Mode::Terminal;
        self.schedule_session_save();
        Ok(idx)
    }

    pub(crate) fn handle_new_workspace_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.state.new_workspace_picker.as_mut() else {
            self.close_new_workspace_picker();
            return;
        };
        match key.code {
            KeyCode::Esc => self.close_new_workspace_picker(),
            KeyCode::Up | KeyCode::Char('k') => {
                if picker.highlighted > 0 {
                    picker.highlighted -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !picker.entries.is_empty() && picker.highlighted + 1 < picker.entries.len() {
                    picker.highlighted += 1;
                }
            }
            KeyCode::Home => picker.highlighted = 0,
            KeyCode::End => {
                if !picker.entries.is_empty() {
                    picker.highlighted = picker.entries.len() - 1;
                }
            }
            KeyCode::Enter => match self.state.mode {
                Mode::NewWorkspaceTypePicker => self.confirm_type_picker_selection(),
                Mode::NewWorkspaceRemotePicker => self.confirm_remote_picker_selection(),
                _ => {}
            },
            _ => {}
        }
    }

    pub(crate) fn create_tab(&mut self) {
        let custom_name = self.state.requested_new_tab_name.take();
        let initial_cwd = self
            .state
            .active
            .and_then(|ws_idx| self.seed_cwd_from_workspace(ws_idx))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        match self.create_tab_with_options(initial_cwd, true) {
            Ok(tab_idx) => {
                if let Some(name) = custom_name {
                    if let Some(ws) = self
                        .state
                        .active
                        .and_then(|ws_idx| self.state.workspaces.get_mut(ws_idx))
                    {
                        if let Some(tab) = ws.tabs.get_mut(tab_idx) {
                            tab.set_custom_name(name);
                        }
                        self.schedule_session_save();
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "failed to create tab");
            }
        }
    }

    pub(super) fn create_tab_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let Some(ws_idx) = self.state.active else {
            return self.create_workspace_with_options(initial_cwd, focus);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let ws = &mut self.state.workspaces[ws_idx];
        let (idx, terminal, runtime) = if let Some(argv) = ws.remote_argv.clone() {
            ws.create_tab_argv(
                rows,
                cols,
                initial_cwd,
                &argv,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
            )?
        } else {
            ws.create_tab(
                rows,
                cols,
                initial_cwd,
                self.state.pane_scrollback_limit_bytes,
                self.state.host_terminal_theme,
            )?
        };
        self.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        if focus {
            ws.switch_tab(idx);
            self.state.mode = Mode::Terminal;
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, idx)
            .unwrap_or_else(|| format!("{}:{}", workspace_id, idx + 1));
        let root_pane = self.state.workspaces[ws_idx].tabs[idx].root_pane.raw();
        crate::logging::tab_created(&workspace_id, &tab_id, root_pane);
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn create_workspace_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let (ws, terminal, runtime) = Workspace::new(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        self.state
            .terminal_runtimes
            .insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .filter_map(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab_idx + 1,
            label: tab.display_name(),
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: pane_agent_status(agg_state, seen),
        })
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(
                    pane_id,
                    &self.state.terminals,
                    &self.state.terminal_runtimes,
                )
                .map(|cwd| cwd.display().to_string()),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            agent_status: pane_agent_status(terminal.state, pane.seen),
            custom_status: terminal.effective_custom_status().map(str::to_string),
            revision: terminal.revision,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime = self.state.runtime_for_pane_in_workspace(ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state.runtime_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.state.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self
                .public_tab_id(index, ws.active_tab)
                .unwrap_or_else(|| format!("{}:{}", ws.id, ws.active_tab + 1)),
            agent_status: pane_agent_status(agg_state, seen),
        }
    }
}
