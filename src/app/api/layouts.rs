use std::path::PathBuf;

use ratatui::layout::Direction;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, LayoutApplyParams, LayoutDescription, LayoutExportParams,
    LayoutNode, LayoutPane, ResponseResult, SplitDirection,
};
use crate::app::{App, Mode};
use crate::layout::{Node, PaneId};
use crate::workspace::NewPane;

use super::responses::{encode_error, encode_success};

const MAX_LAYOUT_PANES: usize = 24;
const MAX_LAYOUT_DEPTH: usize = 16;

impl App {
    pub(super) fn handle_layout_export(
        &mut self,
        id: String,
        params: LayoutExportParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.resolve_layout_export_target(&params) else {
            return encode_error(id, "layout_not_found", "layout target not found");
        };
        let Some(layout) = self.layout_description(ws_idx, tab_idx) else {
            return encode_error(id, "layout_not_found", "layout unavailable");
        };

        encode_success(id, ResponseResult::LayoutExport { layout })
    }

    pub(super) fn handle_layout_apply(&mut self, id: String, params: LayoutApplyParams) -> String {
        let replace_target = match params.tab_id.as_deref() {
            Some(tab_id) => match self.parse_tab_id(tab_id) {
                Some(target) => Some(target),
                None => {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
                }
            },
            None => None,
        };
        if replace_target.is_some() && params.workspace_id.is_some() {
            return encode_error(
                id,
                "invalid_target",
                "use either tab_id or workspace_id, not both",
            );
        }

        let ws_idx = if let Some((ws_idx, _)) = replace_target {
            ws_idx
        } else if let Some(workspace_id) = params.workspace_id.as_deref() {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return encode_error(
                    id,
                    "workspace_not_found",
                    format!("workspace {workspace_id} not found"),
                );
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        if let Err(message) = validate_layout_tree(&params.root) {
            return encode_error(id, "invalid_layout", message);
        }

        let replacement_label = params.tab_label.clone().or_else(|| {
            let (_, tab_idx) = replace_target?;
            self.state
                .workspaces
                .get(ws_idx)?
                .tabs
                .get(tab_idx)?
                .custom_name
                .clone()
        });
        let replace_was_active = replace_target.is_some_and(|(target_ws, target_tab)| {
            self.state.active == Some(target_ws)
                && self
                    .state
                    .workspaces
                    .get(target_ws)
                    .is_some_and(|ws| ws.active_tab_index() == target_tab)
        });
        let root_leaf = first_layout_leaf(&params.root);
        let first_cwd = self.layout_root_cwd(ws_idx, replace_target, root_leaf);
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let extra_env = match super::env::normalize_launch_env(root_leaf.env.clone()) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let command = match layout_command(root_leaf) {
            Ok(command) => command,
            Err(message) => return encode_error(id, "invalid_layout", message),
        };

        let created = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return encode_error(id, "workspace_not_found", "workspace not found");
            };
            if let Some(argv) = command.as_deref() {
                ws.create_tab_argv_command(
                    rows,
                    cols,
                    first_cwd,
                    argv,
                    extra_env,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                )
            } else {
                ws.create_tab(
                    rows,
                    cols,
                    first_cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                )
            }
        };

        let (new_tab_idx, terminal, runtime) = match created {
            Ok(result) => result,
            Err(err) => return encode_error(id, "layout_apply_failed", err.to_string()),
        };
        let new_root_pane = self.state.workspaces[ws_idx].tabs[new_tab_idx].root_pane;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.remove_alias_shadowed_by_new_pane(new_root_pane);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        if let Some(label) = replacement_label {
            self.state.workspaces[ws_idx].tabs[new_tab_idx].set_custom_name(label);
        }
        self.apply_layout_pane_label(ws_idx, new_root_pane, root_leaf);

        if let Err(message) = self.apply_layout_node_to_pane(ws_idx, new_root_pane, &params.root) {
            self.rollback_layout_tab(ws_idx, new_root_pane);
            return encode_error(id, "layout_apply_failed", message);
        }

        if let Some((target_ws_idx, target_tab_idx)) = replace_target {
            let closed_tab_id = self
                .public_tab_id(target_ws_idx, target_tab_idx)
                .unwrap_or_else(|| {
                    crate::workspace::public_tab_id_for_number(
                        &self.public_workspace_id(target_ws_idx),
                        target_tab_idx + 1,
                    )
                });
            let terminal_ids = self
                .state
                .terminal_ids_for_tab(target_ws_idx, target_tab_idx);
            let plugin_pane_ids = self.state.pane_ids_for_tab(target_ws_idx, target_tab_idx);
            let Some(ws) = self.state.workspaces.get_mut(target_ws_idx) else {
                return encode_error(id, "tab_not_found", "tab not found");
            };
            if ws.close_tab(target_tab_idx) {
                self.state.remove_plugin_pane_records(plugin_pane_ids);
                self.state.remove_unattached_terminal_ids(terminal_ids);
                self.shutdown_detached_terminal_runtimes();
                self.emit_event(EventEnvelope {
                    event: EventKind::TabClosed,
                    data: EventData::TabClosed {
                        tab_id: closed_tab_id,
                        workspace_id: self.public_workspace_id(target_ws_idx),
                    },
                });
            }
        }

        let Some(new_tab_idx) = self.state.workspaces[ws_idx]
            .tabs
            .iter()
            .position(|tab| tab.root_pane == new_root_pane)
        else {
            return encode_error(id, "layout_apply_failed", "new layout tab disappeared");
        };

        if params.focus || replace_was_active {
            self.state.switch_workspace_tab(ws_idx, new_tab_idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        if let Some(tab) = self.tab_info(ws_idx, new_tab_idx) {
            self.emit_event(EventEnvelope {
                event: EventKind::TabCreated,
                data: EventData::TabCreated { tab },
            });
        }
        for pane_id in self.state.workspaces[ws_idx].tabs[new_tab_idx]
            .layout
            .pane_ids()
        {
            if let Some(pane) = self.pane_info(ws_idx, pane_id) {
                self.emit_event(EventEnvelope {
                    event: EventKind::PaneCreated,
                    data: EventData::PaneCreated { pane },
                });
            }
        }

        let Some(layout) = self.layout_description(ws_idx, new_tab_idx) else {
            return encode_error(id, "layout_apply_failed", "new layout unavailable");
        };
        encode_success(id, ResponseResult::LayoutApply { layout })
    }

    fn resolve_layout_export_target(&self, params: &LayoutExportParams) -> Option<(usize, usize)> {
        match (params.tab_id.as_deref(), params.pane_id.as_deref()) {
            (Some(_), Some(_)) => None,
            (Some(tab_id), None) => self.parse_tab_id(tab_id),
            (None, Some(pane_id)) => {
                let (ws_idx, pane_id) = self.parse_pane_id(pane_id)?;
                let tab_idx = self
                    .state
                    .workspaces
                    .get(ws_idx)?
                    .find_tab_index_for_pane(pane_id)?;
                Some((ws_idx, tab_idx))
            }
            (None, None) => {
                let ws_idx = self.state.active?;
                let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                Some((ws_idx, tab_idx))
            }
        }
    }

    fn layout_description(&self, ws_idx: usize, tab_idx: usize) -> Option<LayoutDescription> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        Some(LayoutDescription {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            focused_pane_id: self.public_pane_id(ws_idx, tab.layout.focused())?,
            root: self.layout_node_description(ws_idx, tab_idx, tab.layout.root())?,
        })
    }

    fn layout_node_description(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        node: &Node,
    ) -> Option<LayoutNode> {
        match node {
            Node::Pane(pane_id) => Some(LayoutNode::Pane {
                pane: self.layout_pane_description(ws_idx, tab_idx, *pane_id)?,
            }),
            Node::Split {
                direction,
                ratio,
                first,
                second,
            } => Some(LayoutNode::Split {
                direction: match direction {
                    Direction::Horizontal => SplitDirection::Right,
                    Direction::Vertical => SplitDirection::Down,
                },
                ratio: *ratio,
                first: Box::new(self.layout_node_description(ws_idx, tab_idx, first)?),
                second: Box::new(self.layout_node_description(ws_idx, tab_idx, second)?),
            }),
        }
    }

    fn layout_pane_description(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    ) -> Option<LayoutPane> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let terminal_id = tab.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id);
        Some(LayoutPane {
            pane_id: Some(self.public_pane_id(ws_idx, pane_id)?),
            label: terminal.and_then(|terminal| terminal.manual_label.clone()),
            cwd: tab
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            command: terminal.and_then(|terminal| terminal.launch_argv.clone()),
            env: Default::default(),
        })
    }

    fn layout_root_cwd(
        &self,
        ws_idx: usize,
        replace_target: Option<(usize, usize)>,
        pane: &LayoutPane,
    ) -> PathBuf {
        if let Some(cwd) = pane.cwd.as_ref() {
            return PathBuf::from(cwd);
        }
        let follow_cwd = replace_target.and_then(|(_, tab_idx)| {
            let ws = self.state.workspaces.get(ws_idx)?;
            let tab = ws.tabs.get(tab_idx)?;
            tab.cwd_for_pane(
                tab.layout.focused(),
                &self.state.terminals,
                &self.terminal_runtimes,
            )
        });
        self.resolve_new_terminal_cwd(follow_cwd.or_else(|| {
            self.state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
                .and_then(|runtime| runtime.cwd())
        }))
    }

    fn apply_layout_node_to_pane(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        node: &LayoutNode,
    ) -> Result<(), String> {
        match node {
            LayoutNode::Pane { pane } => {
                self.apply_layout_pane_label(ws_idx, pane_id, pane);
                Ok(())
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let second_leaf = first_layout_leaf(second);
                let new_pane = self.layout_split_pane(
                    ws_idx,
                    pane_id,
                    direction.clone(),
                    *ratio,
                    second_leaf,
                )?;
                self.apply_layout_node_to_pane(ws_idx, pane_id, first)?;
                self.apply_layout_node_to_pane(ws_idx, new_pane, second)
            }
        }
    }

    fn layout_split_pane(
        &mut self,
        ws_idx: usize,
        target_pane_id: PaneId,
        direction: SplitDirection,
        ratio: f32,
        pane: &LayoutPane,
    ) -> Result<PaneId, String> {
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let cwd = pane.cwd.as_ref().map(PathBuf::from).or_else(|| {
            self.state.workspaces.get(ws_idx).and_then(|ws| {
                let tab_idx = ws.find_tab_index_for_pane(target_pane_id)?;
                ws.tabs.get(tab_idx)?.cwd_for_pane(
                    target_pane_id,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            })
        });
        let extra_env = super::env::normalize_launch_env(pane.env.clone())
            .map_err(|(_, message)| message.to_string())?;
        let direction = match direction {
            SplitDirection::Right => Direction::Horizontal,
            SplitDirection::Down => Direction::Vertical,
        };
        let command = layout_command(pane)?;
        let result = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return Err("workspace not found".into());
            };
            if let Some(argv) = command.as_deref() {
                ws.split_pane_argv_command_with_ratio(
                    target_pane_id,
                    direction,
                    ratio,
                    rows,
                    cols,
                    cwd,
                    argv,
                    extra_env,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    false,
                )
            } else {
                ws.split_pane_with_ratio(
                    target_pane_id,
                    direction,
                    ratio,
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                    extra_env,
                    false,
                )
            }
        };
        let (_, new_pane) = result
            .ok_or_else(|| "pane not found".to_string())?
            .map_err(|err| err.to_string())?;
        let new_pane_id = new_pane.pane_id;
        self.attach_new_layout_pane(new_pane);
        self.apply_layout_pane_label(ws_idx, new_pane_id, pane);
        Ok(new_pane_id)
    }

    fn attach_new_layout_pane(&mut self, new_pane: NewPane) {
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
    }

    fn apply_layout_pane_label(&mut self, ws_idx: usize, pane_id: PaneId, pane: &LayoutPane) {
        let Some(label) = pane
            .label
            .as_ref()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
        else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.set_manual_label(label.to_string());
        }
    }

    fn rollback_layout_tab(&mut self, ws_idx: usize, root_pane: PaneId) {
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.iter().position(|tab| tab.root_pane == root_pane))
        else {
            return;
        };
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let plugin_pane_ids = self.state.pane_ids_for_tab(ws_idx, tab_idx);
        if self
            .state
            .workspaces
            .get_mut(ws_idx)
            .is_some_and(|ws| ws.close_tab(tab_idx))
        {
            self.state.remove_plugin_pane_records(plugin_pane_ids);
            self.state.remove_unattached_terminal_ids(terminal_ids);
            self.shutdown_detached_terminal_runtimes();
        }
    }
}

fn first_layout_leaf(node: &LayoutNode) -> &LayoutPane {
    match node {
        LayoutNode::Pane { pane } => pane,
        LayoutNode::Split { first, .. } => first_layout_leaf(first),
    }
}

fn layout_command(pane: &LayoutPane) -> Result<Option<Vec<String>>, String> {
    match pane.command.as_ref() {
        Some(command) if command.is_empty() => Err("pane command must not be empty".into()),
        Some(command) => Ok(Some(command.clone())),
        None => Ok(None),
    }
}

fn validate_layout_tree(root: &LayoutNode) -> Result<(), String> {
    let mut stats = LayoutTreeStats {
        panes: 0,
        max_depth: 0,
    };
    validate_layout_node(root, 1, &mut stats)?;
    if stats.panes > MAX_LAYOUT_PANES {
        return Err(format!(
            "layout has {} panes; maximum is {}",
            stats.panes, MAX_LAYOUT_PANES
        ));
    }
    if stats.max_depth > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout depth is {}; maximum is {}",
            stats.max_depth, MAX_LAYOUT_DEPTH
        ));
    }
    Ok(())
}

struct LayoutTreeStats {
    panes: usize,
    max_depth: usize,
}

fn validate_layout_node(
    node: &LayoutNode,
    depth: usize,
    stats: &mut LayoutTreeStats,
) -> Result<(), String> {
    stats.max_depth = stats.max_depth.max(depth);
    if depth > MAX_LAYOUT_DEPTH {
        return Err(format!(
            "layout depth is {}; maximum is {}",
            depth, MAX_LAYOUT_DEPTH
        ));
    }
    match node {
        LayoutNode::Pane { pane } => {
            stats.panes += 1;
            if stats.panes > MAX_LAYOUT_PANES {
                return Err(format!("layout has more than {} panes", MAX_LAYOUT_PANES));
            }
            layout_command(pane)?;
            super::env::normalize_launch_env(pane.env.clone())
                .map_err(|(_, message)| message.to_string())?;
            Ok(())
        }
        LayoutNode::Split {
            first,
            second,
            ratio,
            ..
        } => {
            if !ratio.is_finite() {
                return Err("split ratio must be finite".into());
            }
            validate_layout_node(first, depth + 1, stats)?;
            validate_layout_node(second, depth + 1, stats)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, ResponseResult, SuccessResponse},
        config::Config,
        workspace::Workspace,
    };

    fn app_with_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("layout")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    #[test]
    fn layout_export_returns_portable_tree() {
        let mut app = app_with_workspace();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0]
            .layout
            .set_ratio_at(&[], 0.65);
        let right_terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(right)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&right_terminal_id)
            .unwrap()
            .set_manual_label("tests".into());

        let response = app.handle_layout_export(
            "req".into(),
            LayoutExportParams {
                tab_id: None,
                pane_id: None,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutExport { layout } = success.result else {
            panic!("expected layout export response");
        };
        assert_eq!(layout.workspace_id, app.public_workspace_id(0));
        assert_eq!(layout.focused_pane_id, app.public_pane_id(0, root).unwrap());
        let LayoutNode::Split {
            direction,
            ratio,
            second,
            ..
        } = layout.root
        else {
            panic!("expected split layout root");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert!((ratio - 0.65).abs() < f32::EPSILON);
        let LayoutNode::Pane { pane } = *second else {
            panic!("expected second pane");
        };
        assert_eq!(pane.label.as_deref(), Some("tests"));
        assert_eq!(pane.pane_id, Some(app.public_pane_id(0, right).unwrap()));
    }

    #[tokio::test]
    async fn layout_apply_replaces_tab_with_requested_tree() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: Some("dev".into()),
                focus: true,
                root: LayoutNode::Split {
                    direction: SplitDirection::Right,
                    ratio: 0.7,
                    first: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("editor".into()),
                            ..Default::default()
                        },
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("tests".into()),
                            command: Some(vec!["sh".into(), "-c".into(), "true".into()]),
                            env: std::collections::HashMap::from([(
                                "HERDR_ROLE".into(),
                                "tests".into(),
                            )]),
                            ..Default::default()
                        },
                    }),
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::LayoutApply { layout } = success.result else {
            panic!("expected layout apply response");
        };
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tab_display_name(0).as_deref(),
            Some("dev")
        );
        let LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } = layout.root
        else {
            panic!("expected split layout root");
        };
        assert_eq!(direction, SplitDirection::Right);
        assert!((ratio - 0.7).abs() < f32::EPSILON);
        let LayoutNode::Pane { pane: first_pane } = *first else {
            panic!("expected first pane");
        };
        let LayoutNode::Pane { pane: second_pane } = *second else {
            panic!("expected second pane");
        };
        assert_eq!(first_pane.label.as_deref(), Some("editor"));
        assert_eq!(second_pane.label.as_deref(), Some("tests"));
        assert_eq!(
            second_pane.command,
            Some(vec!["sh".into(), "-c".into(), "true".into()])
        );
    }

    #[tokio::test]
    async fn layout_apply_replace_drops_plugin_pane_records_of_replaced_tab() {
        let mut app = app_with_workspace();
        let original_tab_id = app.public_tab_id(0, 0).unwrap();
        let replaced_pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.plugin_panes.insert(
            replaced_pane,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.layout".into(),
                entrypoint: "board".into(),
            },
        );

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: None,
                tab_id: Some(original_tab_id),
                tab_label: Some("dev".into()),
                focus: true,
                root: LayoutNode::Pane {
                    pane: LayoutPane {
                        label: Some("editor".into()),
                        ..Default::default()
                    },
                },
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::LayoutApply { .. }));
        assert!(!app.state.plugin_panes.contains_key(&replaced_pane));
        app.state.assert_invariants_for_test();
    }

    #[tokio::test]
    async fn layout_apply_rejects_invalid_deep_leaf_without_creating_tab() {
        let mut app = app_with_workspace();
        let original_tab_count = app.state.workspaces[0].tabs.len();

        let response = app.handle_layout_apply(
            "req".into(),
            LayoutApplyParams {
                workspace_id: Some(app.public_workspace_id(0)),
                tab_id: None,
                tab_label: Some("bad".into()),
                focus: false,
                root: LayoutNode::Split {
                    direction: SplitDirection::Right,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            label: Some("editor".into()),
                            ..Default::default()
                        },
                    }),
                    second: Box::new(LayoutNode::Pane {
                        pane: LayoutPane {
                            command: Some(Vec::new()),
                            ..Default::default()
                        },
                    }),
                },
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_layout");
        assert_eq!(app.state.workspaces[0].tabs.len(), original_tab_count);
    }

    #[test]
    fn layout_validation_rejects_too_many_panes() {
        let mut root = LayoutNode::Pane {
            pane: LayoutPane::default(),
        };
        for _ in 0..MAX_LAYOUT_PANES {
            root = LayoutNode::Split {
                direction: SplitDirection::Right,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(LayoutNode::Pane {
                    pane: LayoutPane::default(),
                }),
            };
        }

        let err = validate_layout_tree(&root).unwrap_err();
        assert!(err.contains("maximum"));
    }
}
