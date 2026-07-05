mod context;
mod env;
mod manifest;
mod panes;
mod runtime;

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    InstalledPluginInfo, PluginActionInfo, PluginActionInvokeParams, PluginActionListParams,
    PluginLinkParams, PluginListParams, PluginLogListParams, PluginManifestAction,
    PluginManifestLinkHandler, PluginPaneCloseParams, PluginPaneFocusParams, PluginPaneInfo,
    PluginPaneOpenParams, PluginPanePlacement, PluginSetEnabledParams, PluginUnlinkParams,
    ResponseResult,
};
use crate::app::App;
use manifest::{
    effective_platforms, ensure_platform_supported, normalize_action_id, normalize_plugin_id,
    normalize_plugin_source,
};

#[cfg(test)]
use crate::api::schema::{PluginCommandStatus, PluginInvocationContext};
pub(crate) use manifest::load_plugin_manifest;
#[cfg(test)]
use runtime::{read_capped_plugin_output, MAX_PLUGIN_COMMANDS_IN_FLIGHT};

impl App {
    pub(super) fn handle_plugin_link(&mut self, id: String, params: PluginLinkParams) -> String {
        let mut plugin = match load_plugin_manifest(&params.path, params.enabled) {
            Ok(plugin) => plugin,
            Err((code, message)) => return encode_error(id, code, message),
        };
        if let Some(source) = params.source {
            match normalize_plugin_source(&plugin, source) {
                Ok(source) => plugin.source = source,
                Err((code, message)) => return encode_error(id, code, message),
            }
        }
        if let Err(err) = env::ensure_plugin_user_dirs(&plugin) {
            return encode_error(id, "plugin_user_dir_create_failed", err.to_string());
        }
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
        let plugin_id = match normalize_optional_plugin_id(&id, params.plugin_id) {
            Ok(plugin_id) => plugin_id,
            Err(response) => return response,
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
        let plugin_id = match normalize_optional_plugin_id(&id, params.plugin_id) {
            Ok(plugin_id) => plugin_id,
            Err(response) => return response,
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
        if let Err((code, message)) = ensure_platform_supported(
            effective_platforms(&action.platforms, &plugin.platforms),
            &format!("action '{}'", action.qualified_id()),
        ) {
            return encode_error(id, code, message);
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
        ensure_platform_supported(
            effective_platforms(&action.platforms, &plugin.platforms),
            &action.qualified_id(),
        )
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
        let plugin_id = match normalize_optional_plugin_id(&id, params.plugin_id) {
            Ok(plugin_id) => plugin_id,
            Err(response) => return response,
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
        if !plugin_manifest_available(&plugin) {
            return encode_error(
                id,
                "plugin_manifest_unavailable",
                format!("plugin {plugin_id} manifest is unavailable"),
            );
        }
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
        if let Err(response) = self.close_pane(
            id.clone(),
            &crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            },
        ) {
            return response;
        }
        encode_success(id, ResponseResult::PluginPaneClosed { pane_id })
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
            if !plugin_manifest_available(&plugin) {
                return Err((
                    "plugin_manifest_unavailable",
                    format!("plugin {plugin_id} manifest is unavailable"),
                ));
            }
            let action_info = plugin
                .actions
                .iter()
                .find(|a| a.id == action_id)
                .map(|a| manifest_action_info(&plugin_id, &plugin.platforms, a))
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
            .filter(|plugin| plugin.enabled && plugin_manifest_available(plugin))
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

fn invalid_plugin_id(id: String) -> String {
    encode_error(
        id,
        "invalid_plugin_id",
        "plugin id must be non-empty, <= 120 characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen",
    )
}

/// Normalize an optional plugin id filter; `Err` carries the encoded
/// `invalid_plugin_id` error response.
fn normalize_optional_plugin_id(
    id: &str,
    plugin_id: Option<String>,
) -> Result<Option<String>, String> {
    match plugin_id {
        Some(plugin_id) => match normalize_plugin_id(&plugin_id) {
            Some(plugin_id) => Ok(Some(plugin_id)),
            None => Err(invalid_plugin_id(id.to_string())),
        },
        None => Ok(None),
    }
}

fn plugin_manifest_available(plugin: &InstalledPluginInfo) -> bool {
    !plugin.warnings.iter().any(|warning| {
        warning.starts_with(crate::persist::plugin_registry::MANIFEST_UNAVAILABLE_WARNING_PREFIX)
    })
}

fn manifest_action_info(
    plugin_id: &str,
    plugin_platforms: &Option<Vec<crate::api::schema::PluginPlatform>>,
    action: &PluginManifestAction,
) -> PluginActionInfo {
    PluginActionInfo {
        plugin_id: plugin_id.to_string(),
        action_id: action.id.clone(),
        title: action.title.clone(),
        description: action.description.clone(),
        contexts: action.contexts.clone(),
        command: action.command.clone(),
        platforms: effective_platforms(&action.platforms, plugin_platforms).clone(),
    }
}

fn manifest_actions(
    plugins: &crate::app::state::InstalledPluginRegistry,
) -> impl Iterator<Item = PluginActionInfo> + '_ {
    plugins
        .values()
        .filter(|plugin| plugin_manifest_available(plugin))
        .flat_map(|plugin| {
            plugin
                .actions
                .iter()
                .map(|action| manifest_action_info(&plugin.plugin_id, &plugin.platforms, action))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        Method, PluginSourceInfo, PluginSourceKind, Request, SuccessResponse,
    };
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

    fn canonical_path_string(path: &std::path::Path) -> String {
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string()
    }

    /// Wait for non-empty contents at `path`. Shell `>` creates the file empty
    /// before the command writes, so waiting on existence alone can read EOF.
    /// `pump` advances any event loop the command depends on.
    fn read_capture_when_ready(path: &std::path::Path, mut pump: impl FnMut()) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            pump();
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.is_empty() {
                    return contents;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "plugin command did not write {} within deadline",
                path.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
min_herdr_version = "0.6.10"
description = "Prepare new worktrees"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["bun", "install"]

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
                source: None,
            }),
        });
        assert!(
            result.contains("plugin_linked"),
            "expected plugin_linked: {result}"
        );
    }

    #[test]
    fn plugin_link_creates_stable_config_and_state_dirs() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-link-dirs");
        let config_dir = super::env::plugin_config_dir("example.config-dirs");
        let state_dir = super::env::plugin_state_dir("example.config-dirs");
        let _ = std::fs::remove_dir_all(&config_dir);
        let _ = std::fs::remove_dir_all(&state_dir);
        write_manifest_content(
            &root,
            r#"
id = "example.config-dirs"
name = "Config Dirs"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#,
        );

        link_manifest(&mut app, &root);

        assert!(config_dir.is_dir());
        assert!(state_dir.is_dir());

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(config_dir);
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn plugin_link_seeds_stable_config_dir_from_legacy_unhashed_dir() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-link-legacy-config");
        let config_dir = super::env::plugin_config_dir("example.legacy-config");
        let state_dir = super::env::plugin_state_dir("example.legacy-config");
        let legacy_dir = crate::config::config_dir()
            .join("plugins")
            .join("example.legacy-config");
        let _ = std::fs::remove_dir_all(&config_dir);
        let _ = std::fs::remove_dir_all(&state_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join(".env"), "TELEGRAM_BOT_TOKEN=test\n").unwrap();
        write_manifest_content(
            &root,
            r#"
id = "example.legacy-config"
name = "Legacy Config"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#,
        );

        link_manifest(&mut app, &root);

        assert_eq!(
            std::fs::read_to_string(config_dir.join(".env")).unwrap(),
            "TELEGRAM_BOT_TOKEN=test\n"
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(config_dir);
        let _ = std::fs::remove_dir_all(state_dir);
        let _ = std::fs::remove_dir_all(legacy_dir);
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
                source: None,
            }),
        });
        let ResponseResult::PluginLinked { plugin } = response_result(&link) else {
            panic!("expected plugin linked response: {link}");
        };
        assert_eq!(plugin.plugin_id, "example.worktree-bootstrap");
        assert_eq!(plugin.name, "Worktree Bootstrap");
        assert_eq!(plugin.version, "0.1.0");
        assert_eq!(plugin.plugin_root, canonical_path_string(&root));
        assert!(plugin.enabled);
        assert_eq!(plugin.build.len(), 1);
        assert_eq!(plugin.build[0].command, ["bun", "install"]);
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
    fn plugin_link_rejects_invalid_github_source_path() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-invalid-source");
        write_manifest(&root);

        let response = app.handle_api_request(Request {
            id: "link-invalid-source".into(),
            method: Method::PluginLink(PluginLinkParams {
                path: root.display().to_string(),
                enabled: true,
                source: Some(PluginSourceInfo {
                    kind: PluginSourceKind::Github,
                    owner: Some("ogulcancelik".into()),
                    repo: Some("herdr-plugin-examples".into()),
                    subdir: Some("worktree-bootstrap".into()),
                    requested_ref: None,
                    resolved_commit: Some("abc123".into()),
                    managed_path: Some(root.display().to_string()),
                    installed_unix_ms: Some(42),
                }),
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], "invalid_plugin_source");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn link_rejects_invalid_min_herdr_versions() {
        let cases = [
            (
                "plugin-missing-min-herdr",
                r#"
id = "example.missing-min-herdr"
name = "Missing Min Herdr"
version = "0.1.0"
platforms = ["linux", "macos", "windows"]
"#,
                "invalid_plugin_min_herdr_version",
            ),
            (
                "plugin-invalid-min-herdr",
                r#"
id = "example.invalid-min-herdr"
name = "Invalid Min Herdr"
version = "0.1.0"
min_herdr_version = "soon"
platforms = ["linux", "macos", "windows"]
"#,
                "invalid_plugin_min_herdr_version",
            ),
            (
                "plugin-future-min-herdr",
                r#"
id = "example.future-min-herdr"
name = "Future Min Herdr"
version = "0.1.0"
min_herdr_version = "999.0.0"
platforms = ["linux", "macos", "windows"]
"#,
                "plugin_requires_newer_herdr",
            ),
        ];

        for (name, manifest, expected_code) in cases {
            let root = unique_temp_path(name);
            write_manifest_content(&root, manifest);

            let result = load_plugin_manifest(&root.display().to_string(), true);
            assert!(
                matches!(result, Err((code, _)) if code == expected_code),
                "{name}: expected {expected_code}, got {result:?}"
            );
            let _ = std::fs::remove_dir_all(root);
        }
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
min_herdr_version = "0.6.10"
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
    fn link_rejects_dotted_action_ids() {
        let root = unique_temp_path("plugin-dotted-action");
        write_manifest_content(
            &root,
            r#"
id = "example.dotted-action"
name = "Dotted Action"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "build.release"
title = "Build"
command = ["echo", "build"]
"#,
        );

        let result = load_plugin_manifest(&root.display().to_string(), true);
        assert!(matches!(result, Err(("invalid_plugin_action_id", _))));
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
min_herdr_version = "0.6.10"
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
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
command = ["sh", "-c", "printf '%s\n%s\n%s\n%s\n%s\n%s\n%s\n' \"$PWD\" \"$HERDR_PLUGIN_ID\" \"$HERDR_PLUGIN_ENTRYPOINT_ID\" \"$HERDR_WORKSPACE_ID\" \"$HERDR_PANE_ID\" \"$HERDR_BIN_PATH\" \"$HERDR_PLUGIN_CONTEXT_JSON\" > {}"]
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
                env: std::collections::HashMap::from([
                    ("HERDR_PLUGIN_ID".to_string(), "spoofed-plugin".to_string()),
                    (
                        "HERDR_PLUGIN_ENTRYPOINT_ID".to_string(),
                        "spoofed-entrypoint".to_string(),
                    ),
                    (
                        "HERDR_PLUGIN_CONTEXT_JSON".to_string(),
                        "{\"spoofed\":true}".to_string(),
                    ),
                    (
                        "HERDR_BIN_PATH".to_string(),
                        "/tmp/spoofed-herdr".to_string(),
                    ),
                ]),
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

        let text = read_capture_when_ready(&capture, || {});
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(canonical_path_string(&root).as_str()));
        assert_eq!(lines.next(), Some("example.pane"));
        assert_eq!(lines.next(), Some("board"));
        assert_eq!(lines.next(), Some(plugin_pane.pane.workspace_id.as_str()));
        assert_eq!(lines.next(), Some(plugin_pane.pane.pane_id.as_str()));
        let bin_path = lines.next().expect("bin path");
        assert_ne!(bin_path, "/tmp/spoofed-herdr");
        assert_eq!(
            bin_path,
            std::env::current_exe()
                .expect("current exe should resolve")
                .display()
                .to_string()
        );
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
    async fn plugin_pane_open_injects_plugin_paths_and_protects_overrides() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-path-env")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        let root = unique_temp_path("plugin-pane-path-env");
        let capture = root.join("capture.txt");
        write_manifest_content(
            &root,
            &format!(
                r#"
id = "example.path-env"
name = "Path Env"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
command = ["sh", "-c", "printf '%s\n%s\n%s\n' \"$HERDR_PLUGIN_ROOT\" \"$HERDR_PLUGIN_CONFIG_DIR\" \"$HERDR_PLUGIN_STATE_DIR\" > {}"]
"#,
                capture.display()
            ),
        );
        link_manifest(&mut app, &root);

        let open = app.handle_api_request(Request {
            id: "pane-open".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.path-env".into(),
                entrypoint: "board".into(),
                placement: Some(PluginPanePlacement::Overlay),
                workspace_id: None,
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: true,
                env: std::collections::HashMap::from([
                    (
                        "HERDR_PLUGIN_ROOT".to_string(),
                        "/tmp/spoofed-root".to_string(),
                    ),
                    (
                        "HERDR_PLUGIN_CONFIG_DIR".to_string(),
                        "/tmp/spoofed-config".to_string(),
                    ),
                    (
                        "HERDR_PLUGIN_STATE_DIR".to_string(),
                        "/tmp/spoofed-state".to_string(),
                    ),
                ]),
            }),
        });
        let ResponseResult::PluginPaneOpened { .. } = response_result(&open) else {
            panic!("expected plugin pane opened response: {open}");
        };

        let text = read_capture_when_ready(&capture, || {});
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(canonical_path_string(&root).as_str()));
        assert_eq!(
            lines.next(),
            Some(
                crate::config::config_dir()
                    .join("plugins")
                    .join("config")
                    .join("example.path-env")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(
            lines.next(),
            Some(
                crate::config::state_dir()
                    .join("plugins")
                    .join("example.path-env")
                    .display()
                    .to_string()
                    .as_str()
            )
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
min_herdr_version = "0.6.10"
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
                placement: None,
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
        let layout_updated = events
            .iter()
            .position(|event| *event == crate::api::schema::EventKind::LayoutUpdated)
            .expect("layout.updated should be emitted");
        assert!(tab_created < pane_created);
        assert!(pane_created < layout_updated);

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_pane_open_zoomed_split_emits_layout_updated() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-split")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;

        let root = unique_temp_path("plugin-pane-split-layout-event");
        write_manifest_content(
            &root,
            r#"
id = "example.split"
name = "Split Plugin"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
placement = "split"
command = ["sh", "-c", "sleep 1"]
"#,
        );
        link_manifest(&mut app, &root);

        let open = app.handle_api_request(Request {
            id: "pane-open-split".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.split".into(),
                entrypoint: "board".into(),
                placement: Some(PluginPanePlacement::Zoomed),
                workspace_id: None,
                target_pane_id: None,
                direction: Some(crate::api::schema::SplitDirection::Right),
                cwd: None,
                focus: true,
                env: std::collections::HashMap::new(),
            }),
        });
        let ResponseResult::PluginPaneOpened { .. } = response_result(&open) else {
            panic!("expected plugin pane opened response: {open}");
        };

        let events = event_hub.events_after(0);
        let pane_created = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::PaneCreated)
            .expect("pane.created should be emitted");
        let layout_updated = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::LayoutUpdated)
            .expect("layout.updated should be emitted");
        assert!(pane_created < layout_updated);
        assert!(matches!(
            &events[layout_updated].1.data,
            crate::api::schema::EventData::LayoutUpdated { layout }
                if layout.zoomed && layout.panes.len() == 2
        ));

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn plugin_pane_open_overlay_emits_layout_updated() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            event_hub.clone(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-overlay")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;

        let root = unique_temp_path("plugin-pane-overlay-layout-event");
        write_manifest_content(
            &root,
            r#"
id = "example.overlay"
name = "Overlay Plugin"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[panes]]
id = "board"
title = "Plugin Board"
placement = "overlay"
command = ["sh", "-c", "sleep 1"]
"#,
        );
        link_manifest(&mut app, &root);

        let open = app.handle_api_request(Request {
            id: "pane-open-overlay".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.overlay".into(),
                entrypoint: "board".into(),
                placement: None,
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

        let events = event_hub.events_after(0);
        let pane_created = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::PaneCreated)
            .expect("pane.created should be emitted");
        let layout_updated = events
            .iter()
            .position(|(_, event)| event.event == crate::api::schema::EventKind::LayoutUpdated)
            .expect("layout.updated should be emitted");
        assert!(pane_created < layout_updated);
        assert!(matches!(
            &events[layout_updated].1.data,
            crate::api::schema::EventData::LayoutUpdated { layout }
                if layout.zoomed && layout.panes.len() == 2
        ));

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
        assert_eq!(
            actions[0].platforms,
            Some(vec![
                crate::api::schema::PluginPlatform::Linux,
                crate::api::schema::PluginPlatform::Macos,
                crate::api::schema::PluginPlatform::Windows,
            ])
        );

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

    #[test]
    fn stale_registry_entries_are_visible_but_not_runnable() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-stale-registry");
        write_manifest(&root);
        let plugin = load_plugin_manifest(&root.display().to_string(), true).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let reloaded =
            crate::persist::plugin_registry::reload_manifests(vec![plugin], |path, enabled| {
                load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
            });
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded[0].warnings.iter().any(|warning| warning
            .starts_with(crate::persist::plugin_registry::MANIFEST_UNAVAILABLE_WARNING_PREFIX)));
        app.state
            .installed_plugins
            .insert(reloaded[0].plugin_id.clone(), reloaded[0].clone());

        let list = app.handle_api_request(Request {
            id: "plugin-list".into(),
            method: Method::PluginList(PluginListParams { plugin_id: None }),
        });
        let ResponseResult::PluginList { plugins } = response_result(&list) else {
            panic!("expected plugin list: {list}");
        };
        assert_eq!(plugins.len(), 1);

        let actions = app.handle_api_request(Request {
            id: "action-list".into(),
            method: Method::PluginActionList(PluginActionListParams { plugin_id: None }),
        });
        let ResponseResult::PluginActionList { actions } = response_result(&actions) else {
            panic!("expected action list: {actions}");
        };
        assert!(actions.is_empty());

        let invoke = app.handle_api_request(Request {
            id: "invoke".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.worktree-bootstrap".into()),
                action_id: "bootstrap".into(),
                context: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&invoke).unwrap();
        assert_eq!(value["error"]["code"], "plugin_manifest_unavailable");

        let pane = app.handle_api_request(Request {
            id: "pane-open".into(),
            method: Method::PluginPaneOpen(PluginPaneOpenParams {
                plugin_id: "example.worktree-bootstrap".into(),
                entrypoint: "board".into(),
                placement: None,
                workspace_id: None,
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: true,
                env: std::collections::HashMap::new(),
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&pane).unwrap();
        assert_eq!(value["error"]["code"], "plugin_manifest_unavailable");
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
min_herdr_version = "0.6.10"
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
    fn manifest_action_invoke_injects_plugin_paths() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-action-path-env");
        write_manifest_content(
            &root,
            r#"
id = "example.action-paths"
name = "Action Paths"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[actions]]
id = "run"
title = "Run"
command = ["sh", "-c", "printf '%s\n%s\n%s' \"$HERDR_PLUGIN_ROOT\" \"$HERDR_PLUGIN_CONFIG_DIR\" \"$HERDR_PLUGIN_STATE_DIR\""]
"#,
        );
        link_manifest(&mut app, &root);

        let invoke = app.handle_api_request(Request {
            id: "invoke-runner".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.action-paths".into()),
                action_id: "run".into(),
                context: None,
            }),
        });
        let ResponseResult::PluginActionInvoked { log, .. } = response_result(&invoke) else {
            panic!("expected plugin action invocation: {invoke}");
        };

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
                plugin_id: Some("example.action-paths".into()),
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
        let mut lines = finished.stdout.as_deref().unwrap_or_default().lines();
        assert_eq!(lines.next(), Some(canonical_path_string(&root).as_str()));
        assert_eq!(
            lines.next(),
            Some(
                crate::config::config_dir()
                    .join("plugins")
                    .join("config")
                    .join("example.action-paths")
                    .display()
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(
            lines.next(),
            Some(
                crate::config::state_dir()
                    .join("plugins")
                    .join("example.action-paths")
                    .display()
                    .to_string()
                    .as_str()
            )
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn current_plugin_context_includes_selected_text_for_focused_pane() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("plugin-selection");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"hello plugin\n"),
        );
        app.state.selection = Some(crate::selection::Selection::range(pane_id, 0, 0, 4, None));

        let context = app.current_plugin_context("selection-test");

        assert_eq!(context.selected_text.as_deref(), Some("hello"));
    }

    #[cfg(unix)]
    #[test]
    fn event_hooks_use_event_target_context() {
        let mut app = test_app();
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("active"),
            crate::workspace::Workspace::test_new("event-target"),
        ];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let active_workspace_id = app.public_workspace_id(0);
        let target_workspace = app.workspace_info(1);

        let root = unique_temp_path("plugin-event-context");
        let capture = root.join("context.json");
        write_manifest_content(
            &root,
            &format!(
                r#"
id = "example.event-context"
name = "Event Context"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos"]

[[events]]
on = "worktree.created"
command = ["sh", "-c", "printf '%s' \"$HERDR_PLUGIN_CONTEXT_JSON\" > {}"]
"#,
                capture.display()
            ),
        );
        link_manifest(&mut app, &root);

        app.run_plugin_event_hooks(&crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::WorktreeCreated,
            data: crate::api::schema::EventData::WorktreeCreated {
                workspace: target_workspace.clone(),
                worktree: crate::api::schema::WorktreeInfo {
                    path: "/tmp/repo".into(),
                    branch: Some("feature".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                    is_linked_worktree: true,
                    open_workspace_id: Some(target_workspace.workspace_id.clone()),
                    label: "feature".into(),
                },
            },
        });

        let context: PluginInvocationContext =
            serde_json::from_str(&read_capture_when_ready(&capture, || {
                app.drain_all_internal_events();
            }))
            .unwrap();
        assert_eq!(
            context.workspace_id.as_deref(),
            Some(target_workspace.workspace_id.as_str())
        );
        assert_ne!(
            context.workspace_id.as_deref(),
            Some(active_workspace_id.as_str())
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_command_limit_rejects_and_logs() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-command-limit");
        write_manifest(&root);
        link_manifest(&mut app, &root);
        app.state.plugin_commands_in_flight = MAX_PLUGIN_COMMANDS_IN_FLIGHT;

        let invoke = app.handle_api_request(Request {
            id: "invoke-limit".into(),
            method: Method::PluginActionInvoke(PluginActionInvokeParams {
                plugin_id: Some("example.worktree-bootstrap".into()),
                action_id: "bootstrap".into(),
                context: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&invoke).unwrap();
        assert_eq!(value["error"]["code"], "plugin_command_limit_reached");
        let log = app
            .state
            .plugin_command_logs
            .last()
            .expect("rejected command should be logged");
        assert_eq!(log.status, PluginCommandStatus::Failed);
        assert!(log
            .error
            .as_deref()
            .is_some_and(|error| error.contains("maximum concurrent plugin commands")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn closed_event_context_uses_closed_target_ids() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("closed-events")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let active_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let active_public_pane_id = app.public_pane_id(0, active_pane_id).unwrap();
        let workspace_id = app.public_workspace_id(0);
        let closed_tab_id = format!("{workspace_id}:t99");
        let closed_pane_id = format!("{workspace_id}:p99");

        let tab_context = app.plugin_context_for_event(
            &crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::TabClosed,
                data: crate::api::schema::EventData::TabClosed {
                    tab_id: closed_tab_id.clone(),
                    workspace_id: workspace_id.clone(),
                },
            },
            "tab.closed",
        );
        assert_eq!(
            tab_context.workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );
        assert_eq!(tab_context.tab_id.as_deref(), Some(closed_tab_id.as_str()));
        assert_eq!(tab_context.focused_pane_id, None);

        let pane_context = app.plugin_context_for_event(
            &crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneClosed,
                data: crate::api::schema::EventData::PaneClosed {
                    pane_id: closed_pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                },
            },
            "pane.closed",
        );
        assert_eq!(
            pane_context.workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );
        assert_eq!(
            pane_context.focused_pane_id.as_deref(),
            Some(closed_pane_id.as_str())
        );
        assert_ne!(
            pane_context.focused_pane_id.as_deref(),
            Some(active_public_pane_id.as_str())
        );

        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        let workspace = app.workspace_info(0);
        let worktree = crate::api::schema::WorktreeInfo {
            path: "/repo/herdr-issue".into(),
            branch: Some("worktree/issue".into()),
            is_bare: false,
            is_detached: false,
            is_prunable: false,
            is_linked_worktree: true,
            open_workspace_id: None,
            label: "herdr".into(),
        };
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-other".into(),
            is_linked_worktree: true,
        });
        let changed_context = app.plugin_context_for_event(
            &crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorktreeRemoved,
                data: crate::api::schema::EventData::WorktreeRemoved {
                    workspace_id: workspace_id.clone(),
                    workspace: Some(workspace.clone()),
                    worktree: worktree.clone(),
                    forced: true,
                },
            },
            "worktree.removed",
        );
        assert_eq!(
            changed_context
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.as_str()),
            Some("/repo/herdr-issue")
        );

        app.state.workspaces.clear();
        let removed_context = app.plugin_context_for_event(
            &crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorktreeRemoved,
                data: crate::api::schema::EventData::WorktreeRemoved {
                    workspace_id: workspace_id.clone(),
                    workspace: Some(workspace),
                    worktree,
                    forced: true,
                },
            },
            "worktree.removed",
        );
        assert_eq!(
            removed_context.workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );
        assert_eq!(
            removed_context
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.as_str()),
            Some("/repo/herdr-issue")
        );
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
min_herdr_version = "0.6.10"
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
min_herdr_version = "0.6.10"
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
min_herdr_version = "0.6.10"
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
                source: None,
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
min_herdr_version = "0.6.10"
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
                source: None,
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
min_herdr_version = "0.6.10"

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
                source: None,
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
min_herdr_version = "0.6.10"

[[events]]
on = "worktree.craeted"
command = ["sh", "-c", "echo hi"]

[[events]]
on = "pane.output_changed"
command = ["sh", "-c", "echo too noisy"]

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
                source: None,
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
        assert!(
            plugin
                .warnings
                .iter()
                .any(|w| w.contains("pane.output_changed")),
            "expected warning for unemitted output-change hook, got: {:?}",
            plugin.warnings
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn output_changed_event_hooks_do_not_run_even_if_event_is_emitted() {
        let mut app = test_app();
        let root = unique_temp_path("plugin-output-hook");
        write_manifest_with_bad_event(&root);
        link_manifest(&mut app, &root);

        app.run_plugin_event_hooks(&crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneOutputChanged,
            data: crate::api::schema::EventData::PaneOutputChanged {
                pane_id: "w1-1".into(),
                workspace_id: "w1".into(),
                revision: 1,
            },
        });

        assert!(
            app.state.plugin_command_logs.is_empty(),
            "pane.output_changed hooks should be warning-only until hook semantics exist"
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
    fn pane_exit_removes_plugin_pane_record() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("plugin-exit")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state.plugin_panes.insert(
            pane_id,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
            },
        );

        app.handle_internal_event(crate::events::AppEvent::PaneDied { pane_id });

        assert!(!app.state.plugin_panes.contains_key(&pane_id));
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
min_herdr_version = "0.6.10"
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
min_herdr_version = "0.6.10"
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
                source: None,
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
min_herdr_version = "0.6.10"
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
                source: None,
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
min_herdr_version = "0.6.10"

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
                source: None,
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
min_herdr_version = "0.6.10"
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
min_herdr_version = "0.6.10"
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
