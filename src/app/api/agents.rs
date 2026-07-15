use bytes::Bytes;

use crate::api::schema::{
    AgentCheckedInputResult, AgentCheckedReadResult, AgentReadCheckedParams, AgentRenameParams,
    AgentSendInputCheckedParams, AgentSendParams, AgentStartParams, AgentTarget, PaneAgentState,
    PaneReadResult, ReadFormat, ReadSource, ResponseResult,
};
use crate::app::App;

use super::responses::{encode_error, encode_error_body, encode_success};

use super::super::api_helpers::{encode_api_text_for_mode, parse_api_keys};

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

    pub(super) fn handle_agent_read_checked(
        &mut self,
        id: String,
        params: AgentReadCheckedParams,
    ) -> String {
        let Some(target) = self.checked_terminal_target(&params.terminal_id) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };
        let Some(runtime) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            target.ws_idx,
            target.pane_id,
        ) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };
        let Some(snapshot) = runtime.checked_input_snapshot() else {
            return encode_error(
                id,
                "checked_input_unavailable",
                "terminal detection snapshot is unavailable",
            );
        };
        let Some(terminal) = self.state.terminals.get(&target.terminal_id) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };

        encode_success(
            id,
            ResponseResult::AgentCheckedRead {
                read: AgentCheckedReadResult {
                    terminal_id: terminal.id.to_string(),
                    workspace_id: target.workspace_id,
                    tab_id: target.tab_id,
                    pane_id: target.pane_id_public,
                    agent: terminal.effective_agent_label().map(str::to_string),
                    status: semantic_agent_status(terminal.state),
                    input_revision: snapshot.input_revision,
                    content_hash: snapshot.content_hash,
                    text: snapshot.text,
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

    pub(super) fn handle_agent_send_input_checked(
        &mut self,
        id: String,
        params: AgentSendInputCheckedParams,
    ) -> String {
        let Some(target) = self.checked_terminal_target(&params.terminal_id) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };
        if params.text.is_empty() && params.keys.is_empty() {
            return encode_error(
                id,
                "invalid_params",
                "checked input requires non-empty text or at least one key",
            );
        }
        let parsed_keys = match parse_api_keys(&params.keys) {
            Ok(keys) => keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        let Some(terminal) = self.state.terminals.get(&target.terminal_id) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };
        let actual_agent = terminal.effective_agent_label().map(str::to_string);
        let actual_status = semantic_agent_status(terminal.state);
        let Some(runtime) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            target.ws_idx,
            target.pane_id,
        ) else {
            return checked_input_unsupported_target(id, &params.terminal_id);
        };

        let outcome =
            runtime.with_checked_input(|snapshot, bracketed_paste, protocol, consume_revision| {
                if snapshot.input_revision != params.expected_input_revision {
                    return Err((
                        "stale_input_revision",
                        format!(
                            "expected input revision {}, got {}",
                            params.expected_input_revision, snapshot.input_revision
                        ),
                    ));
                }
                if actual_agent.as_deref() != Some(params.expected_agent.as_str()) {
                    return Err((
                        "stale_agent",
                        format!(
                            "expected agent {}, got {}",
                            params.expected_agent,
                            actual_agent.as_deref().unwrap_or("none")
                        ),
                    ));
                }
                if !params.allowed_statuses.contains(&actual_status) {
                    return Err((
                        "stale_status",
                        format!("agent status {actual_status:?} is not allowed")
                            .to_ascii_lowercase(),
                    ));
                }
                if params
                    .expected_content_hash
                    .as_ref()
                    .is_some_and(|expected| expected != &snapshot.content_hash)
                {
                    return Err((
                        "stale_content",
                        "detection content hash no longer matches".to_string(),
                    ));
                }

                let mut bytes = if params.text.is_empty() {
                    Vec::new()
                } else {
                    encode_api_text_for_mode(&params.text, bracketed_paste)
                };
                for key in &parsed_keys {
                    bytes.extend(runtime.encode_terminal_key_with_protocol(*key, protocol));
                }
                let consumed_revision = consume_revision();
                runtime
                    .try_send_bytes_untracked(Bytes::from(bytes))
                    .map_err(|err| ("checked_input_send_failed", err.to_string()))?;
                Ok(consumed_revision)
            });

        match outcome {
            Some(Ok(input_revision)) => encode_success(
                id,
                ResponseResult::AgentCheckedInput {
                    input: AgentCheckedInputResult {
                        terminal_id: params.terminal_id,
                        input_revision,
                    },
                },
            ),
            Some(Err((code, message))) => encode_error(id, code, message),
            None => encode_error(
                id,
                "checked_input_unavailable",
                "terminal input lock is unavailable",
            ),
        }
    }

    fn checked_terminal_target(&self, terminal_id: &str) -> Option<CheckedTerminalTarget> {
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if terminal.id.to_string() == terminal_id {
                        let tab_id_public = self.public_tab_id(ws_idx, tab_idx)?;
                        let pane_id_public = self.public_pane_id(ws_idx, *pane_id)?;
                        return Some(CheckedTerminalTarget {
                            ws_idx,
                            pane_id: *pane_id,
                            terminal_id: pane.attached_terminal_id.clone(),
                            workspace_id: self.public_workspace_id(ws_idx),
                            tab_id: tab_id_public,
                            pane_id_public,
                        });
                    }
                }
            }
        }
        None
    }
}

struct CheckedTerminalTarget {
    ws_idx: usize,
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    workspace_id: String,
    tab_id: String,
    pane_id_public: String,
}

fn semantic_agent_status(state: crate::detect::AgentState) -> PaneAgentState {
    match state {
        crate::detect::AgentState::Idle => PaneAgentState::Idle,
        crate::detect::AgentState::Working => PaneAgentState::Working,
        crate::detect::AgentState::Blocked => PaneAgentState::Blocked,
        crate::detect::AgentState::Unknown => PaneAgentState::Unknown,
    }
}

fn checked_input_unsupported_target(id: String, terminal_id: &str) -> String {
    encode_error(
        id,
        "unsupported_target",
        format!("live terminal {terminal_id} not found"),
    )
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
        api::schema::{AgentStatus, ErrorResponse, SuccessResponse},
        app::Mode,
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_agent() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app
    }

    fn app_with_checked_agent(
        channel_capacity: usize,
        screen: &[u8],
    ) -> (App, String, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let mut app = app_with_agent();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                1000,
                screen,
                channel_capacity,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "custom:checked-test".into(),
            agent_label: "codex".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: Some(1),
            session_ref: None,
        });
        (app, terminal_id.to_string(), rx)
    }

    fn checked_read(app: &mut App, terminal_id: &str) -> AgentCheckedReadResult {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "read".into(),
            method: crate::api::schema::Method::AgentReadChecked(AgentReadCheckedParams {
                terminal_id: terminal_id.into(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response)
            .unwrap_or_else(|err| panic!("invalid checked read response {response}: {err}"));
        let ResponseResult::AgentCheckedRead { read } = success.result else {
            panic!("expected checked read response");
        };
        read
    }

    fn checked_send_params(read: &AgentCheckedReadResult) -> AgentSendInputCheckedParams {
        AgentSendInputCheckedParams {
            terminal_id: read.terminal_id.clone(),
            expected_input_revision: read.input_revision,
            expected_agent: read.agent.clone().expect("agent label"),
            allowed_statuses: vec![read.status],
            expected_content_hash: Some(read.content_hash.clone()),
            text: "answer".into(),
            keys: vec!["enter".into()],
        }
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap()
            .error
            .code
    }

    fn checked_send_revision(response: &str) -> u64 {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::AgentCheckedInput { input } = success.result else {
            panic!("expected checked input response");
        };
        input.input_revision
    }

    fn sha256(value: &str) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(value.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn agent_focus_marks_already_focused_done_agent_seen() {
        let mut app = app_with_agent();
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let response = app.handle_agent_focus(
            "req".into(),
            AgentTarget {
                target: "pi".into(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::AgentInfo { agent } = success.result else {
            panic!("expected agent info response");
        };
        assert_eq!(agent.agent_status, AgentStatus::Idle);
    }

    #[tokio::test]
    async fn checked_read_returns_one_bound_detection_snapshot() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"blocked prompt");

        let read = checked_read(&mut app, &terminal_id);

        assert_eq!(read.terminal_id, terminal_id);
        assert_eq!(read.agent.as_deref(), Some("codex"));
        assert_eq!(read.status, PaneAgentState::Blocked);
        assert!(read.input_revision > 0);
        assert_eq!(read.content_hash, sha256(&read.text));
        assert!(read.text.contains("blocked prompt"));
    }

    #[tokio::test]
    async fn detection_content_change_advances_revision_without_status_change() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"first prompt");
        let first = checked_read(&mut app, &terminal_id);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\rsecond prompt");
        let second = checked_read(&mut app, &terminal_id);

        assert_eq!(first.status, second.status);
        assert!(second.input_revision > first.input_revision);
        assert_ne!(second.content_hash, first.content_hash);
    }

    #[tokio::test]
    async fn raw_output_without_normalized_detection_change_keeps_revision() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"prompt");
        let first = checked_read(&mut app, &terminal_id);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;

        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2026h");
        let second = checked_read(&mut app, &terminal_id);

        assert_eq!(second.text, first.text);
        assert_eq!(second.content_hash, first.content_hash);
        assert_eq!(second.input_revision, first.input_revision);
    }

    #[tokio::test]
    async fn semantic_status_transitions_each_advance_revision() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"prompt");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let blocked = checked_read(&mut app, &terminal_id);

        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "custom:checked-test".into(),
            agent_label: "codex".into(),
            state: AgentState::Working,
            message: None,
            custom_status: None,
            seq: Some(2),
            session_ref: None,
        });
        let working = checked_read(&mut app, &terminal_id);
        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "custom:checked-test".into(),
            agent_label: "codex".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: Some(3),
            session_ref: None,
        });
        let blocked_again = checked_read(&mut app, &terminal_id);

        assert_eq!(working.status, PaneAgentState::Working);
        assert_eq!(blocked_again.status, PaneAgentState::Blocked);
        assert!(working.input_revision > blocked.input_revision);
        assert!(blocked_again.input_revision > working.input_revision);
    }

    #[tokio::test]
    async fn semantic_agent_identity_change_advances_revision() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"prompt");
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let codex = checked_read(&mut app, &terminal_id);

        app.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            source: "custom:checked-test".into(),
            agent_label: "claude".into(),
            state: AgentState::Blocked,
            message: None,
            custom_status: None,
            seq: Some(2),
            session_ref: None,
        });
        let claude = checked_read(&mut app, &terminal_id);

        assert_eq!(claude.agent.as_deref(), Some("claude"));
        assert_eq!(claude.status, codex.status);
        assert!(claude.input_revision > codex.input_revision);
    }

    #[tokio::test]
    async fn checked_send_enqueues_text_and_keys_once() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(1, b"prompt");
        let read = checked_read(&mut app, &terminal_id);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "send".into(),
            method: crate::api::schema::Method::AgentSendInputChecked(checked_send_params(&read)),
        });

        assert_eq!(checked_send_revision(&response), read.input_revision + 1);
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"answer\r")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_send_consumes_revision_and_rejects_reuse() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(2, b"prompt");
        let read = checked_read(&mut app, &terminal_id);
        let params = checked_send_params(&read);

        let first = app.handle_agent_send_input_checked("first".into(), params.clone());
        let consumed_revision = checked_send_revision(&first);
        let reused = app.handle_agent_send_input_checked("reused".into(), params);

        assert_eq!(consumed_revision, read.input_revision + 1);
        assert_eq!(error_code(&reused), "stale_input_revision");
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"answer\r")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn two_checked_senders_cannot_use_the_same_revision() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(2, b"prompt");
        let read = checked_read(&mut app, &terminal_id);
        let client_a = checked_send_params(&read);
        let client_b = checked_send_params(&read);

        let first = app.handle_agent_send_input_checked("client-a".into(), client_a);
        let second = app.handle_agent_send_input_checked("client-b".into(), client_b);

        assert_eq!(checked_send_revision(&first), read.input_revision + 1);
        assert_eq!(error_code(&second), "stale_input_revision");
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"answer\r")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn legacy_input_invalidates_an_interleaved_checked_send() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(2, b"prompt");
        let read = checked_read(&mut app, &terminal_id);

        let legacy = app.handle_agent_send(
            "legacy".into(),
            AgentSendParams {
                target: terminal_id,
                text: "legacy".into(),
            },
        );
        let checked =
            app.handle_agent_send_input_checked("checked".into(), checked_send_params(&read));

        let success: SuccessResponse = serde_json::from_str(&legacy).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(error_code(&checked), "stale_input_revision");
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"legacy"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_enqueue_failure_still_consumes_revision() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(1, b"prompt");
        let fill = app.handle_agent_send(
            "fill".into(),
            AgentSendParams {
                target: terminal_id.clone(),
                text: "queued".into(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&fill).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        let read = checked_read(&mut app, &terminal_id);
        let params = checked_send_params(&read);

        let failed = app.handle_agent_send_input_checked("failed".into(), params.clone());
        let after_failure = checked_read(&mut app, &terminal_id);
        let reused = app.handle_agent_send_input_checked("reused".into(), params);

        assert_eq!(error_code(&failed), "checked_input_send_failed");
        assert_eq!(after_failure.input_revision, read.input_revision + 1);
        assert_eq!(error_code(&reused), "stale_input_revision");
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"queued"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_key_only_send_does_not_emit_empty_bracketed_paste() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(1, b"\x1b[?2004hprompt");
        let read = checked_read(&mut app, &terminal_id);
        let mut params = checked_send_params(&read);
        params.text.clear();

        let response = app.handle_agent_send_input_checked("key-only".into(), params);

        assert_eq!(checked_send_revision(&response), read.input_revision + 1);
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\r"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_send_rejects_completely_empty_input_without_consuming_revision() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(1, b"prompt");
        let read = checked_read(&mut app, &terminal_id);
        let mut params = checked_send_params(&read);
        params.text.clear();
        params.keys.clear();

        let response = app.handle_agent_send_input_checked("empty".into(), params);
        let after = checked_read(&mut app, &terminal_id);

        assert_eq!(error_code(&response), "invalid_params");
        assert_eq!(after.input_revision, read.input_revision);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_send_rejects_invalid_key_without_partial_enqueue() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(4, b"prompt");
        let read = checked_read(&mut app, &terminal_id);
        let mut params = checked_send_params(&read);
        params.keys = vec!["enter".into(), "not-a-key".into()];

        let response = app.handle_agent_send_input_checked("send".into(), params);

        assert_eq!(error_code(&response), "invalid_key");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_send_rejects_stale_revision_hash_agent_and_status() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(8, b"first");
        let first = checked_read(&mut app, &terminal_id);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\rsecond");
        let current = checked_read(&mut app, &terminal_id);

        let stale_revision =
            app.handle_agent_send_input_checked("revision".into(), checked_send_params(&first));
        assert_eq!(error_code(&stale_revision), "stale_input_revision");

        let mut stale_hash = checked_send_params(&current);
        stale_hash.expected_content_hash = Some(first.content_hash.clone());
        let stale_hash = app.handle_agent_send_input_checked("hash".into(), stale_hash);
        assert_eq!(error_code(&stale_hash), "stale_content");

        let mut wrong_agent = checked_send_params(&current);
        wrong_agent.expected_agent = "claude".into();
        let wrong_agent = app.handle_agent_send_input_checked("agent".into(), wrong_agent);
        assert_eq!(error_code(&wrong_agent), "stale_agent");

        let mut wrong_status = checked_send_params(&current);
        wrong_status.allowed_statuses = vec![PaneAgentState::Working];
        let wrong_status = app.handle_agent_send_input_checked("status".into(), wrong_status);
        assert_eq!(error_code(&wrong_status), "stale_status");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn checked_methods_require_stable_terminal_id() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(4, b"prompt");
        let pane_id = app
            .public_pane_id(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap();

        let read = app.handle_agent_read_checked(
            "read".into(),
            AgentReadCheckedParams {
                terminal_id: pane_id.clone(),
            },
        );
        assert_eq!(error_code(&read), "unsupported_target");

        let current = checked_read(&mut app, &terminal_id);
        let mut send = checked_send_params(&current);
        send.terminal_id = pane_id;
        let send = app.handle_agent_send_input_checked("send".into(), send);
        assert_eq!(error_code(&send), "unsupported_target");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_move_preserves_checked_terminal_identity_and_revision() {
        let (mut app, terminal_id, _rx) = app_with_checked_agent(4, b"prompt");
        let before = checked_read(&mut app, &terminal_id);

        let response = app.handle_pane_move(
            "move".into(),
            crate::api::schema::PaneMoveParams {
                pane_id: before.pane_id.clone(),
                destination: crate::api::schema::PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
        let after = checked_read(&mut app, &terminal_id);

        assert_eq!(after.terminal_id, before.terminal_id);
        assert_eq!(after.input_revision, before.input_revision);
        assert_eq!(after.pane_id, before.pane_id);
        assert_ne!(after.tab_id, before.tab_id);
    }

    #[tokio::test]
    async fn legacy_agent_read_and_send_remain_compatible() {
        let (mut app, terminal_id, mut rx) = app_with_checked_agent(2, b"legacy prompt");

        let read = app.handle_agent_read(
            "read".into(),
            crate::api::schema::AgentReadParams {
                target: terminal_id.clone(),
                source: ReadSource::Detection,
                lines: None,
                format: ReadFormat::Text,
                strip_ansi: true,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&read).unwrap();
        assert!(matches!(success.result, ResponseResult::PaneRead { .. }));

        let send = app.handle_agent_send(
            "send".into(),
            AgentSendParams {
                target: terminal_id,
                text: "legacy".into(),
            },
        );
        let success: SuccessResponse = serde_json::from_str(&send).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"legacy"));
    }
}
