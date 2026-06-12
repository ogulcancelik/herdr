use bytes::Bytes;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, PaneClearAgentAuthorityParams, PaneListParams,
    PaneReadParams, PaneReadResult, PaneReleaseAgentParams, PaneRenameParams,
    PaneReportAgentParams, PaneReportAgentSessionParams, PaneReportMetadataParams,
    PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams, PaneSplitParams, PaneTarget,
    ReadFormat, ReadSource, ResponseResult,
};
use crate::app::{App, Mode};

use super::super::api_helpers::{
    detect_state_from_api, encode_api_keys, encode_api_text, normalize_custom_status,
    normalize_reported_agent_label, sanitize_reported_prompt,
};
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_pane_split(&mut self, id: String, params: PaneSplitParams) -> String {
        let Some((ws_idx, target_pane_id)) = self.parse_pane_id(&params.target_pane_id) else {
            return pane_not_found(id, &params.target_pane_id);
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let split_cwd = params.cwd.map(std::path::PathBuf::from).or_else(|| {
            let follow_cwd = self.state.workspaces.get(ws_idx).and_then(|ws| {
                let tab_idx = ws.find_tab_index_for_pane(target_pane_id)?;
                ws.tabs.get(tab_idx)?.cwd_for_pane(
                    target_pane_id,
                    &self.state.terminals,
                    &self.terminal_runtimes,
                )
            });
            Some(self.resolve_new_terminal_cwd(follow_cwd))
        });
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let previous_focus = self.state.current_pane_focus_target();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return pane_not_found(id, &params.target_pane_id);
        };
        let direction = match params.direction {
            crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            crate::api::schema::SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let (target_tab_idx, new_pane) = match ws.split_pane(
            target_pane_id,
            direction,
            rows,
            cols,
            split_cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode),
            params.focus,
        ) {
            Some(Ok(result)) => result,
            Some(Err(err)) => return encode_error(id, "pane_split_failed", err.to_string()),
            None => return pane_not_found(id, &params.target_pane_id),
        };
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.state.mode = Mode::Terminal;
        }
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        self.schedule_session_save();
        let pane = self.pane_info(ws_idx, new_pane.pane_id).unwrap();
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: pane.clone() },
        });

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_list(&mut self, id: String, params: PaneListParams) -> String {
        match self.collect_panes_for_workspace(params.workspace_id.as_deref()) {
            Ok(panes) => encode_success(id, ResponseResult::PaneList { panes }),
            Err((code, message)) => encode_error(id, &code, message),
        }
    }

    pub(super) fn handle_pane_get(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_rename(&mut self, id: String, params: PaneRenameParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        match params.label.map(|label| label.trim().to_string()) {
            Some(label) if !label.is_empty() => terminal.set_manual_label(label),
            _ => terminal.clear_manual_label(),
        }
        self.state.mark_session_dirty();
        let pane = self.pane_info(ws_idx, pane_id).unwrap();

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_read(&mut self, id: String, params: PaneReadParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
        let text = match params.format {
            ReadFormat::Text => match params.source {
                ReadSource::Visible => pane.visible_text(),
                ReadSource::Recent => pane.recent_text(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
            },
            ReadFormat::Ansi => match params.source {
                ReadSource::Visible => pane.visible_ansi(),
                ReadSource::Recent => pane.recent_ansi(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_ansi(requested_lines),
            },
        };

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: params.pane_id,
                    workspace_id,
                    tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_pane_report_agent(
        &mut self,
        id: String,
        params: PaneReportAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            state: detect_state_from_api(params.state),
            message: params.message,
            custom_status: normalize_custom_status(params.custom_status),
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_prompt(
        &mut self,
        id: String,
        params: crate::api::schema::PaneReportPromptParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        if normalize_reported_agent_label(&params.agent).is_none() {
            return invalid_agent(id);
        }
        let prompt = sanitize_reported_prompt(&params.prompt);
        if prompt.is_empty() {
            return encode_success(id, ResponseResult::Ok {});
        }
        self.handle_internal_event(crate::events::AppEvent::HookPromptReported { pane_id, prompt });
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_agent_session(
        &mut self,
        id: String,
        params: PaneReportAgentSessionParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::AgentSessionReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_metadata(
        &mut self,
        id: String,
        params: PaneReportMetadataParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent_label = match params.agent.as_deref() {
            Some(agent) => match normalize_reported_agent_label(agent) {
                Some(agent_label) => Some(agent_label),
                None => return invalid_agent(id),
            },
            None => None,
        };
        let Some(source) = normalize_optional_text(Some(params.source)) else {
            return encode_error(id, "invalid_metadata_request", "missing metadata source");
        };
        let raw_title_set = params.title.is_some();
        let raw_display_agent_set = params.display_agent.is_some();
        let raw_custom_status_set = params.custom_status.is_some();
        let raw_state_labels_set = !params.state_labels.is_empty();
        let ttl = params.ttl_ms.map(std::time::Duration::from_millis);
        let title = normalize_presentation_text(params.title);
        let display_agent = normalize_presentation_text(params.display_agent);
        let custom_status = normalize_custom_status(params.custom_status);
        let applies_to_source = match params.applies_to_source {
            Some(applies_to_source) => {
                let Some(applies_to_source) = normalize_optional_text(Some(applies_to_source))
                else {
                    return encode_error(
                        id,
                        "invalid_metadata_request",
                        "missing metadata authority source",
                    );
                };
                Some(applies_to_source)
            }
            None => None,
        };
        let state_labels = match normalize_state_labels(params.state_labels) {
            Ok(labels) => labels,
            Err(status) => {
                return encode_error(
                    id,
                    "invalid_state_label",
                    format!("unknown state label: {status}"),
                );
            }
        };
        if raw_title_set && params.clear_title
            || raw_display_agent_set && params.clear_display_agent
            || raw_custom_status_set && params.clear_custom_status
            || raw_state_labels_set && params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "cannot set and clear the same metadata field",
            );
        }
        if title.is_none()
            && display_agent.is_none()
            && custom_status.is_none()
            && state_labels.is_empty()
            && !params.clear_title
            && !params.clear_display_agent
            && !params.clear_custom_status
            && !params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "missing metadata field to set or clear",
            );
        }
        self.handle_internal_event(crate::events::AppEvent::HookMetadataReported {
            pane_id,
            source,
            agent_label,
            applies_to_source,
            title,
            display_agent,
            custom_status,
            state_labels,
            clear_title: params.clear_title,
            clear_display_agent: params.clear_display_agent,
            clear_custom_status: params.clear_custom_status,
            clear_state_labels: params.clear_state_labels,
            seq: params.seq,
            ttl,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    /// Promote a session-specific field onto the calling pane's header.
    /// Validation (lengths, cap) is answered synchronously here; the actual
    /// mutation rides `update_terminal_state` via an internal event, the
    /// shared chokepoint both event loops consume — same path as
    /// `pane.report_prompt`.
    pub(super) fn handle_pane_set_header_field(
        &mut self,
        id: String,
        params: crate::api::schema::PaneSetHeaderFieldParams,
    ) -> String {
        let Some((ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let (key, value) = match crate::terminal::validate_header_field(&params.key, &params.value)
        {
            Ok(field) => field,
            Err(err) => return encode_error(id, "invalid_header_field", err.to_string()),
        };
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        if !terminal.has_header_field_capacity(&key, std::time::Instant::now()) {
            return encode_error(
                id,
                "too_many_header_fields",
                crate::terminal::HeaderFieldError::TooManyFields.to_string(),
            );
        }
        self.handle_internal_event(crate::events::AppEvent::PaneHeaderFieldSet {
            pane_id,
            key,
            value,
            ttl: params.ttl_secs.map(std::time::Duration::from_secs),
        });

        encode_success(id, ResponseResult::Ok {})
    }

    /// Clear a promoted header field on the calling pane. Idempotent.
    pub(super) fn handle_pane_clear_header_field(
        &mut self,
        id: String,
        params: crate::api::schema::PaneClearHeaderFieldParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let key = params.key.trim().to_string();
        if key.is_empty() {
            return encode_error(
                id,
                "invalid_header_field",
                crate::terminal::HeaderFieldError::EmptyKey.to_string(),
            );
        }
        self.handle_internal_event(crate::events::AppEvent::PaneHeaderFieldCleared {
            pane_id,
            key,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_clear_agent_authority(
        &mut self,
        id: String,
        params: PaneClearAgentAuthorityParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
            pane_id,
            source: params.source,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_release_agent(
        &mut self,
        id: String,
        params: PaneReleaseAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) =
            self.parse_pane_id_or_peer(&params.pane_id, self.current_api_peer_pid)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
            pane_id,
            source: params.source,
            known_agent: crate::detect::parse_agent_label(&agent_label),
            agent_label,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_text(
        &mut self,
        id: String,
        params: PaneSendTextParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "pane_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_input(
        &mut self,
        id: String,
        params: PaneSendInputParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = match encode_api_keys(runtime, &params.keys) {
            Ok(encoded_keys) => encoded_keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        if !params.text.is_empty() {
            let text_bytes = encode_api_text(runtime, &params.text);
            if let Err(err) = runtime.try_send_bytes(Bytes::from(text_bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }
        for bytes in encoded_keys {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_close(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        if self.state.close_pane_would_close_workspace(ws_idx, pane_id)
            && self.state.confirm_implicit_worktree_group_close(ws_idx)
        {
            return encode_error(
                id,
                "confirmation_required",
                "closing this pane would close a worktree group",
            );
        }
        let workspace_id = self.state.workspaces[ws_idx].id.clone();
        let terminal_id = self.state.terminal_id_for_pane(ws_idx, pane_id);
        let should_close_workspace = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return pane_not_found(id, &target.pane_id);
            };
            ws.close_pane(pane_id)
        };
        if should_close_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: target.pane_id.clone(),
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed { workspace_id },
            });
        } else {
            self.state.remove_unattached_terminal_ids(terminal_id);
            self.shutdown_detached_terminal_runtimes();
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: target.pane_id,
                    workspace_id,
                },
            });
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_keys(
        &mut self,
        id: String,
        params: PaneSendKeysParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = match encode_api_keys(runtime, &params.keys) {
            Ok(encoded_keys) => encoded_keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        for bytes in encoded_keys {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_presentation_text(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    let normalized: String = trimmed
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
}

fn normalize_state_labels(
    labels: std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    labels
        .into_iter()
        .map(|(status, label)| {
            let status = status.trim().to_ascii_lowercase();
            if !matches!(
                status.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(status);
            }
            Ok(normalize_presentation_text(Some(label)).map(|label| (status, label)))
        })
        .filter_map(Result::transpose)
        .collect()
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

fn invalid_agent(id: String) -> String {
    encode_error(id, "invalid_agent", "agent label must not be empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::schema::SuccessResponse, config::Config, workspace::Workspace};

    fn app_with_linked_worktree() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("issue")];
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    #[test]
    fn api_pane_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_pane_close(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }
    /// App with one workspace whose root pane has a live TerminalState.
    /// Returns the app, the terminal id, and the public pane id.
    fn app_with_terminal() -> (App, crate::terminal::TerminalId, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("main")];
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane state")
            .attached_terminal_id
            .clone();
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into()),
        );
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        (app, terminal_id, public_pane_id)
    }

    fn set_field_response(app: &mut App, pane_id: &str, key: &str, value: &str) -> String {
        app.handle_pane_set_header_field(
            "req".into(),
            crate::api::schema::PaneSetHeaderFieldParams {
                pane_id: pane_id.to_string(),
                key: key.to_string(),
                value: value.to_string(),
                ttl_secs: None,
            },
        )
    }

    #[test]
    fn set_and_clear_header_field_round_trip_through_update_terminal_state() {
        let (mut app, terminal_id, public_pane_id) = app_with_terminal();

        assert!(set_field_response(&mut app, &public_pane_id, "build", "73%").contains("\"ok\""));
        assert!(set_field_response(&mut app, &public_pane_id, "pg", "up").contains("\"ok\""));
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .active_header_fields(),
            vec![
                ("build".to_string(), "73%".to_string()),
                ("pg".to_string(), "up".to_string()),
            ]
        );

        let response = app.handle_pane_clear_header_field(
            "req-clear".into(),
            crate::api::schema::PaneClearHeaderFieldParams {
                pane_id: public_pane_id,
                key: "build".into(),
            },
        );
        assert!(response.contains("\"ok\""));
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .active_header_fields(),
            vec![("pg".to_string(), "up".to_string())]
        );
    }

    #[test]
    fn set_header_field_rejects_over_cap_requests() {
        let (mut app, terminal_id, public_pane_id) = app_with_terminal();

        // Fill the per-pane cap (6 fields).
        for i in 0..6 {
            assert!(
                set_field_response(&mut app, &public_pane_id, &format!("k{i}"), "v")
                    .contains("\"ok\"")
            );
        }
        let response = set_field_response(&mut app, &public_pane_id, "k6", "v");
        assert!(response.contains("too_many_header_fields"));
        // Updating an existing key is still allowed at the cap.
        assert!(set_field_response(&mut app, &public_pane_id, "k0", "v2").contains("\"ok\""));

        // Key/value length caps reject via RPC error.
        let response = set_field_response(&mut app, &public_pane_id, &"k".repeat(17), "v");
        assert!(response.contains("invalid_header_field"));
        let response = set_field_response(&mut app, &public_pane_id, "k", &"v".repeat(49));
        assert!(response.contains("invalid_header_field"));
        let response = set_field_response(&mut app, &public_pane_id, "  ", "v");
        assert!(response.contains("invalid_header_field"));

        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .unwrap()
                .active_header_fields()
                .len(),
            6
        );
    }

    #[test]
    fn set_header_field_with_ttl_arms_the_shared_expiry_deadline() {
        let (mut app, terminal_id, public_pane_id) = app_with_terminal();
        assert_eq!(app.agent_metadata_deadline, None);

        let response = app.handle_pane_set_header_field(
            "req".into(),
            crate::api::schema::PaneSetHeaderFieldParams {
                pane_id: public_pane_id,
                key: "build".into(),
                value: "73%".into(),
                ttl_secs: Some(30),
            },
        );
        assert!(response.contains("\"ok\""));

        // The TTL rides the same scheduled tick that expires agent metadata,
        // in both event loops.
        let deadline = app.agent_metadata_deadline.expect("deadline armed");
        let now = std::time::Instant::now();
        assert!(deadline > now + std::time::Duration::from_secs(25));
        assert!(deadline <= now + std::time::Duration::from_secs(30));

        // Firing the shared sweep past the deadline drops the chip.
        let updates = app
            .state
            .expire_agent_metadata_at(deadline, deadline + std::time::Duration::from_millis(1));
        assert!(updates.is_empty());
        assert!(app
            .state
            .terminals
            .get(&terminal_id)
            .unwrap()
            .active_header_fields()
            .is_empty());
        assert_eq!(app.state.next_agent_metadata_expiry(), None);
    }

    #[test]
    fn header_field_requests_for_unknown_panes_fail() {
        let (mut app, _terminal_id, _public_pane_id) = app_with_terminal();
        let response = set_field_response(&mut app, "w_99-1", "build", "73%");
        assert!(response.contains("pane_not_found"));
    }

    #[test]
    fn report_prompt_stores_sanitized_last_prompt() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("main")];
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .expect("pane state")
            .attached_terminal_id
            .clone();
        app.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into()),
        );
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_pane_report_prompt(
            "req-1".into(),
            crate::api::schema::PaneReportPromptParams {
                pane_id: public_pane_id,
                source: "herdr:claude".into(),
                agent: "claude".into(),
                prompt: "  fix the \u{1b}[31mparser\u{1b}[0m bug  ".into(),
                seq: Some(1),
            },
        );
        assert!(response.contains("\"ok\""));
        assert_eq!(
            app.state
                .terminals
                .get(&terminal_id)
                .and_then(|terminal| terminal.last_prompt.as_deref()),
            Some("fix the parser bug")
        );
    }
}
