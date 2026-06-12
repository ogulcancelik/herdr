use std::io::Read;
use std::process::{Command, Stdio};

use ratatui::layout::Direction;

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    InstalledPluginInfo, PluginActionInfo, PluginActionInvokeParams, PluginActionListParams,
    PluginCommandLogInfo, PluginCommandStatus, PluginInvocationContext, PluginLinkParams,
    PluginListParams, PluginLogListParams, PluginManifestAction, PluginManifestEventHook,
    PluginManifestLinkHandler, PluginManifestPane, PluginPaneCloseParams, PluginPaneFocusParams,
    PluginPaneInfo, PluginPaneOpenParams, PluginPanePlacement, PluginPlatform,
    PluginSetEnabledParams, PluginUnlinkParams, ResponseResult,
};
use crate::app::App;

const PLUGIN_ID_MAX_CHARS: usize = 120;
const PLUGIN_ACTION_ID_MAX_CHARS: usize = 120;
const PLUGIN_COMMAND_OUTPUT_MAX_BYTES: usize = 64 * 1024;
impl App {
    pub(super) fn handle_plugin_link(&mut self, id: String, params: PluginLinkParams) -> String {
        let plugin = match load_plugin_manifest(&params.path, params.enabled) {
            Ok(plugin) => plugin,
            Err((code, message)) => return encode_error(id, code, message),
        };
        let previous = self.state.installed_plugins.get(&plugin.plugin_id).cloned();
        self.state
            .installed_plugins
            .insert(plugin.plugin_id.clone(), plugin.clone());
        if let Err(err) = self.save_plugin_registry() {
            match previous {
                Some(previous) => {
                    self.state
                        .installed_plugins
                        .insert(previous.plugin_id.clone(), previous);
                }
                None => {
                    self.state.installed_plugins.remove(&plugin.plugin_id);
                }
            }
            return encode_error(id, "plugin_registry_save_failed", err.to_string());
        }
        encode_success(id, ResponseResult::PluginLinked { plugin })
    }

    pub(super) fn handle_plugin_list(&mut self, id: String, params: PluginListParams) -> String {
        let plugin_id = match params.plugin_id {
            Some(plugin_id) => {
                let Some(plugin_id) = normalize_plugin_id(&plugin_id) else {
                    return invalid_plugin_id(id);
                };
                Some(plugin_id)
            }
            None => None,
        };
        let mut plugins = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| {
                plugin_id
                    .as_deref()
                    .is_none_or(|plugin_id| plugin.plugin_id == plugin_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        plugins.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
        encode_success(id, ResponseResult::PluginList { plugins })
    }

    pub(super) fn handle_plugin_unlink(
        &mut self,
        id: String,
        params: PluginUnlinkParams,
    ) -> String {
        let Some(plugin_id) = normalize_plugin_id(&params.plugin_id) else {
            return invalid_plugin_id(id);
        };
        let previous = self.state.installed_plugins.remove(&plugin_id);
        let removed = previous.is_some();
        let previous_panes = if removed {
            Some(self.state.plugin_panes.clone())
        } else {
            None
        };
        if removed {
            // Drop plugin_panes records for this plugin (panes keep running).
            self.state
                .plugin_panes
                .retain(|_, record| record.plugin_id != plugin_id);
            if let Err(err) = self.save_plugin_registry() {
                if let Some(previous) = previous {
                    self.state
                        .installed_plugins
                        .insert(plugin_id.clone(), previous);
                }
                if let Some(previous_panes) = previous_panes {
                    self.state.plugin_panes = previous_panes;
                }
                return encode_error(id, "plugin_registry_save_failed", err.to_string());
            }
        }
        encode_success(id, ResponseResult::PluginUnlinked { plugin_id, removed })
    }

    pub(super) fn handle_plugin_enable(
        &mut self,
        id: String,
        params: PluginSetEnabledParams,
    ) -> String {
        self.set_plugin_enabled(id, params.plugin_id, true)
    }

    pub(super) fn handle_plugin_disable(
        &mut self,
        id: String,
        params: PluginSetEnabledParams,
    ) -> String {
        self.set_plugin_enabled(id, params.plugin_id, false)
    }

    pub(super) fn handle_plugin_action_list(
        &mut self,
        id: String,
        params: PluginActionListParams,
    ) -> String {
        let plugin_id = match params.plugin_id {
            Some(plugin_id) => {
                let Some(plugin_id) = normalize_plugin_id(&plugin_id) else {
                    return invalid_plugin_id(id);
                };
                Some(plugin_id)
            }
            None => None,
        };
        let mut actions = manifest_actions(&self.state.installed_plugins)
            .filter(|action| {
                plugin_id
                    .as_deref()
                    .is_none_or(|plugin_id| action.plugin_id == plugin_id)
            })
            .collect::<Vec<_>>();
        actions.sort_by_key(|action| action.qualified_id());
        encode_success(id, ResponseResult::PluginActionList { actions })
    }

    pub(super) fn handle_plugin_action_invoke(
        &mut self,
        id: String,
        params: PluginActionInvokeParams,
    ) -> String {
        let (plugin, action) =
            match self.find_plugin_action(params.plugin_id.as_deref(), &params.action_id) {
                Ok(pair) => pair,
                Err((code, message)) => return encode_error(id, code, message),
            };
        if !plugin.enabled {
            return encode_error(
                id,
                "plugin_disabled",
                format!("plugin {} is disabled", plugin.plugin_id),
            );
        }
        let action_manifest = plugin.actions.iter().find(|a| a.id == action.action_id);
        let action_platforms = action_manifest.and_then(|a| a.platforms.clone());
        let eff_platforms = effective_platforms(&action_platforms, &plugin.platforms);
        if let Some(platforms) = eff_platforms {
            let host = current_platform();
            if !platforms.contains(&host) {
                let platform_str = platform_name(host);
                return encode_error(
                    id,
                    "platform_unsupported",
                    format!(
                        "action '{}' does not support the current platform ({})",
                        action.qualified_id(),
                        platform_str
                    ),
                );
            }
        }
        let context = self.merge_plugin_context(params.context, &id);
        let log = match self.start_plugin_command(
            &plugin,
            Some(action.action_id.clone()),
            None,
            action.command.clone(),
            &context,
            None,
        ) {
            Ok(log) => log,
            Err((code, message)) => return encode_error(id, code, message),
        };
        encode_success(
            id,
            ResponseResult::PluginActionInvoked {
                action,
                context,
                log,
            },
        )
    }

    pub(crate) fn invoke_plugin_action_from_keybind(
        &mut self,
        action_id: String,
    ) -> Result<(), String> {
        let (plugin, action) = self
            .find_plugin_action(None, &action_id)
            .map_err(|(_, message)| message)?;
        if !plugin.enabled {
            return Err(format!("plugin {} is disabled", plugin.plugin_id));
        }
        let action_manifest = plugin.actions.iter().find(|a| a.id == action.action_id);
        let action_platforms = action_manifest.and_then(|a| a.platforms.clone());
        let eff_platforms = effective_platforms(&action_platforms, &plugin.platforms);
        ensure_platform_supported(eff_platforms, &action.qualified_id())
            .map_err(|(_, message)| message)?;
        let mut context = self.current_plugin_context("keybinding");
        context.invocation_source = Some("keybinding".to_string());
        self.start_plugin_command(
            &plugin,
            Some(action.action_id),
            None,
            action.command,
            &context,
            None,
        )
        .map(|_| ())
        .map_err(|(_, message)| message)
    }

    pub(crate) fn invoke_plugin_link_handler_for_url(
        &mut self,
        url: &str,
        pane_id: crate::layout::PaneId,
    ) -> Result<bool, String> {
        let Some((plugin, handler)) = self.find_plugin_link_handler(url) else {
            return Ok(false);
        };
        if ensure_platform_supported(
            &effective_platforms(&handler.platforms, &plugin.platforms).clone(),
            &handler.id,
        )
        .is_err()
        {
            return Ok(false);
        }
        let action = plugin
            .actions
            .iter()
            .find(|action| action.id == handler.action)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "plugin {} link handler {} references missing action {}",
                    plugin.plugin_id, handler.id, handler.action
                )
            })?;
        ensure_platform_supported(
            &effective_platforms(&action.platforms, &plugin.platforms).clone(),
            &action.id,
        )
        .map_err(|(_, message)| message)?;
        let Some(ws_idx) = self.state.active else {
            return Ok(false);
        };
        let mut context = self.plugin_context_for_pane(ws_idx, pane_id, "link_click");
        context.invocation_source = Some("link_click".to_string());
        context.clicked_url = Some(url.to_string());
        context.link_handler_id = Some(handler.id);
        self.start_plugin_command(
            &plugin,
            Some(action.id),
            None,
            action.command,
            &context,
            None,
        )
        .map(|_| true)
        .map_err(|(_, message)| message)
    }

    pub(super) fn handle_plugin_log_list(
        &mut self,
        id: String,
        params: PluginLogListParams,
    ) -> String {
        let plugin_id = match params.plugin_id {
            Some(plugin_id) => {
                let Some(plugin_id) = normalize_plugin_id(&plugin_id) else {
                    return invalid_plugin_id(id);
                };
                Some(plugin_id)
            }
            None => None,
        };
        let limit = params.limit.unwrap_or(50).clamp(1, 200);
        let mut logs = self
            .state
            .plugin_command_logs
            .iter()
            .filter(|log| {
                plugin_id
                    .as_deref()
                    .is_none_or(|plugin_id| log.plugin_id == plugin_id)
            })
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        logs.reverse();
        encode_success(id, ResponseResult::PluginLogList { logs })
    }

    pub(super) fn handle_plugin_pane_open(
        &mut self,
        id: String,
        params: PluginPaneOpenParams,
    ) -> String {
        let Some(plugin_id) = normalize_plugin_id(&params.plugin_id) else {
            return invalid_plugin_id(id);
        };
        let Some(plugin) = self.state.installed_plugins.get(&plugin_id).cloned() else {
            return encode_error(id, "plugin_not_found", "plugin not found");
        };
        if !plugin.enabled {
            return encode_error(
                id,
                "plugin_disabled",
                format!("plugin {plugin_id} is disabled"),
            );
        }
        let Some(entrypoint) = normalize_action_id(&params.entrypoint) else {
            return encode_error(id, "invalid_plugin_entrypoint", "invalid entrypoint id");
        };
        let Some(pane) = plugin
            .panes
            .iter()
            .find(|pane| pane.id == entrypoint)
            .cloned()
        else {
            return encode_error(
                id,
                "plugin_pane_not_found",
                format!("plugin pane entrypoint '{entrypoint}' not found"),
            );
        };
        if let Err((code, message)) = ensure_platform_supported(
            effective_platforms(&pane.platforms, &plugin.platforms),
            "plugin pane",
        ) {
            return encode_error(id, code, message);
        }
        let placement = params.placement.unwrap_or(pane.placement);
        match placement {
            PluginPanePlacement::Overlay => {
                if params.workspace_id.is_some()
                    || params.target_pane_id.is_some()
                    || params.direction.is_some()
                {
                    return encode_error(
                        id,
                        "invalid_params",
                        "overlay plugin panes target the active pane",
                    );
                }
            }
            PluginPanePlacement::Split | PluginPanePlacement::Zoomed => {
                if params.workspace_id.is_some() {
                    return encode_error(
                        id,
                        "invalid_params",
                        "split and zoomed plugin panes target an existing pane; use target_pane_id",
                    );
                }
            }
            PluginPanePlacement::Tab => {
                if params.target_pane_id.is_some() || params.direction.is_some() {
                    return encode_error(
                        id,
                        "invalid_params",
                        "tab plugin panes support workspace_id but not target_pane_id or direction",
                    );
                }
            }
        }

        match placement {
            PluginPanePlacement::Overlay => {
                self.open_plugin_overlay_pane(id, params, &plugin, pane)
            }
            PluginPanePlacement::Split | PluginPanePlacement::Zoomed => {
                self.open_plugin_split_pane(id, params, &plugin, pane, placement)
            }
            PluginPanePlacement::Tab => self.open_plugin_tab(id, params, &plugin, pane),
        }
    }

    pub(super) fn handle_plugin_pane_focus(
        &mut self,
        id: String,
        params: PluginPaneFocusParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        };
        if !self.state.plugin_panes.contains_key(&pane_id) {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        }
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.mode = crate::app::Mode::Terminal;
        let Some(record) = self.state.plugin_panes.get(&pane_id).cloned() else {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        };
        encode_success(
            id,
            ResponseResult::PluginPaneFocused {
                plugin_pane: PluginPaneInfo {
                    plugin_id: record.plugin_id,
                    entrypoint: record.entrypoint,
                    pane,
                },
            },
        )
    }

    pub(super) fn handle_plugin_pane_close(
        &mut self,
        id: String,
        params: PluginPaneCloseParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        };
        if !self.state.plugin_panes.contains_key(&pane_id) {
            return encode_error(id, "plugin_pane_not_found", "plugin pane not found");
        }
        let pane_id = params.pane_id;
        let close = self.handle_pane_close(
            id.clone(),
            crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        );
        if serde_json::from_str::<crate::api::schema::ErrorResponse>(&close).is_ok() {
            return close;
        }
        encode_success(id, ResponseResult::PluginPaneClosed { pane_id })
    }

    fn open_plugin_overlay_pane(
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

    fn open_plugin_split_pane(
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

    fn open_plugin_tab(
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
        let mut env = super::env::normalize_launch_env(env)?;
        let context_json = serde_json::to_string(&context)
            .map_err(|err| ("invalid_plugin_context".to_string(), err.to_string()))?;
        env.push(("HERDR_PLUGIN_ID".to_string(), plugin.plugin_id.clone()));
        env.push((
            "HERDR_PLUGIN_ENTRYPOINT_ID".to_string(),
            entrypoint.to_string(),
        ));
        env.push(("HERDR_PLUGIN_CONTEXT_JSON".to_string(), context_json));
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

    fn find_plugin_action(
        &self,
        plugin_id: Option<&str>,
        action_id: &str,
    ) -> Result<(crate::api::schema::InstalledPluginInfo, PluginActionInfo), (&'static str, String)>
    {
        if let Some(plugin_id) = plugin_id {
            let plugin_id = normalize_plugin_id(plugin_id)
                .ok_or_else(|| ("invalid_plugin_id", "invalid plugin id".to_string()))?;
            let action_id = normalize_action_id(action_id)
                .ok_or_else(|| ("invalid_plugin_action_id", "invalid action id".to_string()))?;
            let plugin = self
                .state
                .installed_plugins
                .get(&plugin_id)
                .ok_or_else(|| ("plugin_not_found", "plugin not found".to_string()))?
                .clone();
            let action_info = plugin
                .actions
                .iter()
                .find(|a| a.id == action_id)
                .map(|a| manifest_action_info(&plugin_id, a))
                .ok_or_else(|| {
                    (
                        "plugin_action_not_found",
                        "plugin action not found".to_string(),
                    )
                })?;
            return Ok((plugin, action_info));
        }

        let action_id = action_id.trim();
        let matches = manifest_actions(&self.state.installed_plugins)
            .filter(|action| action.action_id == action_id || action.qualified_id() == action_id)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [action] => {
                let plugin = self
                    .state
                    .installed_plugins
                    .get(&action.plugin_id)
                    .cloned()
                    .ok_or_else(|| ("plugin_not_found", "plugin not found".to_string()))?;
                Ok((plugin, action.clone()))
            }
            [] => Err((
                "plugin_action_not_found",
                "plugin action not found".to_string(),
            )),
            _ => Err((
                "ambiguous_plugin_action",
                "plugin action id matches more than one action; include plugin_id".to_string(),
            )),
        }
    }

    fn find_plugin_link_handler(
        &self,
        url: &str,
    ) -> Option<(InstalledPluginInfo, PluginManifestLinkHandler)> {
        let mut plugins = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| plugin.enabled)
            .cloned()
            .collect::<Vec<_>>();
        plugins.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
        for plugin in plugins {
            for handler in &plugin.link_handlers {
                if ensure_platform_supported(
                    &effective_platforms(&handler.platforms, &plugin.platforms).clone(),
                    &handler.id,
                )
                .is_err()
                {
                    continue;
                }
                let Some(action) = plugin
                    .actions
                    .iter()
                    .find(|action| action.id == handler.action)
                else {
                    continue;
                };
                if ensure_platform_supported(
                    &effective_platforms(&action.platforms, &plugin.platforms).clone(),
                    &action.id,
                )
                .is_err()
                {
                    continue;
                }
                let Ok(regex) = regex::Regex::new(&handler.pattern) else {
                    continue;
                };
                if regex.is_match(url) {
                    return Some((plugin.clone(), handler.clone()));
                }
            }
        }
        None
    }

    fn merge_plugin_context(
        &self,
        provided: Option<PluginInvocationContext>,
        correlation_id: &str,
    ) -> PluginInvocationContext {
        let mut context = self.current_plugin_context(correlation_id);
        if let Some(provided) = provided {
            context.workspace_id = provided.workspace_id.or(context.workspace_id);
            context.workspace_label = provided.workspace_label.or(context.workspace_label);
            context.workspace_cwd = provided.workspace_cwd.or(context.workspace_cwd);
            context.worktree = provided.worktree.or(context.worktree);
            context.tab_id = provided.tab_id.or(context.tab_id);
            context.tab_label = provided.tab_label.or(context.tab_label);
            context.focused_pane_id = provided.focused_pane_id.or(context.focused_pane_id);
            context.focused_pane_cwd = provided.focused_pane_cwd.or(context.focused_pane_cwd);
            context.focused_pane_agent = provided.focused_pane_agent.or(context.focused_pane_agent);
            context.focused_pane_status =
                provided.focused_pane_status.or(context.focused_pane_status);
            context.selected_text = provided.selected_text.or(context.selected_text);
            context.invocation_source = provided.invocation_source.or(context.invocation_source);
            context.correlation_id = provided.correlation_id.or(context.correlation_id);
            context.clicked_url = provided.clicked_url.or(context.clicked_url);
            context.link_handler_id = provided.link_handler_id.or(context.link_handler_id);
        }
        context
    }

    fn current_plugin_context(&self, correlation_id: &str) -> PluginInvocationContext {
        let Some(ws_idx) = self.state.active else {
            return empty_plugin_context(correlation_id);
        };
        self.plugin_context_for_workspace(ws_idx, correlation_id)
    }

    fn plugin_context_for_workspace(
        &self,
        ws_idx: usize,
        correlation_id: &str,
    ) -> PluginInvocationContext {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return empty_plugin_context(correlation_id);
        };
        let workspace = self.workspace_info(ws_idx);
        let tab_idx = ws.active_tab_index();
        let tab_id = self.public_tab_id(ws_idx, tab_idx);
        let tab_label = ws.tabs.get(tab_idx).map(|tab| tab.display_name());
        let focused_pane = ws
            .focused_pane_id()
            .and_then(|pane_id| self.pane_info(ws_idx, pane_id));
        self.plugin_context_from_parts(
            ws_idx,
            workspace,
            tab_id,
            tab_label,
            focused_pane,
            correlation_id,
        )
    }

    fn plugin_context_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        correlation_id: &str,
    ) -> PluginInvocationContext {
        let ws = &self.state.workspaces[ws_idx];
        let workspace = self.workspace_info(ws_idx);
        let tab_idx = ws
            .find_tab_index_for_pane(pane_id)
            .unwrap_or_else(|| ws.active_tab_index());
        let tab_id = self.public_tab_id(ws_idx, tab_idx);
        let tab_label = ws.tabs.get(tab_idx).map(|tab| tab.display_name());
        let focused_pane = self.pane_info(ws_idx, pane_id);
        self.plugin_context_from_parts(
            ws_idx,
            workspace,
            tab_id,
            tab_label,
            focused_pane,
            correlation_id,
        )
    }

    fn plugin_context_from_parts(
        &self,
        ws_idx: usize,
        workspace: crate::api::schema::WorkspaceInfo,
        tab_id: Option<String>,
        tab_label: Option<String>,
        focused_pane: Option<crate::api::schema::PaneInfo>,
        correlation_id: &str,
    ) -> PluginInvocationContext {
        let workspace_cwd = focused_pane
            .as_ref()
            .and_then(|pane| pane.cwd.clone())
            .or_else(|| Some(self.default_cwd_for_workspace(ws_idx).display().to_string()));
        PluginInvocationContext {
            workspace_id: Some(workspace.workspace_id),
            workspace_label: Some(workspace.label),
            workspace_cwd,
            worktree: workspace.worktree,
            tab_id,
            tab_label,
            focused_pane_id: focused_pane.as_ref().map(|pane| pane.pane_id.clone()),
            focused_pane_cwd: focused_pane.as_ref().and_then(|pane| pane.cwd.clone()),
            focused_pane_agent: focused_pane.as_ref().and_then(|pane| pane.agent.clone()),
            focused_pane_status: focused_pane.as_ref().map(|pane| pane.agent_status),
            selected_text: None,
            invocation_source: Some("api".to_string()),
            correlation_id: Some(correlation_id.to_string()),
            clicked_url: None,
            link_handler_id: None,
        }
    }

    fn current_public_pane_id(&self) -> Option<String> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.public_pane_id(ws_idx, pane_id)
    }

    fn default_cwd_for_workspace(&self, ws_idx: usize) -> std::path::PathBuf {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| {
                ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()))
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

    fn set_plugin_enabled(&mut self, id: String, plugin_id: String, enabled: bool) -> String {
        let Some(plugin_id) = normalize_plugin_id(&plugin_id) else {
            return invalid_plugin_id(id);
        };
        let Some(plugin) = self.state.installed_plugins.get_mut(&plugin_id) else {
            return encode_error(id, "plugin_not_found", "plugin not found");
        };
        let previous_enabled = plugin.enabled;
        plugin.enabled = enabled;
        if let Err(err) = self.save_plugin_registry() {
            if let Some(plugin) = self.state.installed_plugins.get_mut(&plugin_id) {
                plugin.enabled = previous_enabled;
            }
            return encode_error(id, "plugin_registry_save_failed", err.to_string());
        }
        let Some(plugin) = self.state.installed_plugins.get(&plugin_id).cloned() else {
            return encode_error(id, "plugin_not_found", "plugin not found");
        };
        if enabled {
            encode_success(id, ResponseResult::PluginEnabled { plugin })
        } else {
            encode_success(id, ResponseResult::PluginDisabled { plugin })
        }
    }

    fn start_plugin_command(
        &mut self,
        plugin: &InstalledPluginInfo,
        action_id: Option<String>,
        event: Option<String>,
        command: Vec<String>,
        context: &PluginInvocationContext,
        event_json: Option<String>,
    ) -> Result<PluginCommandLogInfo, (&'static str, String)> {
        let Some(program) = command.first().cloned() else {
            return Err((
                "invalid_plugin_command",
                "command must not be empty".to_string(),
            ));
        };
        let args = command.iter().skip(1).cloned().collect::<Vec<_>>();
        let context_json = serde_json::to_string(context)
            .map_err(|err| ("invalid_plugin_context", err.to_string()))?;
        let log_id = format!("plugin-log-{}", self.state.next_plugin_command_log_id);
        self.state.next_plugin_command_log_id += 1;
        let started_unix_ms = current_unix_ms();
        let mut env = vec![
            (
                crate::api::SOCKET_PATH_ENV_VAR.to_string(),
                crate::api::socket_path().display().to_string(),
            ),
            ("HERDR_ENV".to_string(), "1".to_string()),
            ("HERDR_PLUGIN_ID".to_string(), plugin.plugin_id.clone()),
            ("HERDR_PLUGIN_CONTEXT_JSON".to_string(), context_json),
        ];
        if let Some(action_id) = action_id.as_ref() {
            env.push(("HERDR_PLUGIN_ACTION_ID".to_string(), action_id.clone()));
        }
        if let Some(event) = event.as_ref() {
            env.push(("HERDR_PLUGIN_EVENT".to_string(), event.clone()));
        }
        if let Some(event_json) = event_json {
            env.push(("HERDR_PLUGIN_EVENT_JSON".to_string(), event_json));
        }
        if let Some(workspace_id) = context.workspace_id.as_ref() {
            env.push(("HERDR_WORKSPACE_ID".to_string(), workspace_id.clone()));
        }
        if let Some(tab_id) = context.tab_id.as_ref() {
            env.push(("HERDR_TAB_ID".to_string(), tab_id.clone()));
        }
        if let Some(pane_id) = context.focused_pane_id.as_ref() {
            env.push(("HERDR_PANE_ID".to_string(), pane_id.clone()));
        }
        if let Some(clicked_url) = context.clicked_url.as_ref() {
            env.push(("HERDR_PLUGIN_CLICKED_URL".to_string(), clicked_url.clone()));
        }
        if let Some(link_handler_id) = context.link_handler_id.as_ref() {
            env.push((
                "HERDR_PLUGIN_LINK_HANDLER_ID".to_string(),
                link_handler_id.clone(),
            ));
        }
        let plugin_root = std::path::PathBuf::from(&plugin.plugin_root);
        let log = PluginCommandLogInfo {
            log_id: log_id.clone(),
            plugin_id: plugin.plugin_id.clone(),
            action_id,
            event,
            command: command.clone(),
            status: PluginCommandStatus::Running,
            started_unix_ms,
            finished_unix_ms: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            error: None,
        };
        self.state.plugin_command_logs.push(log.clone());
        const MAX_PLUGIN_COMMAND_LOGS: usize = 200;
        if self.state.plugin_command_logs.len() > MAX_PLUGIN_COMMAND_LOGS {
            let extra = self.state.plugin_command_logs.len() - MAX_PLUGIN_COMMAND_LOGS;
            self.state.plugin_command_logs.drain(0..extra);
        }
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let child = Command::new(&program)
                .args(args)
                .current_dir(plugin_root)
                .envs(env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let finished = match child {
                Ok(mut child) => {
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    let stdout_reader = stdout.map(|stdout| {
                        std::thread::spawn(move || {
                            read_capped_plugin_output(stdout, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
                        })
                    });
                    let stderr_reader = stderr.map(|stderr| {
                        std::thread::spawn(move || {
                            read_capped_plugin_output(stderr, PLUGIN_COMMAND_OUTPUT_MAX_BYTES)
                        })
                    });
                    match child.wait() {
                        Ok(status) => crate::events::AppEvent::PluginCommandFinished {
                            log_id,
                            finished_unix_ms: current_unix_ms(),
                            exit_code: status.code(),
                            stdout: stdout_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            stderr: stderr_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            error: None,
                        },
                        Err(err) => crate::events::AppEvent::PluginCommandFinished {
                            log_id,
                            finished_unix_ms: current_unix_ms(),
                            exit_code: None,
                            stdout: stdout_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            stderr: stderr_reader
                                .and_then(|reader| reader.join().ok())
                                .unwrap_or_default(),
                            error: Some(err.to_string()),
                        },
                    }
                }
                Err(err) => crate::events::AppEvent::PluginCommandFinished {
                    log_id,
                    finished_unix_ms: current_unix_ms(),
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(err.to_string()),
                },
            };
            let _ = event_tx.blocking_send(finished);
        });
        Ok(log)
    }

    pub(crate) fn run_plugin_event_hooks(&mut self, event: &crate::api::schema::EventEnvelope) {
        let event_name = event_kind_name(event.event);
        let event_json = serde_json::to_string(event).ok();
        let context = self.current_plugin_context(event_name);
        let plugins = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| plugin.enabled)
            .cloned()
            .collect::<Vec<_>>();
        for plugin in plugins {
            for hook in plugin.events.clone() {
                if hook.on != event_name {
                    continue;
                }
                if ensure_platform_supported(
                    &effective_platforms(&hook.platforms, &plugin.platforms).clone(),
                    event_name,
                )
                .is_err()
                {
                    continue;
                }
                let _ = self.start_plugin_command(
                    &plugin,
                    None,
                    Some(event_name.to_string()),
                    hook.command.clone(),
                    &context,
                    event_json.clone(),
                );
            }
        }
    }

    pub(crate) fn save_plugin_registry(&self) -> std::io::Result<()> {
        if self.no_session {
            return Ok(());
        }
        let plugins = self
            .state
            .installed_plugins
            .values()
            .cloned()
            .collect::<Vec<_>>();
        crate::persist::plugin_registry::save(&plugins)
    }
}

#[derive(serde::Deserialize)]
struct RawPluginManifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    actions: Vec<RawPluginManifestAction>,
    #[serde(default)]
    events: Vec<RawPluginManifestEventHook>,
    #[serde(default)]
    panes: Vec<RawPluginManifestPane>,
    #[serde(default)]
    link_handlers: Vec<RawPluginManifestLinkHandler>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestAction {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<crate::api::schema::PluginActionContext>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestEventHook {
    on: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestPane {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    placement: PluginPanePlacement,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestLinkHandler {
    id: String,
    title: String,
    pattern: String,
    action: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
}

/// Raw string platform value from the manifest, validated before conversion.
#[derive(serde::Deserialize)]
#[serde(try_from = "String")]
struct RawPlatform(PluginPlatform);

impl TryFrom<String> for RawPlatform {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "linux" => Ok(RawPlatform(PluginPlatform::Linux)),
            "macos" => Ok(RawPlatform(PluginPlatform::Macos)),
            "windows" => Ok(RawPlatform(PluginPlatform::Windows)),
            other => Err(format!(
                "invalid_plugin_platform: unknown platform '{other}'"
            )),
        }
    }
}

pub(crate) fn load_plugin_manifest(
    path: &str,
    enabled: bool,
) -> Result<InstalledPluginInfo, (&'static str, String)> {
    let path = std::path::PathBuf::from(path);
    let manifest_path = if path.is_dir() {
        path.join("herdr-plugin.toml")
    } else {
        path
    };
    let manifest_path = manifest_path
        .canonicalize()
        .map_err(|err| ("plugin_manifest_not_found", err.to_string()))?;
    let plugin_root = manifest_path
        .parent()
        .ok_or_else(|| {
            (
                "invalid_plugin_manifest_path",
                "manifest path has no parent directory".to_string(),
            )
        })?
        .to_path_buf();
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|err| ("plugin_manifest_read_failed", err.to_string()))?;
    let raw: RawPluginManifest = toml::from_str(&content)
        .map_err(|err| ("plugin_manifest_parse_failed", err.to_string()))?;
    let plugin_id = normalize_plugin_id(&raw.id)
        .ok_or_else(|| ("invalid_plugin_id", "invalid plugin id".to_string()))?;
    let name = non_empty_trimmed(&raw.name, "invalid_plugin_name", "plugin name is required")?;
    let version = non_empty_trimmed(
        &raw.version,
        "invalid_plugin_version",
        "plugin version is required",
    )?;
    let description = raw
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(raw.platforms)?;
    let mut actions = raw
        .actions
        .into_iter()
        .map(normalize_manifest_action)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_action_ids(&actions)?;
    actions.sort_by(|a, b| a.id.cmp(&b.id));
    let mut events = raw
        .events
        .into_iter()
        .map(normalize_manifest_event)
        .collect::<Result<Vec<_>, _>>()?;
    events.sort_by(|a, b| a.on.cmp(&b.on).then_with(|| a.command.cmp(&b.command)));
    let mut panes = raw
        .panes
        .into_iter()
        .map(normalize_manifest_pane)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_pane_ids(&panes)?;
    panes.sort_by(|a, b| a.id.cmp(&b.id));
    let link_handlers = raw
        .link_handlers
        .into_iter()
        .map(normalize_manifest_link_handler)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_link_handler_ids(&link_handlers)?;
    validate_link_handler_actions(&link_handlers, &actions)?;

    let mut warnings = validate_event_names(&events);
    if platforms.is_none() {
        warnings.push("manifest does not declare platforms; platform support unknown".to_string());
    }

    Ok(InstalledPluginInfo {
        plugin_id,
        name,
        version,
        description,
        manifest_path: manifest_path.display().to_string(),
        plugin_root: plugin_root.display().to_string(),
        enabled,
        platforms,
        actions,
        events,
        panes,
        link_handlers,
        warnings,
    })
}

fn reject_duplicate_action_ids(
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        if !seen.insert(action.id.as_str()) {
            return Err((
                "duplicate_plugin_action_id",
                format!("duplicate action id '{}'", action.id),
            ));
        }
    }
    Ok(())
}

fn validate_event_names(events: &[crate::api::schema::PluginManifestEventHook]) -> Vec<String> {
    let known = crate::api::schema::known_event_names();
    events
        .iter()
        .filter(|hook| !known.contains(&hook.on.as_str()))
        .map(|hook| format!("unknown event '{}'", hook.on))
        .collect()
}

fn reject_duplicate_pane_ids(panes: &[PluginManifestPane]) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for pane in panes {
        if !seen.insert(pane.id.as_str()) {
            return Err((
                "duplicate_plugin_pane_id",
                format!("duplicate pane id '{}'", pane.id),
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_link_handler_ids(
    handlers: &[PluginManifestLinkHandler],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for handler in handlers {
        if !seen.insert(handler.id.as_str()) {
            return Err((
                "duplicate_plugin_link_handler_id",
                format!("duplicate link handler id '{}'", handler.id),
            ));
        }
    }
    Ok(())
}

fn validate_link_handler_actions(
    handlers: &[PluginManifestLinkHandler],
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    for handler in handlers {
        if !actions.iter().any(|action| action.id == handler.action) {
            return Err((
                "invalid_plugin_link_handler_action",
                format!(
                    "link handler '{}' references unknown action '{}'",
                    handler.id, handler.action
                ),
            ));
        }
    }
    Ok(())
}

fn normalize_manifest_action(
    action: RawPluginManifestAction,
) -> Result<PluginManifestAction, (&'static str, String)> {
    let id = normalize_action_id(&action.id)
        .ok_or_else(|| ("invalid_plugin_action_id", "invalid action id".to_string()))?;
    let title = non_empty_trimmed(
        &action.title,
        "invalid_plugin_action_title",
        "action title is required",
    )?;
    let description = action
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(action.platforms)?;
    let command = normalize_command(action.command)?;
    Ok(PluginManifestAction {
        id,
        title,
        description,
        contexts: action.contexts,
        platforms,
        command,
    })
}

fn normalize_manifest_pane(
    pane: RawPluginManifestPane,
) -> Result<PluginManifestPane, (&'static str, String)> {
    let id = normalize_action_id(&pane.id)
        .ok_or_else(|| ("invalid_plugin_pane_id", "invalid pane id".to_string()))?;
    let title = non_empty_trimmed(
        &pane.title,
        "invalid_plugin_pane_title",
        "pane title is required",
    )?;
    let description = pane
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(pane.platforms)?;
    let command = normalize_command(pane.command)?;
    Ok(PluginManifestPane {
        id,
        title,
        description,
        platforms,
        placement: pane.placement,
        command,
    })
}

fn normalize_manifest_event(
    event: RawPluginManifestEventHook,
) -> Result<PluginManifestEventHook, (&'static str, String)> {
    let on = non_empty_trimmed(&event.on, "invalid_plugin_event", "event name is required")?;
    let platforms = normalize_platforms(event.platforms)?;
    let command = normalize_command(event.command)?;
    Ok(PluginManifestEventHook {
        on,
        platforms,
        command,
    })
}

fn normalize_manifest_link_handler(
    handler: RawPluginManifestLinkHandler,
) -> Result<PluginManifestLinkHandler, (&'static str, String)> {
    let id = normalize_action_id(&handler.id).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_id",
            "invalid link handler id".to_string(),
        )
    })?;
    let title = non_empty_trimmed(
        &handler.title,
        "invalid_plugin_link_handler_title",
        "link handler title is required",
    )?;
    let pattern = non_empty_trimmed(
        &handler.pattern,
        "invalid_plugin_link_handler_pattern",
        "link handler pattern is required",
    )?;
    regex::Regex::new(&pattern)
        .map_err(|err| ("invalid_plugin_link_handler_pattern", err.to_string()))?;
    let action = normalize_action_id(&handler.action).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_action",
            "invalid link handler action".to_string(),
        )
    })?;
    let platforms = normalize_platforms(handler.platforms)?;
    Ok(PluginManifestLinkHandler {
        id,
        title,
        pattern,
        action,
        platforms,
    })
}

fn normalize_platforms(
    raw: Option<Vec<RawPlatform>>,
) -> Result<Option<Vec<PluginPlatform>>, (&'static str, String)> {
    match raw {
        None => Ok(None),
        Some(list) if list.is_empty() => Err((
            "invalid_plugin_platform",
            "platforms must not be an empty array; omit the field to leave platforms undeclared"
                .to_string(),
        )),
        Some(list) => Ok(Some(list.into_iter().map(|p| p.0).collect())),
    }
}

/// Returns the platform the current binary was compiled for.
fn current_platform() -> PluginPlatform {
    if cfg!(target_os = "linux") {
        PluginPlatform::Linux
    } else if cfg!(target_os = "macos") {
        PluginPlatform::Macos
    } else {
        PluginPlatform::Windows
    }
}

/// Resolve the effective platforms for an action or event: use the item's own
/// platforms if declared, otherwise inherit from the plugin-level platforms.
/// Returns a reference to whichever `Option<Vec<PluginPlatform>>` applies.
fn effective_platforms<'a>(
    item_platforms: &'a Option<Vec<PluginPlatform>>,
    plugin_platforms: &'a Option<Vec<PluginPlatform>>,
) -> &'a Option<Vec<PluginPlatform>> {
    if item_platforms.is_some() {
        item_platforms
    } else {
        plugin_platforms
    }
}

fn ensure_platform_supported(
    platforms: &Option<Vec<PluginPlatform>>,
    subject: &str,
) -> Result<(), (&'static str, String)> {
    if let Some(platforms) = platforms {
        let host = current_platform();
        if !platforms.contains(&host) {
            return Err((
                "platform_unsupported",
                format!(
                    "{subject} does not support the current platform ({})",
                    platform_name(host)
                ),
            ));
        }
    }
    Ok(())
}

fn platform_name(p: PluginPlatform) -> &'static str {
    match p {
        PluginPlatform::Linux => "linux",
        PluginPlatform::Macos => "macos",
        PluginPlatform::Windows => "windows",
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn empty_plugin_context(correlation_id: &str) -> PluginInvocationContext {
    PluginInvocationContext {
        workspace_id: None,
        workspace_label: None,
        workspace_cwd: None,
        worktree: None,
        tab_id: None,
        tab_label: None,
        focused_pane_id: None,
        focused_pane_cwd: None,
        focused_pane_agent: None,
        focused_pane_status: None,
        selected_text: None,
        invocation_source: Some("api".to_string()),
        correlation_id: Some(correlation_id.to_string()),
        clicked_url: None,
        link_handler_id: None,
    }
}

fn read_capped_plugin_output(mut reader: impl Read, cap: usize) -> String {
    let mut kept = Vec::with_capacity(cap.min(8192));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = cap.saturating_sub(kept.len());
                if remaining > 0 {
                    kept.extend_from_slice(&buf[..n.min(remaining)]);
                }
                if n > remaining {
                    truncated = true;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let mut output = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        output.push_str(&format!(
            "\n[herdr truncated plugin output after {cap} bytes]"
        ));
    }
    output
}

fn event_kind_name(kind: crate::api::schema::EventKind) -> &'static str {
    match kind {
        crate::api::schema::EventKind::WorkspaceCreated => "workspace.created",
        crate::api::schema::EventKind::WorkspaceUpdated => "workspace.updated",
        crate::api::schema::EventKind::WorkspaceClosed => "workspace.closed",
        crate::api::schema::EventKind::WorkspaceRenamed => "workspace.renamed",
        crate::api::schema::EventKind::WorkspaceFocused => "workspace.focused",
        crate::api::schema::EventKind::WorktreeCreated => "worktree.created",
        crate::api::schema::EventKind::WorktreeOpened => "worktree.opened",
        crate::api::schema::EventKind::WorktreeRemoved => "worktree.removed",
        crate::api::schema::EventKind::TabCreated => "tab.created",
        crate::api::schema::EventKind::TabClosed => "tab.closed",
        crate::api::schema::EventKind::TabRenamed => "tab.renamed",
        crate::api::schema::EventKind::TabFocused => "tab.focused",
        crate::api::schema::EventKind::PaneCreated => "pane.created",
        crate::api::schema::EventKind::PaneClosed => "pane.closed",
        crate::api::schema::EventKind::PaneFocused => "pane.focused",
        crate::api::schema::EventKind::PaneMoved => "pane.moved",
        crate::api::schema::EventKind::PaneOutputChanged => "pane.output_changed",
        crate::api::schema::EventKind::PaneExited => "pane.exited",
        crate::api::schema::EventKind::PaneAgentDetected => "pane.agent_detected",
        crate::api::schema::EventKind::PaneAgentStatusChanged => "pane.agent_status_changed",
    }
}

fn normalize_command(command: Vec<String>) -> Result<Vec<String>, (&'static str, String)> {
    let command = command
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .collect::<Vec<_>>();
    if command.is_empty() || command.iter().any(|arg| arg.is_empty()) {
        return Err((
            "invalid_plugin_command",
            "command must contain non-empty argv strings".to_string(),
        ));
    }
    Ok(command)
}

fn non_empty_trimmed(
    value: &str,
    code: &'static str,
    message: &'static str,
) -> Result<String, (&'static str, String)> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err((code, message.to_string()))
    } else {
        Ok(value)
    }
}

fn normalize_plugin_id(value: &str) -> Option<String> {
    normalize_identifier(value, PLUGIN_ID_MAX_CHARS)
}

fn normalize_action_id(value: &str) -> Option<String> {
    normalize_identifier(value, PLUGIN_ACTION_ID_MAX_CHARS)
}

fn normalize_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-')))
    .then(|| value.to_string())
}

fn invalid_plugin_id(id: String) -> String {
    encode_error(
        id,
        "invalid_plugin_id",
        "plugin id must be non-empty, <= 120 characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen",
    )
}

fn manifest_action_info(plugin_id: &str, action: &PluginManifestAction) -> PluginActionInfo {
    PluginActionInfo {
        plugin_id: plugin_id.to_string(),
        action_id: action.id.clone(),
        title: action.title.clone(),
        description: action.description.clone(),
        contexts: action.contexts.clone(),
        command: action.command.clone(),
    }
}

fn manifest_actions(
    plugins: &crate::app::state::InstalledPluginRegistry,
) -> impl Iterator<Item = PluginActionInfo> + '_ {
    plugins.values().flat_map(|plugin| {
        plugin
            .actions
            .iter()
            .map(|action| manifest_action_info(&plugin.plugin_id, action))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request, SuccessResponse};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn response_result(response: &str) -> ResponseResult {
        serde_json::from_str::<SuccessResponse>(response)
            .expect("success response")
            .result
    }

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn write_manifest(root: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let manifest = root.join("herdr-plugin.toml");
        std::fs::write(
            &manifest,
            r#"
id = "example.worktree-bootstrap"
name = "Worktree Bootstrap"
version = "0.1.0"
description = "Prepare new worktrees"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "bootstrap"
title = "Bootstrap worktree"
contexts = ["workspace"]
command = ["bun", "run", "bootstrap.ts"]

[[events]]
on = "worktree.created"
command = ["bun", "run", "bootstrap.ts"]

[[panes]]
id = "board"
title = "Worktree board"
command = ["bun", "run", "board.ts"]

[[link_handlers]]
id = "github-pr"
title = "Open GitHub PR"
pattern = "^https://github\\.com/[^/]+/[^/]+/(issues|pull)/[0-9]+$"
action = "bootstrap"
"#,
        )
        .unwrap();
        manifest
    }

    fn write_manifest_content(root: &std::path::Path, content: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let manifest = root.join("herdr-plugin.toml");
        std::fs::write(&manifest, content).unwrap();
        manifest
    }

    fn link_manifest(app: &mut App, root: &std::path::Path) {
        let result = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        assert!(
            result.contains("plugin_linked"),
            "expected plugin_linked: {result}"
        );
    }

    #[test]
    fn plugin_link_lists_and_unlinks_manifest() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-link");
        write_manifest(&root);

        let link = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        let ResponseResult::PluginLinked { plugin } = response_result(&link) else {
            panic!("expected plugin linked response: {link}");
        };
        assert_eq!(plugin.plugin_id, "example.worktree-bootstrap");
        assert_eq!(plugin.name, "Worktree Bootstrap");
        assert_eq!(plugin.version, "0.1.0");
        assert_eq!(plugin.plugin_root, root.display().to_string());
        assert!(plugin.enabled);
        assert_eq!(plugin.actions.len(), 1);
        assert_eq!(plugin.actions[0].id, "bootstrap");
        assert_eq!(plugin.actions[0].command, ["bun", "run", "bootstrap.ts"]);
        assert_eq!(plugin.events.len(), 1);
        assert_eq!(plugin.events[0].on, "worktree.created");
        assert_eq!(plugin.panes.len(), 1);
        assert_eq!(plugin.panes[0].id, "board");
        assert_eq!(plugin.panes[0].placement, PluginPanePlacement::Overlay);
        assert_eq!(plugin.link_handlers.len(), 1);
        assert_eq!(plugin.link_handlers[0].id, "github-pr");
        assert_eq!(plugin.link_handlers[0].action, "bootstrap");

        let list = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::PluginList(PluginListParams { plugin_id: None }),
        });
        let ResponseResult::PluginList { plugins } = response_result(&list) else {
            panic!("expected plugin list response: {list}");
        };
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].plugin_id, "example.worktree-bootstrap");

        let unlink = app.handle_api_request(Request {
            id: "unlink".into(),
            method: Method::PluginUnlink(PluginUnlinkParams {
                plugin_id: "example.worktree-bootstrap".into(),
            }),
        });
        assert!(matches!(
            response_result(&unlink),
            ResponseResult::PluginUnlinked {
                plugin_id,
                removed: true
            } if plugin_id == "example.worktree-bootstrap"
        ));

        let list = app.handle_api_request(Request {
            id: "list-empty".into(),
            method: Method::PluginList(PluginListParams { plugin_id: None }),
        });
        let ResponseResult::PluginList { plugins } = response_result(&list) else {
            panic!("expected plugin list response: {list}");
        };
        assert!(plugins.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn link_rejects_duplicate_action_ids() {
        let root = unique_temp_path("plugin-duplicate-action");
        write_manifest_content(
            &root,
            r#"
id = "example.duplicate"
name = "Duplicate"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "run"
title = "Run"
command = ["echo", "a"]

[[actions]]
id = "run"
title = "Run again"
command = ["echo", "b"]
"#,
        );

        let result = load_plugin_manifest(&root.display().to_string(), true);
        assert!(matches!(result, Err(("duplicate_plugin_action_id", _))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn link_rejects_duplicate_pane_ids() {
        let root = unique_temp_path("plugin-duplicate-pane");
        write_manifest_content(
            &root,
            r#"
id = "example.duplicate-pane"
name = "Duplicate Pane"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[panes]]
id = "ui"
title = "UI"
command = ["echo", "a"]

[[panes]]
id = "ui"
title = "UI again"
command = ["echo", "b"]
"#,
        );

        let result = load_plugin_manifest(&root.display().to_string(), true);
        assert!(matches!(result, Err(("duplicate_plugin_pane_id", _))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn smoke_fixture_manifest_loads() {
        let plugin = load_plugin_manifest("tests/fixtures/plugin-smoke", true)
            .expect("smoke fixture should load");
        assert_eq!(plugin.plugin_id, "example.smoke");
        assert_eq!(plugin.actions.len(), 1);
        assert_eq!(plugin.events.len(), 1);
        assert_eq!(plugin.panes.len(), 1);
        assert!(plugin.warnings.is_empty());
    }

    #[test]
    fn plugin_command_output_reader_caps_and_marks_truncation() {
        let output = read_capped_plugin_output("abcdef".as_bytes(), 3);

        assert_eq!(output, "abc\n[herdr truncated plugin output after 3 bytes]");
    }

    #[test]
    fn plugin_enable_disable_updates_registry_state() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-enable-disable");
        write_manifest(&root);
        link_manifest(&mut app, &root);

        let disabled = app.handle_api_request(Request {
            id: "disable".into(),
            method: Method::PluginDisable(PluginSetEnabledParams {
                plugin_id: "example.worktree-bootstrap".into(),
            }),
        });
        let ResponseResult::PluginDisabled { plugin } = response_result(&disabled) else {
            panic!("expected disabled response: {disabled}");
        };
        assert!(!plugin.enabled);

        let enabled = app.handle_api_request(Request {
            id: "enable".into(),
            method: Method::PluginEnable(PluginSetEnabledParams {
                plugin_id: "example.worktree-bootstrap".into(),
            }),
        });
        let ResponseResult::PluginEnabled { plugin } = response_result(&enabled) else {
            panic!("expected enabled response: {enabled}");
        };
        assert!(plugin.enabled);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_pane_open_requires_installed_plugin() {
        let mut app = test_app();
        let response = app.handle_api_request(Request {
            id: "pane-open".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.missing".into(),
                entrypoint: "ui".into(),
                placement: Some(PluginPanePlacement::Split),
                workspace_id: None,
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: false,
                env: std::collections::HashMap::new(),
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "plugin_not_found");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_pane_open_uses_plugin_root_title_env_and_target_context() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("plugin-target");
        workspace.custom_name = None;
        let root_pane = workspace.tabs[0].root_pane;
        let root_terminal = workspace.terminal_id(root_pane).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        app.state.terminals.get_mut(&root_terminal).unwrap().cwd = "/tmp".into();
        let target_public_pane_id = app.public_pane_id(0, root_pane).unwrap();

        let root = unique_temp_path("plugin-pane-open");
        let capture = root.join("capture.txt");
        write_manifest_content(
            &root,
            &format!(
                r#"
id = "example.pane"
name = "Pane Plugin"
version = "0.1.0"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
command = ["sh", "-c", "printf '%s\n%s\n%s\n%s\n%s\n' \"$PWD\" \"$HERDR_PLUGIN_ENTRYPOINT_ID\" \"$HERDR_WORKSPACE_ID\" \"$HERDR_PANE_ID\" \"$HERDR_PLUGIN_CONTEXT_JSON\" > {}"]
"#,
                capture.display()
            ),
        );
        link_manifest(&mut app, &root);

        let open = app.handle_api_request(Request {
            id: "pane-open".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
                placement: Some(PluginPanePlacement::Overlay),
                workspace_id: None,
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: true,
                env: std::collections::HashMap::new(),
            }),
        });
        let ResponseResult::PluginPaneOpened { plugin_pane } = response_result(&open) else {
            panic!("expected plugin pane opened response: {open}");
        };
        assert_eq!(plugin_pane.plugin_id, "example.pane");
        assert_eq!(plugin_pane.entrypoint, "board");
        assert_eq!(plugin_pane.pane.label.as_deref(), Some("Plugin Board"));
        let Some((_, opened_pane_id)) = app.parse_pane_id(&plugin_pane.pane.pane_id) else {
            panic!("opened pane id should parse");
        };
        assert!(app.state.plugin_panes.contains_key(&opened_pane_id));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !capture.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let text = std::fs::read_to_string(&capture).expect("plugin pane command should write env");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(root.display().to_string().as_str()));
        assert_eq!(lines.next(), Some("board"));
        assert_eq!(lines.next(), Some(plugin_pane.pane.workspace_id.as_str()));
        assert_eq!(lines.next(), Some(plugin_pane.pane.pane_id.as_str()));
        let context: PluginInvocationContext =
            serde_json::from_str(lines.next().expect("context json")).unwrap();
        assert_eq!(
            context.workspace_id.as_deref(),
            Some(plugin_pane.pane.workspace_id.as_str())
        );
        assert_eq!(
            context.focused_pane_id.as_deref(),
            Some(target_public_pane_id.as_str())
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_pane_open_tab_emits_tab_created_before_pane_created() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-tab")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;

        let root = unique_temp_path("plugin-pane-tab-events");
        write_manifest_content(
            &root,
            r#"
id = "example.tab"
name = "Tab Plugin"
version = "0.1.0"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
placement = "tab"
command = ["sh", "-c", "sleep 1"]
"#,
        );
        link_manifest(&mut app, &root);

        let open = app.handle_api_request(Request {
            id: "pane-open-tab".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.tab".into(),
                entrypoint: "board".into(),
                placement: Some(PluginPanePlacement::Tab),
                workspace_id: None,
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: true,
                env: std::collections::HashMap::new(),
            }),
        });
        let ResponseResult::PluginPaneOpened { .. } = response_result(&open) else {
            panic!("expected plugin pane opened response: {open}");
        };

        let events = event_hub
            .events_after(0)
            .into_iter()
            .map(|(_, event)| event.event)
            .collect::<Vec<_>>();
        let tab_created = events
            .iter()
            .position(|event| *event == crate::api::schema::EventKind::TabCreated)
            .expect("tab.created should be emitted");
        let pane_created = events
            .iter()
            .position(|event| *event == crate::api::schema::EventKind::PaneCreated)
            .expect("pane.created should be emitted");
        assert!(tab_created < pane_created);

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_action_list_and_invoke_with_context() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-action-list");
        write_manifest(&root);
        link_manifest(&mut app, &root);

        let list = app.handle_api_request(Request {
            id: "list".into(),
            method: Method::PluginActionList(PluginActionListParams { plugin_id: None }),
        });
        let ResponseResult::PluginActionList { actions } = response_result(&list) else {
            panic!("expected plugin action list: {list}");
        };
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].qualified_id(),
            "example.worktree-bootstrap.bootstrap"
        );
        assert_eq!(actions[0].command, ["bun", "run", "bootstrap.ts"]);

        let invoke = app.handle_api_request(Request {
            id: "invoke".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.worktree-bootstrap".into()),
                action_id: "bootstrap".into(),
                context: Some(PluginInvocationContext {
                    workspace_id: Some("1".into()),
                    workspace_label: None,
                    workspace_cwd: None,
                    worktree: None,
                    tab_id: None,
                    tab_label: None,
                    focused_pane_id: None,
                    focused_pane_cwd: None,
                    focused_pane_agent: None,
                    focused_pane_status: None,
                    selected_text: None,
                    invocation_source: Some("test".into()),
                    correlation_id: Some("external-correlation".into()),
                    clicked_url: None,
                    link_handler_id: None,
                }),
            }),
        });
        let ResponseResult::PluginActionInvoked {
            action,
            context,
            log,
        } = response_result(&invoke)
        else {
            panic!("expected plugin action invocation: {invoke}");
        };
        assert_eq!(
            action.qualified_id(),
            "example.worktree-bootstrap.bootstrap"
        );
        assert_eq!(action.command, ["bun", "run", "bootstrap.ts"]);
        assert_eq!(log.plugin_id, "example.worktree-bootstrap");
        assert_eq!(log.action_id.as_deref(), Some("bootstrap"));
        assert_eq!(context.workspace_id.as_deref(), Some("1"));
        assert_eq!(context.invocation_source.as_deref(), Some("test"));
        assert_eq!(
            context.correlation_id.as_deref(),
            Some("external-correlation")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn manifest_action_invoke_runs_command_and_captures_log() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-action-runner");
        write_manifest_content(
            &root,
            r#"
id = "example.runner"
name = "Runner"
version = "0.1.0"
platforms = ["linux", "macos"]

[[actions]]
id = "run"
title = "Run"
command = ["sh", "-c", "printf '%s' \"$HERDR_PLUGIN_ACTION_ID\""]
"#,
        );
        link_manifest(&mut app, &root);

        let invoke = app.handle_api_request(Request {
            id: "invoke-runner".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.runner".into()),
                action_id: "run".into(),
                context: None,
            }),
        });
        let ResponseResult::PluginActionInvoked { log, .. } = response_result(&invoke) else {
            panic!("expected plugin action invocation: {invoke}");
        };
        assert_eq!(log.status, PluginCommandStatus::Running);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.drain_all_internal_events();
            if app.state.plugin_command_logs.iter().any(|entry| {
                entry.log_id == log.log_id && entry.status != PluginCommandStatus::Running
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let logs = app.handle_api_request(Request {
            id: "logs".into(),
            method: Method::PluginLogList(PluginLogListParams {
                plugin_id: Some("example.runner".into()),
                limit: Some(10),
            }),
        });
        let ResponseResult::PluginLogList { logs } = response_result(&logs) else {
            panic!("expected plugin logs: {logs}");
        };
        let finished = logs
            .iter()
            .find(|entry| entry.log_id == log.log_id)
            .expect("log should exist");
        assert_eq!(finished.status, PluginCommandStatus::Succeeded);
        assert_eq!(finished.stdout.as_deref(), Some("run"));
        assert_eq!(finished.exit_code, Some(0));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn plugin_link_handler_invokes_action_with_clicked_url_context() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("link-handler")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let root = unique_temp_path("plugin-link-handler");
        write_manifest_content(
            &root,
            r#"
id = "example.links"
name = "Links"
version = "0.1.0"
platforms = ["linux", "macos"]

[[actions]]
id = "open"
title = "Open link"
command = ["sh", "-c", "printf '%s|%s' \"$HERDR_PLUGIN_LINK_HANDLER_ID\" \"$HERDR_PLUGIN_CLICKED_URL\""]

[[link_handlers]]
id = "github-issue"
title = "Open GitHub issue"
pattern = "^https://github\\.com/[^/]+/[^/]+/(issues|pull)/[0-9]+$"
action = "open"
"#,
        );
        link_manifest(&mut app, &root);

        let handled = app
            .invoke_plugin_link_handler_for_url(
                "https://github.com/ogulcancelik/herdr/issues/398",
                pane_id,
            )
            .expect("link handler should invoke");
        assert!(handled);
        let log = app
            .state
            .plugin_command_logs
            .last()
            .expect("plugin command log should be recorded")
            .clone();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            app.drain_all_internal_events();
            if app.state.plugin_command_logs.iter().any(|entry| {
                entry.log_id == log.log_id && entry.status != PluginCommandStatus::Running
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let finished = app
            .state
            .plugin_command_logs
            .iter()
            .find(|entry| entry.log_id == log.log_id)
            .expect("log should exist");
        assert_eq!(finished.status, PluginCommandStatus::Succeeded);
        assert_eq!(finished.action_id.as_deref(), Some("open"));
        assert_eq!(
            finished.stdout.as_deref(),
            Some("github-issue|https://github.com/ogulcancelik/herdr/issues/398")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_link_handlers_keep_manifest_order_for_overlapping_patterns() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-link-handler-order");
        write_manifest_content(
            &root,
            r#"
id = "example.link-order"
name = "Link Order"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "specific"
title = "Specific"
command = ["true"]

[[actions]]
id = "generic"
title = "Generic"
command = ["true"]

[[link_handlers]]
id = "z-specific"
title = "Specific GitHub issue"
pattern = "^https://github\\.com/[^/]+/[^/]+/issues/[0-9]+$"
action = "specific"

[[link_handlers]]
id = "a-generic"
title = "Generic GitHub"
pattern = "^https://github\\.com/"
action = "generic"
"#,
        );
        link_manifest(&mut app, &root);

        let (_plugin, handler) = app
            .find_plugin_link_handler("https://github.com/ogulcancelik/herdr/issues/398")
            .expect("handler should match");
        assert_eq!(handler.id, "z-specific");
        assert_eq!(handler.action, "specific");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_link_rejects_invalid_link_handler_pattern() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-bad-link-handler-pattern");
        write_manifest_content(
            &root,
            r#"
id = "example.bad-links"
name = "Bad Links"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "open"
title = "Open link"
command = ["true"]

[[link_handlers]]
id = "bad"
title = "Bad"
pattern = "["
action = "open"
"#,
        );

        let response = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["error"]["code"],
            "invalid_plugin_link_handler_pattern"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_link_rejects_link_handler_unknown_action() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-bad-link-handler-action");
        write_manifest_content(
            &root,
            r#"
id = "example.bad-link-action"
name = "Bad Link Action"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "open"
title = "Open link"
command = ["true"]

[[link_handlers]]
id = "github"
title = "GitHub"
pattern = "^https://github\\.com/"
action = "missing"
"#,
        );

        let response = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "invalid_plugin_link_handler_action");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_action_invoke_builds_default_workspace_context() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("issue")];
        app.state.workspaces[0].identity_cwd = "/tmp/issue".into();
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].custom_name = Some("Plugin Work".into());
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let pane_public = app.public_pane_id(0, pane_id).unwrap();
        let tab_public = app.public_tab_id(0, 0).unwrap();
        let workspace_public = app.public_workspace_id(0);
        let _ = app.handle_pane_report_agent(
            "report".into(),
            crate::api::schema::PaneReportAgentParams {
                pane_id: pane_public.clone(),
                source: "test".into(),
                agent: "codex".into(),
                state: crate::api::schema::PaneAgentState::Working,
                message: None,
                custom_status: None,
                seq: None,
                agent_session_id: None,
                agent_session_path: None,
            },
        );

        let root = unique_temp_path("plugin-action-context");
        // write a manifest with a "show" action in pane context
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("herdr-plugin.toml"),
            r#"
id = "example.context"
name = "Context"
version = "0.1.0"

[[actions]]
id = "show"
title = "Show Context"
contexts = ["pane"]
command = ["show-ctx"]
"#,
        )
        .unwrap();
        link_manifest(&mut app, &root);

        let invoke = app.handle_api_request(Request {
            id: "invoke-context".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.context".into()),
                action_id: "show".into(),
                context: None,
            }),
        });

        let ResponseResult::PluginActionInvoked { context, .. } = response_result(&invoke) else {
            panic!("expected plugin action invocation: {invoke}");
        };
        assert_eq!(
            context.workspace_id.as_deref(),
            Some(workspace_public.as_str())
        );
        assert_eq!(context.workspace_label.as_deref(), Some("Plugin Work"));
        assert_eq!(context.workspace_cwd.as_deref(), Some("/tmp/issue"));
        assert_eq!(context.tab_id.as_deref(), Some(tab_public.as_str()));
        assert_eq!(context.tab_label.as_deref(), Some("1"));
        assert_eq!(
            context.focused_pane_id.as_deref(),
            Some(pane_public.as_str())
        );
        assert_eq!(context.focused_pane_cwd.as_deref(), Some("/tmp/issue"));
        assert_eq!(context.focused_pane_agent.as_deref(), Some("codex"));
        assert_eq!(
            context.focused_pane_status,
            Some(crate::api::schema::AgentStatus::Working)
        );
        assert_eq!(context.invocation_source.as_deref(), Some("api"));
        assert_eq!(context.correlation_id.as_deref(), Some("invoke-context"));
        let worktree = context.worktree.as_ref().unwrap();
        assert_eq!(worktree.repo_key, "repo-key");
        assert_eq!(worktree.repo_name, "herdr");
        assert_eq!(worktree.repo_root, "/repo/herdr");
        assert_eq!(worktree.checkout_path, "/repo/herdr-issue");
        assert!(worktree.is_linked_worktree);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_action_invoke_returns_plugin_disabled_when_disabled() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-disabled");
        write_manifest(&root);

        // link as disabled
        let link_result = app.handle_api_request(Request {
            id: "link-disabled".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: false,
            }),
        });
        assert!(
            link_result.contains("plugin_linked"),
            "expected plugin_linked: {link_result}"
        );

        let invoke = app.handle_api_request(Request {
            id: "invoke-disabled".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.worktree-bootstrap".into()),
                action_id: "bootstrap".into(),
                context: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&invoke).unwrap();
        assert_eq!(value["error"]["code"], "plugin_disabled");

        let _ = std::fs::remove_dir_all(root);
    }

    fn write_manifest_with_bad_event(root: &std::path::Path) -> std::path::PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let manifest = root.join("herdr-plugin.toml");
        std::fs::write(
            &manifest,
            r#"
id = "example.bad-event"
name = "Bad Event Plugin"
version = "0.1.0"

[[events]]
on = "worktree.craeted"
command = ["sh", "-c", "echo hi"]

[[events]]
on = "worktree.created"
command = ["sh", "-c", "echo ok"]
"#,
        )
        .unwrap();
        manifest
    }

    #[test]
    fn link_with_unknown_event_name_succeeds_with_warning() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-bad-event");
        write_manifest_with_bad_event(&root);

        let link = app.handle_api_request(Request {
            id: "link-bad-event".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });

        let ResponseResult::PluginLinked { plugin } = response_result(&link) else {
            panic!("expected plugin_linked: {link}");
        };
        assert_eq!(plugin.plugin_id, "example.bad-event");
        assert!(
            plugin
                .warnings
                .iter()
                .any(|w| w.contains("worktree.craeted")),
            "expected warning for misspelled event, got: {:?}",
            plugin.warnings
        );
        // The correctly named event produces no extra warning
        assert_eq!(
            plugin
                .warnings
                .iter()
                .filter(|w| w.contains("worktree.created"))
                .count(),
            0
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unlink_removes_plugin_pane_records_for_that_plugin() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-unlink-panes");
        write_manifest(&root);
        link_manifest(&mut app, &root);

        // Manually insert plugin_pane records as if pane.open was called.
        let pane_a = crate::layout::PaneId::from_raw(1001u32);
        let pane_b = crate::layout::PaneId::from_raw(1002u32);
        let pane_other = crate::layout::PaneId::from_raw(1003u32);
        app.state.plugin_panes.insert(
            pane_a,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.worktree-bootstrap".into(),
                entrypoint: "main".into(),
            },
        );
        app.state.plugin_panes.insert(
            pane_b,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.worktree-bootstrap".into(),
                entrypoint: "side".into(),
            },
        );
        app.state.plugin_panes.insert(
            pane_other,
            crate::app::state::PluginPaneRecord {
                plugin_id: "other.plugin".into(),
                entrypoint: "other".into(),
            },
        );

        let unlink = app.handle_api_request(Request {
            id: "unlink-panes".into(),
            method: Method::PluginUnlink(PluginUnlinkParams {
                plugin_id: "example.worktree-bootstrap".into(),
            }),
        });
        assert!(matches!(
            response_result(&unlink),
            ResponseResult::PluginUnlinked { removed: true, .. }
        ));

        // plugin_panes records for the unlinked plugin are gone
        assert!(!app.state.plugin_panes.contains_key(&pane_a));
        assert!(!app.state.plugin_panes.contains_key(&pane_b));
        // other plugin's pane record survives
        assert!(app.state.plugin_panes.contains_key(&pane_other));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_pane_record_survives_pane_move() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-move")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        app.state.plugin_panes.insert(
            pane_id,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
            },
        );

        let response = app.handle_api_request(Request {
            id: "move".into(),
            method: Method::PaneMove(crate::api::schema::PaneMoveParams {
                pane_id: public_pane_id,
                destination: crate::api::schema::PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            }),
        });
        let ResponseResult::PaneMove { move_result } = response_result(&response) else {
            panic!("expected pane move: {response}");
        };
        assert!(app.state.plugin_panes.contains_key(&pane_id));

        let focus = app.handle_api_request(Request {
            id: "focus".into(),
            method: Method::PluginPaneFocus(PluginPaneFocusParams {
                pane_id: move_result.pane.pane_id.clone(),
            }),
        });
        let ResponseResult::PluginPaneFocused { plugin_pane } = response_result(&focus) else {
            panic!("expected plugin pane focus: {focus}");
        };
        assert_eq!(plugin_pane.plugin_id, "example.pane");
        assert_eq!(plugin_pane.entrypoint, "board");
        assert_eq!(plugin_pane.pane.pane_id, move_result.pane.pane_id);
    }

    #[test]
    fn registry_round_trip_via_explicit_path() {
        let root = unique_temp_path("plugin-registry-rt");
        write_manifest(&root);

        let registry_dir = unique_temp_path("registry-dir");
        std::fs::create_dir_all(&registry_dir).unwrap();
        let registry_path = registry_dir.join("plugins.json");

        // link via load_plugin_manifest + save_to_path (simulating what the App does)
        let plugin = load_plugin_manifest(&root.display().to_string(), true).unwrap();
        let plugins = vec![plugin.clone()];
        crate::persist::plugin_registry::save_to_path(&registry_path, &plugins).unwrap();
        assert!(registry_path.exists());

        // load back and verify
        let loaded = crate::persist::plugin_registry::load_from_path(&registry_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].plugin_id, "example.worktree-bootstrap");
        assert_eq!(loaded[0].name, "Worktree Bootstrap");

        // reload_manifests with real manifest still on disk → fresh parse succeeds
        let reloaded =
            crate::persist::plugin_registry::reload_manifests(loaded, |path, enabled| {
                load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
            });
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].warnings.is_empty());
        assert_eq!(reloaded[0].version, "0.1.0");

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(registry_dir);
    }

    #[test]
    fn reload_manifests_keeps_entry_with_warning_when_manifest_gone() {
        let root = unique_temp_path("plugin-missing-manifest");
        write_manifest(&root);

        // load_plugin_manifest resolves the absolute path via canonicalize()
        let plugin = load_plugin_manifest(&root.display().to_string(), true).unwrap();
        let stored_manifest_path = plugin.manifest_path.clone();

        // Now delete the manifest
        let _ = std::fs::remove_dir_all(&root);

        // Simulate registry load + reload
        let entries = vec![plugin];
        let reloaded =
            crate::persist::plugin_registry::reload_manifests(entries, |path, enabled| {
                load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
            });

        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].plugin_id, "example.worktree-bootstrap");
        assert!(!reloaded[0].warnings.is_empty(), "expected load warning");
        // The stored manifest_path is preserved so the entry is still identifiable
        assert_eq!(reloaded[0].manifest_path, stored_manifest_path);
    }

    // ── Platform compatibility tests ─────────────────────────────────────────

    #[test]
    fn manifest_with_platforms_parses_correctly() {
        let root = unique_temp_path("plugin-platforms");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("herdr-plugin.toml"),
            r#"
id = "example.platforms"
name = "Platforms"
version = "0.1.0"
platforms = ["linux", "macos"]

[[actions]]
id = "run"
title = "Run"
command = ["./run.sh"]

[[actions]]
id = "run-win"
title = "Run Windows"
platforms = ["windows"]
command = ["run.bat"]
"#,
        )
        .unwrap();

        let plugin = load_plugin_manifest(&root.display().to_string(), true).unwrap();
        use crate::api::schema::PluginPlatform;
        assert_eq!(
            plugin.platforms,
            Some(vec![PluginPlatform::Linux, PluginPlatform::Macos])
        );
        // Action without own platforms inherits from plugin level
        let run = plugin.actions.iter().find(|a| a.id == "run").unwrap();
        assert!(run.platforms.is_none());
        // Action with own platforms has them set
        let run_win = plugin.actions.iter().find(|a| a.id == "run-win").unwrap();
        assert_eq!(run_win.platforms, Some(vec![PluginPlatform::Windows]));
        // No missing-platforms warning because platforms is declared
        assert!(
            plugin.warnings.is_empty(),
            "expected no warnings: {:?}",
            plugin.warnings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn effective_platform_resolution_inherits_from_plugin() {
        use crate::api::schema::PluginPlatform;
        let plugin_platforms = Some(vec![PluginPlatform::Linux, PluginPlatform::Macos]);
        let no_override: Option<Vec<PluginPlatform>> = None;
        let action_override = Some(vec![PluginPlatform::Windows]);

        // No action-level platforms → inherit from plugin
        assert_eq!(
            effective_platforms(&no_override, &plugin_platforms),
            &plugin_platforms
        );
        // Action-level platforms → use action's own list
        assert_eq!(
            effective_platforms(&action_override, &plugin_platforms),
            &action_override
        );
        // Both None → None (undeclared)
        assert_eq!(effective_platforms(&no_override, &no_override), &None);
    }

    #[test]
    fn invoke_on_unsupported_platform_returns_error() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-platform-reject");
        std::fs::create_dir_all(&root).unwrap();

        // Declare only platforms that are NOT the current build target so the
        // invoke is guaranteed to be rejected regardless of which OS this runs on.
        let excluded_platforms = if cfg!(target_os = "linux") {
            r#"platforms = ["macos", "windows"]"#
        } else if cfg!(target_os = "macos") {
            r#"platforms = ["linux", "windows"]"#
        } else {
            r#"platforms = ["linux", "macos"]"#
        };

        std::fs::write(
            root.join("herdr-plugin.toml"),
            format!(
                r#"
id = "example.reject"
name = "Reject"
version = "0.1.0"
{excluded_platforms}

[[actions]]
id = "act"
title = "Act"
command = ["act"]
"#
            ),
        )
        .unwrap();

        let link = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        assert!(link.contains("plugin_linked"), "link failed: {link}");

        let invoke = app.handle_api_request(Request {
            id: "invoke".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.reject".into()),
                action_id: "act".into(),
                context: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&invoke).unwrap();
        assert_eq!(
            value["error"]["code"], "platform_unsupported",
            "expected platform_unsupported error: {invoke}"
        );
        assert!(
            invoke.contains("example.reject.act"),
            "error message should name the action: {invoke}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invoke_with_action_platform_override_uses_action_platforms() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-platform-action-override");
        std::fs::create_dir_all(&root).unwrap();

        // Plugin declares all platforms; action declares only the non-current platforms.
        let excluded_platforms = if cfg!(target_os = "linux") {
            r#"platforms = ["macos", "windows"]"#
        } else if cfg!(target_os = "macos") {
            r#"platforms = ["linux", "windows"]"#
        } else {
            r#"platforms = ["linux", "macos"]"#
        };

        std::fs::write(
            root.join("herdr-plugin.toml"),
            format!(
                r#"
id = "example.override"
name = "Override"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "act"
title = "Act"
{excluded_platforms}
command = ["act"]
"#
            ),
        )
        .unwrap();

        let link = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        assert!(link.contains("plugin_linked"), "link failed: {link}");

        let invoke = app.handle_api_request(Request {
            id: "invoke".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.override".into()),
                action_id: "act".into(),
                context: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&invoke).unwrap();
        assert_eq!(
            value["error"]["code"], "platform_unsupported",
            "expected platform_unsupported for action override: {invoke}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn invoke_with_undeclared_platforms_succeeds() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-platform-undeclared");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("herdr-plugin.toml"),
            r#"
id = "example.nodecl"
name = "No Decl"
version = "0.1.0"

[[actions]]
id = "act"
title = "Act"
command = ["act"]
"#,
        )
        .unwrap();

        let link = app.handle_api_request(Request {
            id: "link".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
            }),
        });
        let ResponseResult::PluginLinked { plugin } = response_result(&link) else {
            panic!("expected plugin_linked: {link}");
        };
        // Should get the missing-platforms warning
        assert!(
            plugin
                .warnings
                .iter()
                .any(|w| w.contains("does not declare platforms")),
            "expected missing-platforms warning: {:?}",
            plugin.warnings
        );

        // Invoke should succeed regardless (local dev allowance)
        let invoke = app.handle_api_request(Request {
            id: "invoke".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.nodecl".into()),
                action_id: "act".into(),
                context: None,
            }),
        });
        assert!(
            invoke.contains("plugin_action_invoked"),
            "expected success for undeclared platforms: {invoke}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn link_with_invalid_platform_string_fails() {
        let root = unique_temp_path("plugin-bad-platform");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("herdr-plugin.toml"),
            r#"
id = "example.badplatform"
name = "Bad Platform"
version = "0.1.0"
platforms = ["linux", "beos"]

[[actions]]
id = "act"
title = "Act"
command = ["act"]
"#,
        )
        .unwrap();

        let result = load_plugin_manifest(&root.display().to_string(), true);
        assert!(result.is_err(), "expected parse error for unknown platform");
        let (_, msg) = result.unwrap_err();
        assert!(
            msg.contains("beos") || msg.contains("platform"),
            "error message should mention the bad platform: {msg}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn registry_round_trip_preserves_platforms() {
        let root = unique_temp_path("plugin-platform-rt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("herdr-plugin.toml"),
            r#"
id = "example.platform-rt"
name = "Platform RT"
version = "0.1.0"
platforms = ["linux", "macos"]

[[actions]]
id = "act"
title = "Act"
platforms = ["windows"]
command = ["act.exe"]
"#,
        )
        .unwrap();

        let plugin = load_plugin_manifest(&root.display().to_string(), true).unwrap();
        use crate::api::schema::PluginPlatform;
        assert_eq!(
            plugin.platforms,
            Some(vec![PluginPlatform::Linux, PluginPlatform::Macos])
        );
        assert_eq!(
            plugin.actions[0].platforms,
            Some(vec![PluginPlatform::Windows])
        );

        let registry_dir = unique_temp_path("platform-rt-registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        let registry_path = registry_dir.join("plugins.json");
        crate::persist::plugin_registry::save_to_path(&registry_path, &[plugin]).unwrap();

        let loaded = crate::persist::plugin_registry::load_from_path(&registry_path);
        assert_eq!(
            loaded[0].platforms,
            Some(vec![PluginPlatform::Linux, PluginPlatform::Macos])
        );
        assert_eq!(
            loaded[0].actions[0].platforms,
            Some(vec![PluginPlatform::Windows])
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(registry_dir);
    }
}
