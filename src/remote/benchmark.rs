//! Explicit network benchmark for the Terminal-ANSI QUIC path.
//!
//! Run with `cargo test terminal_ansi_3g_benchmark -- --ignored --nocapture`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use super::quic::{reconstruct_frame, ConnectParams, QuicSession, ResourceCache, SessionExit};
use crate::protocol::{
    ClientKeybindings, ClientMessage, RemoteQuicRenderRecord, RemoteQuicResourceRef,
    RemoteTransportStatus, ServerMessage, TerminalFrame,
};
use crate::server::client_transport::ServerEvent;

const SOURCE_FRAMES: u64 = 180;
const INPUT_SAMPLES: u64 = 20;
const DOWN_BYTES_PER_SECOND: f64 = 200_000.0;
const UP_BYTES_PER_SECOND: f64 = 93_750.0;

#[derive(Clone, Copy)]
enum NetworkMode {
    Online,
    ThreeG,
    Blackhole,
}

async fn run_udp_proxy(
    socket: Arc<tokio::net::UdpSocket>,
    server: SocketAddr,
    mode: watch::Receiver<NetworkMode>,
) {
    let mut client = None;
    let mut packet_index = 0u64;
    let mut buffer = vec![0u8; 65_535];
    let mut next_upstream = tokio::time::Instant::now();
    let mut next_downstream = tokio::time::Instant::now();
    loop {
        let Ok((length, source)) = socket.recv_from(&mut buffer).await else {
            return;
        };
        let from_server = source == server;
        let target = if from_server {
            let Some(client) = client else { continue };
            client
        } else {
            client = Some(source);
            server
        };
        packet_index = packet_index.saturating_add(1);
        let current_mode = *mode.borrow();
        if matches!(current_mode, NetworkMode::Blackhole)
            || matches!(current_mode, NetworkMode::ThreeG) && packet_index.is_multiple_of(101)
        {
            continue;
        }
        let packet = buffer[..length].to_vec();
        let socket = Arc::clone(&socket);
        let delay = if matches!(current_mode, NetworkMode::ThreeG) {
            let rate = if from_server {
                DOWN_BYTES_PER_SECOND
            } else {
                UP_BYTES_PER_SECOND
            };
            let serialization = Duration::from_secs_f64(packet.len() as f64 / rate);
            let next_delivery = if from_server {
                &mut next_downstream
            } else {
                &mut next_upstream
            };
            let now = tokio::time::Instant::now();
            *next_delivery = (*next_delivery).max(now) + serialization;
            let jitter_ms = packet_index.wrapping_mul(17) % 41;
            next_delivery
                .saturating_duration_since(now)
                .saturating_add(Duration::from_millis(130 + jitter_ms))
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
        &ServerMessage::Terminal(TerminalFrame {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit network performance benchmark"]
async fn terminal_ansi_3g_benchmark() {
    let probe = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind port probe");
    let server_port = probe.local_addr().expect("probe address").port();
    drop(probe);
    let config = crate::config::RemoteConfig {
        quic_port_range: format!("{server_port}-{server_port}"),
        ..Default::default()
    };
    let (server_event_tx, mut server_event_rx) = mpsc::channel(64);
    let server = crate::server::remote_quic::RemoteQuicServer::start(&config, server_event_tx)
        .expect("start QUIC server");

    let proxy_socket = Arc::new(
        tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind UDP proxy"),
    );
    let proxy_address = proxy_socket.local_addr().expect("proxy address");
    let (mode_tx, mode_rx) = watch::channel(NetworkMode::Online);
    let proxy_task = tokio::spawn(run_udp_proxy(
        Arc::clone(&proxy_socket),
        SocketAddr::from((Ipv4Addr::LOCALHOST, server_port)),
        mode_rx,
    ));

    let logical_client_id = [13; crate::protocol::REMOTE_QUIC_ID_BYTES];
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
    let writer = match server_event_rx.recv().await.expect("client event") {
        ServerEvent::ClientConnected { writer, .. } => writer,
        _ => panic!("expected client connection"),
    };

    let (forwarded_event_tx, mut forwarded_event_rx) = mpsc::unbounded_channel();
    let event_forwarder = tokio::spawn(async move {
        while let Some(event) = server_event_rx.recv().await {
            if forwarded_event_tx.send(event).is_err() {
                break;
            }
        }
    });
    let (input_tx, input_rx) = mpsc::channel(16);
    let (output_tx, mut output_rx) = mpsc::channel(16);
    let session_task = tokio::spawn(session.run(input_rx, output_tx, false));
    let (application_tx, mut application_rx) = mpsc::unbounded_channel();
    let output_forwarder = tokio::spawn(async move {
        while let Some(message) = output_rx.recv().await {
            if application_tx.send(message).is_err() {
                break;
            }
        }
    });
    mode_tx
        .send(NetworkMode::ThreeG)
        .expect("enable 3G profile");

    let producer_writer = writer.clone();
    let producer = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_nanos(16_666_667));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let source_started = Instant::now();
        let mut accepted = 0u64;
        let mut encoded_bytes = 0usize;
        for source_index in 0..SOURCE_FRAMES {
            interval.tick().await;
            let seq = accepted + 1;
            let framed = framed_terminal(
                seq,
                seq == 1,
                format!("\x1b[Hmeaningful-frame-{source_index:03}").into_bytes(),
            );
            let length = framed.len();
            if producer_writer.render.try_send(framed).is_ok() {
                accepted += 1;
                encoded_bytes += length;
            }
        }
        (source_started.elapsed(), accepted, encoded_bytes)
    });
    let (source_duration, accepted, encoded_bytes) = producer.await.expect("frame producer");
    let mut delivered = 0u64;
    let mut first_visible = None;
    let mut last_visible = None;
    tokio::time::timeout(Duration::from_secs(8), async {
        while delivered < accepted {
            if let Some(ServerMessage::Terminal(_)) = application_rx.recv().await {
                let now = Instant::now();
                first_visible.get_or_insert(now);
                last_visible = Some(now);
                delivered += 1;
            }
        }
    })
    .await
    .expect("60 fps delivery timeout");
    let visible_span = last_visible
        .expect("last visible frame")
        .duration_since(first_visible.expect("first visible frame"));
    let delivered_fps = delivered.saturating_sub(1) as f64 / source_duration.as_secs_f64();
    let source_seconds = source_duration.as_secs_f64();
    let bytes_per_frame = encoded_bytes as f64 / accepted as f64;
    let bytes_per_second = encoded_bytes as f64 / source_seconds;

    let mut input_latencies = Vec::new();
    for (next_seq, sample) in (accepted + 1..).zip(0..INPUT_SAMPLES) {
        let payload = format!("latency-{sample}").into_bytes();
        let started = Instant::now();
        input_tx
            .send(ClientMessage::Input {
                data: payload.clone(),
            })
            .await
            .expect("send latency input");
        loop {
            match forwarded_event_rx.recv().await {
                Some(ServerEvent::ClientInput { data, .. }) if data == payload => break,
                Some(_) => {}
                None => panic!("server events ended"),
            }
        }
        let marker = format!("visible-{sample}").into_bytes();
        let framed = framed_terminal(next_seq, false, marker.clone());
        loop {
            match writer.render.try_send(framed.clone()) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    while !matches!(
                        forwarded_event_rx.recv().await,
                        Some(ServerEvent::ClientWriterDrained { .. })
                    ) {}
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    panic!("render writer disconnected")
                }
            }
        }
        loop {
            match application_rx.recv().await {
                Some(ServerMessage::Terminal(frame)) if frame.bytes == marker => break,
                Some(_) => {}
                None => panic!("application output ended"),
            }
        }
        input_latencies.push(started.elapsed());
    }
    input_latencies.sort_unstable();
    let percentile = |percent: usize| {
        let index = (input_latencies.len() * percent)
            .div_ceil(100)
            .saturating_sub(1);
        input_latencies[index]
    };
    let p50 = percentile(50);
    let p95 = percentile(95);
    let p99 = percentile(99);

    let application_record = RemoteQuicRenderRecord {
        connection_generation: 1,
        render_generation: 1,
        state_revision: 1,
        frame: TerminalFrame {
            seq: 1,
            width: 80,
            height: 24,
            full: true,
            bytes: b"beforemiddleafter".to_vec(),
        },
        resources: vec![
            RemoteQuicResourceRef {
                hash: [1; crate::protocol::REMOTE_QUIC_HASH_BYTES],
                text_offset: 6,
            },
            RemoteQuicResourceRef {
                hash: [2; crate::protocol::REMOTE_QUIC_HASH_BYTES],
                text_offset: 12,
            },
        ],
    };
    let application_started = Instant::now();
    for _ in 0..10_000 {
        let frame = reconstruct_frame(
            application_record.clone(),
            vec![
                b"\x1b_Gpayload\x1b\\".to_vec(),
                b"\x1b_Gplace\x1b\\".to_vec(),
            ],
        )
        .expect("reconstruct benchmark frame");
        std::hint::black_box(frame);
    }
    let application_cpu = application_started.elapsed() / 10_000;

    let serialization_started = Instant::now();
    for revision in 1..=10_000u64 {
        let _ = framed_terminal(revision, revision == 1, b"\x1b[Hcpu-sample".to_vec());
    }
    let serialization_cpu = serialization_started.elapsed() / 10_000;

    let rss_before_blackhole = linux_rss_bytes();
    mode_tx
        .send(NetworkMode::Blackhole)
        .expect("enable benchmark blackhole");
    tokio::time::timeout(Duration::from_secs(12), async {
        while !matches!(
            application_rx.recv().await,
            Some(ServerMessage::TransportStatus {
                status: RemoteTransportStatus::PathRecovering,
                ..
            })
        ) {}
    })
    .await
    .expect("benchmark recovery status timeout");
    let restored_at = Instant::now();
    mode_tx
        .send(NetworkMode::Online)
        .expect("restore benchmark path");
    while !matches!(
        forwarded_event_rx.recv().await,
        Some(ServerEvent::ClientSyncRequest { .. })
    ) {}
    writer.render.reset_generation();
    writer
        .render
        .try_send(framed_terminal(1, true, b"\x1b[2J\x1b[Hrecovered".to_vec()))
        .expect("send recovery keyframe");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            application_rx.recv().await,
            Some(ServerMessage::TransportStatus {
                status: RemoteTransportStatus::Connected,
                ..
            })
        ) {}
    })
    .await
    .expect("recovery keyframe timeout");
    let recovery_keyframe_latency = restored_at.elapsed();
    let stalled_memory_delta = linux_rss_bytes().saturating_sub(rss_before_blackhole);

    eprintln!(
        "3G profile: 1.6 Mbit/s down, 0.75 Mbit/s up, 260-340 ms RTT jitter, deterministic 0.99% packet loss"
    );
    eprintln!(
        "frames: source={SOURCE_FRAMES} accepted={accepted} delivered={delivered} fps={delivered_fps:.2} delivery-burst-span={visible_span:?} bytes/frame={bytes_per_frame:.1} bytes/s={bytes_per_second:.1}"
    );
    eprintln!(
        "input-to-visible: p50={p50:?} p95={p95:?} p99={p99:?}; serialization CPU/frame={serialization_cpu:?}; client application CPU/frame={application_cpu:?}"
    );
    eprintln!(
        "stalled RSS delta={stalled_memory_delta} bytes; recovery keyframe latency={recovery_keyframe_latency:?}"
    );

    assert_eq!(delivered, accepted);
    assert!(
        accepted >= 170,
        "render coalescing was excessive: {accepted}"
    );
    assert!(
        delivered_fps >= 57.0,
        "delivered fps was {delivered_fps:.2}"
    );
    assert!(
        p95 <= Duration::from_secs(1),
        "p95 input latency was {p95:?}"
    );
    assert!(
        recovery_keyframe_latency <= Duration::from_secs(3),
        "recovery keyframe took {recovery_keyframe_latency:?}"
    );
    assert!(stalled_memory_delta <= 128 * 1024 * 1024);

    input_tx.send(ClientMessage::Detach).await.expect("detach");
    assert!(matches!(
        session_task.await.expect("session task"),
        SessionExit::Detached
    ));
    proxy_task.abort();
    event_forwarder.abort();
    output_forwarder.abort();
}

fn linux_rss_bytes() -> usize {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .saturating_mul(1024)
}
