use ratatui::layout::Direction;

use super::super::responses::{encode_error, encode_success};
use crate::api::schema::{
    InstalledPluginInfo, PluginInvocationContext, PluginManifestPane, PluginPaneInfo,
    PluginPaneOpenParams, PluginPanePlacement, ResponseResult,
};
use crate::app::App;

impl App {
    pub(super) fn open_plugin_overlay_pane(
        &mut self,
        id: String,
        params: PluginPaneOpenParams,
        plugin: &InstalledPluginInfo,
        pane: PluginManifestPane,
    ) -> String {
        let context = self.current_plugin_context("plugin-pane");
        let extra_env =
            match self.plugin_pane_launch_env(plugin, &pane.id, params.env.clone(), &context) {
                Ok(env) => env,
                Err((code, message)) => return encode_error(id, &code, message),
            };
        let cwd = Some(self.plugin_pane_cwd(plugin, params.cwd));
        let (ws_idx, new_pane) =
            match self.spawn_overlay_argv_command(&pane.command, cwd, extra_env, Vec::new()) {
                Ok(result) => result,
                Err(err) => return encode_error(id, "plugin_pane_open_failed", err.to_string()),
            };
        self.finish_plugin_pane_open(id, ws_idx, None, new_pane, plugin.plugin_id.clone(), pane)
    }

    pub(super) fn open_plugin_split_pane(
        &mut self,
        id: String,
        params: PluginPaneOpenParams,
        plugin: &InstalledPluginInfo,
        pane: PluginManifestPane,
        placement: PluginPanePlacement,
    ) -> String {
        let target_pane_id = params
            .target_pane_id
            .clone()
            .or_else(|| self.current_public_pane_id());
        let Some(target_pane_id) = target_pane_id else {
            return encode_error(id, "no_active_pane", "no active pane");
        };
        let Some((ws_idx, target_pane)) = self.parse_pane_id(&target_pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {target_pane_id} not found"),
            );
        };
        let context = self.plugin_context_for_pane(ws_idx, target_pane, "plugin-pane");
        let extra_env =
            match self.plugin_pane_launch_env(plugin, &pane.id, params.env.clone(), &context) {
                Ok(env) => env,
                Err((code, message)) => return encode_error(id, &code, message),
            };
        let direction = match params
            .direction
            .unwrap_or(crate::api::schema::SplitDirection::Right)
        {
            crate::api::schema::SplitDirection::Right => Direction::Horizontal,
            crate::api::schema::SplitDirection::Down => Direction::Vertical,
        };
        let cwd = Some(self.plugin_pane_cwd(plugin, params.cwd));
        let (rows, cols) = self.state.estimate_pane_size();
        let previous_focus = self.state.current_pane_focus_target();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return encode_error(id, "workspace_not_found", "workspace not found");
        };
        let result = ws.split_pane_argv_command(
            target_pane,
            direction,
            rows.max(4),
            cols.max(10),
            cwd,
            &pane.command,
            extra_env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            params.focus || placement == PluginPanePlacement::Zoomed,
        );
        let (tab_idx, new_pane) = match result {
            Some(Ok(result)) => result,
            Some(Err(err)) => return encode_error(id, "plugin_pane_open_failed", err.to_string()),
            None => {
                return encode_error(
                    id,
                    "pane_not_found",
                    format!("pane {target_pane_id} not found"),
                )
            }
        };
        if params.focus || placement == PluginPanePlacement::Zoomed {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.state.mode = crate::app::Mode::Terminal;
        }
        if placement == PluginPanePlacement::Zoomed {
            if let Some(tab) = self
                .state
                .workspaces
                .get_mut(ws_idx)
                .and_then(|ws| ws.tabs.get_mut(tab_idx))
            {
                tab.zoomed = true;
            }
        }
        self.finish_plugin_pane_open(id, ws_idx, None, new_pane, plugin.plugin_id.clone(), pane)
    }

    pub(super) fn open_plugin_tab(
        &mut self,
        id: String,
        params: PluginPaneOpenParams,
        plugin: &InstalledPluginInfo,
        pane: PluginManifestPane,
    ) -> String {
        let ws_idx = match params.workspace_id.as_deref() {
            Some(workspace_id) => match self.parse_workspace_id(workspace_id) {
                Some(ws_idx) => ws_idx,
                None => return encode_error(id, "workspace_not_found", "workspace not found"),
            },
            None => match self.state.active {
                Some(ws_idx) => ws_idx,
                None => return encode_error(id, "no_active_workspace", "no active workspace"),
            },
        };
        let cwd = self.plugin_pane_cwd(plugin, params.cwd);
        let context = self.plugin_context_for_workspace(ws_idx, "plugin-pane");
        let extra_env =
            match self.plugin_pane_launch_env(plugin, &pane.id, params.env.clone(), &context) {
                Ok(env) => env,
                Err((code, message)) => return encode_error(id, &code, message),
            };
        let (rows, cols) = self.state.estimate_pane_size();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return encode_error(id, "workspace_not_found", "workspace not found");
        };
        let (tab_idx, terminal, runtime) = match ws.create_tab_argv_command(
            rows.max(4),
            cols.max(10),
            cwd,
            &pane.command,
            extra_env,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
        ) {
            Ok(result) => result,
            Err(err) => return encode_error(id, "plugin_pane_open_failed", err.to_string()),
        };
        let pane_id = ws.tabs[tab_idx].root_pane;
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state.mode = crate::app::Mode::Terminal;
        }
        let new_pane = crate::workspace::NewPane {
            pane_id,
            terminal,
            runtime,
        };
        self.finish_plugin_pane_open(
            id,
            ws_idx,
            Some(tab_idx),
            new_pane,
            plugin.plugin_id.clone(),
            pane,
        )
    }

    fn plugin_pane_launch_env(
        &self,
        plugin: &InstalledPluginInfo,
        entrypoint: &str,
        env: std::collections::HashMap<String, String>,
        context: &PluginInvocationContext,
    ) -> Result<Vec<(String, String)>, (String, String)> {
        let mut env = super::super::env::normalize_launch_env(env)?;
        let context_json = serde_json::to_string(&context)
            .map_err(|err| ("invalid_plugin_context".to_string(), err.to_string()))?;
        super::env::ensure_plugin_user_dirs(plugin)
            .map_err(|err| ("plugin_user_dir_create_failed".to_string(), err.to_string()))?;
        env.retain(|(key, _)| !plugin_pane_protected_env_key(key));
        env.extend(super::env::plugin_path_env(plugin));
        env.push((
            crate::api::SOCKET_PATH_ENV_VAR.to_string(),
            crate::api::socket_path().display().to_string(),
        ));
        env.push(("HERDR_ENV".to_string(), "1".to_string()));
        env.push(("HERDR_PLUGIN_ID".to_string(), plugin.plugin_id.clone()));
        env.push((
            "HERDR_PLUGIN_ENTRYPOINT_ID".to_string(),
            entrypoint.to_string(),
        ));
        env.push(("HERDR_PLUGIN_CONTEXT_JSON".to_string(), context_json));
        if let Ok(current_exe) = std::env::current_exe() {
            env.push((
                "HERDR_BIN_PATH".to_string(),
                current_exe.display().to_string(),
            ));
        }
        Ok(env)
    }

    fn finish_plugin_pane_open(
        &mut self,
        id: String,
        ws_idx: usize,
        created_tab_idx: Option<usize>,
        new_pane: crate::workspace::NewPane,
        plugin_id: String,
        pane_manifest: PluginManifestPane,
    ) -> String {
        let entrypoint = pane_manifest.id.clone();
        let mut terminal = new_pane.terminal;
        terminal.set_manual_label(pane_manifest.title.clone());
        let terminal_id = terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state.terminals.insert(terminal_id, terminal);
        self.state.plugin_panes.insert(
            new_pane.pane_id,
            crate::app::state::PluginPaneRecord {
                plugin_id: plugin_id.clone(),
                entrypoint: entrypoint.clone(),
            },
        );
        if let Some(tab_idx) = created_tab_idx {
            if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabCreated,
                    data: crate::api::schema::EventData::TabCreated { tab },
                });
            }
        }
        self.schedule_session_save();
        let Some(pane) = self.pane_info(ws_idx, new_pane.pane_id) else {
            return encode_error(id, "plugin_pane_open_failed", "plugin pane disappeared");
        };
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneCreated,
            data: crate::api::schema::EventData::PaneCreated { pane: pane.clone() },
        });
        encode_success(
            id,
            ResponseResult::PluginPaneOpened {
                plugin_pane: PluginPaneInfo {
                    plugin_id,
                    entrypoint,
                    pane,
                },
            },
        )
    }

    fn plugin_pane_cwd(
        &self,
        plugin: &InstalledPluginInfo,
        override_cwd: Option<String>,
    ) -> std::path::PathBuf {
        override_cwd
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(&plugin.plugin_root))
    }

    fn current_public_pane_id(&self) -> Option<String> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.public_pane_id(ws_idx, pane_id)
    }
}

fn plugin_pane_protected_env_key(key: &str) -> bool {
    matches!(
        key,
        crate::api::SOCKET_PATH_ENV_VAR
            | "HERDR_ENV"
            | "HERDR_PLUGIN_ID"
            | "HERDR_PLUGIN_ROOT"
            | "HERDR_PLUGIN_CONFIG_DIR"
            | "HERDR_PLUGIN_STATE_DIR"
            | "HERDR_PLUGIN_ENTRYPOINT_ID"
            | "HERDR_PLUGIN_CONTEXT_JSON"
            | "HERDR_BIN_PATH"
    )
}
