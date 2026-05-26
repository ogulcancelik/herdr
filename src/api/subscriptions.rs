use regex::Regex;

use crate::api::schema::{
    ErrorBody, ErrorResponse, Method, PaneAgentStatusChangedEvent, PaneOutputMatchedEvent, Request,
    Subscription, SubscriptionEventData, SubscriptionEventEnvelope, SubscriptionEventKind,
};
use crate::api::server::{dispatch_to_app_with_timeout, APP_RESPONSE_TIMEOUT};
use crate::api::{ApiRequestSender, EventHub};

pub(super) fn output_match_read_source(
    source: &crate::api::schema::ReadSource,
) -> crate::api::schema::ReadSource {
    match source {
        crate::api::schema::ReadSource::Recent => crate::api::schema::ReadSource::RecentUnwrapped,
        other => *other,
    }
}

pub(super) fn match_output(
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

pub(super) struct ActiveOutputMatchedSubscription {
    pane_id: String,
    source: crate::api::schema::ReadSource,
    lines: Option<u32>,
    matcher: crate::api::schema::OutputMatch,
    regex: Option<Regex>,
    strip_ansi: bool,
    currently_matching: bool,
    request_prefix: String,
}

pub(super) struct ActiveAgentStatusChangedSubscription {
    pane_id: String,
    status_filter: Option<crate::api::schema::AgentStatus>,
    last_status: Option<crate::api::schema::AgentStatus>,
    emit_initial_match: bool,
    request_prefix: String,
}

pub(super) struct ActiveEventSubscription {
    event_kind: crate::api::schema::EventKind,
    last_sequence: u64,
}

pub(super) enum ActiveSubscription {
    Event(ActiveEventSubscription),
    OutputMatched(ActiveOutputMatchedSubscription),
    AgentStatusChanged(ActiveAgentStatusChangedSubscription),
}

impl ActiveSubscription {
    pub(super) fn new(
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
            Subscription::WorkspaceUpdated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceUpdated,
                last_sequence: 0,
            })),
            Subscription::WorkspaceRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::WorkspaceRenamed,
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
            Subscription::TabCreated {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabCreated,
                last_sequence: 0,
            })),
            Subscription::TabClosed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabClosed,
                last_sequence: 0,
            })),
            Subscription::TabFocused {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabFocused,
                last_sequence: 0,
            })),
            Subscription::TabRenamed {} => Ok(Self::Event(ActiveEventSubscription {
                event_kind: crate::api::schema::EventKind::TabRenamed,
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
                    source,
                    lines,
                    strip_ansi,
                    api_tx,
                );
                probe?;

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
            Subscription::PaneAgentStatusChanged {
                pane_id,
                agent_status,
            } => {
                let probe = pane_get(format!("{request_id}:sub:{index}:probe"), &pane_id, api_tx)?;
                let emit_initial_match =
                    agent_status.is_some_and(|wanted| wanted == probe.agent_status);

                Ok(Self::AgentStatusChanged(
                    ActiveAgentStatusChangedSubscription {
                        pane_id,
                        status_filter: agent_status,
                        last_status: Some(probe.agent_status),
                        emit_initial_match,
                        request_prefix: format!("{request_id}:sub:{index}"),
                    },
                ))
            }
        }
    }

    pub(super) fn poll(
        &mut self,
        api_tx: &ApiRequestSender,
        event_hub: &EventHub,
    ) -> Option<serde_json::Value> {
        match self {
            Self::Event(subscription) => subscription.poll(event_hub),
            Self::OutputMatched(subscription) => {
                serde_json::to_value(subscription.poll(api_tx)?).ok()
            }
            Self::AgentStatusChanged(subscription) => {
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
            output_match_read_source(&self.source),
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

impl ActiveAgentStatusChangedSubscription {
    fn poll(&mut self, api_tx: &ApiRequestSender) -> Option<SubscriptionEventEnvelope> {
        let pane = pane_get(
            format!("{}:pane", self.request_prefix),
            &self.pane_id,
            api_tx,
        )
        .ok()?;
        let current_status = pane.agent_status;
        let previous_status = self.last_status.replace(current_status);
        let should_emit = if self.emit_initial_match {
            self.emit_initial_match = false;
            self.status_filter
                .is_some_and(|wanted| wanted == current_status)
        } else {
            previous_status.is_some_and(|previous| previous != current_status)
                && !self
                    .status_filter
                    .is_some_and(|wanted| wanted != current_status)
        };
        if !should_emit {
            return None;
        }

        Some(SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStatusChanged,
            data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                agent_status: current_status,
                agent: pane.agent,
                custom_status: pane.custom_status,
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
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: pane_id.to_string(),
                source,
                lines,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi,
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
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
    let response = dispatch_to_app_with_timeout(
        Request {
            id: request_id.clone(),
            method: Method::PaneGet(crate::api::schema::PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
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
