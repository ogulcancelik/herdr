use std::time::{Duration, Instant};

use super::{
    api_helpers::{
        detect_state_from_api, encode_api_keys, encode_api_text, normalize_reported_agent_label,
        pane_agent_status,
    },
    App, Mode, OverlayPaneState, ToastKind,
};
use crate::events::AppEvent;

impl App {
    pub(crate) fn handle_internal_event(&mut self, ev: AppEvent) {
        if let AppEvent::ClipboardWrite { content } = ev {
            crate::selection::write_osc52_bytes(&content);
            return;
        }

        let overlay_state = if let AppEvent::PaneDied { pane_id } = &ev {
            self.overlay_panes.remove(pane_id)
        } else {
            None
        };

        if let AppEvent::PaneDied { pane_id } = &ev {
            if let Some((ws_idx, _)) = self.find_pane(*pane_id) {
                if let Some(public_pane_id) = self.public_pane_id(ws_idx, *pane_id) {
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneExited,
                        data: crate::api::schema::EventData::PaneExited {
                            pane_id: public_pane_id,
                            workspace_id: self.public_workspace_id(ws_idx),
                        },
                    });
                }
            }
        }

        let released_agent = if let AppEvent::HookAgentReleased {
            pane_id,
            known_agent,
            ..
        } = &ev
        {
            known_agent.map(|agent| (*pane_id, agent))
        } else {
            None
        };

        let update_ready_version = if let AppEvent::UpdateReady { version } = &ev {
            Some(version.clone())
        } else {
            None
        };
        let previous_toast = self.state.toast.clone();
        let pane_updates = self.state.handle_app_event(ev);
        for update in &pane_updates {
            self.emit_pane_state_update(update);
        }
        if let Some((pane_id, agent)) = released_agent {
            if pane_updates.iter().any(|update| update.pane_id == pane_id) {
                if let Some((ws_idx, _)) = self.find_pane(pane_id) {
                    if let Some(runtime) = self.state.workspaces[ws_idx].runtimes.get(&pane_id) {
                        runtime.begin_graceful_release(agent);
                    }
                }
            }
        }
        if let Some(overlay) = overlay_state {
            self.restore_overlay_after_exit(overlay);
        }

        if self.local_terminal_notifications
            && matches!(
                self.state.toast_config.delivery,
                crate::config::ToastDelivery::Terminal
            )
        {
            if let Some(version) = update_ready_version {
                let _ = crate::terminal_notify::show_notification(
                    &format!("v{version} available"),
                    Some("detach, then run `herdr update`"),
                );
            } else {
                for update in &pane_updates {
                    let is_active_tab = self
                        .state
                        .pane_is_in_active_tab(update.ws_idx, update.pane_id);
                    let suppress_active_tab_notifications =
                        crate::app::actions::active_tab_suppresses_notifications(
                            is_active_tab,
                            self.state.outer_terminal_focus,
                        );
                    let Some(kind) = crate::app::actions::notification_toast_for_state_change(
                        suppress_active_tab_notifications,
                        update.previous_state,
                        update.state,
                    ) else {
                        continue;
                    };
                    let Some(ws) = self.state.workspaces.get(update.ws_idx) else {
                        continue;
                    };
                    let Some(pane) = ws
                        .tabs
                        .iter()
                        .find_map(|tab| tab.panes.get(&update.pane_id))
                    else {
                        continue;
                    };
                    let Some(agent_label) = pane.effective_agent_label() else {
                        continue;
                    };
                    let event_text = match kind {
                        ToastKind::NeedsAttention => "needs attention",
                        ToastKind::Finished => "finished",
                        ToastKind::UpdateInstalled => "updated",
                    };
                    let _ = crate::terminal_notify::show_notification(
                        &format!("{} {}", agent_label, event_text),
                        Some(&crate::app::actions::notification_context(
                            ws,
                            update.ws_idx,
                            update.pane_id,
                        )),
                    );
                }
            }
        }

        self.sync_toast_deadline(previous_toast);
    }

    fn restore_overlay_after_exit(&mut self, overlay: OverlayPaneState) {
        let Some(ws) = self.state.workspaces.get_mut(overlay.ws_idx) else {
            return;
        };
        if overlay.tab_idx >= ws.tabs.len() {
            return;
        }

        ws.active_tab = overlay.tab_idx;
        let tab = &mut ws.tabs[overlay.tab_idx];
        if tab.panes.contains_key(&overlay.previous_focus) {
            tab.layout.focus_pane(overlay.previous_focus);
        }
        tab.zoomed = overlay.previous_zoomed;

        if self.state.active == Some(overlay.ws_idx) {
            self.state.mode = Mode::Terminal;
        }
    }

    fn emit_pane_state_update(&self, update: &crate::app::actions::PaneStateUpdate) {
        let Some(pane_id) = self.public_pane_id(update.ws_idx, update.pane_id) else {
            return;
        };
        let workspace_id = self.public_workspace_id(update.ws_idx);

        if update.previous_agent_label != update.agent_label {
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentDetected,
                data: crate::api::schema::EventData::PaneAgentDetected {
                    pane_id: pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                    agent: update.agent_label.clone(),
                },
            });
        }

        if update.previous_state != update.state {
            let agent_status = self
                .state
                .workspaces
                .get(update.ws_idx)
                .and_then(|ws| ws.pane_state(update.pane_id))
                .map(|pane| pane_agent_status(pane.state, pane.seen))
                .unwrap_or_else(|| pane_agent_status(update.state, true));
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id,
                    workspace_id,
                    agent_status,
                },
            });
        }
    }

    pub(super) fn sync_toast_deadline(
        &mut self,
        previous_toast: Option<crate::app::state::ToastNotification>,
    ) {
        if self.state.toast != previous_toast {
            self.toast_deadline = self.state.toast.as_ref().map(|toast| {
                let duration = match toast.kind {
                    ToastKind::NeedsAttention => Duration::from_secs(8),
                    ToastKind::Finished => Duration::from_secs(5),
                    ToastKind::UpdateInstalled => Duration::from_secs(3),
                };
                Instant::now() + duration
            });
        }
    }

    pub(super) fn emit_event(&self, event: crate::api::schema::EventEnvelope) {
        self.event_hub.push(event);
    }

    pub(crate) fn sync_focus_events(&mut self) {
        let current_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        if current_focus == self.last_focus {
            return;
        }

        if let Some((ws_idx, pane_id)) = self.last_focus {
            self.send_pane_focus_event(ws_idx, pane_id, crate::ghostty::FocusEvent::Lost);
        }
        if let Some((ws_idx, pane_id)) = current_focus {
            self.send_pane_focus_event(ws_idx, pane_id, crate::ghostty::FocusEvent::Gained);
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::WorkspaceFocused,
                data: crate::api::schema::EventData::WorkspaceFocused {
                    workspace_id: self.public_workspace_id(ws_idx),
                },
            });
            if let Some(tab_id) =
                self.public_tab_id(ws_idx, self.state.workspaces[ws_idx].active_tab)
            {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabFocused,
                    data: crate::api::schema::EventData::TabFocused {
                        tab_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
            }
            if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneFocused,
                    data: crate::api::schema::EventData::PaneFocused {
                        pane_id: public_pane_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
            }
        }

        self.last_focus = current_focus;
    }

    fn send_pane_focus_event(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        event: crate::ghostty::FocusEvent,
    ) {
        let Some(runtime) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.runtime(pane_id))
        else {
            return;
        };
        runtime.try_send_focus_event(event);
    }

    pub(crate) fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_internal_events();
        use bytes::Bytes;

        use crate::api::schema::{
            ErrorBody, ErrorResponse, IntegrationInstallResult, IntegrationUninstallResult, Method,
            PaneListParams, PaneReadResult, ReadSource, ResponseResult, SuccessResponse,
            TabListParams,
        };

        let response = match request.method {
            Method::ServerStop(_) => {
                self.state.should_quit = true;
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::ServerReloadConfig(_) => {
                let report = self.reload_config();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::ConfigReload {
                        status: report.status,
                        diagnostics: report.diagnostics,
                    },
                }
            }
            Method::WorkspaceList(_) => SuccessResponse {
                id: request.id,
                result: ResponseResult::WorkspaceList {
                    workspaces: self
                        .state
                        .workspaces
                        .iter()
                        .enumerate()
                        .map(|(idx, _)| self.workspace_info(idx))
                        .collect(),
                },
            },
            Method::WorkspaceGet(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                let Some(_) = self.state.workspaces.get(index) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceCreate(params) => {
                let cwd = params
                    .cwd
                    .map(std::path::PathBuf::from)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("/"));
                match self.create_workspace_with_options(cwd, params.focus) {
                    Ok(index) => {
                        if let Some(label) = params.label {
                            if let Some(workspace) = self.state.workspaces.get_mut(index) {
                                workspace.set_custom_name(label);
                                crate::logging::workspace_renamed(&workspace.id);
                            }
                        }
                        let workspace = self.workspace_info(index);
                        let tab = self
                            .tab_info(index, 0)
                            .expect("new workspace should have an initial tab");
                        let root_pane = self
                            .root_pane_info(index, 0)
                            .expect("new workspace should have an initial root pane");
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::WorkspaceCreated,
                            data: crate::api::schema::EventData::WorkspaceCreated {
                                workspace: workspace.clone(),
                            },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::TabCreated,
                            data: crate::api::schema::EventData::TabCreated { tab: tab.clone() },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::PaneCreated,
                            data: crate::api::schema::EventData::PaneCreated {
                                pane: root_pane.clone(),
                            },
                        });
                        SuccessResponse {
                            id: request.id,
                            result: self
                                .workspace_created_result(index)
                                .expect("new workspace should produce a complete create response"),
                        }
                    }
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_create_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
            }
            Method::WorkspaceFocus(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                if self.state.workspaces.get(index).is_none() {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                }
                self.state.switch_workspace(index);
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceRename(params) => {
                let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", params.workspace_id),
                        },
                    })
                    .unwrap();
                };
                let Some(ws) = self.state.workspaces.get_mut(index) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", params.workspace_id),
                        },
                    })
                    .unwrap();
                };
                ws.set_custom_name(params.label.clone());
                crate::logging::workspace_renamed(&ws.id);
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::WorkspaceRenamed,
                    data: crate::api::schema::EventData::WorkspaceRenamed {
                        workspace_id: self.public_workspace_id(index),
                        label: params.label,
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::WorkspaceInfo {
                        workspace: self.workspace_info(index),
                    },
                }
            }
            Method::WorkspaceClose(target) => {
                let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                };
                if self.state.workspaces.get(index).is_none() {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: format!("workspace {} not found", target.workspace_id),
                        },
                    })
                    .unwrap();
                }
                self.state.selected = index;
                self.state.close_selected_workspace();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::WorkspaceClosed,
                    data: crate::api::schema::EventData::WorkspaceClosed {
                        workspace_id: target.workspace_id,
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::TabList(TabListParams { workspace_id }) => {
                let tabs = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
                    };
                    let Some(ws) = self.state.workspaces.get(ws_idx) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
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
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabList { tabs },
                }
            }
            Method::TabGet(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabCreate(params) => {
                let crate::api::schema::TabCreateParams {
                    workspace_id,
                    cwd,
                    focus,
                    label,
                } = params;
                let ws_idx = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "workspace_not_found".into(),
                                message: format!("workspace {} not found", workspace_id),
                            },
                        })
                        .unwrap();
                    };
                    ws_idx
                } else if let Some(active) = self.state.active {
                    active
                } else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "workspace_not_found".into(),
                            message: "no active workspace".into(),
                        },
                    })
                    .unwrap();
                };
                let cwd = cwd
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        self.state.workspaces.get(ws_idx).and_then(|ws| {
                            ws.active_tab()
                                .and_then(|tab| tab.focused_runtime())
                                .and_then(|rt| rt.cwd())
                        })
                    })
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("/"));
                let (rows, cols) = self.state.estimate_pane_size();
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
                            self.state.pane_scrollback_limit_bytes,
                            self.state.host_terminal_theme,
                        )
                    });
                match result {
                    Ok(tab_idx) => {
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
                            self.state.switch_workspace(ws_idx);
                            self.state.switch_tab(tab_idx);
                            self.state.mode = Mode::Terminal;
                        }
                        self.schedule_session_save();
                        let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                        let root_pane = self
                            .root_pane_info(ws_idx, tab_idx)
                            .expect("new tab should have a root pane");
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::TabCreated,
                            data: crate::api::schema::EventData::TabCreated { tab: tab.clone() },
                        });
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::PaneCreated,
                            data: crate::api::schema::EventData::PaneCreated {
                                pane: root_pane.clone(),
                            },
                        });
                        SuccessResponse {
                            id: request.id,
                            result: self
                                .tab_created_result(ws_idx, tab_idx)
                                .expect("new tab should produce a complete create response"),
                        }
                    }
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "tab_create_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
            }
            Method::TabFocus(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                self.state.switch_workspace(ws_idx);
                self.state.switch_tab(tab_idx);
                let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabRename(params) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", params.tab_id),
                        },
                    })
                    .unwrap();
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
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", params.tab_id),
                        },
                    })
                    .unwrap();
                };
                tab.set_custom_name(params.label.clone());
                crate::logging::tab_renamed(&workspace_id, &tab_id);
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabRenamed,
                    data: crate::api::schema::EventData::TabRenamed {
                        tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                        workspace_id: self.public_workspace_id(ws_idx),
                        label: params.label,
                    },
                });
                let tab = self.tab_info(ws_idx, tab_idx).unwrap();
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::TabInfo { tab },
                }
            }
            Method::TabClose(target) => {
                let Some((ws_idx, tab_idx)) = self.parse_tab_id(&target.tab_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_not_found".into(),
                            message: format!("tab {} not found", target.tab_id),
                        },
                    })
                    .unwrap();
                };
                if ws.tabs.len() <= 1 {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_close_failed".into(),
                            message: "cannot close the last tab in a workspace".into(),
                        },
                    })
                    .unwrap();
                }
                if !ws.close_tab(tab_idx) {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "tab_close_failed".into(),
                            message: format!("tab {} could not be closed", target.tab_id),
                        },
                    })
                    .unwrap();
                }
                self.schedule_session_save();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::TabClosed,
                    data: crate::api::schema::EventData::TabClosed {
                        tab_id: target.tab_id,
                        workspace_id: self.public_workspace_id(ws_idx),
                    },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSplit(params) => {
                let Some((ws_idx, target_pane_id)) = self.parse_pane_id(&params.target_pane_id)
                else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.target_pane_id),
                        },
                    })
                    .unwrap();
                };
                let (rows, cols) = self.state.estimate_pane_size();
                let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.target_pane_id),
                        },
                    })
                    .unwrap();
                };
                ws.layout.focus_pane(target_pane_id);
                let direction = match params.direction {
                    crate::api::schema::SplitDirection::Right => {
                        ratatui::layout::Direction::Horizontal
                    }
                    crate::api::schema::SplitDirection::Down => {
                        ratatui::layout::Direction::Vertical
                    }
                };
                let new_pane_id = match ws.split_focused(
                    direction,
                    rows,
                    cols,
                    params.cwd.map(std::path::PathBuf::from),
                    self.state.pane_scrollback_limit_bytes,
                    self.state.host_terminal_theme,
                ) {
                    Ok(new_pane_id) => new_pane_id,
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_split_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                };
                if !params.focus {
                    ws.layout.focus_pane(target_pane_id);
                }
                self.schedule_session_save();
                let pane = self.pane_info(ws_idx, new_pane_id).unwrap();
                self.emit_event(crate::api::schema::EventEnvelope {
                    event: crate::api::schema::EventKind::PaneCreated,
                    data: crate::api::schema::EventData::PaneCreated { pane: pane.clone() },
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneInfo { pane },
                }
            }
            Method::PaneList(PaneListParams { workspace_id }) => {
                match self.collect_panes_for_workspace(workspace_id.as_deref()) {
                    Ok(panes) => SuccessResponse {
                        id: request.id,
                        result: ResponseResult::PaneList { panes },
                    },
                    Err((code, message)) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody { code, message },
                        })
                        .unwrap();
                    }
                }
            }
            Method::PaneGet(target) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(pane) = self.pane_info(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneInfo { pane },
                }
            }
            Method::PaneRead(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(tab_idx) = self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
                else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
                let text = match params.source {
                    ReadSource::Visible => pane.visible_text(),
                    ReadSource::Recent => pane.recent_text(requested_lines),
                    ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
                };
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneRead {
                        read: PaneReadResult {
                            pane_id: params.pane_id,
                            workspace_id,
                            tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                            source: params.source,
                            text,
                            revision: 0,
                            truncated: false,
                        },
                    },
                }
            }
            Method::PaneReportAgent(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "invalid_agent".into(),
                            message: "agent label must not be empty".into(),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookStateReported {
                    pane_id,
                    source: params.source,
                    agent_label,
                    state: detect_state_from_api(params.state),
                    message: params.message,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneClearAgentAuthority(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
                    pane_id,
                    source: params.source,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneReleaseAgent(params) => {
                let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "invalid_agent".into(),
                            message: "agent label must not be empty".into(),
                        },
                    })
                    .unwrap();
                };
                self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
                    pane_id,
                    source: params.source,
                    known_agent: crate::detect::parse_agent_label(&agent_label),
                    agent_label,
                });
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendText(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_send_failed".into(),
                            message: err.to_string(),
                        },
                    })
                    .unwrap();
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendInput(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let encoded_keys = match encode_api_keys(runtime, &params.keys) {
                    Ok(encoded_keys) => encoded_keys,
                    Err(key) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "invalid_key".into(),
                                message: format!("unsupported key {key}"),
                            },
                        })
                        .unwrap();
                    }
                };
                if !params.text.is_empty() {
                    let text_bytes = encode_api_text(runtime, &params.text);
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(text_bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                for bytes in encoded_keys {
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneClose(target) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", target.pane_id),
                        },
                    })
                    .unwrap();
                };
                let workspace_id = self.state.workspaces[ws_idx].id.clone();
                let should_close_workspace = {
                    let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_not_found".into(),
                                message: format!("pane {} not found", target.pane_id),
                            },
                        })
                        .unwrap();
                    };
                    ws.close_pane(pane_id)
                };
                if should_close_workspace {
                    self.state.selected = ws_idx;
                    self.state.close_selected_workspace();
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneClosed,
                        data: crate::api::schema::EventData::PaneClosed {
                            pane_id: target.pane_id.clone(),
                            workspace_id: workspace_id.clone(),
                        },
                    });
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::WorkspaceClosed,
                        data: crate::api::schema::EventData::WorkspaceClosed { workspace_id },
                    });
                } else {
                    self.schedule_session_save();
                    self.emit_event(crate::api::schema::EventEnvelope {
                        event: crate::api::schema::EventKind::PaneClosed,
                        data: crate::api::schema::EventData::PaneClosed {
                            pane_id: target.pane_id,
                            workspace_id,
                        },
                    });
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::PaneSendKeys(params) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return serde_json::to_string(&ErrorResponse {
                        id: request.id,
                        error: ErrorBody {
                            code: "pane_not_found".into(),
                            message: format!("pane {} not found", params.pane_id),
                        },
                    })
                    .unwrap();
                };
                let encoded_keys = match encode_api_keys(runtime, &params.keys) {
                    Ok(encoded_keys) => encoded_keys,
                    Err(key) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "invalid_key".into(),
                                message: format!("unsupported key {key}"),
                            },
                        })
                        .unwrap();
                    }
                };
                for bytes in encoded_keys {
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "pane_send_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                }
                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::Ok {},
                }
            }
            Method::IntegrationInstall(params) => {
                let target = params.target;
                let messages = match crate::integration::install_target(target) {
                    Ok(messages) => messages,
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "integration_install_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                };

                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::IntegrationInstall {
                        target,
                        details: IntegrationInstallResult { messages },
                    },
                }
            }
            Method::IntegrationUninstall(params) => {
                let target = params.target;
                let messages = match crate::integration::uninstall_target(target) {
                    Ok(messages) => messages,
                    Err(err) => {
                        return serde_json::to_string(&ErrorResponse {
                            id: request.id,
                            error: ErrorBody {
                                code: "integration_uninstall_failed".into(),
                                message: err.to_string(),
                            },
                        })
                        .unwrap();
                    }
                };

                SuccessResponse {
                    id: request.id,
                    result: ResponseResult::IntegrationUninstall {
                        target,
                        details: IntegrationUninstallResult { messages },
                    },
                }
            }
            _ => {
                return serde_json::to_string(&ErrorResponse {
                    id: request.id,
                    error: ErrorBody {
                        code: "not_implemented".into(),
                        message: "method not implemented yet".into(),
                    },
                })
                .unwrap();
            }
        };

        serde_json::to_string(&response).unwrap()
    }
}
