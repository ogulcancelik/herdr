//! Maps remote panes to local identity. The local server allocates a real
//! local PaneId per adopted remote pane; this registry owns the bijection.
//!
//! Also owns the per-host API poll loop: it speaks line-delimited JSON over
//! a `LinkTransport`'s API channel and turns what it reads into `HostEvent`
//! values on an mpsc channel. It never touches `AppState` -- consuming
//! `HostEvent` to mutate app state and this registry is Task 7-9's job
//! (single mutation point, per "state separated from runtime").

use crate::api::client::ApiClientError;
use crate::api::schema::{
    AgentStatus, EventData, EventEnvelope, EventKind, EventsSubscribeParams, Method,
    PaneListParams, Request, ResponseResult, Subscription, SubscriptionEventData,
    SubscriptionEventEnvelope,
};
use crate::layout::PaneId;
use crate::protocol::{
    self, ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};
use crate::server::host_link::{HostLinkId, LinkState};
use crate::server::host_transport::{LinkTransport, ReadWriteStream};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RemotePaneKey {
    pub(crate) host: HostLinkId,
    pub(crate) remote_pane_id: String, // remote server's public pane id, e.g. "w1:p2"
}

#[derive(Debug, Default)]
pub(crate) struct RemotePaneRegistry {
    by_key: HashMap<RemotePaneKey, PaneId>,
    by_local: HashMap<PaneId, RemotePaneKey>,
}

// Not driven outside tests yet; the host-event loop (this file, below) and
// the server main-loop consumer (Task 7-9) will call these through the real
// adoption/teardown lifecycle.
#[allow(dead_code)]
impl RemotePaneRegistry {
    /// Adopt: returns existing local id if already adopted (idempotent).
    pub(crate) fn adopt(&mut self, key: RemotePaneKey, alloc: impl FnOnce() -> PaneId) -> PaneId {
        if let Some(existing) = self.by_key.get(&key) {
            return *existing;
        }
        let local = alloc();
        self.by_key.insert(key.clone(), local);
        self.by_local.insert(local, key);
        local
    }

    /// Drop every pane adopted from `host`, returning the local ids that were
    /// released so the caller can tear down their runtime state too.
    pub(crate) fn release_host(&mut self, host: &HostLinkId) -> Vec<PaneId> {
        let released: HashSet<PaneId> = self
            .by_key
            .iter()
            .filter(|(key, _)| &key.host == host)
            .map(|(_, local)| *local)
            .collect();
        self.by_key.retain(|key, _| &key.host != host);
        self.by_local.retain(|local, _| !released.contains(local));
        released.into_iter().collect()
    }

    pub(crate) fn key_for(&self, local: PaneId) -> Option<&RemotePaneKey> {
        self.by_local.get(&local)
    }

    /// Drop one adopted pane (e.g. on a remote `pane.closed`), returning its
    /// local id so the caller can tear down that pane's state. `None` if the
    /// key was never adopted or already released.
    pub(crate) fn release(&mut self, key: &RemotePaneKey) -> Option<PaneId> {
        let local = self.by_key.remove(key)?;
        self.by_local.remove(&local);
        Some(local)
    }

    /// Look up the local id for a remote pane WITHOUT adopting it; `adopt`
    /// is the only path that allocates.
    pub(crate) fn local_for(&self, key: &RemotePaneKey) -> Option<PaneId> {
        self.by_key.get(key).copied()
    }

    /// Every pane currently adopted from `host`, for reconciling a fresh
    /// `HostEvent::Snapshot` against the adopted set (retire panes missing
    /// from the snapshot, adopt panes new to it).
    pub(crate) fn panes_for_host<'a>(
        &'a self,
        host: &'a HostLinkId,
    ) -> impl Iterator<Item = (&'a RemotePaneKey, PaneId)> + 'a {
        self.by_key
            .iter()
            .filter(move |(key, _)| &key.host == host)
            .map(|(key, local)| (key, *local))
    }

    pub(crate) fn assert_bijection_for_test(&self) {
        assert_eq!(
            self.by_key.len(),
            self.by_local.len(),
            "registry maps are out of sync: {} by_key entries vs {} by_local entries",
            self.by_key.len(),
            self.by_local.len()
        );
        for (key, local) in &self.by_key {
            assert_eq!(
                self.by_local.get(local),
                Some(key),
                "by_local does not round-trip to the by_key entry for {local:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Host event poll loop
// ---------------------------------------------------------------------------

/// A remote pane as reported by the remote server's `pane.list`, trimmed to
/// what adoption needs: identity, the status to seed with, and the display
/// metadata the sidebar presents for a pane (label/agent/title and friends,
/// straight from `PaneInfo`).
// Consumed by the server host-event loop integration (Task 7-9).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemotePaneInfo {
    pub(crate) remote_pane_id: String,
    pub(crate) agent_status: AgentStatus,
    pub(crate) label: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) custom_status: Option<String>,
}

impl From<crate::api::schema::PaneInfo> for RemotePaneInfo {
    fn from(pane: crate::api::schema::PaneInfo) -> Self {
        Self {
            remote_pane_id: pane.pane_id,
            agent_status: pane.agent_status,
            label: pane.label,
            agent: pane.agent,
            title: pane.title,
            display_agent: pane.display_agent,
            custom_status: pane.custom_status,
        }
    }
}

/// Runs on its own thread per connected host. Speaks line-delimited JSON
/// over the transport's API channel: initial pane.list snapshot, then an
/// events.subscribe stream for pane.agent_status_changed / pane.created /
/// pane.moved / pane.closed. Sends parsed updates to the server event
/// channel as HostEvent values; never touches AppState directly.
///
/// The remote API server handles one request per connection, or converts
/// the connection into a long-lived subscription stream (see
/// `src/api/server.rs`'s `handle_connection` / `stream_subscriptions`). So
/// this loop's shape is: connection A = one-shot `pane.list` request/
/// response, then EOF; connection B = `events.subscribe` held open, lines
/// read forever. Each connection is one `transport.open_api()` call.
///
/// Remote pane creation surfaces as a refreshed `Snapshot`, not a variant
/// of its own: connection B's subscription list is fixed at setup, so a
/// newly created pane cannot gain a per-pane `pane.agent_status_changed`
/// subscription mid-connection. Instead, a `pane.created` -- or a
/// `pane.moved`, which changes the pane's public id when it crosses
/// workspaces -- carrying a pane id missing from the current snapshot makes
/// the loop bounce: cleanly close connection B, re-run the one-shot
/// `pane.list`, emit a fresh `Snapshot`, and re-open connection B with
/// per-pane status subscriptions covering the new pane set (the consumer's
/// snapshot reconciliation retires ids that vanished). That internal
/// refresh is not a link failure and emits no `LinkDown`.
// Consumed by the server host-event loop integration (Task 7-9).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostEvent {
    Snapshot {
        host: HostLinkId,
        panes: Vec<RemotePaneInfo>,
    },
    StatusChanged {
        host: HostLinkId,
        remote_pane_id: String,
        status: AgentStatus,
    },
    PaneClosed {
        host: HostLinkId,
        remote_pane_id: String,
    },
    LinkDown {
        host: HostLinkId,
    },
    /// Raw terminal-ANSI bytes received on the focused remote pane's
    /// terminal channel (see `RemotePaneAttach`, below). Not consumed
    /// anywhere yet -- `RemotePaneAttach`'s doc comment spells out the seam
    /// this defines for Task 9: routing these bytes through the same
    /// `PaneTerminal::process_pty_bytes` hand-off local PTY output goes
    /// through (`src/pane.rs`'s `on_read` closures), so detection/copy
    /// mode/scrollback behave identically for adopted remote panes. Never a
    /// link-health signal by itself: an attach ENDING (EOF, read error, or a
    /// deliberate `detach()`) never emits this or any other `HostEvent`.
    ///
    /// `width`/`height` are the remote's actual (post-`clamp_terminal_size`,
    /// see `src/server/client_transport.rs`) frame dimensions, which can
    /// differ from the `cols`/`rows` this side requested; Task 9 must size
    /// the local emulator to these, not to what it asked for. `seq`/`full`
    /// from the `TerminalFrame` are deliberately dropped:
    /// `process_pty_bytes` is a pure ANSI byte sink that reconstructs a full
    /// grid from the diff stream itself.
    TerminalBytes {
        host: HostLinkId,
        local_pane: PaneId,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    },
    /// The remote refused the attach: a `ServerShutdown` arrived on the
    /// terminal channel BEFORE any `Terminal` frame. `attach()` returns
    /// `Ok` the instant `AttachTerminal` is written (there is no ack in the
    /// wire protocol), so an unknown/closed/already-owned `terminal_id`
    /// surfaces only here, distinct from a mid-stream teardown (which is
    /// silent). Common in practice because a stale snapshot can name a pane
    /// that has since closed. Task 9 reacts (release the adopted pane, or
    /// re-fetch the snapshot); a mid-stream `ServerShutdown` never produces
    /// this.
    AttachFailed {
        host: HostLinkId,
        local_pane: PaneId,
        reason: Option<String>,
    },
}

/// Tracks the closer for whichever transport channel the loop currently has
/// open, so `HostEventLoopHandle::stop` can interrupt a blocked read from
/// another thread without racing the loop's own channel-to-channel handoff
/// (connection A's one-shot pane.list, then connection B's long-lived
/// events.subscribe). All transitions go through one mutex so "was stop
/// already requested" and "register/clear the active closer" never race.
enum ChannelSlot {
    Idle,
    Open(Box<dyn Fn() + Send + Sync>),
    Stopped,
}

struct StopHandle {
    slot: Mutex<ChannelSlot>,
}

impl StopHandle {
    fn new() -> Self {
        Self {
            slot: Mutex::new(ChannelSlot::Idle),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChannelSlot> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Registers `closer` as the currently-open channel's closer. Returns
    /// `false` (after immediately invoking `closer`) if a stop was already
    /// requested, so the caller knows not to proceed using that channel --
    /// this closes a channel opened just after `stop()` ran instead of
    /// leaking it.
    fn set_active(&self, closer: Box<dyn Fn() + Send + Sync>) -> bool {
        let mut slot = self.lock();
        if matches!(*slot, ChannelSlot::Stopped) {
            closer();
            return false;
        }
        *slot = ChannelSlot::Open(closer);
        true
    }

    /// Drops the current channel's closer once the loop is done with it, so
    /// closers never accumulate across the snapshot/subscribe handoff.
    fn clear_active(&self) {
        let mut slot = self.lock();
        if !matches!(*slot, ChannelSlot::Stopped) {
            *slot = ChannelSlot::Idle;
        }
    }

    fn is_stopped(&self) -> bool {
        matches!(*self.lock(), ChannelSlot::Stopped)
    }

    fn stop(&self) {
        let mut slot = self.lock();
        if let ChannelSlot::Open(close) = &*slot {
            close();
        }
        *slot = ChannelSlot::Stopped;
    }
}

/// Owns the spawned per-host thread. `stop()` is safe to call from any
/// thread and unblocks a read the loop thread is currently stuck in (see
/// `host_transport`'s `closer_unblocks_a_reader_stuck_in_read` test for the
/// underlying proof that closing a `LinkChannel` does this); `join()` waits
/// for the thread to actually exit.
// Consumed by the server host-event loop integration (Task 7-9).
#[allow(dead_code)]
pub(crate) struct HostEventLoopHandle {
    thread: std::thread::JoinHandle<()>,
    stop: Arc<StopHandle>,
}

#[allow(dead_code)]
impl HostEventLoopHandle {
    pub(crate) fn stop(&self) {
        self.stop.stop();
    }

    pub(crate) fn join(self) {
        let _ = self.thread.join();
    }
}

/// Spawns the per-host event loop on its own thread: fetches an initial
/// `pane.list` snapshot over one `open_api()` connection, then holds a
/// second `open_api()` connection open as an `events.subscribe` stream for
/// `pane.created`, `pane.moved`, and `pane.closed` plus one
/// `pane.agent_status_changed` subscription per pane known from the
/// snapshot. An unknown pane id on a created/moved event triggers the
/// internal refresh cycle (fresh `pane.list` + `Snapshot` + re-subscribe
/// over the new pane set) with no `LinkDown`. Emits `HostEvent`s to
/// `events_tx`; never touches AppState. Reconnect/backoff policy belongs to
/// the owner (via the host link registry), not this loop: any genuine
/// failure just emits `LinkDown` and the thread exits. EOF before the first
/// successful response is a connect failure; EOF after is a link drop --
/// either way, `LinkDown` and exit.
// Consumed by the server host-event loop integration (Task 7-9).
#[allow(dead_code)]
pub(crate) fn spawn_host_event_loop(
    host: HostLinkId,
    transport: Box<dyn LinkTransport>,
    events_tx: mpsc::Sender<HostEvent>,
) -> HostEventLoopHandle {
    let stop = Arc::new(StopHandle::new());
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        run_host_event_loop(&host, transport.as_ref(), &events_tx, &thread_stop);
    });
    HostEventLoopHandle { thread, stop }
}

/// How one pass over connection B ended, deciding the outer loop's next move.
enum SubscriptionEnd {
    /// The stream was established, then an unknown pane appeared: re-snapshot
    /// and re-subscribe. Not a link failure; no `LinkDown`.
    Refresh,
    /// The remote answered the subscribe with an error ack -- alive and
    /// talking, but the subscription list was rejected (e.g. a snapshot pane
    /// closed between `pane.list` and `events.subscribe`, so its per-pane
    /// status probe failed). Re-snapshot and retry; counted against
    /// `MAX_CONSECUTIVE_SETUP_REFRESHES` because unlike `Refresh` it never
    /// establishes a stream, so on a pathological remote it would otherwise
    /// bounce in a tight loop forever.
    SetupRefresh,
    /// `stop()` was requested or the `HostEvent` consumer dropped its
    /// receiver: exit without `LinkDown`.
    Stopped,
    /// Genuine EOF or protocol error on a healthy loop: caller emits
    /// `LinkDown` and exits.
    LinkFailed,
}

/// How many back-to-back error-acked `events.subscribe` attempts (each with
/// a fresh snapshot in between) are tolerated before the link is declared
/// down. A successfully established stream resets the count.
const MAX_CONSECUTIVE_SETUP_REFRESHES: u32 = 3;

fn run_host_event_loop(
    host: &HostLinkId,
    transport: &dyn LinkTransport,
    events_tx: &mpsc::Sender<HostEvent>,
    stop: &StopHandle,
) {
    // Unknown pane ids that already triggered a refresh, carried across
    // refresh cycles. The remote event hub replays its buffered events to
    // every fresh subscription (`ActiveEventSubscription` starts at
    // sequence 0), so each rebuilt connection B re-delivers old
    // pane.created / pane.moved events. Ones whose pane id is present in
    // the current snapshot are ignored outright; this set additionally caps
    // a created-then-already-closed pane (never in any snapshot) at one
    // refresh ever, instead of a replay-driven refresh storm. Remote public
    // pane ids are never reused within a server run (see the monotonic
    // counters in src/workspace.rs), so entries can't suppress a later
    // legitimate creation.
    let mut refreshed_for: HashSet<String> = HashSet::new();
    let mut consecutive_setup_refreshes = 0u32;
    loop {
        let panes = match fetch_snapshot(host, transport, stop) {
            Ok(panes) => panes,
            Err(()) => {
                if !stop.is_stopped() {
                    let _ = events_tx.send(HostEvent::LinkDown { host: host.clone() });
                }
                return;
            }
        };
        if events_tx
            .send(HostEvent::Snapshot {
                host: host.clone(),
                panes: panes.clone(),
            })
            .is_err()
        {
            // Receiver gone = consumer gone: exit (dropping the transport
            // channel state) instead of holding the link open for nobody.
            debug!(host = %host.0, "host event consumer dropped; stopping loop");
            return;
        }

        match run_subscription(transport, host, &panes, events_tx, stop, &mut refreshed_for) {
            SubscriptionEnd::Refresh => {
                consecutive_setup_refreshes = 0;
                continue;
            }
            SubscriptionEnd::SetupRefresh => {
                consecutive_setup_refreshes += 1;
                if consecutive_setup_refreshes >= MAX_CONSECUTIVE_SETUP_REFRESHES {
                    warn!(
                        host = %host.0,
                        attempts = consecutive_setup_refreshes,
                        "giving up after repeated events.subscribe error acks"
                    );
                    if !stop.is_stopped() {
                        let _ = events_tx.send(HostEvent::LinkDown { host: host.clone() });
                    }
                    return;
                }
                continue;
            }
            SubscriptionEnd::Stopped => return,
            SubscriptionEnd::LinkFailed => {
                if !stop.is_stopped() {
                    let _ = events_tx.send(HostEvent::LinkDown { host: host.clone() });
                }
                return;
            }
        }
    }
}

/// Connection A: one-shot `pane.list` request/response.
fn fetch_snapshot(
    host: &HostLinkId,
    transport: &dyn LinkTransport,
    stop: &StopHandle,
) -> Result<Vec<RemotePaneInfo>, ()> {
    if stop.is_stopped() {
        return Err(());
    }
    let channel = match transport.open_api() {
        Ok(channel) => channel,
        Err(err) => {
            warn!(host = %host.0, err = %err, "failed to open api channel for pane.list");
            return Err(());
        }
    };
    if !stop.set_active(channel.close) {
        return Err(());
    }

    let result = (|| -> Result<Vec<RemotePaneInfo>, ()> {
        let mut stream = channel.stream;
        if let Err(err) = write_request_line(
            &mut stream,
            &Request {
                id: "remote-pane:pane.list".to_string(),
                method: Method::PaneList(PaneListParams::default()),
            },
        ) {
            if !stop.is_stopped() {
                warn!(host = %host.0, err = %err, "failed to send pane.list request");
            }
            return Err(());
        }
        let mut reader = BufReader::new(stream);
        let value = match read_json_line(&mut reader) {
            Ok(Some(value)) => value,
            Ok(None) => {
                if !stop.is_stopped() {
                    warn!(host = %host.0, "remote closed channel before pane.list response");
                }
                return Err(());
            }
            Err(err) => {
                if !stop.is_stopped() {
                    warn!(host = %host.0, err = %err, "failed to read pane.list response");
                }
                return Err(());
            }
        };
        match crate::api::client::parse_response_value(value) {
            Ok(response) => match response.result {
                ResponseResult::PaneList { panes } => {
                    Ok(panes.into_iter().map(RemotePaneInfo::from).collect())
                }
                other => {
                    warn!(host = %host.0, result = ?other, "unexpected pane.list result shape");
                    Err(())
                }
            },
            Err(err) => {
                warn!(host = %host.0, err = %err, "pane.list request failed");
                Err(())
            }
        }
    })();
    stop.clear_active();
    result
}

/// What a single subscription-stream line asks of the read loop.
enum LineOutcome {
    /// Keep reading (event forwarded or ignored).
    Continue,
    /// An unknown pane appeared (created/moved): bounce the subscription
    /// connection.
    Refresh,
    /// The `HostEvent` receiver was dropped: the consumer is gone, exit.
    ConsumerGone,
}

/// How the `events.subscribe` setup round-trip ended.
enum SetupOutcome {
    Established(BufReader<Box<dyn ReadWriteStream>>),
    /// The remote parsed the request and answered with an error ack: it is
    /// alive, the subscription list was just not acceptable (typically a
    /// pane closed between `pane.list` and the subscribe's per-pane probe).
    ErrorAck,
    /// I/O or protocol failure.
    Failed,
}

/// Connection B: `events.subscribe` held open, read until EOF, stop, or an
/// unknown-pane refresh. Returning `Refresh`/`SetupRefresh` tears the
/// connection down the same way the normal A->B handoff does (clear the
/// closer slot, drop the reader/stream) so the outer loop can immediately
/// open connection A again.
fn run_subscription(
    transport: &dyn LinkTransport,
    host: &HostLinkId,
    panes: &[RemotePaneInfo],
    events_tx: &mpsc::Sender<HostEvent>,
    stop: &StopHandle,
    refreshed_for: &mut HashSet<String>,
) -> SubscriptionEnd {
    if stop.is_stopped() {
        return SubscriptionEnd::Stopped;
    }
    let channel = match transport.open_api() {
        Ok(channel) => channel,
        Err(err) => {
            warn!(host = %host.0, err = %err, "failed to open api channel for events.subscribe");
            return SubscriptionEnd::LinkFailed;
        }
    };
    if !stop.set_active(channel.close) {
        return SubscriptionEnd::Stopped;
    }

    let setup = subscribe_setup(host, channel.stream, panes, stop);
    let mut reader = match setup {
        SetupOutcome::Established(reader) => reader,
        SetupOutcome::ErrorAck => {
            stop.clear_active();
            return if stop.is_stopped() {
                SubscriptionEnd::Stopped
            } else {
                SubscriptionEnd::SetupRefresh
            };
        }
        SetupOutcome::Failed => {
            stop.clear_active();
            return if stop.is_stopped() {
                SubscriptionEnd::Stopped
            } else {
                SubscriptionEnd::LinkFailed
            };
        }
    };

    let known: HashSet<&str> = panes
        .iter()
        .map(|pane| pane.remote_pane_id.as_str())
        .collect();
    loop {
        match read_json_line(&mut reader) {
            Ok(Some(value)) => {
                match handle_event_line(host, value, events_tx, &known, refreshed_for) {
                    LineOutcome::Continue => {}
                    LineOutcome::Refresh => {
                        // Same teardown as the normal channel handoff:
                        // release the closer slot, then drop the reader
                        // (and with it the stream) on return.
                        stop.clear_active();
                        return SubscriptionEnd::Refresh;
                    }
                    LineOutcome::ConsumerGone => {
                        debug!(host = %host.0, "host event consumer dropped; stopping loop");
                        stop.clear_active();
                        return SubscriptionEnd::Stopped;
                    }
                }
            }
            Ok(None) => {
                if !stop.is_stopped() {
                    warn!(host = %host.0, "events.subscribe stream ended");
                }
                stop.clear_active();
                return if stop.is_stopped() {
                    SubscriptionEnd::Stopped
                } else {
                    SubscriptionEnd::LinkFailed
                };
            }
            Err(err) => {
                if !stop.is_stopped() {
                    warn!(host = %host.0, err = %err, "failed to read events.subscribe stream");
                }
                stop.clear_active();
                return if stop.is_stopped() {
                    SubscriptionEnd::Stopped
                } else {
                    SubscriptionEnd::LinkFailed
                };
            }
        }
    }
}

/// Sends the `events.subscribe` request and classifies the ack.
fn subscribe_setup(
    host: &HostLinkId,
    mut stream: Box<dyn ReadWriteStream>,
    panes: &[RemotePaneInfo],
    stop: &StopHandle,
) -> SetupOutcome {
    if let Err(err) = write_request_line(
        &mut stream,
        &Request {
            id: "remote-pane:events.subscribe".to_string(),
            method: Method::EventsSubscribe(EventsSubscribeParams {
                subscriptions: build_subscriptions(panes),
            }),
        },
    ) {
        if !stop.is_stopped() {
            warn!(host = %host.0, err = %err, "failed to send events.subscribe request");
        }
        return SetupOutcome::Failed;
    }
    let mut reader = BufReader::new(stream);
    let ack = match read_json_line(&mut reader) {
        Ok(Some(value)) => value,
        Ok(None) => {
            if !stop.is_stopped() {
                warn!(host = %host.0, "remote closed channel before events.subscribe ack");
            }
            return SetupOutcome::Failed;
        }
        Err(err) => {
            if !stop.is_stopped() {
                warn!(host = %host.0, err = %err, "failed to read events.subscribe ack");
            }
            return SetupOutcome::Failed;
        }
    };
    match crate::api::client::parse_response_value(ack) {
        Ok(response) if matches!(response.result, ResponseResult::SubscriptionStarted {}) => {
            SetupOutcome::Established(reader)
        }
        Ok(response) => {
            warn!(host = %host.0, result = ?response.result, "unexpected events.subscribe ack shape");
            SetupOutcome::Failed
        }
        Err(ApiClientError::ErrorResponse(response)) => {
            // The remote is alive and rejected the subscription -- usually a
            // pane that closed between pane.list and the subscribe's
            // per-pane probe. A fresh snapshot should clear it.
            debug!(
                host = %host.0,
                code = %response.error.code,
                message = %response.error.message,
                "events.subscribe rejected; refreshing snapshot"
            );
            SetupOutcome::ErrorAck
        }
        Err(err) => {
            warn!(host = %host.0, err = %err, "failed to parse events.subscribe ack");
            SetupOutcome::Failed
        }
    }
}

/// `pane.created`, `pane.moved`, and `pane.closed` are global whole-kind
/// subscriptions; `pane.agent_status_changed` is per-pane and the remote
/// API probes each `pane_id` synchronously while setting up the
/// subscription, so every pane from the snapshot needs its own entry here.
fn build_subscriptions(panes: &[RemotePaneInfo]) -> Vec<Subscription> {
    let mut subscriptions = vec![
        Subscription::PaneCreated {},
        Subscription::PaneMoved {},
        Subscription::PaneClosed {},
    ];
    subscriptions.extend(
        panes
            .iter()
            .map(|pane| Subscription::PaneAgentStatusChanged {
                pane_id: pane.remote_pane_id.clone(),
                agent_status: None,
            }),
    );
    subscriptions
}

/// Subscription-stream lines come in two wire shapes depending on which
/// subscription produced them: `SubscriptionEventEnvelope` (dotted event
/// names, e.g. "pane.agent_status_changed") for the per-pane subscriptions,
/// or the plain `EventEnvelope` (snake_case event names, e.g.
/// "pane_created" / "pane_moved" / "pane_closed") for whole-event-kind
/// subscriptions. Try both; ignore lines that decode as neither
/// (forward-compat with event kinds this loop didn't ask for).
///
/// A `pane_created` / `pane_moved` whose pane id is already in `known` (or
/// already triggered a refresh) is ignored: the remote event hub replays
/// buffered events to every fresh subscription, so most such events seen
/// here are echoes of panes the current snapshot already covers. A
/// `pane_moved` id is unknown exactly when the move crossed workspaces and
/// re-minted the public pane id -- the same divergence as a creation, healed
/// by the same bounce. `pane_closed` for an unknown id is dropped for the
/// same replay reason: the consumer never adopted that pane.
/// Forwards one event; a send failure means the receiver was dropped.
fn forward(events_tx: &mpsc::Sender<HostEvent>, event: HostEvent) -> LineOutcome {
    if events_tx.send(event).is_err() {
        LineOutcome::ConsumerGone
    } else {
        LineOutcome::Continue
    }
}

fn handle_event_line(
    host: &HostLinkId,
    value: serde_json::Value,
    events_tx: &mpsc::Sender<HostEvent>,
    known: &HashSet<&str>,
    refreshed_for: &mut HashSet<String>,
) -> LineOutcome {
    if let Ok(envelope) = serde_json::from_value::<SubscriptionEventEnvelope>(value.clone()) {
        if let SubscriptionEventData::PaneAgentStatusChanged(event) = envelope.data {
            return forward(
                events_tx,
                HostEvent::StatusChanged {
                    host: host.clone(),
                    remote_pane_id: event.pane_id,
                    status: event.agent_status,
                },
            );
        }
        return LineOutcome::Continue;
    }
    if let Ok(envelope) = serde_json::from_value::<EventEnvelope>(value) {
        match envelope.data {
            EventData::PaneClosed { pane_id, .. }
                if envelope.event == EventKind::PaneClosed && known.contains(pane_id.as_str()) =>
            {
                return forward(
                    events_tx,
                    HostEvent::PaneClosed {
                        host: host.clone(),
                        remote_pane_id: pane_id,
                    },
                );
            }
            EventData::PaneCreated { pane }
                if envelope.event == EventKind::PaneCreated
                    && !known.contains(pane.pane_id.as_str())
                    && refreshed_for.insert(pane.pane_id.clone()) =>
            {
                return LineOutcome::Refresh;
            }
            EventData::PaneMoved { pane, .. }
                if envelope.event == EventKind::PaneMoved
                    && !known.contains(pane.pane_id.as_str())
                    && refreshed_for.insert(pane.pane_id.clone()) =>
            {
                return LineOutcome::Refresh;
            }
            _ => {}
        }
    }
    LineOutcome::Continue
}

fn write_request_line(
    stream: &mut Box<dyn ReadWriteStream>,
    request: &Request,
) -> std::io::Result<()> {
    let encoded = serde_json::to_string(request)
        .map_err(|err| std::io::Error::other(format!("failed to encode request: {err}")))?;
    stream.write_all(encoded.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn read_json_line(
    reader: &mut BufReader<Box<dyn ReadWriteStream>>,
) -> std::io::Result<Option<serde_json::Value>> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        // Treated like EOF. Fine today -- the API server never writes blank
        // lines -- but if a keepalive/heartbeat blank line is ever added to
        // the stream, this must skip instead of ending the link.
        return Ok(None);
    }
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

// ---------------------------------------------------------------------------
// Terminal attach for the focused remote pane
// ---------------------------------------------------------------------------

/// Attaches to one remote pane's terminal channel as a wire-protocol
/// *client* of the remote herdr server -- the same role `src/client/mod.rs`
/// plays against this local server, mirrored exactly: `do_handshake`'s
/// Hello/Welcome exchange (`ClientMessage::Hello` with
/// `launch_mode: ClientLaunchMode::TerminalAttach`,
/// `requested_encoding: RenderEncoding::TerminalAnsi`), then
/// `ClientMessage::AttachTerminal`, then a blocking reader thread mirroring
/// `server_reader_thread` / `write_terminal_session_output` in
/// `src/client/mod.rs`.
///
/// Meant to be opened when a remote pane becomes the focused/visible pane
/// (memory budget: only the focused remote pane keeps a live terminal
/// channel open) and torn down via `detach()` when the pane is hidden or the
/// host link drops. Feeds `HostEvent::TerminalBytes` to the caller's channel
/// so a server integration can enter the bytes through the same
/// `PaneTerminal::process_pty_bytes` hand-off real PTY output uses
/// (`src/pane.rs`'s `on_read` closures), giving the adopted remote pane a
/// real local vt grid: detection, copy mode, and scrollback keep working
/// unchanged because they read that same grid.
///
/// Nothing drives this through a real focus-change hook yet, and nothing
/// consumes `HostEvent::TerminalBytes` yet -- both are Task 9's server
/// integration. This type defines and tests the protocol-client half of
/// that seam: handshake, streaming reads, input writes, and graceful
/// teardown.
/// Upper bound on the blocking attach handshake (Hello -> Welcome ->
/// AttachTerminal). Mirrors `src/client/mod.rs`'s remote handshake timeout
/// (`REMOTE_HANDSHAKE_READ_TIMEOUT`, 60s): the terminal channel runs over a
/// fresh `ssh -T` child whose cold-connect happens inside this window, and
/// that child's liveness is independent of the API channel, so a bridge that
/// starts but wedges on this `terminal_id` would otherwise hang `attach()`
/// forever and leak the ssh child. The watchdog force-closes the channel
/// after this deadline, unblocking the blocking read.
const ATTACH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

// Consumed by the server visibility-hook integration (Task 9).
#[allow(dead_code)]
pub(crate) struct RemotePaneAttach {
    write: Mutex<Box<dyn Write + Send>>,
    close: Arc<dyn Fn() + Send + Sync>,
    reader: Option<std::thread::JoinHandle<()>>,
}

#[allow(dead_code)]
impl RemotePaneAttach {
    /// Opens `transport.open_terminal()`, performs the client handshake,
    /// sends `AttachTerminal`, and spawns the reader thread. `cols`/`rows`
    /// are the local pane's current size, reported in `Hello` the same way a
    /// real terminal-attach client reports its outer terminal size.
    ///
    /// Callers must have already completed one request/response round trip
    /// on this host's API channel before calling this (see the
    /// double-spawn-avoidance note on `host_transport`'s module doc). In the
    /// intended integration a pane only becomes focusable/visible once its
    /// host link is `Connected`, and reaching `Connected` is meant to happen
    /// only after the host-event loop's first `pane.list` round trip -- but
    /// nothing wires `LinkState` transitions to that loop yet (Task 9), so
    /// this function does not itself verify the ordering; it only documents
    /// the invariant the eventual caller must preserve.
    pub(crate) fn attach(
        host: HostLinkId,
        local_pane: PaneId,
        terminal_id: String,
        cols: u16,
        rows: u16,
        transport: &dyn LinkTransport,
        events_tx: mpsc::Sender<HostEvent>,
    ) -> io::Result<Self> {
        Self::attach_with_timeout(
            ATTACH_HANDSHAKE_TIMEOUT,
            host,
            local_pane,
            terminal_id,
            cols,
            rows,
            transport,
            events_tx,
        )
    }

    /// `attach()` with an explicit handshake deadline so tests can trigger
    /// the watchdog quickly instead of waiting the production 60s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn attach_with_timeout(
        handshake_deadline: Duration,
        host: HostLinkId,
        local_pane: PaneId,
        terminal_id: String,
        cols: u16,
        rows: u16,
        transport: &dyn LinkTransport,
        events_tx: mpsc::Sender<HostEvent>,
    ) -> io::Result<Self> {
        let channel = transport.open_terminal()?;
        // Capture the closer BEFORE the blocking handshake so the watchdog
        // can force-close (shutdown(Both)) an unresponsive remote's channel
        // and unblock the read -- the exact mechanism host_transport's
        // closer_unblocks_a_reader_stuck_in_read test proves. Shared via Arc
        // so both the watchdog and the returned `self` hold the one closer.
        let close: Arc<dyn Fn() + Send + Sync> = Arc::from(channel.close);
        let mut stream = channel.stream;

        // Arm the watchdog: it force-closes the channel once the deadline
        // passes, unless disarmed first. `recv_timeout` returning `Err`
        // (timeout) fires it; a disarm send returns `Ok` and it exits
        // without touching the (now healthy) channel.
        let (disarm_tx, disarm_rx) = mpsc::channel::<()>();
        let watchdog_close = Arc::clone(&close);
        let watchdog = std::thread::spawn(move || {
            if disarm_rx.recv_timeout(handshake_deadline).is_err() {
                watchdog_close();
            }
        });

        // Run the whole blocking handshake, then disarm the watchdog on
        // every path (success or error) so a fast rejection doesn't leave it
        // sleeping for the full deadline.
        let handshake = Self::run_handshake(&mut stream, cols, rows, terminal_id);
        let _ = disarm_tx.send(());
        let _ = watchdog.join();
        handshake?;

        let (read_half, write_half) = stream.split();
        let reader_host = host.clone();
        let reader = std::thread::spawn(move || {
            run_attach_reader(read_half, reader_host, local_pane, events_tx);
        });

        Ok(Self {
            write: Mutex::new(write_half),
            close,
            reader: Some(reader),
        })
    }

    /// The blocking Hello/Welcome/AttachTerminal exchange, factored out so
    /// `attach_with_timeout` can wrap it between arming and disarming the
    /// watchdog. On watchdog fire the socket is closed underneath these
    /// reads/writes, surfacing as an `Err` here -- which is exactly the
    /// "handshake timed out" outcome the caller wants.
    fn run_handshake(
        stream: &mut Box<dyn ReadWriteStream>,
        cols: u16,
        rows: u16,
        terminal_id: String,
    ) -> io::Result<()> {
        let hello = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            cols,
            rows,
            cell_width_px: 0,
            cell_height_px: 0,
            requested_encoding: RenderEncoding::TerminalAnsi,
            keybindings: ClientKeybindings::Server,
            launch_mode: ClientLaunchMode::TerminalAttach,
        };
        protocol::write_message(stream, &hello)
            .map_err(|err| io::Error::other(format!("failed to send attach hello: {err}")))?;

        // The Welcome payload is a plain handshake ack, so bound it at
        // MAX_FRAME_SIZE like do_handshake does; the larger graphics cap
        // only matters for the streaming Terminal frames read afterwards.
        let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)
            .map_err(|err| io::Error::other(format!("failed to read attach welcome: {err}")))?;
        match welcome {
            ServerMessage::Welcome {
                error: Some(reason),
                ..
            } => {
                return Err(io::Error::other(format!(
                    "remote rejected terminal attach: {reason}"
                )));
            }
            ServerMessage::Welcome {
                encoding: RenderEncoding::TerminalAnsi,
                error: None,
                ..
            } => {}
            other => {
                return Err(io::Error::other(format!(
                    "unexpected reply to terminal attach handshake: {other:?}"
                )));
            }
        }

        let attach_msg = ClientMessage::AttachTerminal {
            terminal_id,
            takeover: false,
        };
        protocol::write_message(stream, &attach_msg)
            .map_err(|err| io::Error::other(format!("failed to send AttachTerminal: {err}")))
    }

    /// Sends raw input bytes to the remote pane. Performs no link-health
    /// check itself -- callers route through `route_remote_pane_input`
    /// first so a non-`Connected` link drops the input instead of reaching
    /// here.
    pub(crate) fn send_input(&self, data: Vec<u8>) -> io::Result<()> {
        let mut write = self
            .write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        protocol::write_message(&mut *write, &ClientMessage::Input { data })
            .map_err(|err| io::Error::other(err.to_string()))
    }

    /// Forwards a local pane resize to the remote as `ClientMessage::Resize`
    /// so the remote re-renders at the new size (the attach `Hello` only
    /// carried the size at attach time). Symmetric with `send_input`: same
    /// write-half mutex. Task 9 wires this to the local pane-resize events
    /// that already drive the local emulator's resize; nothing calls it yet.
    pub(crate) fn resize(
        &self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> io::Result<()> {
        let mut write = self
            .write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        protocol::write_message(
            &mut *write,
            &ClientMessage::Resize {
                cols,
                rows,
                cell_width_px,
                cell_height_px,
            },
        )
        .map_err(|err| io::Error::other(err.to_string()))
    }

    /// Graceful teardown: best-effort `ClientMessage::Detach`, then drop
    /// `self` (see the `Drop` impl for the half-close / force-close / reader
    /// join sequence that runs either way).
    pub(crate) fn detach(self) {
        let mut write = self
            .write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(err) = protocol::write_message(&mut *write, &ClientMessage::Detach) {
            debug!(err = %err, "failed to send Detach before closing remote pane attach");
        }
    }
}

impl Drop for RemotePaneAttach {
    /// Split -> drop write half -> (reader thread drains any remaining
    /// frames until EOF) -> force-close so a remote that never reacts can't
    /// wedge the reader thread -> join. Runs for every teardown path,
    /// whether `detach()` was called first (graceful: `Detach` already sent)
    /// or `self` was simply dropped (e.g. an error path before the caller
    /// ever got to call `detach()`).
    fn drop(&mut self) {
        {
            // Replacing (rather than moving out of) the write half is what
            // lets this type have both a consuming `detach()` and a `Drop`
            // impl: moving a field out of a type that implements `Drop` is
            // rejected by the compiler, but swapping the boxed value through
            // `&mut` is not, and still runs the old value's `Drop` (the
            // unix/ssh write-half impls that half-close the channel).
            let mut write = self
                .write
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *write = Box::new(io::sink());
        }
        // Force-close in case the remote never reacts to Detach/EOF -- this
        // is what actually unblocks the reader thread's blocking read
        // (proven for both transports by host_transport's
        // closer_unblocks_a_reader_stuck_in_read test).
        (self.close)();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Reader thread body: blocking-reads `ServerMessage`s from the remote and
/// turns `Terminal` frames into `HostEvent::TerminalBytes`. Mirrors
/// `write_terminal_session_output`'s match shape in `src/client/mod.rs`
/// (Terminal / ServerShutdown / ignore-the-rest / EOF), reading with
/// `MAX_GRAPHICS_FRAME_SIZE` exactly like that canonical reader does,
/// because the server INLINES Kitty graphics bytes into the terminal-ANSI
/// frame on this path (`insert_graphics_before_sync_end` in
/// `src/server/render_stream.rs`), so a single frame carrying an image can
/// exceed `MAX_FRAME_SIZE` (2 MiB) up to `MAX_GRAPHICS_FRAME_SIZE` (32 MiB);
/// reading with the smaller cap would `Oversized`-error the reader dead and
/// silently freeze the pane.
///
/// Two `HostEvent`-emitting cases, both deliberate:
/// - `Terminal` frame -> `TerminalBytes` (with the frame's actual dims).
/// - `ServerShutdown` BEFORE any `Terminal` frame -> `AttachFailed`: the
///   attach was refused (`attach()` returns `Ok` at write time, so this is
///   the only place an unknown/closed/already-owned `terminal_id` surfaces).
///
/// Everything else ends the reader silently, emitting nothing: a
/// `ServerShutdown` AFTER the first frame is an ordinary mid-stream
/// teardown, and EOF / read errors are never themselves link-health signals
/// -- the host-event poll loop's `Snapshot`/`LinkDown` pair already owns
/// that judgment from the API channel, and a hidden pane's ordinary
/// `detach()` must not be misclassified as a link failure.
fn run_attach_reader(
    mut read_half: Box<dyn Read + Send>,
    host: HostLinkId,
    local_pane: PaneId,
    events_tx: mpsc::Sender<HostEvent>,
) {
    let mut seen_first_frame = false;
    loop {
        match protocol::read_message::<_, ServerMessage>(&mut read_half, MAX_GRAPHICS_FRAME_SIZE) {
            Ok(ServerMessage::Terminal(frame)) => {
                seen_first_frame = true;
                if events_tx
                    .send(HostEvent::TerminalBytes {
                        host: host.clone(),
                        local_pane,
                        width: frame.width,
                        height: frame.height,
                        bytes: frame.bytes,
                    })
                    .is_err()
                {
                    debug!(
                        host = %host.0,
                        pane = local_pane.raw(),
                        "terminal bytes consumer dropped; ending remote pane attach reader"
                    );
                    return;
                }
            }
            Ok(ServerMessage::ServerShutdown { reason }) => {
                if seen_first_frame {
                    debug!(
                        host = %host.0,
                        pane = local_pane.raw(),
                        ?reason,
                        "remote server shut down mid-stream; ending remote pane attach"
                    );
                } else {
                    debug!(
                        host = %host.0,
                        pane = local_pane.raw(),
                        ?reason,
                        "remote refused terminal attach"
                    );
                    // Ignore a send failure: a dropped consumer means Task
                    // 9's integration is already gone, and there is no
                    // reader work left to do either way.
                    let _ = events_tx.send(HostEvent::AttachFailed {
                        host: host.clone(),
                        local_pane,
                        reason,
                    });
                }
                return;
            }
            Ok(_) => {
                // Other ServerMessage variants (Frame/Graphics/Notify/...)
                // don't apply to this headless grid mirror; ignore, same as
                // write_terminal_session_output's `Ok(_) => {}` arm.
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                debug!(
                    host = %host.0,
                    pane = local_pane.raw(),
                    "remote pane attach channel closed"
                );
                return;
            }
            Err(err) => {
                warn!(
                    host = %host.0,
                    pane = local_pane.raw(),
                    err = %err,
                    "remote pane attach read failed"
                );
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Input-drop guard for a non-Connected host link
// ---------------------------------------------------------------------------

/// Result of routing one input payload toward a remote pane's attach.
// Consumed by the server input-routing integration (Task 9).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputRouteOutcome {
    /// The link was `Connected`; the caller should forward the input (e.g.
    /// via `RemotePaneAttach::send_input`).
    Forward,
    /// The link was not `Connected`; the caller must drop the input
    /// outright instead of queuing it -- retry belongs to the host link's
    /// own reconnect lifecycle (`host_link.rs`), not to buffered keystrokes
    /// for one pane -- and surface `notice` through the existing pane
    /// notification mechanism (`send_notify_to_foreground_client` in
    /// `src/server/headless.rs`).
    Drop { notice: String },
}

/// Pure decision, independent of any live `RemotePaneAttach`: should input
/// directed at a pane adopted from `host` reach the remote right now? Kept
/// standalone so it is testable purely off a `LinkState` value, the way
/// `host_link.rs` tests `HostLinkRegistry`'s state machine -- no transport,
/// no queue, nothing a dropped payload could accumulate in.
// Consumed by the server input-routing integration (Task 9).
#[allow(dead_code)]
pub(crate) fn route_remote_pane_input(host: &HostLinkId, state: LinkState) -> InputRouteOutcome {
    if matches!(state, LinkState::Connected) {
        InputRouteOutcome::Forward
    } else {
        InputRouteOutcome::Drop {
            notice: format!("input dropped: {} is not connected", host.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: &str, remote_pane_id: &str) -> RemotePaneKey {
        RemotePaneKey {
            host: HostLinkId(host.to_string()),
            remote_pane_id: remote_pane_id.to_string(),
        }
    }

    /// A fresh FnOnce allocator each call, sharing a counter across calls so
    /// tests can assert exactly how many times `alloc` actually ran.
    fn alloc_with(counter: &std::cell::Cell<u32>) -> impl FnOnce() -> PaneId + '_ {
        move || {
            counter.set(counter.get() + 1);
            PaneId::from_raw(counter.get())
        }
    }

    #[test]
    fn adoption_is_idempotent_per_key() {
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);
        let k = key("workbox", "w1:p1");

        let first = registry.adopt(k.clone(), alloc_with(&counter));
        let second = registry.adopt(k.clone(), alloc_with(&counter));

        assert_eq!(
            first, second,
            "repeated adopt of the same key must return the same local id"
        );
        assert_eq!(
            counter.get(),
            1,
            "alloc must run exactly once for a repeated key"
        );
        assert_eq!(registry.key_for(first), Some(&k));
        registry.assert_bijection_for_test();
    }

    #[test]
    fn same_remote_id_on_two_hosts_gets_distinct_local_ids() {
        // THE identity requirement: "w1:p1" on workbox and on buildfarm never collide.
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);

        let workbox = registry.adopt(key("workbox", "w1:p1"), alloc_with(&counter));
        let buildfarm = registry.adopt(key("buildfarm", "w1:p1"), alloc_with(&counter));

        assert_ne!(workbox, buildfarm);
        assert_eq!(registry.key_for(workbox), Some(&key("workbox", "w1:p1")));
        assert_eq!(
            registry.key_for(buildfarm),
            Some(&key("buildfarm", "w1:p1"))
        );
        registry.assert_bijection_for_test();
    }

    #[test]
    fn release_host_returns_exactly_that_hosts_panes() {
        // two hosts, release one
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);

        let a1 = registry.adopt(key("workbox", "w1:p1"), alloc_with(&counter));
        let a2 = registry.adopt(key("workbox", "w1:p2"), alloc_with(&counter));
        let b1 = registry.adopt(key("buildfarm", "w1:p1"), alloc_with(&counter));

        let mut released = registry.release_host(&HostLinkId("workbox".to_string()));
        released.sort_by_key(|id| id.raw());
        let mut expected = vec![a1, a2];
        expected.sort_by_key(|id| id.raw());
        assert_eq!(released, expected);

        assert!(registry.key_for(a1).is_none());
        assert!(registry.key_for(a2).is_none());
        assert_eq!(registry.key_for(b1), Some(&key("buildfarm", "w1:p1")));
        registry.assert_bijection_for_test();

        // Releasing again (or an unknown host) is a no-op, not an error.
        assert!(registry
            .release_host(&HostLinkId("workbox".to_string()))
            .is_empty());
    }

    #[test]
    fn release_removes_one_pane_and_preserves_bijection() {
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);

        let a1 = registry.adopt(key("workbox", "w1:p1"), alloc_with(&counter));
        let a2 = registry.adopt(key("workbox", "w1:p2"), alloc_with(&counter));

        assert_eq!(registry.release(&key("workbox", "w1:p1")), Some(a1));
        assert!(registry.key_for(a1).is_none());
        assert_eq!(registry.local_for(&key("workbox", "w1:p1")), None);
        assert_eq!(registry.key_for(a2), Some(&key("workbox", "w1:p2")));
        registry.assert_bijection_for_test();

        // Releasing an already-released or unknown key is None, not a panic.
        assert_eq!(registry.release(&key("workbox", "w1:p1")), None);
        assert_eq!(registry.release(&key("ghost", "w9:p9")), None);
        registry.assert_bijection_for_test();
    }

    #[test]
    fn local_for_looks_up_without_adopting() {
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);

        assert_eq!(registry.local_for(&key("workbox", "w1:p1")), None);
        assert_eq!(
            counter.get(),
            0,
            "lookup of an unknown key must not allocate"
        );
        let local = registry.adopt(key("workbox", "w1:p1"), alloc_with(&counter));
        assert_eq!(registry.local_for(&key("workbox", "w1:p1")), Some(local));
        assert_eq!(counter.get(), 1, "lookup never allocates");
        registry.assert_bijection_for_test();
    }

    #[test]
    fn panes_for_host_lists_only_that_hosts_panes() {
        let mut registry = RemotePaneRegistry::default();
        let counter = std::cell::Cell::new(0u32);

        let a1 = registry.adopt(key("workbox", "w1:p1"), alloc_with(&counter));
        let a2 = registry.adopt(key("workbox", "w1:p2"), alloc_with(&counter));
        let _b1 = registry.adopt(key("buildfarm", "w1:p1"), alloc_with(&counter));

        let mut listed: Vec<(RemotePaneKey, PaneId)> = registry
            .panes_for_host(&HostLinkId("workbox".to_string()))
            .map(|(k, local)| (k.clone(), local))
            .collect();
        listed.sort_by_key(|(_, local)| local.raw());
        assert_eq!(
            listed,
            vec![(key("workbox", "w1:p1"), a1), (key("workbox", "w1:p2"), a2)]
        );
        assert_eq!(
            registry
                .panes_for_host(&HostLinkId("ghost".to_string()))
                .count(),
            0
        );
    }

    // -----------------------------------------------------------------
    // Host event poll loop
    // -----------------------------------------------------------------

    use crate::api::schema::{
        ErrorBody, ErrorResponse, PaneAgentStatusChangedEvent, PaneInfo, SubscriptionEventKind,
        SuccessResponse,
    };
    use crate::server::host_transport::UnixSocketTransport;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// Expected-value twin of `canned_pane_info` after `RemotePaneInfo::from`.
    fn remote_pane(id: &str, status: AgentStatus) -> RemotePaneInfo {
        RemotePaneInfo {
            remote_pane_id: id.to_string(),
            agent_status: status,
            label: None,
            agent: None,
            title: None,
            display_agent: None,
            custom_status: None,
        }
    }

    // herdr has no tempfile dev-dependency; matches the pattern in
    // src/server/host_transport.rs's own tests.
    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn canned_pane_info(pane_id: &str, status: AgentStatus) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.to_string(),
            terminal_id: format!("term_{pane_id}"),
            workspace_id: "ws1".to_string(),
            tab_id: "tab1".to_string(),
            focused: false,
            cwd: None,
            foreground_cwd: None,
            label: None,
            agent: None,
            title: None,
            display_agent: None,
            agent_status: status,
            custom_status: None,
            state_labels: HashMap::new(),
            agent_session: None,
            revision: 0,
        }
    }

    fn read_request_line(conn: &mut UnixStream) -> Request {
        let mut line = String::new();
        BufReader::new(&mut *conn).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn write_line<T: serde::Serialize>(conn: &mut UnixStream, value: &T) {
        let encoded = serde_json::to_string(value).unwrap();
        conn.write_all(encoded.as_bytes()).unwrap();
        conn.write_all(b"\n").unwrap();
        conn.flush().unwrap();
    }

    /// Feeds the loop a fake API channel: a real `UnixListener` scripted to
    /// play a canned `pane.list` response on the first connection, then a
    /// `SubscriptionStarted` ack plus one `PaneAgentStatusChanged` event
    /// line on the second, built by serializing the real API structs so the
    /// test can't drift from the wire format. Asserts the emitted
    /// `HostEvent` sequence: Snapshot, StatusChanged, then LinkDown when the
    /// fake remote closes connection B.
    #[test]
    fn poll_loop_emits_snapshot_status_change_then_link_down() {
        let dir = unique_temp_dir("remote-pane-poll-loop");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Connection A: pane.list snapshot.
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            assert!(matches!(request.method, Method::PaneList(_)));
            let response = SuccessResponse {
                id: request.id,
                result: ResponseResult::PaneList {
                    panes: vec![
                        canned_pane_info("w1:p1", AgentStatus::Idle),
                        canned_pane_info("w1:p2", AgentStatus::Working),
                    ],
                },
            };
            write_line(&mut conn, &response);
            conn.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);

            // Connection B: events.subscribe, held open.
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            let Method::EventsSubscribe(params) = request.method else {
                panic!(
                    "expected events.subscribe request, got {:?}",
                    request.method
                );
            };
            assert!(params.subscriptions.contains(&Subscription::PaneClosed {}));
            assert!(params
                .subscriptions
                .contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w1:p1".to_string(),
                    agent_status: None,
                }));
            assert!(params
                .subscriptions
                .contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w1:p2".to_string(),
                    agent_status: None,
                }));

            let ack = SuccessResponse {
                id: request.id,
                result: ResponseResult::SubscriptionStarted {},
            };
            write_line(&mut conn, &ack);

            let event = SubscriptionEventEnvelope {
                event: SubscriptionEventKind::PaneAgentStatusChanged,
                data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                    pane_id: "w1:p2".to_string(),
                    workspace_id: "ws1".to_string(),
                    agent_status: AgentStatus::Working,
                    agent: None,
                    title: None,
                    display_agent: None,
                    custom_status: None,
                    state_labels: HashMap::new(),
                }),
            };
            write_line(&mut conn, &event);
            conn.shutdown(std::net::Shutdown::Both).ok();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        let snapshot = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("snapshot event");
        assert_eq!(
            snapshot,
            HostEvent::Snapshot {
                host: HostLinkId("wb".to_string()),
                panes: vec![
                    remote_pane("w1:p1", AgentStatus::Idle),
                    remote_pane("w1:p2", AgentStatus::Working),
                ],
            }
        );

        let status_changed = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("status changed event");
        assert_eq!(
            status_changed,
            HostEvent::StatusChanged {
                host: HostLinkId("wb".to_string()),
                remote_pane_id: "w1:p2".to_string(),
                status: AgentStatus::Working,
            }
        );

        let link_down = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("link down event");
        assert_eq!(
            link_down,
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );

        server.join().unwrap();
        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pins the refresh cycle end-to-end: a `pane.created` event for a pane
    /// missing from the current snapshot makes the loop bounce its
    /// subscription connection -- re-run `pane.list`, emit a fresh
    /// `Snapshot` covering the new pane, and re-subscribe with per-pane
    /// status subscriptions for the new pane set -- with no `LinkDown` in
    /// between (asserted by event order: the recv after the first Snapshot
    /// must be the second Snapshot). Also pins the replay guard: on the
    /// rebuilt connection, a replayed `pane.created` for an already-known
    /// pane is ignored instead of triggering another bounce (if it bounced,
    /// the fake remote has no third pane.list scripted, so the expected
    /// StatusChanged/LinkDown tail could never arrive). All canned lines
    /// serialize the real API structs.
    #[test]
    fn pane_created_triggers_snapshot_refresh_without_link_down() {
        let dir = unique_temp_dir("remote-pane-refresh");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Connection A1: initial pane.list snapshot, one pane.
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            assert!(matches!(request.method, Method::PaneList(_)));
            write_line(
                &mut conn,
                &SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneList {
                        panes: vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
                    },
                },
            );
            conn.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);

            // Connection B1: subscribe, then announce a new pane.
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            let Method::EventsSubscribe(params) = request.method else {
                panic!("expected events.subscribe, got {:?}", request.method);
            };
            assert!(params.subscriptions.contains(&Subscription::PaneCreated {}));
            assert!(params.subscriptions.contains(&Subscription::PaneClosed {}));
            assert!(params
                .subscriptions
                .contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w1:p1".to_string(),
                    agent_status: None,
                }));
            write_line(
                &mut conn,
                &SuccessResponse {
                    id: request.id,
                    result: ResponseResult::SubscriptionStarted {},
                },
            );
            write_line(
                &mut conn,
                &EventEnvelope {
                    event: EventKind::PaneCreated,
                    data: EventData::PaneCreated {
                        pane: canned_pane_info("w1:p2", AgentStatus::Working),
                    },
                },
            );
            // The loop closes B1 itself and comes back for a fresh snapshot.

            // Connection A2: refreshed pane.list, two panes.
            let (mut conn2, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn2);
            assert!(matches!(request.method, Method::PaneList(_)));
            write_line(
                &mut conn2,
                &SuccessResponse {
                    id: request.id,
                    result: ResponseResult::PaneList {
                        panes: vec![
                            canned_pane_info("w1:p1", AgentStatus::Idle),
                            canned_pane_info("w1:p2", AgentStatus::Working),
                        ],
                    },
                },
            );
            conn2.shutdown(std::net::Shutdown::Both).ok();
            drop(conn2);
            drop(conn);

            // Connection B2: re-subscribe must now cover the new pane too.
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            let Method::EventsSubscribe(params) = request.method else {
                panic!("expected events.subscribe, got {:?}", request.method);
            };
            assert!(params.subscriptions.contains(&Subscription::PaneCreated {}));
            assert!(params
                .subscriptions
                .contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w1:p1".to_string(),
                    agent_status: None,
                }));
            assert!(params
                .subscriptions
                .contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w1:p2".to_string(),
                    agent_status: None,
                }));
            write_line(
                &mut conn,
                &SuccessResponse {
                    id: request.id,
                    result: ResponseResult::SubscriptionStarted {},
                },
            );
            // Replay guard: the event hub replays buffered events to fresh
            // subscriptions (ActiveEventSubscription starts at sequence 0),
            // so B2 sees the pane.created for w1:p2 again -- now already in
            // the snapshot; the loop must ignore it rather than bounce.
            write_line(
                &mut conn,
                &EventEnvelope {
                    event: EventKind::PaneCreated,
                    data: EventData::PaneCreated {
                        pane: canned_pane_info("w1:p2", AgentStatus::Working),
                    },
                },
            );
            write_line(
                &mut conn,
                &SubscriptionEventEnvelope {
                    event: SubscriptionEventKind::PaneAgentStatusChanged,
                    data: SubscriptionEventData::PaneAgentStatusChanged(
                        PaneAgentStatusChangedEvent {
                            pane_id: "w1:p2".to_string(),
                            workspace_id: "ws1".to_string(),
                            agent_status: AgentStatus::Blocked,
                            agent: None,
                            title: None,
                            display_agent: None,
                            custom_status: None,
                            state_labels: HashMap::new(),
                        },
                    ),
                },
            );
            conn.shutdown(std::net::Shutdown::Both).ok();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        let first = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("initial snapshot");
        assert_eq!(
            first,
            HostEvent::Snapshot {
                host: HostLinkId("wb".to_string()),
                panes: vec![remote_pane("w1:p1", AgentStatus::Idle)],
            }
        );

        // The very next event must be the refreshed snapshot -- a LinkDown
        // here would mean the refresh was misclassified as a link failure.
        let second = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("refreshed snapshot");
        assert_eq!(
            second,
            HostEvent::Snapshot {
                host: HostLinkId("wb".to_string()),
                panes: vec![
                    remote_pane("w1:p1", AgentStatus::Idle),
                    remote_pane("w1:p2", AgentStatus::Working),
                ],
            }
        );

        // The replayed pane.created was ignored; the status line and then
        // the EOF-driven LinkDown follow directly.
        let status = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("status changed event");
        assert_eq!(
            status,
            HostEvent::StatusChanged {
                host: HostLinkId("wb".to_string()),
                remote_pane_id: "w1:p2".to_string(),
                status: AgentStatus::Blocked,
            }
        );
        let link_down = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("link down event");
        assert_eq!(
            link_down,
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );

        server.join().unwrap();
        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    use std::os::unix::net::UnixListener;

    /// Fake-remote side of connection A: serve one canned `pane.list`.
    fn serve_pane_list(listener: &UnixListener, panes: Vec<PaneInfo>) {
        let (mut conn, _) = listener.accept().unwrap();
        let request = read_request_line(&mut conn);
        assert!(matches!(request.method, Method::PaneList(_)));
        write_line(
            &mut conn,
            &SuccessResponse {
                id: request.id,
                result: ResponseResult::PaneList { panes },
            },
        );
        conn.shutdown(std::net::Shutdown::Both).ok();
    }

    /// Fake-remote side of connection B setup: ack the subscribe, return the
    /// held-open stream plus the requested subscription list.
    fn accept_subscribe(listener: &UnixListener) -> (UnixStream, Vec<Subscription>) {
        let (mut conn, _) = listener.accept().unwrap();
        let request = read_request_line(&mut conn);
        let Method::EventsSubscribe(params) = request.method else {
            panic!("expected events.subscribe, got {:?}", request.method);
        };
        write_line(
            &mut conn,
            &SuccessResponse {
                id: request.id,
                result: ResponseResult::SubscriptionStarted {},
            },
        );
        (conn, params.subscriptions)
    }

    /// Fake-remote side of connection B setup when a per-pane probe fails:
    /// answer with the API's real error ack (remote alive) and close.
    fn reject_subscribe(listener: &UnixListener) {
        let (mut conn, _) = listener.accept().unwrap();
        let request = read_request_line(&mut conn);
        assert!(matches!(request.method, Method::EventsSubscribe(_)));
        write_line(
            &mut conn,
            &ErrorResponse {
                id: request.id,
                error: ErrorBody {
                    code: "not_found".to_string(),
                    message: "pane not found: w1:p1".to_string(),
                },
            },
        );
        conn.shutdown(std::net::Shutdown::Both).ok();
    }

    fn ghost_created_event() -> EventEnvelope {
        EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated {
                pane: canned_pane_info("w1:ghost", AgentStatus::Working),
            },
        }
    }

    fn status_event(pane_id: &str, status: AgentStatus) -> SubscriptionEventEnvelope {
        SubscriptionEventEnvelope {
            event: SubscriptionEventKind::PaneAgentStatusChanged,
            data: SubscriptionEventData::PaneAgentStatusChanged(PaneAgentStatusChangedEvent {
                pane_id: pane_id.to_string(),
                workspace_id: "ws1".to_string(),
                agent_status: status,
                agent: None,
                title: None,
                display_agent: None,
                custom_status: None,
                state_labels: HashMap::new(),
            }),
        }
    }

    /// Layer 2 of the refresh-storm guard. A created event for a ghost pane
    /// (announced but gone again before the next pane.list -- so layer 1's
    /// known-set check can never cover it) triggers exactly one refresh:
    /// when the rebuilt subscription replays the same ghost event,
    /// `refreshed_for` suppresses a second bounce. Pinned by the loop still
    /// being on connection B2 to deliver the StatusChanged (a third
    /// pane.list connection would instead hit the dropped listener and
    /// surface as LinkDown in StatusChanged's place).
    #[test]
    fn ghost_pane_replay_triggers_at_most_one_refresh() {
        let dir = unique_temp_dir("remote-pane-ghost");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            let (mut conn, _) = accept_subscribe(&listener);
            write_line(&mut conn, &ghost_created_event());

            // The loop bounces once; the ghost is NOT in the fresh list.
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            let (mut conn2, _) = accept_subscribe(&listener);
            // Replay of the identical ghost event on the rebuilt stream.
            write_line(&mut conn2, &ghost_created_event());
            // Still-on-B2 proof; also drop the listener first so a wrongful
            // third pane.list fails fast instead of parking in the backlog.
            write_line(&mut conn2, &status_event("w1:p1", AgentStatus::Working));
            drop(listener);
            conn2.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        for label in ["initial snapshot", "refreshed snapshot"] {
            let event = rx.recv_timeout(Duration::from_secs(5)).expect(label);
            assert_eq!(
                event,
                HostEvent::Snapshot {
                    host: HostLinkId("wb".to_string()),
                    panes: vec![remote_pane("w1:p1", AgentStatus::Idle)],
                },
                "{label}"
            );
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("status changed event"),
            HostEvent::StatusChanged {
                host: HostLinkId("wb".to_string()),
                remote_pane_id: "w1:p1".to_string(),
                status: AgentStatus::Working,
            },
            "replayed ghost created must not bounce a second time"
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("link down event"),
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );

        server.join().unwrap();
        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subscribe rejected with an error ack (remote alive; e.g. a snapshot
    /// pane closed between pane.list and the subscribe's per-pane probe) is
    /// a refresh, not a link failure: the very next event after the first
    /// Snapshot must be the retried Snapshot, never LinkDown.
    #[test]
    fn subscribe_error_ack_refreshes_snapshot_without_link_down() {
        let dir = unique_temp_dir("remote-pane-error-ack");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            reject_subscribe(&listener);
            // Retry round succeeds.
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            let (mut conn, _) = accept_subscribe(&listener);
            write_line(&mut conn, &status_event("w1:p1", AgentStatus::Blocked));
            conn.shutdown(std::net::Shutdown::Both).ok();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        for label in ["initial snapshot", "retried snapshot"] {
            let event = rx.recv_timeout(Duration::from_secs(5)).expect(label);
            assert!(
                matches!(event, HostEvent::Snapshot { .. }),
                "{label}: expected Snapshot, got {event:?}"
            );
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("status changed event"),
            HostEvent::StatusChanged {
                host: HostLinkId("wb".to_string()),
                remote_pane_id: "w1:p1".to_string(),
                status: AgentStatus::Blocked,
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("link down event"),
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );

        server.join().unwrap();
        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A remote that error-acks every subscribe cannot induce an unbounded
    /// bounce loop: after MAX_CONSECUTIVE_SETUP_REFRESHES rounds the loop
    /// degrades to LinkDown. The fake remote is willing to serve more
    /// rounds than the cap, so a missing cap would surface as a fourth
    /// Snapshot where LinkDown is expected.
    #[test]
    fn repeated_subscribe_error_acks_cap_to_link_down() {
        let dir = unique_temp_dir("remote-pane-error-ack-cap");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        // Deliberately not joined: it parks in accept() for rounds the
        // capped loop never starts; nextest's per-test process isolation
        // reaps it when the test process exits.
        let _server = std::thread::spawn(move || {
            for _ in 0..(MAX_CONSECUTIVE_SETUP_REFRESHES + 2) {
                serve_pane_list(&listener, vec![]);
                reject_subscribe(&listener);
            }
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        for round in 0..MAX_CONSECUTIVE_SETUP_REFRESHES {
            let event = rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("snapshot for round {round}"));
            assert!(
                matches!(event, HostEvent::Snapshot { .. }),
                "round {round}: expected Snapshot, got {event:?}"
            );
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("link down after cap"),
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );
        // The loop is done -- no fourth snapshot follows.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cross-workspace `pane.move` re-mints the pane's public id and emits
    /// only pane.moved (no created/closed pair) -- the moved event's unknown
    /// id must heal the divergence through the same refresh cycle as an
    /// unknown created.
    #[test]
    fn pane_moved_to_unknown_id_triggers_refresh() {
        let dir = unique_temp_dir("remote-pane-moved");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            let (mut conn, subscriptions) = accept_subscribe(&listener);
            assert!(subscriptions.contains(&Subscription::PaneMoved {}));
            write_line(
                &mut conn,
                &EventEnvelope {
                    event: EventKind::PaneMoved,
                    data: EventData::PaneMoved {
                        previous_pane_id: "w1:p1".to_string(),
                        previous_workspace_id: "ws1".to_string(),
                        previous_tab_id: "tab1".to_string(),
                        pane: Box::new(canned_pane_info("w2:p1", AgentStatus::Idle)),
                        created_workspace: None,
                        created_tab: None,
                        closed_workspace_id: None,
                        closed_tab_id: None,
                    },
                },
            );

            serve_pane_list(
                &listener,
                vec![canned_pane_info("w2:p1", AgentStatus::Idle)],
            );
            let (conn2, subscriptions) = accept_subscribe(&listener);
            assert!(
                subscriptions.contains(&Subscription::PaneAgentStatusChanged {
                    pane_id: "w2:p1".to_string(),
                    agent_status: None,
                })
            );
            conn2.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("initial snapshot"),
            HostEvent::Snapshot {
                host: HostLinkId("wb".to_string()),
                panes: vec![remote_pane("w1:p1", AgentStatus::Idle)],
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("post-move snapshot"),
            HostEvent::Snapshot {
                host: HostLinkId("wb".to_string()),
                panes: vec![remote_pane("w2:p1", AgentStatus::Idle)],
            }
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5))
                .expect("link down event"),
            HostEvent::LinkDown {
                host: HostLinkId("wb".to_string()),
            }
        );

        server.join().unwrap();
        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping the HostEvent receiver is an implicit stop: the next send
    /// attempt fails, and the loop must exit (releasing its transport
    /// channel) instead of holding the link open for nobody. join()
    /// returning is the proof; a hang here fails the test by timeout.
    #[test]
    fn dropped_receiver_stops_the_loop() {
        let dir = unique_temp_dir("remote-pane-rx-drop");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let (rx_dropped_tx, rx_dropped_rx) = std::sync::mpsc::channel();
        // Deliberately not joined: after the loop exits, this fake remote is
        // parked keeping conn open with nothing left to do; nextest's
        // per-test process isolation reaps it.
        let _server = std::thread::spawn(move || {
            serve_pane_list(
                &listener,
                vec![canned_pane_info("w1:p1", AgentStatus::Idle)],
            );
            let (mut conn, _) = accept_subscribe(&listener);
            // Wait until the main thread has dropped the receiver, then
            // produce an event so the loop's send fails.
            rx_dropped_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("main thread signals receiver drop");
            write_line(&mut conn, &status_event("w1:p1", AgentStatus::Working));
            std::thread::sleep(Duration::from_secs(30));
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        let snapshot = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("snapshot event");
        assert!(matches!(snapshot, HostEvent::Snapshot { .. }));
        drop(rx);
        rx_dropped_tx.send(()).unwrap();

        handle.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Proves the loop is actually stoppable: block the fake remote forever
    /// after the ack (never send the event line, never close), then call
    /// `stop()` and confirm the loop thread exits anyway -- mirroring
    /// `host_transport`'s `closer_unblocks_a_reader_stuck_in_read` test.
    #[test]
    fn stop_unblocks_a_loop_parked_reading_the_subscription_stream() {
        let dir = unique_temp_dir("remote-pane-poll-loop-stop");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("api.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

        let (parked_tx, parked_rx) = std::sync::mpsc::channel();
        // Deliberately not joined: after `stop()` unblocks the loop thread's
        // read, this fake remote's own thread is still parked in its 30s
        // sleep with nothing left to do; nextest's per-test process
        // isolation reaps it when the test process exits.
        let _server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            assert!(matches!(request.method, Method::PaneList(_)));
            let response = SuccessResponse {
                id: request.id,
                result: ResponseResult::PaneList { panes: vec![] },
            };
            write_line(&mut conn, &response);
            conn.shutdown(std::net::Shutdown::Both).ok();
            drop(conn);

            let (mut conn, _) = listener.accept().unwrap();
            let request = read_request_line(&mut conn);
            assert!(matches!(request.method, Method::EventsSubscribe(_)));
            let ack = SuccessResponse {
                id: request.id,
                result: ResponseResult::SubscriptionStarted {},
            };
            write_line(&mut conn, &ack);

            // Park here without sending anything else or closing, so the
            // loop thread's subsequent read genuinely blocks.
            parked_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let handle = spawn_host_event_loop(HostLinkId("wb".to_string()), transport, tx);

        let snapshot = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("snapshot event");
        assert!(matches!(snapshot, HostEvent::Snapshot { .. }));
        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fake remote reached the parked state");
        // Bias toward stopping a reader that is already blocked in read();
        // stop-before-read also succeeds, so this is not a correctness wait.
        std::thread::sleep(Duration::from_millis(50));

        handle.stop();
        handle.join();

        // A deliberate stop is not a link failure: no LinkDown should follow.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------
    // RemotePaneAttach: handshake, streamed terminal bytes, forwarded input
    // -----------------------------------------------------------------

    /// Scripted fake remote server for one `RemotePaneAttach`: reads and
    /// checks `Hello`, replies `Welcome`, reads and checks `AttachTerminal`,
    /// then plays a canned `Terminal` frame. Returns the connection so the
    /// caller can script whatever comes next (an `Input` read, a
    /// `ServerShutdown`, etc).
    fn accept_and_complete_attach_handshake(
        listener: &UnixListener,
        expected_terminal_id: &str,
    ) -> UnixStream {
        let (mut conn, _) = listener.accept().unwrap();
        let hello: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
        match hello {
            ClientMessage::Hello {
                version,
                requested_encoding,
                launch_mode,
                ..
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(requested_encoding, RenderEncoding::TerminalAnsi);
                assert_eq!(launch_mode, ClientLaunchMode::TerminalAttach);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        protocol::write_message(
            &mut conn,
            &ServerMessage::Welcome {
                version: PROTOCOL_VERSION,
                encoding: RenderEncoding::TerminalAnsi,
                error: None,
            },
        )
        .unwrap();

        let attach: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
        match attach {
            ClientMessage::AttachTerminal {
                terminal_id,
                takeover,
            } => {
                assert_eq!(terminal_id, expected_terminal_id);
                assert!(!takeover, "attach must not request takeover");
            }
            other => panic!("expected AttachTerminal, got {other:?}"),
        }
        conn
    }

    /// Pins the real wire protocol end-to-end: the scripted fake remote
    /// plays the real `Welcome`/`Terminal` messages (built from the real
    /// wire types, so this can't drift from the actual protocol), and
    /// asserts `RemotePaneAttach` (1) emits one `HostEvent::TerminalBytes`
    /// carrying the frame's bytes, (2) forwards one written `send_input`
    /// call as a real `ClientMessage::Input`, and (3) `detach()` sends a
    /// real `ClientMessage::Detach`.
    #[test]
    fn attach_streams_terminal_frame_and_forwards_input_then_detaches() {
        let dir = unique_temp_dir("remote-pane-attach");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut conn = accept_and_complete_attach_handshake(&listener, "term_w1:p1");

            protocol::write_message(
                &mut conn,
                &ServerMessage::Terminal(crate::protocol::TerminalFrame {
                    seq: 1,
                    width: 80,
                    height: 24,
                    full: true,
                    bytes: b"hello".to_vec(),
                }),
            )
            .unwrap();

            let input: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            assert_eq!(
                input,
                ClientMessage::Input {
                    data: b"typed".to_vec(),
                }
            );

            let detach: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            assert_eq!(detach, ClientMessage::Detach);
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let host = HostLinkId("wb".to_string());
        let local_pane = PaneId::from_raw(7);

        let attach = RemotePaneAttach::attach(
            host.clone(),
            local_pane,
            "term_w1:p1".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        )
        .expect("attach should succeed against a well-behaved fake remote");

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("terminal bytes event");
        assert_eq!(
            event,
            HostEvent::TerminalBytes {
                host: host.clone(),
                local_pane,
                width: 80,
                height: 24,
                bytes: b"hello".to_vec(),
            }
        );

        attach
            .send_input(b"typed".to_vec())
            .expect("send_input should succeed while attached");

        attach.detach();

        server.join().unwrap();
        // No HostEvent follows a clean detach -- ending an attach is never
        // itself a link-health signal.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `Welcome` with `error: Some(_)` (protocol version mismatch, unknown
    /// terminal id rejected up front, etc.) must fail `attach()` outright
    /// instead of proceeding to send `AttachTerminal`.
    #[test]
    fn attach_fails_when_remote_rejects_the_handshake() {
        let dir = unique_temp_dir("remote-pane-attach-reject");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let _hello: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            protocol::write_message(
                &mut conn,
                &ServerMessage::Welcome {
                    version: PROTOCOL_VERSION,
                    encoding: RenderEncoding::TerminalAnsi,
                    error: Some("terminal not found".to_string()),
                },
            )
            .unwrap();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, _rx) = mpsc::channel();

        let result = RemotePaneAttach::attach(
            HostLinkId("wb".to_string()),
            PaneId::from_raw(1),
            "term_missing".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        );
        assert!(
            result.is_err(),
            "attach must fail when the remote's Welcome carries an error"
        );

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Proves the reader thread is actually stoppable: park the fake remote
    /// after the handshake without ever sending a frame or closing (so the
    /// reader thread's blocking read genuinely blocks), then drop the
    /// `RemotePaneAttach` and confirm teardown still completes -- mirroring
    /// `host_transport`'s `closer_unblocks_a_reader_stuck_in_read` test.
    #[test]
    fn dropping_attach_unblocks_a_reader_parked_in_a_blocking_read() {
        let dir = unique_temp_dir("remote-pane-attach-stop");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let (parked_tx, parked_rx) = std::sync::mpsc::channel();
        // Hold the connection open until the test signals teardown, rather
        // than a fixed multi-second sleep that would linger under plain
        // `cargo test`. The stop channel releases the thread promptly once
        // the client side has been dropped.
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let _conn = accept_and_complete_attach_handshake(&listener, "term_w1:p1");
            parked_tx.send(()).unwrap();
            let _ = stop_rx.recv_timeout(Duration::from_secs(5));
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, _rx) = mpsc::channel();

        let attach = RemotePaneAttach::attach(
            HostLinkId("wb".to_string()),
            PaneId::from_raw(3),
            "term_w1:p1".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        )
        .expect("attach should succeed");

        parked_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fake remote reached the parked state");
        // Bias toward dropping while the reader is already blocked in
        // read(); drop-before-read also succeeds, so this is not a
        // correctness wait.
        std::thread::sleep(Duration::from_millis(50));

        // drop()'s Drop impl must force-close and join the reader thread
        // instead of hanging forever; the test itself timing out is the
        // failure mode here, not an assertion.
        drop(attach);

        let _ = stop_tx.send(());
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression guard for the graphics-frame freeze: the server inlines
    /// Kitty graphics into the terminal-ANSI stream, so a single frame can
    /// exceed `MAX_FRAME_SIZE` (2 MiB). The reader must read with the larger
    /// `MAX_GRAPHICS_FRAME_SIZE` cap and still surface the frame as
    /// `TerminalBytes`, not `Oversized`-error itself dead. The canned frame
    /// carries > 2 MiB of bytes precisely to cross that boundary.
    #[test]
    fn attach_streams_oversized_graphics_frame_without_killing_the_reader() {
        let dir = unique_temp_dir("remote-pane-attach-big");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let big = vec![b'x'; 3 * 1024 * 1024];
        let big_len = big.len();
        assert!(
            big_len > MAX_FRAME_SIZE,
            "the canned frame must exceed the small cap to exercise the fix"
        );

        let server = std::thread::spawn(move || {
            let mut conn = accept_and_complete_attach_handshake(&listener, "term_w1:p1");
            protocol::write_message(
                &mut conn,
                &ServerMessage::Terminal(crate::protocol::TerminalFrame {
                    seq: 1,
                    width: 200,
                    height: 50,
                    full: true,
                    bytes: big,
                }),
            )
            .unwrap();
            conn.shutdown(std::net::Shutdown::Both).ok();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();

        let attach = RemotePaneAttach::attach(
            HostLinkId("wb".to_string()),
            PaneId::from_raw(9),
            "term_w1:p1".to_string(),
            200,
            50,
            transport.as_ref(),
            tx,
        )
        .expect("attach should succeed");

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("oversized terminal bytes event");
        match event {
            HostEvent::TerminalBytes {
                width,
                height,
                bytes,
                ..
            } => {
                assert_eq!(
                    bytes.len(),
                    big_len,
                    "the whole oversized frame must survive"
                );
                assert_eq!((width, height), (200, 50));
            }
            other => panic!("expected TerminalBytes, got {other:?}"),
        }

        drop(attach);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live-but-silent remote (channel opens, `Hello` is read, but no
    /// `Welcome` ever comes) must not hang `attach()` forever: the watchdog
    /// force-closes the channel after the deadline, unblocking the blocking
    /// read so `attach()` returns `Err`. Uses a short injected deadline so
    /// the test is fast; production uses `ATTACH_HANDSHAKE_TIMEOUT`.
    #[test]
    fn attach_times_out_when_remote_never_completes_handshake() {
        let dir = unique_temp_dir("remote-pane-attach-timeout");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let _hello: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            // Never send Welcome; hold the connection open and silent so the
            // client's blocking Welcome read genuinely blocks and only the
            // watchdog can end it.
            let _ = stop_rx.recv_timeout(Duration::from_secs(5));
            drop(conn);
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, _rx) = mpsc::channel();

        let start = std::time::Instant::now();
        let result = RemotePaneAttach::attach_with_timeout(
            Duration::from_millis(300),
            HostLinkId("wb".to_string()),
            PaneId::from_raw(5),
            "term_w1:p1".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        );
        assert!(
            result.is_err(),
            "attach must fail when the remote never completes the handshake"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the watchdog should fire near its short deadline, not hang until a test timeout"
        );

        let _ = stop_tx.send(());
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `resize()` forwards a `ClientMessage::Resize` with the given
    /// dimensions over the same write half as `send_input`.
    #[test]
    fn resize_forwards_client_resize_to_remote() {
        let dir = unique_temp_dir("remote-pane-attach-resize");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut conn = accept_and_complete_attach_handshake(&listener, "term_w1:p1");
            let resize: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            assert_eq!(
                resize,
                ClientMessage::Resize {
                    cols: 120,
                    rows: 40,
                    cell_width_px: 8,
                    cell_height_px: 16,
                }
            );
            let detach: ClientMessage = protocol::read_message(&mut conn, MAX_FRAME_SIZE).unwrap();
            assert_eq!(detach, ClientMessage::Detach);
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, _rx) = mpsc::channel();

        let attach = RemotePaneAttach::attach(
            HostLinkId("wb".to_string()),
            PaneId::from_raw(11),
            "term_w1:p1".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        )
        .expect("attach should succeed");

        attach
            .resize(120, 40, 8, 16)
            .expect("resize should succeed while attached");
        attach.detach();

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `ServerShutdown` arriving BEFORE any `Terminal` frame is an attach
    /// refusal (unknown/closed/already-owned terminal id) -- `attach()`
    /// already returned `Ok` at write time, so this must surface as a
    /// distinct `HostEvent::AttachFailed`, not be swallowed like a
    /// mid-stream teardown.
    #[test]
    fn server_shutdown_before_first_frame_surfaces_as_attach_failed() {
        let dir = unique_temp_dir("remote-pane-attach-failed");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("client.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let mut conn = accept_and_complete_attach_handshake(&listener, "term_missing");
            // Reject exactly the way headless.rs's unknown-terminal path
            // does: a ServerShutdown carrying the failure reason, no frame.
            protocol::write_message(
                &mut conn,
                &ServerMessage::ServerShutdown {
                    reason: Some(
                        "terminal attach failed: terminal term_missing not found".to_string(),
                    ),
                },
            )
            .unwrap();
            conn.shutdown(std::net::Shutdown::Both).ok();
        });

        let transport: Box<dyn LinkTransport> = Box::new(UnixSocketTransport {
            api_socket: sock.clone(),
            client_socket: sock.clone(),
        });
        let (tx, rx) = mpsc::channel();
        let host = HostLinkId("wb".to_string());
        let local_pane = PaneId::from_raw(13);

        let attach = RemotePaneAttach::attach(
            host.clone(),
            local_pane,
            "term_missing".to_string(),
            80,
            24,
            transport.as_ref(),
            tx,
        )
        .expect("attach returns Ok the instant AttachTerminal is written");

        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("attach failed event");
        assert_eq!(
            event,
            HostEvent::AttachFailed {
                host,
                local_pane,
                reason: Some("terminal attach failed: terminal term_missing not found".to_string()),
            }
        );
        // Exactly one event: the refusal, then nothing.
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        drop(attach);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------
    // Input-drop guard
    // -----------------------------------------------------------------

    #[test]
    fn connected_link_forwards_input() {
        let host = HostLinkId("wb".to_string());
        assert_eq!(
            route_remote_pane_input(&host, LinkState::Connected),
            InputRouteOutcome::Forward
        );
    }

    #[test]
    fn reconnecting_link_drops_input_with_a_notice_and_no_queue() {
        let host = HostLinkId("wb".to_string());

        // "No queue": the function takes no queue/buffer parameter and
        // returns a single owned decision by value, so there is nothing for
        // a dropped payload to accumulate in -- calling it twice can only
        // ever produce two independent, equal decisions.
        for _ in 0..2 {
            match route_remote_pane_input(&host, LinkState::Reconnecting { attempt: 2 }) {
                InputRouteOutcome::Drop { notice } => {
                    assert!(
                        notice.contains("wb"),
                        "notice should name the affected host: {notice}"
                    );
                }
                InputRouteOutcome::Forward => {
                    panic!("expected input to be dropped while reconnecting")
                }
            }
        }
    }

    #[test]
    fn connecting_and_offline_links_drop_input_too() {
        let host = HostLinkId("wb".to_string());
        for state in [LinkState::Connecting, LinkState::Offline] {
            assert!(
                matches!(
                    route_remote_pane_input(&host, state),
                    InputRouteOutcome::Drop { .. }
                ),
                "expected drop for {state:?}"
            );
        }
    }
}
