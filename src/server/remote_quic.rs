//! Lazy SSH-authorized QUIC transport for remote app clients.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::{mpsc, watch, Notify, Semaphore};
use tracing::{debug, info, warn};

use crate::config::RemoteConfig;
use crate::protocol::{
    self, ClientLaunchMode, ClientMessage, RemoteBootstrapRecord, RemoteBootstrapRequest,
    RemoteQuicHello, RemoteQuicRenderRecord, RemoteQuicResourceRef, RemoteQuicStreamHeader,
    RenderEncoding, ServerMessage, MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
    REMOTE_QUIC_ALPN, REMOTE_QUIC_HASH_BYTES, REMOTE_QUIC_ID_BYTES,
    REMOTE_QUIC_MAX_RESOURCE_INVENTORY, REMOTE_QUIC_MAX_RESOURCE_SIZE, REMOTE_QUIC_TOKEN_BYTES,
};
use crate::remote::frame::{hash_bytes, lock, read_async_message, write_async_message};

use crate::server::client_transport::{
    clamp_terminal_size, client_message_to_event, parse_client_keybindings, ClientWriter,
    ServerEvent,
};

const MAX_TOKENS: usize = 64;
const MAX_CONTROL_ITEMS: usize = 64;
const MAX_CONTROL_BYTES: usize = 1024 * 1024;
const MAX_RESOURCE_TRANSFERS: usize = 2;
const MAX_RESOURCE_REFS_PER_FRAME: usize = 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN_MIN_LIFETIME: Duration = Duration::from_secs(60);
const TOKEN_MAX_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const QUIC_SEND_WINDOW: u64 = 4 * 1024 * 1024;
const QUIC_STREAM_RECEIVE_WINDOW: u32 = 256 * 1024;
const QUIC_RECEIVE_WINDOW: u32 = 1024 * 1024;
const REPLACED_CODE: u32 = 0x100;
const AUTH_CODE: u32 = 0x101;
const PROTOCOL_CODE: u32 = 0x102;

#[derive(Debug)]
struct Capability {
    session: String,
    logical_client_id: [u8; REMOTE_QUIC_ID_BYTES],
    expires_unix_seconds: u64,
    connection_generation: u64,
    active_connection: Option<Connection>,
    issued_order: u64,
}

struct ServerState {
    server_instance_id: [u8; REMOTE_QUIC_ID_BYTES],
    tokens: Mutex<HashMap<[u8; REMOTE_QUIC_HASH_BYTES], Capability>>,
    token_order: AtomicU64,
    next_client_id: AtomicU64,
    server_event_tx: mpsc::Sender<ServerEvent>,
}

/// Process-lifetime QUIC endpoint. It is constructed only after an authenticated
/// local bootstrap request, so ordinary/local Herdr never binds UDP or creates TLS material.
pub(crate) struct RemoteQuicServer {
    endpoints: Vec<Endpoint>,
    state: Arc<ServerState>,
    port: u16,
    certificate_fingerprint: [u8; REMOTE_QUIC_HASH_BYTES],
    token_lifetime: Duration,
    ssh_fallback_available: bool,
}

impl RemoteQuicServer {
    pub(crate) fn start(
        config: &RemoteConfig,
        server_event_tx: mpsc::Sender<ServerEvent>,
    ) -> Result<Self, String> {
        let (start_port, end_port) = parse_port_range(&config.quic_port_range)?;
        let token_lifetime = Duration::from_secs(config.quic_idle_timeout_seconds)
            .clamp(TOKEN_MIN_LIFETIME, TOKEN_MAX_LIFETIME);
        let (server_config, certificate_fingerprint) =
            make_server_config(token_lifetime).map_err(|err| err.to_string())?;
        let (endpoints, port) =
            bind_endpoints(server_config, start_port, end_port).map_err(|err| {
                format!("failed to bind remote QUIC port {start_port}-{end_port}: {err}")
            })?;

        let mut server_instance_id = [0u8; REMOTE_QUIC_ID_BYTES];
        getrandom::fill(&mut server_instance_id)
            .map_err(|err| format!("failed to generate QUIC server instance id: {err}"))?;
        let state = Arc::new(ServerState {
            server_instance_id,
            tokens: Mutex::new(HashMap::new()),
            token_order: AtomicU64::new(1),
            next_client_id: AtomicU64::new(1u64 << 63),
            server_event_tx,
        });

        for endpoint in &endpoints {
            tokio::spawn(accept_connections(endpoint.clone(), Arc::clone(&state)));
        }
        info!(
            port,
            endpoints = endpoints.len(),
            "remote QUIC endpoint enabled"
        );
        Ok(Self {
            endpoints,
            state,
            port,
            certificate_fingerprint,
            token_lifetime,
            ssh_fallback_available: config.ssh_fallback,
        })
    }

    pub(crate) fn bootstrap(
        &self,
        request: RemoteBootstrapRequest,
    ) -> Result<RemoteBootstrapRecord, String> {
        let active_session = crate::session::active_name()
            .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned());
        if request.session != active_session {
            return Err(format!(
                "remote bootstrap session mismatch: requested {}, server owns {active_session}",
                request.session
            ));
        }

        let mut token = [0u8; REMOTE_QUIC_TOKEN_BYTES];
        getrandom::fill(&mut token)
            .map_err(|err| format!("failed to generate remote capability: {err}"))?;
        let token_hash = hash_bytes(&token);
        let expires_unix_seconds = unix_seconds().saturating_add(self.token_lifetime.as_secs());
        let issued_order = self.state.token_order.fetch_add(1, Ordering::Relaxed);

        let mut tokens = lock(&self.state.tokens);
        tokens.retain(|_, capability| capability.expires_unix_seconds > unix_seconds());
        if tokens.len() >= MAX_TOKENS {
            if let Some(oldest) = tokens
                .iter()
                .min_by_key(|(_, capability)| capability.issued_order)
                .map(|(hash, _)| *hash)
            {
                if let Some(removed) = tokens.remove(&oldest) {
                    if let Some(connection) = removed.active_connection {
                        connection.close(VarInt::from_u32(REPLACED_CODE), b"capability evicted");
                    }
                }
            }
        }
        tokens.insert(
            token_hash,
            Capability {
                session: request.session,
                logical_client_id: request.logical_client_id,
                expires_unix_seconds,
                connection_generation: 0,
                active_connection: None,
                issued_order,
            },
        );

        Ok(RemoteBootstrapRecord {
            version: PROTOCOL_VERSION,
            server_instance_id: self.state.server_instance_id,
            port: self.port,
            certificate_fingerprint: self.certificate_fingerprint,
            capability_token: token,
            expires_unix_seconds,
            ssh_fallback_available: self.ssh_fallback_available,
        })
    }
}

impl Drop for RemoteQuicServer {
    fn drop(&mut self) {
        for endpoint in &self.endpoints {
            endpoint.close(
                VarInt::from_u32(REPLACED_CODE),
                b"server handoff or shutdown",
            );
        }
    }
}

fn parse_port_range(value: &str) -> Result<(u16, u16), String> {
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| "remote.quic_port_range must look like 48000-48100".to_owned())?;
    let start = start
        .trim()
        .parse::<u16>()
        .map_err(|_| "remote.quic_port_range has an invalid start port".to_owned())?;
    let end = end
        .trim()
        .parse::<u16>()
        .map_err(|_| "remote.quic_port_range has an invalid end port".to_owned())?;
    if start < 1024 || start > end {
        return Err("remote.quic_port_range must be an ascending unprivileged range".to_owned());
    }
    Ok((start, end))
}

fn make_server_config(
    idle_timeout: Duration,
) -> Result<(quinn::ServerConfig, [u8; REMOTE_QUIC_HASH_BYTES]), Box<dyn std::error::Error>> {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["herdr".to_owned()])?;
    let cert_der = cert.der().clone();
    let fingerprint = hash_bytes(cert_der.as_ref());
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into();
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)?;
    tls.alpn_protocols = vec![REMOTE_QUIC_ALPN.to_vec()];
    tls.max_early_data_size = 0;

    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    server_config.migration(true);
    server_config.transport_config(Arc::new(transport_config(idle_timeout)?));
    Ok((server_config, fingerprint))
}

fn transport_config(idle_timeout: Duration) -> Result<quinn::TransportConfig, io::Error> {
    let mut transport = quinn::TransportConfig::default();
    let idle = idle_timeout.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "QUIC idle timeout is too large",
        )
    })?;
    transport
        .max_idle_timeout(Some(idle))
        .keep_alive_interval(Some(Duration::from_secs(15)))
        .max_concurrent_bidi_streams(VarInt::from_u32(2))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(QUIC_STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(QUIC_RECEIVE_WINDOW))
        .send_window(QUIC_SEND_WINDOW);
    Ok(transport)
}

fn bind_endpoints(
    server_config: quinn::ServerConfig,
    start_port: u16,
    end_port: u16,
) -> io::Result<(Vec<Endpoint>, u16)> {
    let mut last_error = None;
    for port in start_port..=end_port {
        let ipv4_address = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
        let ipv4_socket = match bind_udp_socket(Domain::IPV4, ipv4_address) {
            Ok(socket) => socket,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        let mut endpoints = vec![make_endpoint(server_config.clone(), ipv4_socket)?];

        let ipv6_address = SocketAddr::from((Ipv6Addr::UNSPECIFIED, port));
        match bind_udp_socket(Domain::IPV6, ipv6_address) {
            Ok(socket) => endpoints.push(make_endpoint(server_config.clone(), socket)?),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                last_error = Some(error);
                continue;
            }
            Err(error) => {
                debug!(%error, port, "IPv6 QUIC listener unavailable; using IPv4");
            }
        }
        return Ok((endpoints, port));
    }
    Err(last_error
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "empty port range")))
}

fn bind_udp_socket(domain: Domain, address: SocketAddr) -> io::Result<UdpSocket> {
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if domain == Domain::IPV6 {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

fn make_endpoint(server_config: quinn::ServerConfig, socket: UdpSocket) -> io::Result<Endpoint> {
    Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        Arc::new(quinn::TokioRuntime),
    )
}

async fn accept_connections(endpoint: Endpoint, state: Arc<ServerState>) {
    while let Some(incoming) = endpoint.accept().await {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    if let Err(err) = serve_connection(connection.clone(), state).await {
                        debug!(err = %err, remote = %connection.remote_address(), "remote QUIC connection ended");
                        connection
                            .close(VarInt::from_u32(PROTOCOL_CODE), err.to_string().as_bytes());
                    }
                }
                Err(err) => debug!(err = %err, "remote QUIC handshake failed"),
            }
        });
    }
}

async fn serve_connection(connection: Connection, state: Arc<ServerState>) -> Result<(), String> {
    let (mut control_send, mut control_recv) =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| "timed out waiting for QUIC control stream".to_owned())?
            .map_err(|err| format!("failed to accept QUIC control stream: {err}"))?;
    let hello: RemoteQuicHello = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_async_message(&mut control_recv, MAX_FRAME_SIZE),
    )
    .await
    .map_err(|_| "timed out waiting for QUIC hello".to_owned())??;

    validate_and_fence(&state, &connection, &hello)?;
    let keybindings = parse_client_keybindings(hello.keybindings.clone())?;
    if hello.launch_mode != ClientLaunchMode::App {
        return Err("remote QUIC currently accepts app clients only".to_owned());
    }
    let (cols, rows) = clamp_terminal_size(hello.cols, hello.rows);
    let client_id = state.next_client_id.fetch_add(1, Ordering::Relaxed);

    write_async_message(
        &mut control_send,
        &ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        },
        MAX_FRAME_SIZE,
    )
    .await?;

    let cached_resources = hello
        .cached_resources
        .into_iter()
        .take(REMOTE_QUIC_MAX_RESOURCE_INVENTORY)
        .collect::<HashSet<_>>();
    let (control_writer, control_queue) = QuicControlSender::new();
    let heartbeat_writer = control_writer.clone();
    let (render_writer, render_rx, generation_rx) = QuicRenderSender::new();
    let render_sender_inner = Arc::clone(&render_writer.inner);
    let writer = ClientWriter::quic(control_writer, render_writer);

    state
        .server_event_tx
        .send(ServerEvent::ClientConnected {
            client_id,
            cols,
            rows,
            cell_width_px: hello.cell_width_px,
            cell_height_px: hello.cell_height_px,
            render_encoding: RenderEncoding::TerminalAnsi,
            keybindings,
            direct_attach_requested: false,
            writer,
        })
        .await
        .map_err(|_| "server event loop stopped".to_owned())?;

    let control_publisher = tokio::spawn(publish_control_output(
        control_send,
        Arc::clone(&control_queue),
    ));
    let render_publisher = tokio::spawn(publish_server_output(
        connection.clone(),
        render_rx,
        generation_rx,
        render_sender_inner,
        hello.connection_generation,
        cached_resources,
        client_id,
        state.server_event_tx.clone(),
    ));

    let read_result = receive_client_control(
        &mut control_recv,
        client_id,
        &state.server_event_tx,
        &heartbeat_writer,
    )
    .await;
    let _ = state
        .server_event_tx
        .send(ServerEvent::ClientDisconnected { client_id })
        .await;
    control_publisher.abort();
    render_publisher.abort();
    clear_active_connection(&state, &hello.capability_token, hello.connection_generation);
    read_result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapabilityValidationError {
    ProtocolMismatch { client: u32 },
    ServerInstanceChanged,
    UnknownOrRevoked,
    Expired,
    DifferentLogicalClient,
    DifferentSession,
    StaleGeneration,
}

impl std::fmt::Display for CapabilityValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolMismatch { client } => write!(
                formatter,
                "remote protocol mismatch: client {client}, server {PROTOCOL_VERSION}"
            ),
            Self::ServerInstanceChanged => {
                formatter.write_str("remote server instance changed; SSH rebootstrap required")
            }
            Self::UnknownOrRevoked => {
                formatter.write_str("remote capability is unknown or revoked")
            }
            Self::Expired => {
                formatter.write_str("remote capability expired; SSH rebootstrap required")
            }
            Self::DifferentLogicalClient => {
                formatter.write_str("remote capability belongs to a different logical client")
            }
            Self::DifferentSession => {
                formatter.write_str("remote capability belongs to a different session")
            }
            Self::StaleGeneration => formatter.write_str("stale remote connection generation"),
        }
    }
}

fn validate_capability(
    server_instance_id: [u8; REMOTE_QUIC_ID_BYTES],
    tokens: &HashMap<[u8; REMOTE_QUIC_HASH_BYTES], Capability>,
    hello: &RemoteQuicHello,
    active_session: &str,
    now: u64,
) -> Result<[u8; REMOTE_QUIC_HASH_BYTES], CapabilityValidationError> {
    if hello.version != PROTOCOL_VERSION {
        return Err(CapabilityValidationError::ProtocolMismatch {
            client: hello.version,
        });
    }
    if hello.server_instance_id != server_instance_id {
        return Err(CapabilityValidationError::ServerInstanceChanged);
    }
    let token_hash = hash_bytes(&hello.capability_token);
    let capability = tokens
        .get(&token_hash)
        .ok_or(CapabilityValidationError::UnknownOrRevoked)?;
    if capability.expires_unix_seconds <= now {
        return Err(CapabilityValidationError::Expired);
    }
    if capability.logical_client_id != hello.logical_client_id {
        return Err(CapabilityValidationError::DifferentLogicalClient);
    }
    if capability.session != active_session {
        return Err(CapabilityValidationError::DifferentSession);
    }
    if hello.connection_generation <= capability.connection_generation {
        return Err(CapabilityValidationError::StaleGeneration);
    }
    Ok(token_hash)
}

fn validate_and_fence(
    state: &ServerState,
    connection: &Connection,
    hello: &RemoteQuicHello,
) -> Result<(), String> {
    let active_session = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned());
    let mut tokens = lock(&state.tokens);
    let token_hash = match validate_capability(
        state.server_instance_id,
        &tokens,
        hello,
        &active_session,
        unix_seconds(),
    ) {
        Ok(token_hash) => token_hash,
        Err(error) => {
            match error {
                CapabilityValidationError::ProtocolMismatch { .. } => {
                    connection.close(VarInt::from_u32(PROTOCOL_CODE), b"protocol mismatch");
                }
                CapabilityValidationError::StaleGeneration => {
                    connection.close(VarInt::from_u32(AUTH_CODE), b"stale connection generation");
                }
                _ => {}
            }
            return Err(error.to_string());
        }
    };
    let Some(capability) = tokens.get_mut(&token_hash) else {
        return Err(CapabilityValidationError::UnknownOrRevoked.to_string());
    };
    if let Some(previous) = capability.active_connection.replace(connection.clone()) {
        previous.close(
            VarInt::from_u32(REPLACED_CODE),
            b"newer connection generation accepted",
        );
    }
    capability.connection_generation = hello.connection_generation;
    Ok(())
}

fn clear_active_connection(state: &ServerState, token: &[u8], generation: u64) {
    let token_hash = hash_bytes(token);
    if let Some(capability) = lock(&state.tokens).get_mut(&token_hash) {
        if capability.connection_generation == generation {
            capability.active_connection = None;
        }
    }
}

async fn receive_client_control(
    recv: &mut RecvStream,
    client_id: u64,
    server_event_tx: &mpsc::Sender<ServerEvent>,
    heartbeat_writer: &QuicControlSender,
) -> Result<(), String> {
    loop {
        let message: ClientMessage = read_async_message(recv, MAX_GRAPHICS_FRAME_SIZE).await?;
        let message = match message {
            ClientMessage::RemotePing { nonce } => {
                let mut framed = Vec::new();
                protocol::write_message(&mut framed, &ServerMessage::RemotePong { nonce })
                    .map_err(|err| format!("failed to encode remote heartbeat: {err}"))?;
                heartbeat_writer
                    .send(framed)
                    .map_err(|_| "remote control writer closed".to_owned())?;
                continue;
            }
            ClientMessage::Hello { .. } | ClientMessage::RemoteBootstrap(_) => continue,
            message => message,
        };
        let Some(event) = client_message_to_event(client_id, message)
            .map_err(|reason| format!("invalid remote client message: {reason}"))?
        else {
            continue;
        };
        let detached = matches!(event, ServerEvent::ClientDetach { .. });
        server_event_tx
            .send(event)
            .await
            .map_err(|_| "server event loop stopped".to_owned())?;
        if detached {
            return Ok(());
        }
    }
}

#[derive(Clone)]
pub(crate) struct QuicControlSender {
    queue: Arc<BoundedControlQueue>,
}

impl std::fmt::Debug for QuicControlSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicControlSender").finish_non_exhaustive()
    }
}

impl QuicControlSender {
    fn new() -> (Self, Arc<BoundedControlQueue>) {
        let queue = Arc::new(BoundedControlQueue::default());
        (
            Self {
                queue: Arc::clone(&queue),
            },
            queue,
        )
    }

    pub(crate) fn send(&self, data: Vec<u8>) -> Result<(), std::sync::mpsc::SendError<Vec<u8>>> {
        self.queue.send(data)
    }
}

#[derive(Default)]
struct BoundedControlQueue {
    state: Mutex<ControlQueueState>,
    ready: Notify,
}

#[derive(Default)]
struct ControlQueueState {
    items: VecDeque<Vec<u8>>,
    bytes: usize,
    closed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlKey {
    WindowTitle,
    MouseCapture,
    PrefixInputSource,
    ReloadConfig,
}

impl BoundedControlQueue {
    fn send(&self, data: Vec<u8>) -> Result<(), std::sync::mpsc::SendError<Vec<u8>>> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(std::sync::mpsc::SendError(data));
        }
        let policy = control_policy(&data);
        if data.len() > MAX_CONTROL_BYTES {
            return if policy.drop_on_overflow {
                Ok(())
            } else {
                Err(std::sync::mpsc::SendError(data))
            };
        }
        if let Some(key) = policy.key {
            if let Some(index) = state
                .items
                .iter()
                .position(|queued| control_policy(queued).key == Some(key))
            {
                if let Some(removed) = state.items.remove(index) {
                    state.bytes = state.bytes.saturating_sub(removed.len());
                }
            }
        }

        while state.items.len() >= MAX_CONTROL_ITEMS
            || state.bytes.saturating_add(data.len()) > MAX_CONTROL_BYTES
        {
            if policy.drop_on_overflow {
                return Ok(());
            }
            let Some(index) = state
                .items
                .iter()
                .position(|queued| control_policy(queued).drop_on_overflow)
            else {
                return Err(std::sync::mpsc::SendError(data));
            };
            if let Some(removed) = state.items.remove(index) {
                state.bytes = state.bytes.saturating_sub(removed.len());
            }
        }
        state.bytes = state.bytes.saturating_add(data.len());
        state.items.push_back(data);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    async fn recv(&self) -> Option<Vec<u8>> {
        loop {
            let notified = self.ready.notified();
            {
                let mut state = lock(&self.state);
                if let Some(data) = state.items.pop_front() {
                    state.bytes = state.bytes.saturating_sub(data.len());
                    return Some(data);
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.ready.notify_waiters();
    }

    #[cfg(test)]
    fn bounds(&self) -> (usize, usize) {
        let state = lock(&self.state);
        (state.items.len(), state.bytes)
    }
}

struct ControlPolicy {
    key: Option<ControlKey>,
    drop_on_overflow: bool,
}

fn control_policy(data: &[u8]) -> ControlPolicy {
    let mut input = data;
    let message = protocol::read_message::<_, ServerMessage>(&mut input, MAX_GRAPHICS_FRAME_SIZE);
    match message {
        Ok(ServerMessage::WindowTitle { .. }) => ControlPolicy {
            key: Some(ControlKey::WindowTitle),
            drop_on_overflow: false,
        },
        Ok(ServerMessage::MouseCapture { .. }) => ControlPolicy {
            key: Some(ControlKey::MouseCapture),
            drop_on_overflow: false,
        },
        Ok(ServerMessage::PrefixInputSource { .. }) => ControlPolicy {
            key: Some(ControlKey::PrefixInputSource),
            drop_on_overflow: false,
        },
        Ok(ServerMessage::ReloadSoundConfig) => ControlPolicy {
            key: Some(ControlKey::ReloadConfig),
            drop_on_overflow: false,
        },
        Ok(
            ServerMessage::Notify { .. }
            | ServerMessage::Clipboard { .. }
            | ServerMessage::OpenUrl { .. },
        ) => ControlPolicy {
            key: None,
            drop_on_overflow: true,
        },
        _ => ControlPolicy {
            key: None,
            drop_on_overflow: false,
        },
    }
}

#[derive(Clone)]
pub(crate) struct QuicRenderSender {
    inner: Arc<QuicRenderSenderInner>,
}

impl std::fmt::Debug for QuicRenderSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicRenderSender")
            .field("busy", &self.inner.busy.load(Ordering::Relaxed))
            .finish()
    }
}

struct QuicRenderSenderInner {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    busy: AtomicBool,
    generation: AtomicU64,
    generation_tx: watch::Sender<u64>,
}

impl QuicRenderSender {
    fn new() -> (Self, mpsc::UnboundedReceiver<Vec<u8>>, watch::Receiver<u64>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (generation_tx, generation_rx) = watch::channel(1);
        (
            Self {
                inner: Arc::new(QuicRenderSenderInner {
                    tx,
                    busy: AtomicBool::new(false),
                    generation: AtomicU64::new(1),
                    generation_tx,
                }),
            },
            rx,
            generation_rx,
        )
    }

    pub(crate) fn try_send(
        &self,
        data: Vec<u8>,
    ) -> Result<(), std::sync::mpsc::TrySendError<Vec<u8>>> {
        if self
            .inner
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(std::sync::mpsc::TrySendError::Full(data));
        }
        if let Err(err) = self.inner.tx.send(data) {
            self.inner.busy.store(false, Ordering::Release);
            return Err(std::sync::mpsc::TrySendError::Disconnected(err.0));
        }
        Ok(())
    }

    pub(crate) fn reset_generation(&self) {
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner.generation_tx.send_replace(generation);
    }
}

async fn publish_control_output(mut stream: SendStream, queue: Arc<BoundedControlQueue>) {
    while let Some(control) = queue.recv().await {
        if stream.write_all(&control).await.is_err() {
            break;
        }
    }
    queue.close();
}

async fn publish_server_output(
    connection: Connection,
    mut render_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut generation_rx: watch::Receiver<u64>,
    render_sender: Arc<QuicRenderSenderInner>,
    connection_generation: u64,
    cached_resources: HashSet<[u8; REMOTE_QUIC_HASH_BYTES]>,
    client_id: u64,
    server_event_tx: mpsc::Sender<ServerEvent>,
) {
    let resource_limit = Arc::new(Semaphore::new(MAX_RESOURCE_TRANSFERS));
    let sent_resources = Arc::new(Mutex::new(cached_resources));
    let state_revision = AtomicU64::new(0);
    let mut render_stream: Option<SendStream> = None;
    let mut active_generation = *generation_rx.borrow_and_update();

    loop {
        tokio::select! {
            biased;
            changed = generation_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                active_generation = *generation_rx.borrow_and_update();
                if let Some(mut stream) = render_stream.take() {
                    let _ = stream.reset(VarInt::from_u32(REPLACED_CODE));
                }
            }
            render = render_rx.recv() => {
                let Some(render) = render else { break; };
                let result = publish_render(
                    &connection,
                    &mut render_stream,
                    &mut generation_rx,
                    connection_generation,
                    active_generation,
                    state_revision.fetch_add(1, Ordering::Relaxed) + 1,
                    render,
                    Arc::clone(&resource_limit),
                    Arc::clone(&sent_resources),
                ).await;
                if result == PublishRenderResult::GenerationChanged {
                    active_generation = *generation_rx.borrow_and_update();
                    if let Some(mut stream) = render_stream.take() {
                        let _ = stream.reset(VarInt::from_u32(REPLACED_CODE));
                    }
                }
                render_sender.busy.store(false, Ordering::Release);
                let _ = server_event_tx.send(ServerEvent::ClientWriterDrained { client_id }).await;
                if result == PublishRenderResult::Closed {
                    break;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishRenderResult {
    Sent,
    GenerationChanged,
    Closed,
}

async fn publish_render(
    connection: &Connection,
    render_stream: &mut Option<SendStream>,
    generation_rx: &mut watch::Receiver<u64>,
    connection_generation: u64,
    render_generation: u64,
    state_revision: u64,
    framed: Vec<u8>,
    resource_limit: Arc<Semaphore>,
    sent_resources: Arc<Mutex<HashSet<[u8; REMOTE_QUIC_HASH_BYTES]>>>,
) -> PublishRenderResult {
    let message = match protocol::read_message::<_, ServerMessage>(
        &mut framed.as_slice(),
        MAX_GRAPHICS_FRAME_SIZE,
    ) {
        Ok(message) => message,
        Err(err) => {
            warn!(err = %err, "failed to decode queued QUIC render");
            return PublishRenderResult::Sent;
        }
    };
    let ServerMessage::Terminal(mut frame) = message else {
        return PublishRenderResult::Sent;
    };
    let original = std::mem::take(&mut frame.bytes);
    let (text, graphics) = split_kitty_sequences(&original);
    let can_externalize = graphics.len() <= MAX_RESOURCE_REFS_PER_FRAME
        && graphics
            .iter()
            .all(|segment| segment.bytes.len() <= REMOTE_QUIC_MAX_RESOURCE_SIZE);
    let mut resources = Vec::new();
    if can_externalize {
        frame.bytes = text;
        for segment in graphics {
            let hash = hash_bytes(&segment.bytes);
            resources.push(RemoteQuicResourceRef {
                hash,
                text_offset: segment.text_offset,
            });
            let should_send = lock(&sent_resources).insert(hash);
            if should_send {
                spawn_resource_transfer(
                    connection.clone(),
                    connection_generation,
                    render_generation,
                    hash,
                    segment.bytes,
                    Arc::clone(&resource_limit),
                );
            }
        }
    } else {
        frame.bytes = original;
    }

    if render_stream.is_none() {
        let mut stream = match connection.open_uni().await {
            Ok(stream) => stream,
            Err(_) => return PublishRenderResult::Closed,
        };
        let header = RemoteQuicStreamHeader::Render {
            connection_generation,
            render_generation,
        };
        if write_async_message(&mut stream, &header, MAX_FRAME_SIZE)
            .await
            .is_err()
        {
            return PublishRenderResult::Closed;
        }
        *render_stream = Some(stream);
    }

    let record = RemoteQuicRenderRecord {
        connection_generation,
        render_generation,
        state_revision,
        frame,
        resources,
    };
    let Some(stream) = render_stream.as_mut() else {
        return PublishRenderResult::Closed;
    };
    tokio::select! {
        result = write_async_message(stream, &record, MAX_GRAPHICS_FRAME_SIZE) => {
            if result.is_ok() { PublishRenderResult::Sent } else { PublishRenderResult::Closed }
        }
        changed = generation_rx.changed() => {
            if changed.is_ok() { PublishRenderResult::GenerationChanged } else { PublishRenderResult::Closed }
        }
    }
}

fn spawn_resource_transfer(
    connection: Connection,
    connection_generation: u64,
    render_generation: u64,
    hash: [u8; REMOTE_QUIC_HASH_BYTES],
    bytes: Vec<u8>,
    limit: Arc<Semaphore>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = limit.acquire_owned().await else {
            return;
        };
        let Ok(mut stream) = connection.open_uni().await else {
            return;
        };
        let Ok(length) = u32::try_from(bytes.len()) else {
            return;
        };
        let header = RemoteQuicStreamHeader::Resource {
            connection_generation,
            render_generation,
            hash,
            length,
        };
        if write_async_message(&mut stream, &header, MAX_FRAME_SIZE)
            .await
            .is_err()
        {
            return;
        }
        if stream.write_all(&bytes).await.is_ok() {
            let _ = stream.finish();
        }
    });
}

struct GraphicsSegment {
    text_offset: u32,
    bytes: Vec<u8>,
}

fn split_kitty_sequences(bytes: &[u8]) -> (Vec<u8>, Vec<GraphicsSegment>) {
    const START: &[u8] = b"\x1b_G";
    const END: &[u8] = b"\x1b\\";
    let mut text = Vec::with_capacity(bytes.len());
    let mut graphics = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = find_subslice(&bytes[cursor..], START) {
        let start = cursor + relative_start;
        text.extend_from_slice(&bytes[cursor..start]);
        let payload_start = start + START.len();
        let Some(relative_end) = find_subslice(&bytes[payload_start..], END) else {
            text.extend_from_slice(&bytes[start..]);
            return (text, graphics);
        };
        let end = payload_start + relative_end + END.len();
        let Ok(text_offset) = u32::try_from(text.len()) else {
            return (bytes.to_vec(), Vec::new());
        };
        graphics.push(GraphicsSegment {
            text_offset,
            bytes: bytes[start..end].to_vec(),
        });
        cursor = end;
    }
    text.extend_from_slice(&bytes[cursor..]);
    (text, graphics)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_range_requires_ascending_unprivileged_ports() {
        assert_eq!(parse_port_range("48000-48100"), Ok((48000, 48100)));
        assert!(parse_port_range("80-81").is_err());
        assert!(parse_port_range("48100-48000").is_err());
        assert!(parse_port_range("invalid").is_err());
    }

    fn capability_fixture() -> (
        [u8; REMOTE_QUIC_ID_BYTES],
        HashMap<[u8; REMOTE_QUIC_HASH_BYTES], Capability>,
        RemoteQuicHello,
    ) {
        let server_instance_id = [2; REMOTE_QUIC_ID_BYTES];
        let capability_token = [3; REMOTE_QUIC_TOKEN_BYTES];
        let logical_client_id = [4; REMOTE_QUIC_ID_BYTES];
        let mut tokens = HashMap::new();
        tokens.insert(
            hash_bytes(&capability_token),
            Capability {
                session: "session-a".to_owned(),
                logical_client_id,
                expires_unix_seconds: 2_000,
                connection_generation: 0,
                active_connection: None,
                issued_order: 1,
            },
        );
        let hello = RemoteQuicHello {
            version: PROTOCOL_VERSION,
            server_instance_id,
            logical_client_id,
            capability_token,
            connection_generation: 1,
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            keybindings: crate::protocol::ClientKeybindings::Server,
            launch_mode: ClientLaunchMode::App,
            cached_resources: Vec::new(),
        };
        (server_instance_id, tokens, hello)
    }

    #[test]
    fn capability_validation_rejects_protocol_identity_and_revocation_failures() {
        let (server_instance_id, tokens, hello) = capability_fixture();
        assert_eq!(
            validate_capability(server_instance_id, &tokens, &hello, "session-a", 1_000),
            Ok(hash_bytes(&hello.capability_token))
        );

        let mut wrong_protocol = hello.clone();
        wrong_protocol.version = PROTOCOL_VERSION.saturating_sub(1);
        assert_eq!(
            validate_capability(
                server_instance_id,
                &tokens,
                &wrong_protocol,
                "session-a",
                1_000,
            ),
            Err(CapabilityValidationError::ProtocolMismatch {
                client: PROTOCOL_VERSION.saturating_sub(1),
            })
        );

        assert_eq!(
            validate_capability(
                [9; REMOTE_QUIC_ID_BYTES],
                &tokens,
                &hello,
                "session-a",
                1_000
            ),
            Err(CapabilityValidationError::ServerInstanceChanged)
        );
        assert_eq!(
            validate_capability(
                server_instance_id,
                &HashMap::new(),
                &hello,
                "session-a",
                1_000,
            ),
            Err(CapabilityValidationError::UnknownOrRevoked)
        );
    }

    #[test]
    fn capability_validation_rejects_expiry_cross_scope_and_stale_generation() {
        let (server_instance_id, mut tokens, hello) = capability_fixture();
        assert_eq!(
            validate_capability(server_instance_id, &tokens, &hello, "session-a", 2_000),
            Err(CapabilityValidationError::Expired)
        );

        let mut wrong_client = hello.clone();
        wrong_client.logical_client_id = [9; REMOTE_QUIC_ID_BYTES];
        assert_eq!(
            validate_capability(
                server_instance_id,
                &tokens,
                &wrong_client,
                "session-a",
                1_000,
            ),
            Err(CapabilityValidationError::DifferentLogicalClient)
        );
        assert_eq!(
            validate_capability(server_instance_id, &tokens, &hello, "session-b", 1_000),
            Err(CapabilityValidationError::DifferentSession)
        );

        tokens
            .get_mut(&hash_bytes(&hello.capability_token))
            .expect("fixture capability")
            .connection_generation = hello.connection_generation;
        assert_eq!(
            validate_capability(server_instance_id, &tokens, &hello, "session-a", 1_000),
            Err(CapabilityValidationError::StaleGeneration)
        );
    }

    #[tokio::test]
    async fn bootstrap_tokens_have_bounded_inventory_lifetime_and_session_scope() {
        let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind port probe");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);
        let config = crate::config::RemoteConfig {
            quic_port_range: format!("{port}-{port}"),
            quic_idle_timeout_seconds: 1,
            ..Default::default()
        };
        let (server_event_tx, _server_event_rx) = mpsc::channel(1);
        let server = RemoteQuicServer::start(&config, server_event_tx).expect("start QUIC server");
        let session = crate::session::active_name()
            .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned());
        assert!(server
            .bootstrap(RemoteBootstrapRequest {
                session: format!("{session}-wrong"),
                logical_client_id: [0; REMOTE_QUIC_ID_BYTES],
            })
            .is_err());

        let issued_at = unix_seconds();
        let first = server
            .bootstrap(RemoteBootstrapRequest {
                session: session.clone(),
                logical_client_id: [0; REMOTE_QUIC_ID_BYTES],
            })
            .expect("issue first capability");
        assert!(first.expires_unix_seconds >= issued_at + TOKEN_MIN_LIFETIME.as_secs());
        assert!(first.expires_unix_seconds <= issued_at + TOKEN_MIN_LIFETIME.as_secs() + 1);
        for index in 1..=MAX_TOKENS {
            server
                .bootstrap(RemoteBootstrapRequest {
                    session: session.clone(),
                    logical_client_id: [index as u8; REMOTE_QUIC_ID_BYTES],
                })
                .expect("issue bounded capability");
        }
        assert_eq!(lock(&server.state.tokens).len(), MAX_TOKENS);
    }

    #[test]
    fn kitty_sequences_are_split_without_touching_text() {
        let input = b"before\x1b_Gf=100,i=3;AAAA\x1b\\after\x1b_Ga=p,i=3\x1b\\";
        let (text, graphics) = split_kitty_sequences(input);
        assert_eq!(text, b"beforeafter");
        assert_eq!(graphics.len(), 2);
        assert_eq!(graphics[0].text_offset, 6);
        assert_eq!(graphics[0].bytes, b"\x1b_Gf=100,i=3;AAAA\x1b\\");
        assert_eq!(graphics[1].text_offset, 11);
        assert_eq!(graphics[1].bytes, b"\x1b_Ga=p,i=3\x1b\\");
    }

    #[tokio::test]
    async fn control_queue_is_bounded_and_coalesces_current_state() {
        let queue = BoundedControlQueue::default();
        for index in 0..200 {
            let mut data = Vec::new();
            protocol::write_message(
                &mut data,
                &ServerMessage::WindowTitle {
                    title: Some(format!("title-{index}")),
                },
            )
            .expect("serialize title");
            queue.send(data).expect("queue open");
        }
        let (items, bytes) = queue.bounds();
        assert_eq!(items, 1);
        assert!(bytes <= MAX_CONTROL_BYTES);
    }

    #[test]
    fn reliable_control_overflow_disconnects_instead_of_dropping_messages() {
        let queue = BoundedControlQueue::default();
        for index in 0..MAX_CONTROL_ITEMS {
            let mut data = Vec::new();
            protocol::write_message(
                &mut data,
                &ServerMessage::ServerShutdown {
                    reason: Some(format!("critical-{index}")),
                },
            )
            .expect("serialize critical control");
            queue.send(data).expect("reliable queue capacity");
        }
        let mut overflow = Vec::new();
        protocol::write_message(
            &mut overflow,
            &ServerMessage::ServerShutdown {
                reason: Some("must not disappear".to_owned()),
            },
        )
        .expect("serialize overflow control");
        assert!(queue.send(overflow).is_err());
        assert_eq!(queue.bounds().0, MAX_CONTROL_ITEMS);
    }

    #[test]
    fn resource_and_certificate_hashes_are_content_addressed() {
        assert_eq!(hash_bytes(b"same"), hash_bytes(b"same"));
        assert_ne!(hash_bytes(b"same"), hash_bytes(b"different"));
    }
}
