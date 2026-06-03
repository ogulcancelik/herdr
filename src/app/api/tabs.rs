use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, TabCreateParams, TabListParams,
    TabMoveParams, TabRenameParams, TabTarget,
};
use crate::app::{App, Mode};

use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_tab_list(&mut self, id: String, params: TabListParams) -> String {
        let tabs = if let Some(workspace_id) = params.workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return workspace_not_found(id, &workspace_id);
            };
            (0..ws.tabs.len())
                .filter_map(|tab_idx| self.tab_info(ws_idx, tab_idx))
                .collect()
        } else {
            let mut tabs = Vec::new();
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab_idx in 0..ws.tabs.len() {
                    if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                        tabs.push(tab);
                    }
                }
            }
            tabs
        };

        encode_success(id, ResponseResult::TabList { tabs })
    }

    pub(super) fn handle_tab_get(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return tab_not_found(id, &target.tab_id);
        };

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_create(&mut self, id: String, params: TabCreateParams) -> String {
        let TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
        } = params;
        let ws_idx = if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return workspace_not_found(id, &workspace_id);
            };
            ws_idx
        } else if let Some(active) = self.state.active {
            active
        } else {
            return encode_error(id, "workspace_not_found", "no active workspace");
        };
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| {
            let follow_cwd = self
                .state
                .focused_runtime_in_workspace(&self.terminal_runtimes, ws_idx)
                .and_then(|rt| rt.cwd());
            self.resolve_new_terminal_cwd(follow_cwd)
        });
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let result = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .ok_or_else(|| std::io::Error::other("workspace disappeared"))
            .and_then(|ws| {
                ws.create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
                )
            });
        match result {
            Ok((tab_idx, terminal, runtime)) => {
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                if let Some(label) = label {
                    let workspace_id = self.state.workspaces[ws_idx].id.clone();
                    let tab_id = self
                        .public_tab_id(ws_idx, tab_idx)
                        .unwrap_or_else(|| format!("{}:{}", workspace_id, tab_idx + 1));
                    if let Some(tab) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.tabs.get_mut(tab_idx))
                    {
                        tab.set_custom_name(label);
                        crate::logging::tab_renamed(&workspace_id, &tab_id);
                    }
                }
                if focus {
                    self.state.switch_workspace_tab(ws_idx, tab_idx);
                    self.state.mode = Mode::Terminal;
                }
                self.schedule_session_save();
                let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                let root_pane = self
                    .root_pane_info(ws_idx, tab_idx)
                    .expect("new tab should have a root pane");
                self.emit_event(EventEnvelope {
                    event: EventKind::TabCreated,
                    data: EventData::TabCreated { tab: tab.clone() },
                });
                self.emit_event(EventEnvelope {
                    event: EventKind::PaneCreated,
                    data: EventData::PaneCreated {
                        pane: root_pane.clone(),
                    },
                });
                encode_success(
                    id,
                    self.tab_created_result(ws_idx, tab_idx)
                        .expect("new tab should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "tab_create_failed", err.to_string()),
        }
    }

    pub(super) fn handle_tab_focus(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        self.state.switch_workspace_tab(ws_idx, tab_idx);
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_rename(&mut self, id: String, params: TabRenameParams) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let tab_id = self
            .public_tab_id(ws_idx, tab_idx)
            .unwrap_or_else(|| format!("{}:{}", workspace_id, tab_idx + 1));
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return tab_not_found(id, &params.tab_id);
        };
        tab.set_custom_name(params.label.clone());
        crate::logging::tab_renamed(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                workspace_id: self.public_workspace_id(ws_idx),
                label: params.label,
            },
        });
        let tab = self.tab_info(ws_idx, tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }

    pub(super) fn handle_tab_close(&mut self, id: String, target: TabTarget) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
            return tab_not_found(id, &target.tab_id);
        };
        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &target.tab_id);
        };
        if ws.tabs.len() <= 1 {
            return encode_error(
                id,
                "tab_close_failed",
                "cannot close the last tab in a workspace",
            );
        }
        if !ws.close_tab(tab_idx) {
            return encode_error(
                id,
                "tab_close_failed",
                format!("tab {} could not be closed", target.tab_id),
            );
        }
        self.state.remove_unattached_terminal_ids(terminal_ids);
        self.shutdown_detached_terminal_runtimes();
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabClosed,
            data: EventData::TabClosed {
                tab_id: target.tab_id,
                workspace_id: self.public_workspace_id(ws_idx),
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_tab_move(&mut self, id: String, params: TabMoveParams) -> String {
        let Some((ws_idx, source_tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return tab_not_found(id, &params.tab_id);
        };
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return tab_not_found(id, &params.tab_id);
        };
        let tab_count = ws.tabs.len();
        if params.position == 0 || params.position > tab_count {
            return encode_error(
                id,
                "invalid_position",
                format!(
                    "position must be between 1 and {} (got {})",
                    tab_count, params.position
                ),
            );
        }
        // Convert 1-based position to 0-based insert_idx.
        let insert_idx = params.position - 1;
        // move_tab uses insertion semantics: to place the tab *at* index N,
        // we pass insert_idx = N when source is after N, or insert_idx = N + 1
        // when source is before N. The simplest correct mapping: pass the raw
        // target index when source >= target (moving left), or target + 1 when
        // source < target (moving right), because remove-then-insert shifts
        // indices. But Workspace::move_tab already handles this internally —
        // it normalises `insert_idx` by subtracting 1 when source < insert_idx.
        // So we can pass `params.position` directly (which equals target + 1 in
        // 0-based remove-then-insert terms when moving right, and target when
        // moving left... actually let's just map cleanly).
        //
        // Workspace::move_tab(source, insert) does:
        //   target = if source < insert { insert - 1 } else { insert }
        //   remove(source); insert(target)
        //
        // We want the tab to end up at 0-based index `desired = position - 1`.
        // If source < desired: target = insert - 1 = desired => insert = desired + 1
        //                                                      = position
        // If source > desired: target = insert = desired => insert = desired
        //                                                  = position - 1
        // If source == desired: no-op (move_tab returns false)
        let insert_idx = if source_tab_idx < insert_idx {
            insert_idx + 1
        } else {
            insert_idx
        };
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let ws = self.state.workspaces.get_mut(ws_idx).unwrap();
        ws.move_tab(source_tab_idx, insert_idx);
        let new_tab_idx = params.position - 1;
        let tab_id = self
            .public_tab_id(ws_idx, new_tab_idx)
            .unwrap_or_else(|| format!("{}:{}", workspace_id, params.position));
        crate::logging::tab_moved(&workspace_id, &tab_id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::TabMoved,
            data: EventData::TabMoved {
                tab_id: self
                    .public_tab_id(ws_idx, new_tab_idx)
                    .unwrap_or_else(|| format!("{}:{}", workspace_id, params.position)),
                workspace_id: self.public_workspace_id(ws_idx),
                position: params.position,
            },
        });
        let tab = self.tab_info(ws_idx, new_tab_idx).unwrap();

        encode_success(id, ResponseResult::TabInfo { tab })
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

fn tab_not_found(id: String, tab_id: &str) -> String {
    encode_error(id, "tab_not_found", format!("tab {tab_id} not found"))
}
