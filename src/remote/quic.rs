//! QUIC client used by the `herdr --remote` local proxy.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::sync::mpsc;
use tracing::debug;

use super::frame::{hash_bytes, lock, read_async_message, write_async_message};

use crate::protocol::{
    ClientKeybindings, ClientLaunchMode, ClientMessage, RemoteBootstrapRecord, RemoteQuicHello,
    RemoteQuicRenderRecord, RemoteQuicStreamHeader, RemoteTransportStatus, ServerMessage,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION, REMOTE_QUIC_ALPN,
    REMOTE_QUIC_HASH_BYTES, REMOTE_QUIC_MAX_RESOURCE_INVENTORY, REMOTE_QUIC_MAX_RESOURCE_SIZE,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(3);
const MAX_RESOURCE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESOURCE_CACHE_ENTRIES: usize = REMOTE_QUIC_MAX_RESOURCE_INVENTORY;
const CLIENT_STREAM_RECEIVE_WINDOW: u32 = 512 * 1024;
const CLIENT_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;
const CLIENT_SEND_WINDOW: u64 = 1024 * 1024;
const MAX_PENDING_RENDER_RECORDS: usize = 8;

pub(crate) type ProxyOutputSender = mpsc::Sender<ServerMessage>;

pub(crate) struct ConnectParams {
    pub(crate) bootstrap: RemoteBootstrapRecord,
    pub(crate) candidates: Vec<SocketAddr>,
    pub(crate) logical_client_id: [u8; crate::protocol::REMOTE_QUIC_ID_BYTES],
    pub(crate) connection_generation: u64,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
    pub(crate) keybindings: ClientKeybindings,
}

pub(crate) struct QuicSession {
    endpoint: Endpoint,
    connection: Connection,
    control_send: SendStream,
    control_recv: RecvStream,
    connection_generation: u64,
    resource_cache: Arc<Mutex<ResourceCache>>,
}

#[derive(Debug)]
pub(crate) enum SessionExit {
    RetryFresh(String),
    Rebootstrap(String),
    Detached,
}

#[derive(Default)]
pub(crate) struct ResourceCache {
    entries: HashMap<[u8; REMOTE_QUIC_HASH_BYTES], Vec<u8>>,
    order: VecDeque<[u8; REMOTE_QUIC_HASH_BYTES]>,
    bytes: usize,
}

impl ResourceCache {
    pub(crate) fn inventory(&self) -> Vec<[u8; REMOTE_QUIC_HASH_BYTES]> {
        self.order
            .iter()
            .rev()
            .take(REMOTE_QUIC_MAX_RESOURCE_INVENTORY)
            .copied()
            .collect()
    }

    fn get(&mut self, hash: &[u8; REMOTE_QUIC_HASH_BYTES]) -> Option<Vec<u8>> {
        let value = self.entries.get(hash)?.clone();
        self.touch(*hash);
        Some(value)
    }

    fn insert(&mut self, hash: [u8; REMOTE_QUIC_HASH_BYTES], bytes: Vec<u8>) {
        if bytes.len() > REMOTE_QUIC_MAX_RESOURCE_SIZE {
            return;
        }
        if let Some(previous) = self.entries.insert(hash, bytes) {
            self.bytes = self.bytes.saturating_sub(previous.len());
        }
        self.bytes = self
            .bytes
            .saturating_add(self.entries.get(&hash).map_or(0, Vec::len));
        self.touch(hash);
        while self.entries.len() > MAX_RESOURCE_CACHE_ENTRIES
            || self.bytes > MAX_RESOURCE_CACHE_BYTES
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.len());
            }
        }
    }

    fn touch(&mut self, hash: [u8; REMOTE_QUIC_HASH_BYTES]) {
        if let Some(index) = self.order.iter().position(|entry| *entry == hash) {
            self.order.remove(index);
        }
        self.order.push_back(hash);
    }
}

impl QuicSession {
    pub(crate) async fn connect(
        params: ConnectParams,
        resource_cache: Arc<Mutex<ResourceCache>>,
    ) -> Result<(Self, ServerMessage), String> {
        if params.bootstrap.version != PROTOCOL_VERSION {
            return Err(format!(
                "remote bootstrap protocol {}, local protocol {PROTOCOL_VERSION}",
                params.bootstrap.version
            ));
        }
        if params.candidates.is_empty() {
            return Err(
                "remote QUIC bootstrap returned no reachable address candidates".to_owned(),
            );
        }

        let mut errors = Vec::new();
        for candidate in params.candidates {
            let endpoint =
                make_client_endpoint(candidate.ip(), params.bootstrap.certificate_fingerprint)?;
            let connecting = endpoint
                .connect(candidate, "herdr")
                .map_err(|err| format!("failed to start QUIC connection to {candidate}: {err}"))?;
            let connection = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(err)) => {
                    errors.push(format!("{candidate}: {err}"));
                    endpoint.close(VarInt::from_u32(0), b"path attempt failed");
                    continue;
                }
                Err(_) => {
                    errors.push(format!("{candidate}: timed out"));
                    endpoint.close(VarInt::from_u32(0), b"path attempt timed out");
                    continue;
                }
            };
            let (mut control_send, mut control_recv) =
                tokio::time::timeout(CONTROL_HANDSHAKE_TIMEOUT, connection.open_bi())
                    .await
                    .map_err(|_| "timed out opening QUIC control stream".to_owned())?
                    .map_err(|err| format!("failed to open QUIC control stream: {err}"))?;
            let cached_resources = lock(&resource_cache).inventory();
            let hello = RemoteQuicHello {
                version: PROTOCOL_VERSION,
                server_instance_id: params.bootstrap.server_instance_id,
                logical_client_id: params.logical_client_id,
                capability_token: params.bootstrap.capability_token,
                connection_generation: params.connection_generation,
                cols: params.cols,
                rows: params.rows,
                cell_width_px: params.cell_width_px,
                cell_height_px: params.cell_height_px,
                keybindings: params.keybindings.clone(),
                launch_mode: ClientLaunchMode::App,
                cached_resources,
            };
            write_async_message(&mut control_send, &hello, MAX_FRAME_SIZE).await?;
            let welcome: ServerMessage = tokio::time::timeout(
                CONTROL_HANDSHAKE_TIMEOUT,
                read_async_message(&mut control_recv, MAX_FRAME_SIZE),
            )
            .await
            .map_err(|_| "timed out waiting for remote QUIC welcome".to_owned())??;
            match &welcome {
                ServerMessage::Welcome {
                    version,
                    error: None,
                    ..
                } if *version == PROTOCOL_VERSION => {}
                ServerMessage::Welcome {
                    version,
                    error: Some(error),
                    ..
                } => {
                    return Err(format!(
                        "remote QUIC server {version} rejected attach: {error}"
                    ));
                }
                _ => return Err("remote QUIC server sent an invalid welcome".to_owned()),
            }
            return Ok((
                Self {
                    endpoint,
                    connection,
                    control_send,
                    control_recv,
                    connection_generation: params.connection_generation,
                    resource_cache,
                },
                welcome,
            ));
        }
        Err(format!(
            "all remote QUIC paths failed: {}",
            errors.join("; ")
        ))
    }

    pub(crate) async fn run(
        mut self,
        mut input_rx: mpsc::Receiver<ClientMessage>,
        output: ProxyOutputSender,
        reconnecting: bool,
    ) -> SessionExit {
        let (control_event_tx, mut control_event_rx) = mpsc::channel(16);
        let mut control_recv = self.control_recv;
        tokio::spawn(async move {
            loop {
                match read_async_message::<ServerMessage>(
                    &mut control_recv,
                    MAX_GRAPHICS_FRAME_SIZE,
                )
                .await
                {
                    Ok(message) => {
                        if control_event_tx
                            .send(ControlEvent::Message(message))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = control_event_tx.send(ControlEvent::Closed(error)).await;
                        break;
                    }
                }
            }
        });

        let (stream_event_tx, mut stream_event_rx) = mpsc::channel(16);
        let connection_for_accept = self.connection.clone();
        tokio::spawn(async move {
            loop {
                match connection_for_accept.accept_uni().await {
                    Ok(stream) => {
                        let stream_event_tx = stream_event_tx.clone();
                        tokio::spawn(async move {
                            read_server_stream(stream, stream_event_tx).await;
                        });
                    }
                    Err(error) => {
                        let _ = stream_event_tx
                            .send(StreamEvent::Closed(error.to_string()))
                            .await;
                        break;
                    }
                }
            }
        });

        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut next_nonce = 1u64;
        let mut outstanding_ping: Option<(u64, Instant)> = None;
        let mut rebound = false;
        let mut render_generation = 0u64;
        let mut last_state_revision = 0u64;
        let mut expected_seq = 1u64;
        let mut connected_announced = !reconnecting;
        let mut pending_renders = VecDeque::<RemoteQuicRenderRecord>::new();

        loop {
            tokio::select! {
                input = input_rx.recv() => {
                    let Some(input) = input else {
                        return SessionExit::RetryFresh("local client input channel closed".to_owned());
                    };
                    let detached = matches!(input, ClientMessage::Detach);
                    match tokio::time::timeout(
                        HEARTBEAT_DEADLINE,
                        write_async_message(&mut self.control_send, &input, MAX_GRAPHICS_FRAME_SIZE),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => return SessionExit::RetryFresh(error),
                        Err(_) => {
                            return SessionExit::RetryFresh(
                                "remote QUIC control write timed out".to_owned(),
                            );
                        }
                    }
                    if detached {
                        let _ = self.control_send.finish();
                        return SessionExit::Detached;
                    }
                }
                event = control_event_rx.recv() => {
                    let Some(event) = event else {
                        return SessionExit::RetryFresh("remote control reader stopped".to_owned());
                    };
                    match event {
                        ControlEvent::Message(ServerMessage::RemotePong { nonce }) => {
                            if outstanding_ping.is_some_and(|(expected, _)| expected == nonce) {
                                outstanding_ping = None;
                                rebound = false;
                            }
                        }
                        ControlEvent::Message(ServerMessage::ClientDetached) => {
                            return SessionExit::Detached;
                        }
                        ControlEvent::Message(ServerMessage::ServerShutdown { reason }) => {
                            let detail = reason.unwrap_or_else(|| "remote server restarted".to_owned());
                            return SessionExit::Rebootstrap(detail);
                        }
                        ControlEvent::Message(message) => {
                            if output.send(message).await.is_err() {
                                return SessionExit::Detached;
                            }
                        }
                        ControlEvent::Closed(error) => {
                            return SessionExit::RetryFresh(error);
                        }
                    }
                }
                event = stream_event_rx.recv() => {
                    let Some(event) = event else {
                        return SessionExit::RetryFresh("remote stream reader stopped".to_owned());
                    };
                    match event {
                        StreamEvent::Render(record) => {
                            if record.connection_generation != self.connection_generation {
                                continue;
                            }
                            if record.render_generation < render_generation {
                                continue;
                            }
                            if record.state_revision <= last_state_revision {
                                continue;
                            }
                            if record.render_generation == render_generation
                                && last_state_revision != 0
                                && record.state_revision != last_state_revision.saturating_add(1)
                            {
                                let _ = write_async_message(
                                    &mut self.control_send,
                                    &ClientMessage::SyncRequest,
                                    MAX_FRAME_SIZE,
                                )
                                .await;
                                render_generation = render_generation.saturating_add(1);
                                expected_seq = 1;
                                pending_renders.clear();
                                continue;
                            }
                            if record.render_generation > render_generation {
                                if !record.frame.full || record.frame.seq != 1 {
                                    let _ = write_async_message(&mut self.control_send, &ClientMessage::SyncRequest, MAX_FRAME_SIZE).await;
                                    continue;
                                }
                                render_generation = record.render_generation;
                                expected_seq = 1;
                                pending_renders.clear();
                            }
                            if record.frame.seq != expected_seq {
                                let _ = write_async_message(&mut self.control_send, &ClientMessage::SyncRequest, MAX_FRAME_SIZE).await;
                                render_generation = render_generation.saturating_add(1);
                                expected_seq = 1;
                                continue;
                            }
                            last_state_revision = record.state_revision;
                            expected_seq = expected_seq.saturating_add(1);
                            pending_renders.push_back(record);
                            let pending_bytes = pending_renders
                                .iter()
                                .map(|pending| pending.frame.bytes.len())
                                .sum::<usize>();
                            if pending_renders.len() > MAX_PENDING_RENDER_RECORDS
                                || pending_bytes > MAX_GRAPHICS_FRAME_SIZE
                            {
                                return SessionExit::RetryFresh(
                                    "remote graphics resources did not arrive before the bounded render queue filled".to_owned(),
                                );
                            }
                            if flush_pending_renders(
                                &mut pending_renders,
                                &self.resource_cache,
                                &output,
                                &mut connected_announced,
                            )
                            .await
                            .is_err()
                            {
                                return SessionExit::Detached;
                            }
                        }
                        StreamEvent::Resource {
                            connection_generation,
                            hash,
                            bytes,
                        } => {
                            if connection_generation != self.connection_generation
                                || hash_bytes(&bytes) != hash
                            {
                                continue;
                            }
                            lock(&self.resource_cache).insert(hash, bytes);
                            if flush_pending_renders(
                                &mut pending_renders,
                                &self.resource_cache,
                                &output,
                                &mut connected_announced,
                            )
                            .await
                            .is_err()
                            {
                                return SessionExit::Detached;
                            }
                        }
                        StreamEvent::Closed(error) => {
                            if !pending_renders.is_empty() {
                                return SessionExit::RetryFresh(format!(
                                    "remote graphics resource stream failed: {error}"
                                ));
                            }
                            debug!(%error, "remote QUIC unidirectional stream closed");
                        }
                    }
                }
                _ = heartbeat.tick() => {
                    if let Some((_, sent_at)) = outstanding_ping {
                        if sent_at.elapsed() >= HEARTBEAT_DEADLINE {
                            while input_rx.try_recv().is_ok() {}
                            if rebound {
                                return SessionExit::RetryFresh("remote QUIC heartbeat timed out after path rebind".to_owned());
                            }
                            rebound = true;
                            connected_announced = false;
                            let _ = output.send(ServerMessage::TransportStatus {
                                status: RemoteTransportStatus::PathRecovering,
                                detail: Some("waiting for the remote path".to_owned()),
                            }).await;
                            if let Err(error) = rebind_endpoint(&self.endpoint, self.connection.remote_address().ip()) {
                                return SessionExit::RetryFresh(format!("failed to rebind QUIC path: {error}"));
                            }
                            let _ = write_async_message(&mut self.control_send, &ClientMessage::SyncRequest, MAX_FRAME_SIZE).await;
                            outstanding_ping = None;
                        }
                    }
                    if outstanding_ping.is_none() {
                        let nonce = next_nonce;
                        next_nonce = next_nonce.saturating_add(1);
                        if let Err(error) = write_async_message(
                            &mut self.control_send,
                            &ClientMessage::RemotePing { nonce },
                            MAX_FRAME_SIZE,
                        ).await {
                            return SessionExit::RetryFresh(error);
                        }
                        outstanding_ping = Some((nonce, Instant::now()));
                    }
                }
                error = self.connection.closed() => {
                    let detail = error.to_string();
                    if detail.contains("capability") || detail.contains("server instance") || detail.contains("protocol") {
                        return SessionExit::Rebootstrap(detail);
                    }
                    return SessionExit::RetryFresh(detail);
                }
            }
        }
    }
}

async fn flush_pending_renders(
    pending: &mut VecDeque<RemoteQuicRenderRecord>,
    cache: &Arc<Mutex<ResourceCache>>,
    output: &ProxyOutputSender,
    connected_announced: &mut bool,
) -> Result<(), ()> {
    loop {
        let Some(record) = pending.front() else {
            return Ok(());
        };
        let graphics = {
            let mut cache = lock(cache);
            let mut graphics = Vec::with_capacity(record.resources.len());
            for resource in &record.resources {
                let Some(bytes) = cache.get(&resource.hash) else {
                    return Ok(());
                };
                graphics.push(bytes);
            }
            graphics
        };
        let Some(record) = pending.pop_front() else {
            return Ok(());
        };
        let frame = reconstruct_frame(record, graphics)?;
        if output.send(ServerMessage::Terminal(frame)).await.is_err() {
            return Err(());
        }
        if !*connected_announced {
            *connected_announced = true;
            if output
                .send(ServerMessage::TransportStatus {
                    status: RemoteTransportStatus::Connected,
                    detail: None,
                })
                .await
                .is_err()
            {
                return Err(());
            }
        }
    }
}

pub(super) fn reconstruct_frame(
    mut record: RemoteQuicRenderRecord,
    graphics: Vec<Vec<u8>>,
) -> Result<crate::protocol::TerminalFrame, ()> {
    if record.resources.is_empty() {
        return Ok(record.frame);
    }
    let stripped = std::mem::take(&mut record.frame.bytes);
    let graphics_bytes = graphics.iter().map(Vec::len).sum::<usize>();
    let mut reconstructed = Vec::with_capacity(stripped.len().saturating_add(graphics_bytes));
    let mut cursor = 0usize;
    for (resource, bytes) in record.resources.iter().zip(graphics) {
        let offset = resource.text_offset as usize;
        if offset < cursor || offset > stripped.len() {
            return Err(());
        }
        reconstructed.extend_from_slice(&stripped[cursor..offset]);
        reconstructed.extend_from_slice(&bytes);
        cursor = offset;
    }
    reconstructed.extend_from_slice(&stripped[cursor..]);
    record.frame.bytes = reconstructed;
    Ok(record.frame)
}

#[derive(Debug)]
enum ControlEvent {
    Message(ServerMessage),
    Closed(String),
}

#[derive(Debug)]
enum StreamEvent {
    Render(RemoteQuicRenderRecord),
    Resource {
        connection_generation: u64,
        hash: [u8; REMOTE_QUIC_HASH_BYTES],
        bytes: Vec<u8>,
    },
    Closed(String),
}

async fn read_server_stream(mut stream: RecvStream, events: mpsc::Sender<StreamEvent>) {
    let header: RemoteQuicStreamHeader = match read_async_message(&mut stream, MAX_FRAME_SIZE).await
    {
        Ok(header) => header,
        Err(error) => {
            let _ = events.send(StreamEvent::Closed(error)).await;
            return;
        }
    };
    match header {
        RemoteQuicStreamHeader::Render { .. } => loop {
            match read_async_message::<RemoteQuicRenderRecord>(&mut stream, MAX_GRAPHICS_FRAME_SIZE)
                .await
            {
                Ok(record) => {
                    if events.send(StreamEvent::Render(record)).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = events.send(StreamEvent::Closed(error)).await;
                    return;
                }
            }
        },
        RemoteQuicStreamHeader::Resource {
            connection_generation,
            render_generation: _,
            hash,
            length,
        } => {
            let length = length as usize;
            if length > REMOTE_QUIC_MAX_RESOURCE_SIZE {
                let _ = stream.stop(VarInt::from_u32(1));
                let _ = events
                    .send(StreamEvent::Closed(format!(
                        "remote resource size {length} exceeds maximum {REMOTE_QUIC_MAX_RESOURCE_SIZE}"
                    )))
                    .await;
                return;
            }
            let mut bytes = vec![0u8; length];
            if let Err(error) = stream.read_exact(&mut bytes).await {
                let _ = events.send(StreamEvent::Closed(error.to_string())).await;
                return;
            }
            let _ = events
                .send(StreamEvent::Resource {
                    connection_generation,
                    hash,
                    bytes,
                })
                .await;
        }
    }
}

fn make_client_endpoint(
    remote_ip: IpAddr,
    fingerprint: [u8; REMOTE_QUIC_HASH_BYTES],
) -> Result<Endpoint, String> {
    let bind_address = match remote_ip {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let mut endpoint = Endpoint::client(bind_address)
        .map_err(|err| format!("failed to bind local QUIC socket: {err}"))?;
    endpoint.set_default_client_config(client_config(fingerprint)?);
    Ok(endpoint)
}

fn client_config(fingerprint: [u8; REMOTE_QUIC_HASH_BYTES]) -> Result<quinn::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(FingerprintVerifier {
        fingerprint,
        provider: Arc::clone(&provider),
    });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| format!("failed to enable TLS 1.3: {err}"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![REMOTE_QUIC_ALPN.to_vec()];
    tls.enable_early_data = false;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|err| format!("failed to configure QUIC TLS: {err}"))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_idle_timeout(Some(
            Duration::from_secs(7 * 24 * 60 * 60)
                .try_into()
                .map_err(|_| "QUIC idle timeout is too large".to_owned())?,
        ))
        .keep_alive_interval(Some(Duration::from_secs(15)))
        .max_concurrent_bidi_streams(VarInt::from_u32(0))
        .max_concurrent_uni_streams(VarInt::from_u32(4))
        .stream_receive_window(VarInt::from_u32(CLIENT_STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(CLIENT_RECEIVE_WINDOW))
        .send_window(CLIENT_SEND_WINDOW);
    config.transport_config(Arc::new(transport));
    Ok(config)
}

#[derive(Debug)]
struct FingerprintVerifier {
    fingerprint: [u8; REMOTE_QUIC_HASH_BYTES],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if hash_bytes(end_entity.as_ref()) == self.fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "remote QUIC certificate fingerprint mismatch".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn rebind_endpoint(endpoint: &Endpoint, remote_ip: IpAddr) -> Result<(), String> {
    let bind_address = match remote_ip {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let socket = UdpSocket::bind(bind_address)
        .map_err(|err| format!("failed to bind replacement UDP socket: {err}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|err| format!("failed to configure replacement UDP socket: {err}"))?;
    endpoint
        .rebind(socket)
        .map_err(|err| format!("failed to migrate QUIC endpoint: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::danger::ServerCertVerifier as _;

    #[test]
    fn pinned_verifier_rejects_a_different_certificate() {
        let expected = hash_bytes(b"expected certificate");
        let verifier = FingerprintVerifier {
            fingerprint: expected,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        };
        let wrong = CertificateDer::from(vec![1, 2, 3]);
        assert!(verifier
            .verify_server_cert(
                &wrong,
                &[],
                &ServerName::try_from("herdr").expect("server name"),
                &[],
                UnixTime::since_unix_epoch(Duration::ZERO),
            )
            .is_err());
    }

    #[test]
    fn resource_cache_is_bounded_and_content_addressed() {
        let mut cache = ResourceCache::default();
        for value in 0..300u16 {
            let bytes = vec![(value & 0xff) as u8; 4];
            cache.insert(hash_bytes(&value.to_le_bytes()), bytes);
        }
        assert!(cache.entries.len() <= MAX_RESOURCE_CACHE_ENTRIES);
        assert!(cache.bytes <= MAX_RESOURCE_CACHE_BYTES);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn quic_round_trip_reconstructs_graphics_before_publishing_frame() {
        let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind port probe");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);

        let config = crate::config::RemoteConfig {
            quic_port_range: format!("{port}-{port}"),
            ..Default::default()
        };
        let (server_event_tx, mut server_event_rx) = mpsc::channel(8);
        let server = crate::server::remote_quic::RemoteQuicServer::start(&config, server_event_tx)
            .expect("start QUIC server");
        let logical_client_id = [7; crate::protocol::REMOTE_QUIC_ID_BYTES];
        let bootstrap = server
            .bootstrap(crate::protocol::RemoteBootstrapRequest {
                session: crate::session::active_name()
                    .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned()),
                logical_client_id,
            })
            .expect("bootstrap QUIC client");
        let cache = Arc::new(Mutex::new(ResourceCache::default()));
        let (session, welcome) = QuicSession::connect(
            ConnectParams {
                bootstrap,
                candidates: vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))],
                logical_client_id,
                connection_generation: 1,
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                keybindings: ClientKeybindings::Server,
            },
            cache,
        )
        .await
        .expect("connect QUIC client");
        assert!(matches!(
            welcome,
            ServerMessage::Welcome { error: None, .. }
        ));

        let writer = match tokio::time::timeout(Duration::from_secs(2), server_event_rx.recv())
            .await
            .expect("client connection event timeout")
            .expect("client connection event")
        {
            crate::server::client_transport::ServerEvent::ClientConnected { writer, .. } => writer,
            _ => panic!("expected QUIC client connection event"),
        };
        let (_input_tx, input_rx) = mpsc::channel(4);
        let (output_tx, mut output_rx) = mpsc::channel(4);
        let session_task = tokio::spawn(session.run(input_rx, output_tx, false));

        let expected = b"before\x1b_Gf=100,i=3;AAAA\x1b\\middle\x1b_Ga=p,i=3\x1b\\after".to_vec();
        let mut framed = Vec::new();
        crate::protocol::write_message(
            &mut framed,
            &ServerMessage::Terminal(crate::protocol::TerminalFrame {
                seq: 1,
                width: 80,
                height: 24,
                full: true,
                bytes: expected.clone(),
            }),
        )
        .expect("frame terminal message");
        writer.render.try_send(framed).expect("queue QUIC render");

        let delivered = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .expect("render delivery timeout")
            .expect("render output");
        let ServerMessage::Terminal(frame) = delivered else {
            panic!("expected terminal frame");
        };
        assert_eq!(frame.bytes, expected);

        let mut framed = Vec::new();
        crate::protocol::write_message(&mut framed, &ServerMessage::ClientDetached)
            .expect("frame server detach");
        writer.control.send(framed).expect("send server detach");
        assert!(matches!(
            session_task.await.expect("session task"),
            SessionExit::Detached
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_render_stream_does_not_block_control_or_input() {
        let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind port probe");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);
        let config = crate::config::RemoteConfig {
            quic_port_range: format!("{port}-{port}"),
            ..Default::default()
        };
        let (server_event_tx, mut server_event_rx) = mpsc::channel(16);
        let server = crate::server::remote_quic::RemoteQuicServer::start(&config, server_event_tx)
            .expect("start QUIC server");
        let logical_client_id = [8; crate::protocol::REMOTE_QUIC_ID_BYTES];
        let bootstrap = server
            .bootstrap(crate::protocol::RemoteBootstrapRequest {
                session: crate::session::active_name()
                    .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned()),
                logical_client_id,
            })
            .expect("bootstrap QUIC client");
        let endpoint = make_client_endpoint(
            Ipv4Addr::LOCALHOST.into(),
            bootstrap.certificate_fingerprint,
        )
        .expect("create QUIC client endpoint");
        let connection = endpoint
            .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), "herdr")
            .expect("start QUIC connection")
            .await
            .expect("connect QUIC client");
        let (mut control_send, mut control_recv) =
            connection.open_bi().await.expect("open control stream");
        write_async_message(
            &mut control_send,
            &RemoteQuicHello {
                version: PROTOCOL_VERSION,
                server_instance_id: bootstrap.server_instance_id,
                logical_client_id,
                capability_token: bootstrap.capability_token,
                connection_generation: 1,
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::App,
                cached_resources: Vec::new(),
            },
            MAX_FRAME_SIZE,
        )
        .await
        .expect("send QUIC hello");
        assert!(matches!(
            read_async_message::<ServerMessage>(&mut control_recv, MAX_GRAPHICS_FRAME_SIZE)
                .await
                .expect("read QUIC welcome"),
            ServerMessage::Welcome { error: None, .. }
        ));
        let writer = match tokio::time::timeout(Duration::from_secs(2), server_event_rx.recv())
            .await
            .expect("client connection event timeout")
            .expect("client connection event")
        {
            crate::server::client_transport::ServerEvent::ClientConnected { writer, .. } => writer,
            _ => panic!("expected QUIC client connection event"),
        };

        let large_frame = vec![b'x'; 2 * 1024 * 1024];
        writer
            .render
            .try_send(framed_terminal(1, true, large_frame.clone()))
            .expect("queue blocking render");
        let mut render_stream =
            tokio::time::timeout(Duration::from_secs(2), connection.accept_uni())
                .await
                .expect("render stream open timeout")
                .expect("accept render stream");
        assert!(matches!(
            read_async_message::<RemoteQuicStreamHeader>(
                &mut render_stream,
                MAX_GRAPHICS_FRAME_SIZE
            )
            .await
            .expect("read render stream header"),
            RemoteQuicStreamHeader::Render { .. }
        ));
        assert!(matches!(
            writer
                .render
                .try_send(framed_terminal(2, false, b"newer".to_vec())),
            Err(std::sync::mpsc::TrySendError::Full(_))
        ));

        let mut control = Vec::new();
        crate::protocol::write_message(
            &mut control,
            &ServerMessage::WindowTitle {
                title: Some("control-progress".to_owned()),
            },
        )
        .expect("frame control message");
        writer.control.send(control).expect("queue control message");
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                read_async_message::<ServerMessage>(&mut control_recv, MAX_FRAME_SIZE),
            )
            .await
            .expect("control progress timeout")
            .expect("read control message"),
            ServerMessage::WindowTitle { title: Some(title) } if title == "control-progress"
        ));

        write_async_message(
            &mut control_send,
            &ClientMessage::Input {
                data: b"input-progress".to_vec(),
            },
            MAX_FRAME_SIZE,
        )
        .await
        .expect("send input while render is stalled");
        let input = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match server_event_rx.recv().await {
                    Some(crate::server::client_transport::ServerEvent::ClientInput {
                        data,
                        ..
                    }) => break data,
                    Some(_) => {}
                    None => panic!("server event channel closed"),
                }
            }
        })
        .await
        .expect("input progress timeout");
        assert_eq!(input, b"input-progress");

        let record = tokio::time::timeout(
            Duration::from_secs(2),
            read_async_message::<RemoteQuicRenderRecord>(
                &mut render_stream,
                MAX_GRAPHICS_FRAME_SIZE,
            ),
        )
        .await
        .expect("render resume timeout")
        .expect("read resumed render");
        assert_eq!(record.frame.bytes, large_frame);
        write_async_message(&mut control_send, &ClientMessage::Detach, MAX_FRAME_SIZE)
            .await
            .expect("send detach");
    }

    #[derive(Clone, Copy)]
    enum NetworkMode {
        Online,
        Degraded,
        Blackhole,
    }

    async fn run_udp_proxy(
        socket: Arc<tokio::net::UdpSocket>,
        server: SocketAddr,
        mode: tokio::sync::watch::Receiver<NetworkMode>,
    ) {
        let mut client = None;
        let mut packet_index = 0u64;
        let mut buffer = vec![0u8; 65_535];
        loop {
            let Ok((length, source)) = socket.recv_from(&mut buffer).await else {
                return;
            };
            let target = if source == server {
                let Some(client) = client else {
                    continue;
                };
                client
            } else {
                client = Some(source);
                server
            };
            packet_index = packet_index.saturating_add(1);
            let current_mode = *mode.borrow();
            if matches!(current_mode, NetworkMode::Blackhole)
                || matches!(current_mode, NetworkMode::Degraded) && packet_index.is_multiple_of(11)
            {
                continue;
            }
            let packet = buffer[..length].to_vec();
            let socket = Arc::clone(&socket);
            let delay = if matches!(current_mode, NetworkMode::Degraded)
                && packet_index.is_multiple_of(4)
            {
                Duration::from_millis(50)
            } else {
                Duration::ZERO
            };
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = socket.send_to(&packet, target).await;
            });
        }
    }

    fn framed_terminal(seq: u64, full: bool, bytes: Vec<u8>) -> Vec<u8> {
        let mut framed = Vec::new();
        crate::protocol::write_message(
            &mut framed,
            &ServerMessage::Terminal(crate::protocol::TerminalFrame {
                seq,
                width: 80,
                height: 24,
                full,
                bytes,
            }),
        )
        .expect("frame terminal message");
        framed
    }

    type CanonicalCell = (
        String,
        crate::ghostty::CellBasicData,
        Option<crate::ghostty::RgbColor>,
        Option<crate::ghostty::RgbColor>,
    );

    #[derive(Debug, PartialEq, Eq)]
    struct CanonicalTerminalState {
        active_screen: crate::ghostty::ActiveScreen,
        cursor: Option<crate::ghostty::CursorViewport>,
        cursor_visible: bool,
        cursor_blinking: bool,
        cursor_style: crate::ghostty::CursorVisualStyle,
        colors: crate::ghostty::RenderColors,
        screen_text: Vec<crate::ghostty::ScreenTextRow>,
        hyperlinks: Vec<Vec<Option<String>>>,
        modes: [bool; 5],
        rows: Vec<Vec<CanonicalCell>>,
    }

    fn canonical_terminal_state(bytes: &[u8]) -> CanonicalTerminalState {
        let mut terminal =
            crate::ghostty::Terminal::new(80, 24, 1_000_000).expect("create canonical terminal");
        terminal.write(bytes);
        let mut render_state =
            crate::ghostty::RenderState::new().expect("create canonical render state");
        render_state
            .update(&terminal)
            .expect("update canonical render state");
        let mut row_iterator =
            crate::ghostty::RowIterator::new().expect("create canonical row iterator");
        let mut row_cells = crate::ghostty::RowCells::new().expect("create canonical row cells");
        let mut rows = Vec::new();
        {
            let mut row = render_state
                .populate_row_iterator(&mut row_iterator)
                .expect("populate canonical rows");
            while row.next() {
                let mut cells = row
                    .populate_cells(&mut row_cells)
                    .expect("populate canonical cells");
                let mut snapshot = Vec::new();
                while cells.next() {
                    snapshot.push((
                        cells.grapheme_text().expect("read canonical grapheme"),
                        cells.basic_data().expect("read canonical cell"),
                        cells.fg_color().expect("read canonical foreground"),
                        cells.bg_color().expect("read canonical background"),
                    ));
                }
                rows.push(snapshot);
            }
        }
        let hyperlinks = (0..24u32)
            .map(|y| {
                (0..80u16)
                    .map(|x| {
                        terminal
                            .viewport_hyperlink_uri(x, y)
                            .expect("read canonical hyperlink")
                    })
                    .collect()
            })
            .collect();
        CanonicalTerminalState {
            active_screen: terminal.active_screen().expect("read active screen"),
            cursor: render_state
                .cursor_viewport()
                .expect("read canonical cursor"),
            cursor_visible: render_state
                .cursor_visible()
                .expect("read canonical cursor visibility"),
            cursor_blinking: render_state
                .cursor_blinking()
                .expect("read canonical cursor blinking"),
            cursor_style: render_state
                .cursor_visual_style()
                .expect("read canonical cursor style"),
            colors: render_state.colors().expect("read canonical colors"),
            screen_text: terminal
                .screen_text_rows()
                .expect("read canonical screen text"),
            hyperlinks,
            modes: [
                terminal
                    .mode_get(crate::ghostty::MODE_APPLICATION_CURSOR_KEYS)
                    .expect("read application cursor mode"),
                terminal
                    .mode_get(crate::ghostty::MODE_FOCUS_EVENT)
                    .expect("read focus mode"),
                terminal
                    .mode_get(crate::ghostty::MODE_MOUSE_SGR)
                    .expect("read mouse mode"),
                terminal
                    .mode_get(crate::ghostty::MODE_BRACKETED_PASTE)
                    .expect("read bracketed paste mode"),
                terminal
                    .mouse_tracking_enabled()
                    .expect("read mouse tracking"),
            ],
            rows,
        }
    }

    fn assert_recovered_terminal_matches_perfect_path(
        initial: &[u8],
        degraded: &[u8],
        recovered: &[u8],
    ) {
        let mut interrupted = Vec::with_capacity(
            initial
                .len()
                .saturating_add(degraded.len())
                .saturating_add(recovered.len()),
        );
        interrupted.extend_from_slice(initial);
        interrupted.extend_from_slice(degraded);
        interrupted.extend_from_slice(recovered);
        assert_eq!(
            canonical_terminal_state(&interrupted),
            canonical_terminal_state(recovered),
            "recovered terminal state must equal an uninterrupted authoritative redraw"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn quic_survives_loss_delay_and_blackhole_with_full_redraw_recovery() {
        let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind port probe");
        let server_port = probe.local_addr().expect("probe address").port();
        drop(probe);
        let config = crate::config::RemoteConfig {
            quic_port_range: format!("{server_port}-{server_port}"),
            ..Default::default()
        };
        let (server_event_tx, mut server_event_rx) = mpsc::channel(16);
        let server = crate::server::remote_quic::RemoteQuicServer::start(&config, server_event_tx)
            .expect("start QUIC server");

        let proxy_socket = Arc::new(
            tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind UDP proxy"),
        );
        let proxy_address = proxy_socket.local_addr().expect("proxy address");
        let (mode_tx, mode_rx) = tokio::sync::watch::channel(NetworkMode::Online);
        let proxy_task = tokio::spawn(run_udp_proxy(
            Arc::clone(&proxy_socket),
            SocketAddr::from((Ipv4Addr::LOCALHOST, server_port)),
            mode_rx,
        ));

        let logical_client_id = [9; crate::protocol::REMOTE_QUIC_ID_BYTES];
        let bootstrap = server
            .bootstrap(crate::protocol::RemoteBootstrapRequest {
                session: crate::session::active_name()
                    .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned()),
                logical_client_id,
            })
            .expect("bootstrap QUIC client");
        let (session, _) = QuicSession::connect(
            ConnectParams {
                bootstrap,
                candidates: vec![proxy_address],
                logical_client_id,
                connection_generation: 1,
                cols: 80,
                rows: 24,
                cell_width_px: 8,
                cell_height_px: 16,
                keybindings: ClientKeybindings::Server,
            },
            Arc::new(Mutex::new(ResourceCache::default())),
        )
        .await
        .expect("connect through UDP proxy");
        let writer = match tokio::time::timeout(Duration::from_secs(2), server_event_rx.recv())
            .await
            .expect("client event timeout")
            .expect("client event")
        {
            crate::server::client_transport::ServerEvent::ClientConnected { writer, .. } => writer,
            _ => panic!("expected client connection"),
        };
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let mut session_task = tokio::spawn(session.run(input_rx, output_tx, false));

        writer
            .render
            .try_send(framed_terminal(1, true, b"initial".to_vec()))
            .expect("send initial frame");
        let initial = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .expect("initial frame timeout")
            .expect("initial frame");
        assert!(matches!(
            initial,
            ServerMessage::Terminal(crate::protocol::TerminalFrame { bytes, .. })
                if bytes == b"initial"
        ));

        mode_tx
            .send(NetworkMode::Degraded)
            .expect("enable degraded network");
        let graphics = vec![b'A'; 64 * 1024];
        let mut degraded_frame = b"before\x1b_Gf=100;".to_vec();
        degraded_frame.extend_from_slice(&graphics);
        degraded_frame.extend_from_slice(b"\x1b\\after");
        writer
            .render
            .try_send(framed_terminal(2, false, degraded_frame.clone()))
            .expect("send degraded frame");
        let degraded = tokio::time::timeout(Duration::from_secs(12), output_rx.recv())
            .await
            .expect("degraded frame timeout")
            .expect("degraded frame");
        assert!(matches!(
            degraded,
            ServerMessage::Terminal(crate::protocol::TerminalFrame { bytes, .. })
                if bytes == degraded_frame
        ));

        mode_tx
            .send(NetworkMode::Blackhole)
            .expect("enable blackhole");
        let status = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if let Some(ServerMessage::TransportStatus { status, .. }) = output_rx.recv().await
                {
                    if status == RemoteTransportStatus::PathRecovering {
                        break status;
                    }
                }
            }
        })
        .await
        .expect("path recovery status timeout");
        assert_eq!(status, RemoteTransportStatus::PathRecovering);
        mode_tx.send(NetworkMode::Online).expect("restore network");

        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match server_event_rx.recv().await {
                    Some(crate::server::client_transport::ServerEvent::ClientSyncRequest {
                        ..
                    }) => {
                        break;
                    }
                    Some(_) => {}
                    None => panic!("server event channel closed"),
                }
            }
        })
        .await
        .expect("full redraw request timeout");
        let canonical_frame = concat!(
            "\x1b[?1049h\x1b[?2026h\x1b[2J\x1b[H",
            "plain e\u{301} 界 👩‍💻\r\n",
            "\x1b[1;3;9;4:3;38;2;12;34;56;48;5;17mstyled\x1b[0m\r\n",
            "\x1b]8;;https://example.test/item\x1b\\Link\x1b]8;;\x1b\\",
            "\x1b[5;1HabcXYZ\x1b[5;4H\x1b[3P\x1b[2@++",
            "\x1b[6;1Hgarbage\x1b[2Kfinal-row",
            "\x1b[7;78Hwrap界",
            "\x1b[?1h\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b[?2004h",
            "\x1b[10;20H\x1b[5 q\x1b[?25l\x1b[?2026l",
        )
        .as_bytes()
        .to_vec();
        writer.render.reset_generation();
        writer
            .render
            .try_send(framed_terminal(1, true, canonical_frame.clone()))
            .expect("send recovery full frame");

        let mut saw_final = false;
        let mut saw_connected = false;
        tokio::time::timeout(Duration::from_secs(8), async {
            while !saw_final || !saw_connected {
                match output_rx.recv().await {
                    Some(ServerMessage::Terminal(frame)) if frame.bytes == canonical_frame => {
                        saw_final = true;
                    }
                    Some(ServerMessage::TransportStatus {
                        status: RemoteTransportStatus::Connected,
                        ..
                    }) => saw_connected = true,
                    Some(_) => {}
                    None => {
                        let exit = (&mut session_task).await.expect("failed session task");
                        panic!("proxy output closed during recovery: {exit:?}");
                    }
                }
            }
        })
        .await
        .expect("canonical recovery timeout");
        assert_recovered_terminal_matches_perfect_path(
            b"initial",
            &degraded_frame,
            &canonical_frame,
        );

        input_tx.send(ClientMessage::Detach).await.expect("detach");
        assert!(matches!(
            session_task.await.expect("session task"),
            SessionExit::Detached
        ));
        proxy_task.abort();
    }
}
