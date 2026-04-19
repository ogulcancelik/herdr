use tracing::error;

use super::{pane_agent_status, tab_attention_priority, App, Mode};
use crate::workspace::Workspace;

impl App {
    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<std::path::PathBuf> {
        self.state.workspaces.get(ws_idx)?.resolved_identity_cwd()
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
    pub(crate) fn create_workspace(&mut self) {
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
        let idx = ws.create_tab(
            rows,
            cols,
            initial_cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        )?;
        if focus {
            ws.switch_tab(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn create_workspace_with_options(
        &mut self,
        initial_cwd: std::path::PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.estimate_pane_size();
        let mut ws = Workspace::new(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;
        ws.refresh_git_ahead_behind();
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
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
            .map(|pane| (pane.state, pane.seen))
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
        let runtime = ws.runtime(pane_id);
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: runtime
                .and_then(|rt| rt.cwd())
                .map(|cwd| cwd.display().to_string()),
            agent: pane.effective_agent_label().map(str::to_string),
            agent_status: pane_agent_status(pane.state, pane.seen),
            revision: 0,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::pane::PaneRuntime, String)> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let runtime = ws.runtime(pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::pane::PaneRuntime> {
        let ws = self.state.workspaces.get(ws_idx)?;
        ws.runtime(pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state();
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name(),
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
