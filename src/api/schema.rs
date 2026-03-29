use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Method {
    #[serde(rename = "ping")]
    Ping(PingParams),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreateParams),
    #[serde(rename = "workspace.list")]
    WorkspaceList(EmptyParams),
    #[serde(rename = "workspace.get")]
    WorkspaceGet(WorkspaceTarget),
    #[serde(rename = "workspace.focus")]
    WorkspaceFocus(WorkspaceTarget),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRenameParams),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceTarget),
    #[serde(rename = "pane.split")]
    PaneSplit(PaneSplitParams),
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),
    #[serde(rename = "pane.get")]
    PaneGet(PaneTarget),
    #[serde(rename = "pane.send_text")]
    PaneSendText(PaneSendTextParams),
    #[serde(rename = "pane.send_keys")]
    PaneSendKeys(PaneSendKeysParams),
    #[serde(rename = "pane.read")]
    PaneRead(PaneReadParams),
    #[serde(rename = "pane.close")]
    PaneClose(PaneTarget),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EventsSubscribeParams),
    #[serde(rename = "events.wait")]
    EventsWait(EventsWaitParams),
    #[serde(rename = "pane.wait_for_output")]
    PaneWaitForOutput(PaneWaitForOutputParams),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PingParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTarget {
    pub workspace_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTarget {
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRenameParams {
    pub workspace_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSplitParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub target_pane_id: String,
    pub direction: SplitDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PaneListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSendTextParams {
    pub pane_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSendKeysParams {
    pub pane_id: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneReadParams {
    pub pane_id: String,
    pub source: ReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default = "default_true")]
    pub strip_ansi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Visible,
    Recent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    pub events: Vec<EventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsWaitParams {
    pub match_event: EventMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneWaitForOutputParams {
    pub pane_id: String,
    pub source: ReadSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    pub r#match: OutputMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default = "default_true")]
    pub strip_ansi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputMatch {
    Substring { value: String },
    Regex { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventMatch {
    WorkspaceCreated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
    },
    WorkspaceClosed {
        workspace_id: String,
    },
    WorkspaceRenamed {
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    WorkspaceFocused {
        workspace_id: String,
    },
    PaneCreated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
    },
    PaneClosed {
        pane_id: String,
    },
    PaneFocused {
        pane_id: String,
    },
    PaneOutputChanged {
        pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_revision: Option<u64>,
    },
    PaneExited {
        pane_id: String,
    },
    PaneAgentDetected {
        pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    PaneAgentStateChanged {
        pane_id: String,
        state: PaneAgentState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WorkspaceCreated,
    WorkspaceClosed,
    WorkspaceRenamed,
    WorkspaceFocused,
    PaneCreated,
    PaneClosed,
    PaneFocused,
    PaneOutputChanged,
    PaneExited,
    PaneAgentDetected,
    PaneAgentStateChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub id: String,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong {
        version: String,
    },
    WorkspaceInfo {
        workspace: WorkspaceInfo,
    },
    WorkspaceList {
        workspaces: Vec<WorkspaceInfo>,
    },
    PaneInfo {
        pane: PaneInfo,
    },
    PaneList {
        panes: Vec<PaneInfo>,
    },
    PaneRead {
        read: PaneReadResult,
    },
    SubscriptionStarted {
        events: Vec<EventKind>,
    },
    WaitMatched {
        event: EventEnvelope,
    },
    OutputMatched {
        pane_id: String,
        revision: u64,
        matched_line: Option<String>,
        read: PaneReadResult,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub agent_state: PaneAgentState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub agent_state: PaneAgentState,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneReadResult {
    pub pane_id: String,
    pub workspace_id: String,
    pub source: ReadSource,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event: EventKind,
    pub data: EventData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventData {
    WorkspaceCreated {
        workspace: WorkspaceInfo,
    },
    WorkspaceClosed {
        workspace_id: String,
    },
    WorkspaceRenamed {
        workspace_id: String,
        label: String,
    },
    WorkspaceFocused {
        workspace_id: String,
    },
    PaneCreated {
        pane: PaneInfo,
    },
    PaneClosed {
        pane_id: String,
        workspace_id: String,
    },
    PaneFocused {
        pane_id: String,
        workspace_id: String,
    },
    PaneOutputChanged {
        pane_id: String,
        workspace_id: String,
        revision: u64,
    },
    PaneExited {
        pane_id: String,
        workspace_id: String,
    },
    PaneAgentDetected {
        pane_id: String,
        workspace_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    PaneAgentStateChanged {
        pane_id: String,
        workspace_id: String,
        state: PaneAgentState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAgentState {
    Idle,
    Busy,
    Waiting,
    Unknown,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_for_pane_read() {
        let request = Request {
            id: "req_1".into(),
            method: Method::PaneRead(PaneReadParams {
                pane_id: "p_1".into(),
                source: ReadSource::Recent,
                lines: Some(80),
                strip_ansi: true,
            }),
        };

        let json = serde_json::to_string(&request).unwrap();
        let restored: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn request_uses_dot_method_names() {
        let request = Request {
            id: "req_1".into(),
            method: Method::WorkspaceCreate(WorkspaceCreateParams {
                cwd: Some("/tmp".into()),
                focus: true,
            }),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "workspace.create");
    }

    #[test]
    fn unknown_method_is_rejected() {
        let json = r#"{"id":"req_1","method":"nope","params":{}}"#;
        let err = serde_json::from_str::<Request>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant"));
    }

    #[test]
    fn missing_required_params_are_rejected() {
        let json = r#"{"id":"req_1","method":"pane.send_text","params":{"pane_id":"p_1"}}"#;
        let err = serde_json::from_str::<Request>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("text"));
    }

    #[test]
    fn pane_wait_for_output_defaults_strip_ansi_to_true() {
        let json = r#"
        {
            "id": "req_1",
            "method": "pane.wait_for_output",
            "params": {
                "pane_id": "p_1",
                "source": "recent",
                "match": { "type": "substring", "value": "ready" }
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::PaneWaitForOutput(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert!(params.strip_ansi);
    }

    #[test]
    fn event_envelope_round_trips() {
        let event = EventEnvelope {
            event: EventKind::PaneOutputChanged,
            data: EventData::PaneOutputChanged {
                pane_id: "p_1".into(),
                workspace_id: "w_1".into(),
                revision: 42,
            },
        };

        let json = serde_json::to_string(&event).unwrap();
        let restored: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, event);
    }

    #[test]
    fn success_response_round_trips() {
        let response = SuccessResponse {
            id: "req_1".into(),
            result: ResponseResult::Pong {
                version: "0.1.2".into(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: SuccessResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn error_response_round_trips() {
        let response = ErrorResponse {
            id: "req_1".into(),
            error: ErrorBody {
                code: "pane_not_found".into(),
                message: "pane p_1 not found".into(),
            },
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn event_wait_parses_typed_match() {
        let json = r#"
        {
            "id": "req_9",
            "method": "events.wait",
            "params": {
                "match_event": {
                    "event": "pane_agent_state_changed",
                    "pane_id": "p_1",
                    "state": "waiting"
                },
                "timeout_ms": 30000
            }
        }
        "#;

        let request: Request = serde_json::from_str(json).unwrap();
        let Method::EventsWait(params) = request.method else {
            panic!("wrong method parsed");
        };
        assert_eq!(
            params.match_event,
            EventMatch::PaneAgentStateChanged {
                pane_id: "p_1".into(),
                state: PaneAgentState::Waiting,
            }
        );
    }
}
