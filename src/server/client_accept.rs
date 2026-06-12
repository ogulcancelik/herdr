use std::io;
use std::os::unix::net::UnixListener;
use std::sync::{atomic::AtomicBool, Arc};

use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::server::client_transport::{self, ServerEvent};

/// Accepts pending thin-client connections and starts their handshake readers.
pub(crate) fn accept_pending_client_connections(
    listener: &UnixListener,
    next_client_id: &mut u64,
    should_quit: &Arc<AtomicBool>,
    server_event_tx: &mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let client_id = *next_client_id;
                *next_client_id = next_client_id.saturating_add(1);

                if let Err(err) = stream.set_nonblocking(true) {
                    warn!(err = %err, "failed to set client stream nonblocking");
                    continue;
                }

                let should_quit = should_quit.clone();
                let server_event_tx = server_event_tx.clone();
                std::thread::spawn(move || {
                    if let Err(err) = client_transport::handle_client_handshake(
                        stream,
                        client_id,
                        &server_event_tx,
                        &should_quit,
                    ) {
                        debug!(client_id, err = %err, "client handshake failed");
                    }
                });
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener accept failed");
                break;
            }
        }
    }

    Ok(())
}

/// Drains pending thin-client connections without starting handshakes.
///
/// During live handoff the old server must not let clients sit in the Unix
/// listener backlog waiting for a welcome frame that will never be sent.
/// Each drained connection gets a rejection `Welcome` carrying the
/// live-handoff notice so the client retries the attach with backoff
/// instead of bailing on an opaque EOF (#38).
pub(crate) fn reject_pending_client_connections(listener: &UnixListener) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => send_live_handoff_refusal(stream),
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener reject failed");
                break;
            }
        }
    }

    Ok(())
}

/// Best-effort write of the live-handoff rejection `Welcome` before the
/// connection is dropped. Runs on the server main loop, so the write is
/// bounded by a short timeout; the frame is tiny and lands in the socket
/// buffer immediately in practice.
fn send_live_handoff_refusal(mut stream: std::os::unix::net::UnixStream) {
    let welcome = crate::protocol::ServerMessage::Welcome {
        version: crate::protocol::PROTOCOL_VERSION,
        encoding: crate::protocol::RenderEncoding::SemanticFrame,
        error: Some(crate::protocol::LIVE_HANDOFF_ATTACH_NOTICE.to_owned()),
    };
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
    if let Err(err) = crate::protocol::write_message(&mut stream, &welcome) {
        debug!(err = %err, "failed to send live-handoff refusal to pending client");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn rejected_connections_receive_the_live_handoff_notice() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("hca-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("client.sock");

        let listener = UnixListener::bind(&socket_path).expect("bind test listener");
        listener.set_nonblocking(true).expect("nonblocking");

        let mut client = UnixStream::connect(&socket_path).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");

        reject_pending_client_connections(&listener).expect("reject");
        let welcome: crate::protocol::ServerMessage =
            crate::protocol::read_message(&mut client, crate::protocol::MAX_FRAME_SIZE)
                .expect("read refusal welcome");
        match welcome {
            crate::protocol::ServerMessage::Welcome { error, version, .. } => {
                assert_eq!(version, crate::protocol::PROTOCOL_VERSION);
                assert_eq!(
                    error.as_deref(),
                    Some(crate::protocol::LIVE_HANDOFF_ATTACH_NOTICE),
                    "pending clients must learn the handoff is in progress"
                );
            }
            other => panic!("expected Welcome, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
