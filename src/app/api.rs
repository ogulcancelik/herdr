use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod agents;
mod integrations;
mod panes;
mod responses;
mod tabs;
mod workspaces;
mod worktrees;

use super::{api_helpers::pane_agent_status, App, Mode, OverlayPaneState, ToastKind};
use crate::events::AppEvent;

impl App {
    pub(crate) fn handle_internal_event(&mut self, ev: AppEvent) {
        if let AppEvent::ClipboardWrite { content } = ev {
            crate::selection::write_osc52_bytes(&content);
            return;
        }

        if let AppEvent::GitStatusRefreshed { results } = ev {
            self.git_refresh_in_flight = false;
            self.last_git_remote_status_refresh = Instant::now();
            if self
                .state
                .apply_workspace_git_statuses(&self.terminal_runtimes, results)
            {
                self.render_dirty.store(true, Ordering::Release);
                self.render_notify.notify_one();
            }
            return;
        }

        if let AppEvent::WorktreeAddFinished(result) = ev {
            self.handle_worktree_add_finished(result);
            return;
        }

        if let AppEvent::WorktreeRemoveFinished(result) = ev {
            self.handle_worktree_remove_finished(result);
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

        let update_ready = if let AppEvent::UpdateReady {
            version,
            install_command,
        } = &ev
        {
            Some((version.clone(), install_command.clone()))
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
                    if let Some(runtime) = self.state.runtime_for_pane_in_workspace(
                        &self.terminal_runtimes,
                        ws_idx,
                        pane_id,
                    ) {
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
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System
            )
        {
            let notify = match self.state.toast_config.delivery {
                crate::config::ToastDelivery::Terminal => crate::terminal_notify::show_notification,
                crate::config::ToastDelivery::System => crate::platform::show_desktop_notification,
                _ => unreachable!("toast delivery was checked above"),
            };

            if let Some((version, install_command)) = update_ready {
                let _ = notify(
                    &format!("v{version} available"),
                    Some(&format!("detach, then run `{install_command}`")),
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
                    let Some(agent_label) = self
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .and_then(|terminal| terminal.effective_agent_label())
                    else {
                        continue;
                    };
                    let event_text = match kind {
                        ToastKind::NeedsAttention => "needs attention",
                        ToastKind::Finished => "finished",
                        ToastKind::UpdateInstalled => "updated",
                    };
                    let _ = notify(
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
        self.shutdown_detached_terminal_runtimes();
    }

    fn restore_overlay_after_exit(&mut self, overlay: OverlayPaneState) {
        for temp_file in &overlay.temp_files {
            let _ = std::fs::remove_file(temp_file);
        }

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
                .map(|pane| pane_agent_status(update.state, pane.seen))
                .unwrap_or_else(|| pane_agent_status(update.state, true));
            let custom_status = update.custom_status.clone();
            self.emit_event(crate::api::schema::EventEnvelope {
                event: crate::api::schema::EventKind::PaneAgentStatusChanged,
                data: crate::api::schema::EventData::PaneAgentStatusChanged {
                    pane_id,
                    workspace_id,
                    agent_status,
                    custom_status,
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
        let Some(runtime) = self.state.workspaces.get(ws_idx).and_then(|_| {
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        }) else {
            return;
        };
        runtime.try_send_focus_event(event);
    }

    pub(crate) fn handle_api_request(&mut self, request: crate::api::schema::Request) -> String {
        self.drain_all_internal_events();
        self.handle_api_request_after_internal_events_drained(request)
    }

    pub(crate) fn handle_api_request_after_internal_events_drained(
        &mut self,
        request: crate::api::schema::Request,
    ) -> String {
        use crate::api::schema::{Method, ResponseResult, SuccessResponse};

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
            Method::WorkspaceList(_) => return self.handle_workspace_list(request.id),
            Method::WorkspaceGet(target) => return self.handle_workspace_get(request.id, target),
            Method::WorkspaceCreate(params) => {
                return self.handle_workspace_create(request.id, params);
            }
            Method::WorkspaceFocus(target) => {
                return self.handle_workspace_focus(request.id, target)
            }
            Method::WorkspaceRename(params) => {
                return self.handle_workspace_rename(request.id, params);
            }
            Method::WorkspaceClose(target) => {
                return self.handle_workspace_close(request.id, target)
            }
            Method::WorktreeList(params) => return self.handle_worktree_list(request.id, params),
            Method::WorktreeCreate(params) => {
                return self.handle_worktree_create(request.id, params);
            }
            Method::WorktreeOpen(params) => return self.handle_worktree_open(request.id, params),
            Method::WorktreeRemove(params) => {
                return self.handle_worktree_remove(request.id, params);
            }
            Method::TabList(params) => return self.handle_tab_list(request.id, params),
            Method::TabGet(target) => return self.handle_tab_get(request.id, target),
            Method::TabCreate(params) => return self.handle_tab_create(request.id, params),
            Method::TabFocus(target) => return self.handle_tab_focus(request.id, target),
            Method::TabRename(params) => return self.handle_tab_rename(request.id, params),
            Method::TabClose(target) => return self.handle_tab_close(request.id, target),
            Method::AgentList(_) => return self.handle_agent_list(request.id),
            Method::AgentGet(target) => return self.handle_agent_get(request.id, target),
            Method::AgentFocus(target) => return self.handle_agent_focus(request.id, target),
            Method::AgentRename(params) => return self.handle_agent_rename(request.id, params),
            Method::AgentStart(params) => return self.handle_agent_start(request.id, params),
            Method::AgentRead(params) => return self.handle_agent_read(request.id, params),
            Method::AgentSend(params) => return self.handle_agent_send(request.id, params),
            Method::PaneSplit(params) => return self.handle_pane_split(request.id, params),
            Method::PaneList(params) => return self.handle_pane_list(request.id, params),
            Method::PaneGet(target) => return self.handle_pane_get(request.id, target),
            Method::PaneRename(params) => return self.handle_pane_rename(request.id, params),
            Method::PaneRead(params) => return self.handle_pane_read(request.id, params),
            Method::PaneReportAgent(params) => {
                return self.handle_pane_report_agent(request.id, params);
            }
            Method::PaneClearAgentAuthority(params) => {
                return self.handle_pane_clear_agent_authority(request.id, params);
            }
            Method::PaneReleaseAgent(params) => {
                return self.handle_pane_release_agent(request.id, params);
            }
            Method::PaneSendText(params) => return self.handle_pane_send_text(request.id, params),
            Method::PaneSendInput(params) => {
                return self.handle_pane_send_input(request.id, params)
            }
            Method::PaneClose(target) => return self.handle_pane_close(request.id, target),
            Method::PaneSendKeys(params) => return self.handle_pane_send_keys(request.id, params),
            Method::IntegrationInstall(params) => {
                return self.handle_integration_install(request.id, params);
            }
            Method::IntegrationUninstall(params) => {
                return self.handle_integration_uninstall(request.id, params);
            }
            _ => {
                return responses::encode_error(
                    request.id,
                    "not_implemented",
                    "method not implemented yet",
                );
            }
        };

        serde_json::to_string(&response).unwrap()
    }
}
