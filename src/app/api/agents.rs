use bytes::Bytes;

use crate::api::schema::{
    AgentRenameParams, AgentSendParams, AgentStartParams, AgentTarget, PaneReadResult, ReadFormat,
    ReadSource, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

impl App {
    pub(super) fn handle_agent_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::AgentList {
                agents: self.collect_agent_infos(),
            },
        )
    }

    pub(super) fn handle_agent_get(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.agent_info_for_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_focus(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.focus_agent_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_mark_read(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.mark_agent_read_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_seen_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_mark_unread(&mut self, id: String, target: AgentTarget) -> String {
        let agent = match self.mark_agent_unread_target(&target.target) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_seen_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_rename(&mut self, id: String, params: AgentRenameParams) -> String {
        let agent = match self.rename_agent_target(&params.target, params.name) {
            Ok(agent) => agent,
            Err(err) => return encode_error_body(id, self.agent_rename_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentInfo { agent })
    }

    pub(super) fn handle_agent_start(&mut self, id: String, params: AgentStartParams) -> String {
        let extra_env = match super::env::normalize_launch_env(params.env.clone()) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let (agent, argv) = match self.start_agent(params, extra_env) {
            Ok(started) => started,
            Err(err) => return encode_error_body(id, self.agent_start_error_body(err)),
        };

        encode_success(id, ResponseResult::AgentStarted { agent, argv })
    }

    pub(super) fn handle_agent_read(
        &mut self,
        id: String,
        params: crate::api::schema::AgentReadParams,
    ) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &params.target);
        };
        let requested_lines = params.lines.unwrap_or(80).min(1000) as usize;
        let text = match params.format {
            ReadFormat::Text => match params.source {
                ReadSource::Visible => pane.visible_text(),
                ReadSource::Recent => pane.recent_text(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_text(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
            ReadFormat::Ansi => match params.source {
                ReadSource::Visible => pane.visible_ansi(),
                ReadSource::Recent => pane.recent_ansi(requested_lines),
                ReadSource::RecentUnwrapped => pane.recent_unwrapped_ansi(requested_lines),
                ReadSource::Detection => pane.detection_text(),
            },
        };

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: self
                        .public_pane_id(resolved.ws_idx, resolved.pane_id)
                        .unwrap_or_else(|| params.target.clone()),
                    workspace_id,
                    tab_id: self
                        .public_tab_id(resolved.ws_idx, resolved.tab_idx)
                        .unwrap(),
                    source: params.source,
                    format: params.format,
                    text,
                    revision: 0,
                    truncated: false,
                },
            },
        )
    }

    pub(super) fn handle_agent_explain(&mut self, id: String, target: AgentTarget) -> String {
        let resolved = match self.resolve_terminal_target(&target.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some((pane, _workspace_id)) = self.lookup_runtime(resolved.ws_idx, resolved.pane_id)
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
        else {
            return agent_not_found(id, &target.target);
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return agent_not_found(id, &target.target);
        };
        if terminal.full_lifecycle_hook_authority_active() {
            let explain = serde_json::json!({
                "agent": terminal.effective_agent_label().unwrap_or("unknown"),
                "state": crate::detect::manifest::agent_state_label(terminal.state),
                "manifest_source": null,
                "manifest_version": null,
                "cached_remote_version": null,
                "local_override_shadowing_remote": false,
                "remote_update_status": null,
                "remote_update_error": null,
                "matched_rule": null,
                "visible_idle": false,
                "visible_blocker": false,
                "visible_working": false,
                "screen_detection_skipped": true,
                "screen_detection_skip_reason": "full_lifecycle_hook_authority",
                "skip_state_update": false,
                "skipped_update_reason": null,
                "fallback_reason": null,
                "warning": null,
                "evaluated_rules": [],
            });
            return encode_success(id, ResponseResult::AgentExplain { explain });
        }
        let Some(agent) = terminal.effective_known_agent().or(terminal.detected_agent) else {
            return encode_error(
                id,
                "agent_explain_unavailable",
                format!(
                    "agent target {} does not have a detected agent label",
                    target.target
                ),
            );
        };

        let screen = pane.detection_text();
        let osc_title = pane.agent_osc_title();
        let osc_progress = pane.agent_osc_progress();
        let explain = crate::detect::manifest::explain_with_input(
            agent,
            crate::detect::manifest::DetectionInput {
                screen: &screen,
                osc_title: &osc_title,
                osc_progress: &osc_progress,
            },
        );
        let value = crate::detect::manifest::explain_to_json_value(&explain);

        encode_success(id, ResponseResult::AgentExplain { explain: value })
    }

    pub(super) fn handle_agent_send(&mut self, id: String, params: AgentSendParams) -> String {
        let resolved = match self.resolve_terminal_target(&params.target) {
            Ok(resolved) => resolved,
            Err(err) => return encode_error_body(id, self.agent_target_error_body(err)),
        };
        let Some(runtime) = self.lookup_runtime_sender(resolved.ws_idx, resolved.pane_id) else {
            return agent_not_found(id, &params.target);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "agent_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn agent_not_found(id: String, target: &str) -> String {
    encode_error(
        id,
        "agent_not_found",
        format!("agent target {target} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{AgentStatus, ErrorResponse, ResponseResult, SuccessResponse},
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn app_with_named_agent(
        name: &str,
        seen: bool,
        state: AgentState,
    ) -> (App, String, crate::layout::PaneId) {
        let mut app = test_app();
        let mut active = Workspace::test_new("active");
        let active_root = active.tabs[0].root_pane;
        let _active_second = active.test_split(ratatui::layout::Direction::Horizontal);
        active.tabs[0].layout.focus_pane(active_root);
        let background = Workspace::test_new("background");
        app.state.workspaces = vec![active, background];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let bg_pane = app.state.workspaces[1].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[1]
            .pane_state(bg_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_agent_name(name.into());
            terminal.set_detected_state(Some(Agent::Pi), state);
        }
        app.state.workspaces[1]
            .panes
            .get_mut(&bg_pane)
            .unwrap()
            .seen = seen;

        (app, name.to_string(), bg_pane)
    }

    #[test]
    fn api_agent_mark_read_on_done_agent_returns_idle_without_navigation() {
        let (mut app, target, bg_pane) = app_with_named_agent("reviewer", false, AgentState::Idle);

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
        assert_eq!(app.state.active, Some(0));
        assert_ne!(
            app.state
                .current_pane_focus_target()
                .map(|target| target.pane_id),
            Some(bg_pane),
        );
        assert!(app.state.workspaces[1].panes[&bg_pane].seen);
    }

    #[test]
    fn api_agent_mark_read_on_working_agent_returns_agent_not_idle() {
        let (mut app, target, _) = app_with_named_agent("worker", false, AgentState::Working);

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.id, "req");
        assert_eq!(error.error.code, "agent_not_idle");
    }

    #[test]
    fn api_agent_mark_unread_on_idle_seen_agent_returns_done() {
        let (mut app, target, bg_pane) = app_with_named_agent("reviewer", true, AgentState::Idle);

        let response = app.handle_agent_mark_unread(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Done);
        assert!(!app.state.workspaces[1].panes[&bg_pane].seen);
    }

    #[test]
    fn api_agent_mark_read_on_currently_viewed_agent_is_noop() {
        let (mut app, target, pane_id) = app_with_named_agent("reviewer", false, AgentState::Idle);
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.workspaces[1].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Done);
        assert!(!app.state.workspaces[1].panes[&pane_id].seen);
    }

    #[test]
    fn api_agent_mark_unread_on_currently_viewed_agent_is_noop() {
        let (mut app, target, pane_id) = app_with_named_agent("reviewer", true, AgentState::Idle);
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.workspaces[1].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_mark_unread(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
        assert!(app.state.workspaces[1].panes[&pane_id].seen);
    }

    #[test]
    fn api_agent_mark_read_is_idempotent_when_already_seen() {
        let (mut app, target, bg_pane) = app_with_named_agent("reviewer", true, AgentState::Idle);

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
        assert!(app.state.workspaces[1].panes[&bg_pane].seen);
    }

    #[test]
    fn api_agent_mark_read_on_unknown_target_returns_agent_not_found() {
        let mut app = test_app();

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: "missing-agent".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.id, "req");
        assert_eq!(error.error.code, "agent_not_found");
    }

    #[test]
    fn api_agent_mark_read_on_blocked_agent_returns_agent_not_idle() {
        let (mut app, target, _) = app_with_named_agent("blocked", false, AgentState::Blocked);

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: target.clone(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.id, "req");
        assert_eq!(error.error.code, "agent_not_idle");
    }

    #[test]
    fn api_agent_mark_read_on_non_agent_pane_returns_agent_not_found_without_mutation() {
        let mut app = test_app();
        let background = Workspace::test_new("background");
        app.state.workspaces = vec![background];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Idle;
        app.state.workspaces[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;

        let response = app.handle_agent_mark_read(
            "req".into(),
            AgentTarget {
                target: terminal_id.to_string(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.id, "req");
        assert_eq!(error.error.code, "agent_not_found");
        assert!(!app.state.workspaces[0].panes[&pane_id].seen);
    }
}
