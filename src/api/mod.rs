pub mod schema;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use regex::Regex;
use tokio::sync::mpsc;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneAgentStateChangedEvent, PaneOutputMatchedEvent, Request,
    ResponseResult, Subscription, SubscriptionEventData, SubscriptionEventEnvelope,
    SubscriptionEventKind, SuccessResponse,
};

pub const SOCKET_PATH_ENV_VAR: &str = "HERDR_SOCKET_PATH";

pub struct ApiRequestMessage {
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

#[derive(Clone, Default)]
pub struct EventHub {
    inner: std::sync::Arc<std::sync::Mutex<EventHubState>>,
}

#[derive(Default)]
struct EventHubState {
    next_sequence: u64,
    events: Vec<(u64, crate::api::schema::EventEnvelope)>,
}

impl EventHub {
    const MAX_EVENTS: usize = 512;

    pub fn push(&self, event: crate::api::schema::EventEnvelope) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        state.next_sequence += 1;
        let sequence = state.next_sequence;
        state.events.push((sequence, event));
        let overflow = state.events.len().saturating_sub(Self::MAX_EVENTS);
        if overflow > 0 {
            state.events.drain(0..overflow);
        }
    }

    pub fn events_after(&self, sequence: u64) -> Vec<(u64, crate::api::schema::EventEnvelope)> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        state
            .events
            .iter()
            .filter(|(event_sequence, _)| *event_sequence > sequence)
            .cloned()
            .collect()
    }
}

pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var(SOCKET_PATH_ENV_VAR) {
        return PathBuf::from(path);
    }

    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("herdr.sock");
    }

    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("herdr/herdr.sock");
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/herdr/herdr.sock");
    }

    PathBuf::from("/tmp/herdr.sock")
}

pub struct ServerHandle {
    _thread: std::thread::JoinHandle<()>,
    path: PathBuf,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), err = %err, "failed to remove api socket on shutdown");
            }
        }
    }
}

pub fn start_server(
    api_tx: ApiRequestSender,
    event_hub: EventHub,
) -> std::io::Result<ServerHandle> {
    let path = socket_path();
    prepare_socket_path(&path)?;

    let listener = UnixListener::bind(&path)?;
    info!(path = %path.display(), "api server listening");

    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let api_tx = api_tx.clone();
                    let event_hub = event_hub.clone();
                    std::thread::spawn(move || {
                        if let Err(err) = handle_connection(stream, &api_tx, &event_hub) {
                            warn!(err = %err, "api connection failed");
                        }
                    });
                }
                Err(err) => {
                    error!(err = %err, "api listener accept failed");
                    break;
                }
            }
        }
        debug!("api server thread exiting");
    });

    Ok(ServerHandle {
        _thread: thread,
        path,
    })
}

fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if let Err(err) = fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

fn handle_connection(
    mut stream: UnixStream,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
) -> std::io::Result<()> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&stream);
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
    }

    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(err) => {
            write_json_line(
                &mut stream,
                &ErrorResponse {
                    id: String::new(),
                    error: ErrorBody {
                        code: "invalid_request".into(),
                        message: format!("invalid request: {err}"),
                    },
                },
            )?;
            return Ok(());
        }
    };

    match request.method {
        Method::EventsSubscribe(params) => {
            stream_subscriptions(stream, request.id, params, api_tx, event_hub)
        }
        method => {
            let response = handle_request(
                Request {
                    id: request.id,
                    method,
                },
                api_tx,
            );
            stream.write_all(response.as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            Ok(())
        }
    }
}

fn handle_request(request: Request, api_tx: &ApiRequestSender) -> String {
    let request_id = request.id.clone();
    match request.method {
        Method::Ping(_) => serde_json::to_string(&SuccessResponse {
            id: request.id,
            result: ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").into(),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
                .to_string()
        }),
        Method::PaneWaitForOutput(params) => wait_for_output(request_id, params, api_tx),
        _ => dispatch_to_app(request, api_tx),
    }
}

fn wait_for_output(
    request_id: String,
    params: crate::api::schema::PaneWaitForOutputParams,
    api_tx: &ApiRequestSender,
) -> String {
    let deadline = params
        .timeout_ms
        .map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));

    let regex = match &params.r#match {
        crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
            Ok(regex) => Some(regex),
            Err(err) => {
                return serde_json::to_string(&ErrorResponse {
                    id: request_id,
                    error: ErrorBody {
                        code: "invalid_regex".into(),
                        message: err.to_string(),
                    },
                })
                .unwrap();
            }
        },
        crate::api::schema::OutputMatch::Substring { .. } => None,
    };

    loop {
        let read_request = Request {
            id: format!("{request_id}:read"),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: params.pane_id.clone(),
                source: params.source.clone(),
                lines: params.lines,
                strip_ansi: params.strip_ansi,
            }),
        };
        let response = dispatch_to_app(read_request, api_tx);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return response;
        };
        if value.get("error").is_some() {
            let mut value = value;
            value["id"] = serde_json::Value::String(request_id);
            return serde_json::to_string(&value).unwrap();
        }

        let read_value = value["result"]["read"].clone();
        let Ok(read) = serde_json::from_value::<crate::api::schema::PaneReadResult>(read_value)
        else {
            return serde_json::to_string(&ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "internal_error".into(),
                    message: "failed to decode pane read result".into(),
                },
            })
            .unwrap();
        };

        let matched_line = match_output(&read.text, &params.r#match, regex.as_ref());
        if matched_line.is_some() {
            let revision = read.revision;
            return serde_json::to_string(&SuccessResponse {
                id: request_id,
                result: ResponseResult::OutputMatched {
                    pane_id: params.pane_id,
                    revision,
                    matched_line,
                    read,
                },
            })
            .unwrap();
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return serde_json::to_string(&ErrorResponse {
                id: request_id,
                error: ErrorBody {
                    code: "timeout".into(),
                    message: "timed out waiting for output match".into(),
                },
            })
            .unwrap();
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn stream_subscriptions(
    mut stream: UnixStream,
    request_id: String,
    params: crate::api::schema::EventsSubscribeParams,
    api_tx: &ApiRequestSender,
    event_hub: &EventHub,
) -> std::io::Result<()> {
    let mut subscriptions = Vec::with_capacity(params.subscriptions.len());
    for (index, subscription) in params.subscriptions.into_iter().enumerate() {
        let active =
            match ActiveSubscription::new(subscription, &request_id, index, api_tx, event_hub) {
                Ok(active) => active,
                Err(response) => {
                    write_json_line(&mut stream, &response)?;
                    return Ok(());
                }
            };
        subscriptions.push(active);
    }

    write_json_line(
        &mut stream,
        &SuccessResponse {
            id: request_id,
            result: ResponseResult::SubscriptionStarted {},
        },
    )?;

    loop {
        for subscription in &mut subscriptions {
            if let Some(event) = subscription.poll(api_tx, event_hub) {
                write_json_line(&mut stream, &event)?;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_json_line<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> std::io::Result<()> {
    let encoded = serde_json::to_string(value)
        .map_err(|err| std::io::Error::other(format!("failed to encode json: {err}")))?;
    stream.write_all(encoded.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn match_output(
    text: &str,
    matcher: &crate::api::schema::OutputMatch,
    regex: Option<&Regex>,
) -> Option<String> {
    match matcher {
        crate::api::schema::OutputMatch::Substring { value } => text
            .lines()
            .find(|line| line.contains(value))
            .map(|line| line.to_string()),
        crate::api::schema::OutputMatch::Regex { .. } => regex.and_then(|re| {
            text.lines()
                .find(|line| re.is_match(line))
                .map(|line| line.to_string())
        }),
    }
}

struct ActiveOutputMatchedSubscription {
    pane_id: String,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    matcher: crate::api::schema::OutputMatch,
    regex: Option<Regex>,
    strip_ansi: bool,
    currently_matching: bool,
    request_prefix: String,
}

struct ActiveAgentStateChangedSubscription {
    pane_id: String,
    state_filter: Option<crate::api::schema::PaneAgentState>,
    last_state: Option<crate::api::schema::PaneAgentState>,
    request_prefix: String,
}

struct ActiveEventSubscription {
    event_kind: crate::api::schema::EventKind,
    last_sequence: u64,
}

enum ActiveSubscription {
    Event(ActiveEventSubscription),
    OutputMatched(ActiveOutputMatchedSubscription),
    AgentStateChanged(ActiveAgentStateChangedSubscription),
}

impl ActiveSubscription {
    fn new(
        subscription: Subscription,
        request_id: &str,
        index: usize,
        api_tx: &ApiRequestSender,
        _event_hub: &EventHub,
    ) -> Result<Self, ErrorResponse> {
        match subscription {
            Subscription::WorkspaceCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceCreated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceClosed,
                last_sequence: 0,
            })),
            Subscription::WorkspaceFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceFocused,
                last_sequence: 0,
            })),
            Subscription::PaneCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneCreated,
                last_sequence: 0,
            })),
            Subscription::PaneClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneClosed,
                last_sequence: 0,
            })),
            Subscription::PaneFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneFocused,
                last_sequence: 0,
            })),
            Subscription::PaneExited {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneExited,
                last_sequence: 0,
            })),
            Subscription::PaneAgentDetected {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::PaneAgentDetected,
                last_sequence: 0,
            })),
            Subscription::PaneOutputMatched {
                pane_id,
                source,
                lines,
                r#match,
                strip_ansi,
            } => {
                let regex = match &r#match {
                    crate::api::schema::OutputMatch::Regex { value } => match Regex::new(value) {
                        Ok(regex) => Some(regex),
                        Err(err) => {
                            return Err(ErrorResponse {
                                id: request_id.to_string(),
                                error: ErrorBody {
                                    code: "invalid_regex".into(),
                                    message: err.to_string(),
                                },
                            });
                        }
                    },
                    crate::api::schema::OutputMatch::Substring { .. } => None,
                };

                let probe = pane_read(
                    format!("{request_id}:sub:{index}:probe"),
                    &pane_id,
                    source.clone(),
                    lines,
                    strip_ansi,
                    api_tx,
                );
                if let Err(error) = probe {
                    return Err(error);
                }

                Ok(Self::OutputMatched(ActiveOutputMatchedSubscription {
                    pane_id,
                    source,
                    lines,
                    matcher: r#match,
                    regex,
                    strip_ansi,
                    currently_matching: false,
                    request_prefix: format!("{request_id}:sub:{index}"),
                }))
            }
            Subscription::PaneAgentStateChanged { pane_id, state } => {
                let probe =
                    match pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx) {
                        Ok(probe) => probe,
                        Err(error) => return Err(error),
                    };

                Ok(Self::AgentStateChanged(
                    ActiveAgentStateChangedSubscription {
                        pane_id,
                        state_filter: state,
                        last_state: Some(probe.agent_state),
                        request_prefix: format!("{request_id}:sub:{index}"),
                    },
                ))
            }
        }
    }

    fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<serde_json::Value> {
        match self {
            Self::Event(subscription) => subscription.poll(event_hub),
            Self::OutputMatched(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
            Self::AgentStateChanged(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
        }
    }
}

impl ActiveEventSubscription {
    fn poll(&mut self, event_hub: &EventHub) -> Option<serde_json::Value> {
        for (sequence, event) in event_hub.events_after(self.last_sequence) {
            self.last_sequence = sequence;
            if event.event == self.event_kind {
                return serde_json::to_value(event).ok();
            }
        }
        None
    }
}

impl ActiveOutputMatchedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let read = pane_read(
            format!("{}:read", self.request_prefix),
            &self.pane_id,
            self.source.clone(),
            self.lines,
            self.strip_ansi,
            api_tx,
        )
        .ok()?;

        let matched_line = match_output(&read.text, &self.matcher, self.regex.as_ref());
        match matched_line {
            Some(matched_line) => {
                if self.currently_matching {
                    return None;
                }
                self.currently_matching = true;
                Some(SubscriptionEventEnvelope {
                    event: SubscriptionEventKind::PaneOutputMatched,
                    data: SubscriptionEventData::PaneOutputMatched(PaneOutputMatchedEvent {
                        pane_id: self.pane_id.clone(),
                        matched_line,
                        read,
                    }),
                })
            }
            None => {
                self.currently_matching = false;
                None
            }
        }
    }
}

impl ActiveAgentStateChangedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        )
        .ok()?;
        let current_state = pane.agent_state;
        let previous_state = self.last_state.replace(current_state);
        if previous_state.is_none() || previous_state == Some(current_state) {
            return None;
        }
        if self
            .state_filter
            .is_some_and(|wanted| wanted != current_state)
        {
            return None;
        }

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStateChanged,
            data: SubscriptionEventData::PaneAgentStateChanged(PaneAgentStateChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                state: current_state,
                agent: pane.agent,
            }),
        })
    }
}

fn pane_read(
    request_id: String,
    pane_id: &str,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    strip_ansi: bool,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneReadResult, ErrorResponse> {
    let response = dispatch_to_app(
        Request {
            id: request_id.clone(),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: pane_id.to_string(),
                source,
                lines,
                strip_ansi,
            }),
        },
        api_tx,
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read response".into(),
        },
    })?;
    if value.get("error").is_some() {
        return serde_json::from_value(value).map_err(|_| ErrorResponse {
            id: request_id,
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode pane read error".into(),
            },
        });
    }
    serde_json::from_value(value["result"]["read"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane read result".into(),
        },
    })
}

fn pane_get(
    request_id: String,
    pane_id: &str,
    api_tx: &ApiRequestSender,
) -> Result<crate::api::schema::PaneInfo, ErrorResponse> {
    let response = dispatch_to_app(
        Request {
            id: request_id.clone(),
            method: Method::PaneGet(crate::api::schema::PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        },
        api_tx,
    );
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|_| ErrorResponse {
        id: request_id.clone(),
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get response".into(),
        },
    })?;
    if value.get("error").is_some() {
        return serde_json::from_value(value).map_err(|_| ErrorResponse {
            id: request_id,
            error: ErrorBody {
                code: "internal_error".into(),
                message: "failed to decode pane get error".into(),
            },
        });
    }
    serde_json::from_value(value["result"]["pane"].clone()).map_err(|_| ErrorResponse {
        id: request_id,
        error: ErrorBody {
            code: "internal_error".into(),
            message: "failed to decode pane get result".into(),
        },
    })
}

fn dispatch_to_app(request: Request, api_tx: &ApiRequestSender) -> String {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    if let Err(err) = api_tx.send(ApiRequestMessage {
        request,
        respond_to,
    }) {
        return serde_json::to_string(&ErrorResponse {
            id: String::new(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: format!("failed to dispatch request: {err}"),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#.to_string()
        });
    }

    response_rx.recv().unwrap_or_else(|err| {
        serde_json::to_string(&ErrorResponse {
            id: String::new(),
            error: ErrorBody {
                code: "server_unavailable".into(),
                message: format!("request handling failed: {err}"),
            },
        })
        .unwrap_or_else(|_| {
            r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#.to_string()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_prefers_explicit_env_override() {
        let unique = format!("/tmp/herdr-test-{}.sock", std::process::id());
        std::env::set_var(SOCKET_PATH_ENV_VAR, &unique);
        assert_eq!(socket_path(), PathBuf::from(&unique));
        std::env::remove_var(SOCKET_PATH_ENV_VAR);
    }

    #[test]
    fn ping_request_returns_pong() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let response = handle_request(
            Request {
                id: "req_1".into(),
                method: Method::Ping(crate::api::schema::PingParams::default()),
            },
            &tx,
        );

        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_1");
        assert!(matches!(parsed.result, ResponseResult::Pong { .. }));
    }

    #[test]
    fn request_dispatches_to_app_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let request = Request {
            id: "req_2".into(),
            method: Method::WorkspaceList(crate::api::schema::EmptyParams::default()),
        };

        let request_for_thread = request.clone();
        let thread = std::thread::spawn(move || handle_request(request_for_thread, &tx));

        let msg = rx.blocking_recv().unwrap();
        assert_eq!(msg.request.id, "req_2");
        msg.respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: "req_2".into(),
                    result: ResponseResult::Ok {},
                })
                .unwrap(),
            )
            .unwrap();

        let response = thread.join().unwrap();
        let parsed: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed.id, "req_2");
    }
}
