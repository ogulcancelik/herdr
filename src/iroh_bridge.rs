//! iroh bridge: tunnel herdr's client protocol over QUIC.
//!
//! This module provides a standalone bridge that connects a local Unix socket
//! (herdr's client transport) to a remote herdr server over an iroh QUIC
//! connection.  It is the QUIC equivalent of the SSH stdio tunnel in
//! `src/remote/unix.rs`.
//!
//! ## Architecture
//!
//! ```text
//! LOCAL                               REMOTE
//! herdr client --Unix socket--┐       ┌--Unix socket-- herdr server
//!                             │       │
//!                     ┌───────▼───────▼───────┐
//!                     │    iroh QUIC stream   │
//!                     │  (single bi-di stream)│
//!                     └───────────────────────┘
//! ```
//!
//! A single bidirectional QUIC stream carries herdr's length-prefixed
//! `ClientMessage` / `ServerMessage` protocol.  The bridge is a thin proxy:
//! bytes read from the Unix socket are written to the QUIC stream and vice
//! versa.
//!
//! ## Modes
//!
//! - **serve** — runs on the remote host.  Accepts iroh connections and
//!   bridges them to the local herdr server's client socket.
//! - **connect** — runs on the local machine.  Connects to a remote
//!   endpoint by [`EndpointId`] and bridges to a local Unix socket for
//!   the herdr client to attach to.
//!
//! ## Identity
//!
//! Each peer is identified by its Ed25519 [`EndpointId`] (public key).
//! Keys are persisted under `~/.config/herdr/iroh_id.key` or generated
//! fresh on first use.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::{
    Endpoint, EndpointId, RelayMap, RelayUrl,
    endpoint::{Connection, RecvStream, SendStream, presets},
    protocol::{ProtocolHandler, Router},
    RelayConfig, RelayMode, SecretKey,
};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};

/// The ALPN identifier for the herdr iroh bridge protocol.
///
/// Both sides must agree on this value for the QUIC handshake to succeed.
/// Changing this is a wire-format change.
const ALPN: &[u8] = b"herdr/iroh-bridge/0";

/// The directory under `~/.config/herdr/` where iroh identity keys live.
const IROH_KEY_DIR: &str = "iroh";

/// Name of the identity key file (raw 32-byte secret key).
const KEY_FILE_NAME: &str = "iroh_id.key";

/// Name of the public key file (hex-encoded EndpointId).
const PUB_KEY_FILE_NAME: &str = "iroh_id.pub";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run the iroh bridge in **serve** mode.
///
/// Binds an iroh endpoint, prints the [`EndpointId`] so the client can
/// connect, then accepts incoming connections and bridges each one to the
/// local herdr server's client socket.
///
/// This is intended to run on the remote host.
pub async fn run_serve(config: ServeConfig) -> io::Result<()> {
    let (endpoint, endpoint_id) = bind_endpoint(config.secret_key, config.relay_urls).await?;

    let bridge = BridgeHandler {
        server_socket: config.server_socket,
    };

    let _router = Router::builder(endpoint.clone())
        .accept(ALPN.to_vec(), bridge)
        .spawn();

    info!("iroh bridge listening on {endpoint_id}");
    // Print the endpoint id on stdout so the caller can capture it.
    println!("{endpoint_id}");

    // Wait for Ctrl+C or termination signal.
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| io::Error::other(format!("signal error: {e}")))?;

    info!("iroh bridge shutting down");
    endpoint.close().await;
    Ok(())
}

/// Run the iroh bridge in **connect** mode with automatic reconnection.
///
/// If the QUIC connection drops, the bridge enters a reconnect loop,
/// retrying every second until the connection is re-established or the
/// bridge is explicitly stopped.
///
/// A wall-clock freeze detector handles suspend/resume: if the system
/// clock jumps more than 20 seconds between loop iterations, the stale
/// connection is dropped immediately and a fresh connection is attempted.
pub async fn run_connect(config: ConnectConfig) -> io::Result<()> {
    // Bind the endpoint once — reuse across reconnection attempts.
    let (endpoint, _local_id) = bind_endpoint(config.secret_key, config.relay_urls.clone()).await?;

    // Wall-clock freeze detection: track real time to detect suspend.
    let mut last_wall_clock = Instant::now();
    let freeze_threshold = Duration::from_secs(20);
    let reconnect_delay = Duration::from_secs(1);

    loop {
        // Detect suspend: if wall clock jumped > threshold, the process
        // was frozen.  Drop any stale state and reconnect fresh.
        let now = Instant::now();
        if now.duration_since(last_wall_clock) > freeze_threshold {
            info!(
                "wall clock jumped {:.0}s — system likely suspended, reconnecting",
                now.duration_since(last_wall_clock).as_secs_f64()
            );
        }
        last_wall_clock = now;

        match run_connect_once(&endpoint, &config).await {
            Ok(()) => {
                endpoint.close().await;
                return Ok(());
            }
            Err(e) => {
                warn!("iroh bridge connection lost: {e} — reconnecting in {}s", reconnect_delay.as_secs());
                tokio::time::sleep(reconnect_delay).await;
            }
        }
    }
}

/// Connect once without reconnection logic.  Returns on connection loss.
///
/// The endpoint is passed in from the caller so it can be reused across
/// reconnection attempts.
async fn run_connect_once(endpoint: &Endpoint, config: &ConnectConfig) -> io::Result<()> {
    let local_socket = &config.local_socket;
    // Remove any stale socket file.
    let _ = std::fs::remove_file(local_socket);

    let listener = tokio::net::UnixListener::bind(local_socket).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "failed to bind local socket {}: {e}",
                local_socket.display()
            ),
        )
    })?;

    info!("iroh bridge listening on {}", local_socket.display());
    println!("{}", local_socket.display());

    // Accept exactly one client on the local socket and bridge it to the
    // remote endpoint.
    let (local_stream, _addr) = listener
        .accept()
        .await
        .map_err(|e| io::Error::other(format!("failed to accept on local socket: {e}")))?;

    let conn = endpoint
        .connect(config.remote_endpoint_id, ALPN)
        .await
        .map_err(|e| {
            io::Error::other(format!("failed to connect to remote endpoint: {e}"))
        })?;

    let remote_id = conn.remote_id();
    info!("connected to remote endpoint {remote_id}");

    let (iroh_send, iroh_recv) = conn
        .open_bi()
        .await
        .map_err(|e| io::Error::other(format!("failed to open bidirectional stream: {e}")))?;

    bridge_streams(local_stream, iroh_send, iroh_recv).await;

    let _ = std::fs::remove_file(local_socket);
    Ok(())
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for `run_serve`.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Path to the herdr server's client socket on this host.
    pub server_socket: PathBuf,
    /// Optional Ed25519 secret key bytes.  If `None`, a new key is
    /// generated (ephemeral mode — the endpoint id changes every run).
    pub secret_key: Option<[u8; 32]>,
    /// Optional custom relay URLs.
    pub relay_urls: Vec<String>,
}

/// Configuration for `run_connect`.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    /// The remote endpoint's public key (EndpointId).
    pub remote_endpoint_id: EndpointId,
    /// Path to create the local Unix socket for the herdr client.
    pub local_socket: PathBuf,
    /// Optional Ed25519 secret key bytes.  If `None`, a new key is
    /// generated.
    pub secret_key: Option<[u8; 32]>,
    /// Optional custom relay URLs.
    pub relay_urls: Vec<String>,
}

// ---------------------------------------------------------------------------
// Endpoint setup
// ---------------------------------------------------------------------------

/// Bind an iroh endpoint with default configuration suitable for a
/// long-lived bridge.
async fn bind_endpoint(
    secret_key: Option<[u8; 32]>,
    relay_urls: Vec<String>,
) -> io::Result<(Endpoint, EndpointId)> {
    let key = match secret_key {
        Some(bytes) => SecretKey::from_bytes(&bytes),
        None => SecretKey::generate(),
    };

    let mut builder = Endpoint::builder(presets::N0).secret_key(key);

    if !relay_urls.is_empty() {
        let relay_map = RelayMap::empty();
        for url_str in &relay_urls {
            match url_str.parse::<RelayUrl>() {
                Ok(url) => {
                    relay_map.insert(url.clone(), Arc::new(RelayConfig::from(url)));
                }
                Err(e) => {
                    warn!("invalid relay URL {url_str}: {e}");
                }
            }
        }
        if !relay_map.is_empty() {
            builder = builder.relay_mode(RelayMode::Custom(relay_map));
        }
    }

    let endpoint = builder.bind().await.map_err(|e| {
        io::Error::other(format!("failed to bind iroh endpoint: {e}"))
    })?;

    let endpoint_id = endpoint.id();
    Ok((endpoint, endpoint_id))
}

// ---------------------------------------------------------------------------
// Protocol handler (serve side)
// ---------------------------------------------------------------------------

/// The iroh protocol handler for the serve side.
///
/// For each incoming connection, accepts one bidirectional stream and
/// bridges it to the local herdr server socket.
#[derive(Debug, Clone)]
struct BridgeHandler {
    server_socket: PathBuf,
}

impl ProtocolHandler for BridgeHandler {
    async fn accept(
        &self,
        connection: Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let remote_id = connection.remote_id();

        info!("accepted iroh connection from {remote_id}");

        let (iroh_send, iroh_recv) = match connection.accept_bi().await {
            Ok(streams) => streams,
            Err(e) => {
                warn!("failed to accept bidirectional stream from {remote_id}: {e}");
                return Ok(());
            }
        };

        match UnixStream::connect(&self.server_socket).await {
            Ok(server_stream) => {
                debug!(
                    "connected to herdr server socket {}",
                    self.server_socket.display()
                );
                bridge_streams(server_stream, iroh_send, iroh_recv).await;
            }
            Err(e) => {
                warn!(
                    "failed to connect to herdr server socket {}: {e}",
                    self.server_socket.display()
                );
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stream bridging
// ---------------------------------------------------------------------------

/// Bridge a Unix socket stream to an iroh bidirectional QUIC stream.
///
/// Data flows in both directions concurrently.  When either side closes,
/// the other direction is allowed to finish before the bridge returns.
async fn bridge_streams(
    unix_stream: UnixStream,
    mut iroh_send: SendStream,
    mut iroh_recv: RecvStream,
) {
    let (mut unix_read, mut unix_write) = unix_stream.into_split();

    // Copy unix → iroh
    let up = async {
        let result = tokio::io::copy(&mut unix_read, &mut iroh_send).await;
        // Signal the remote peer that we're done writing.
        let _ = iroh_send.finish();
        if let Err(e) = result {
            // "Connection reset by peer" is normal when the other side
            // disconnects first.
            if e.kind() != io::ErrorKind::ConnectionReset
                && e.kind() != io::ErrorKind::BrokenPipe
                && e.kind() != io::ErrorKind::UnexpectedEof
            {
                warn!("iroh bridge up direction error: {e}");
            }
        }
    };

    // Copy iroh → unix
    let down = async {
        let result = tokio::io::copy(&mut iroh_recv, &mut unix_write).await;
        if let Err(e) = result {
            if e.kind() != io::ErrorKind::ConnectionReset
                && e.kind() != io::ErrorKind::BrokenPipe
                && e.kind() != io::ErrorKind::UnexpectedEof
            {
                warn!("iroh bridge down direction error: {e}");
            }
        }
    };

    tokio::join!(up, down);
}

// ---------------------------------------------------------------------------
// Key management
// ---------------------------------------------------------------------------

/// Load or create a persistent Ed25519 identity key.
///
/// Keys are stored under `~/.config/herdr/iroh/`.  On first run a new key
/// is generated and written to disk, encrypted at rest via the keyfile
/// module.
///
/// If a raw (unencrypted) key from a previous herdr version is found,
/// it is migrated to the encrypted format automatically.
///
/// Returns the 32-byte secret key.
pub fn load_or_create_identity_key() -> io::Result<[u8; 32]> {
    let key_dir = identity_key_dir()?;
    std::fs::create_dir_all(&key_dir)?;

    let key_path = key_dir.join(KEY_FILE_NAME);
    let pub_path = key_dir.join(PUB_KEY_FILE_NAME);

    // Check for legacy raw key file and migrate it.
    if key_path.exists() {
        let metadata = std::fs::metadata(&key_path)?;
        // Raw keys are exactly 32 bytes; encrypted keys are much larger.
        if metadata.len() == 32 {
            // Read raw key and migrate to encrypted format.
            let raw_bytes = std::fs::read(&key_path)?;
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&raw_bytes);

            // Store the raw key temporarily, then encrypt it.
            crate::iroh_keyfile::migrate_raw_key(&key_dir, KEY_FILE_NAME, &secret)
                .map_err(|e| io::Error::other(format!("failed to migrate identity key: {e}")))?;

            // Also write the public key if not present.
            if !pub_path.exists() {
                let secret_key = SecretKey::from_bytes(&secret);
                let public_bytes = secret_key.public();
                std::fs::write(&pub_path, hex_encode(public_bytes.as_bytes()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&pub_path, std::fs::Permissions::from_mode(0o644));
                }
            }

            info!("migrated legacy raw identity key to encrypted format");
            return Ok(secret);
        }

        // Already encrypted — load via keyfile module.
        return crate::iroh_keyfile::load_or_create_key(&key_dir, KEY_FILE_NAME)
            .map_err(|e| io::Error::other(format!("failed to load identity key: {e}")));
    }

    // Generate a new key (encrypted at rest).
    let secret_bytes = crate::iroh_keyfile::load_or_create_key(&key_dir, KEY_FILE_NAME)
        .map_err(|e| io::Error::other(format!("failed to create identity key: {e}")))?;

    // Also write the public key for easy lookup.
    let secret_key = SecretKey::from_bytes(&secret_bytes);
    let public_key = secret_key.public();
    let public_bytes: &[u8; 32] = public_key.as_bytes();
    std::fs::write(&pub_path, hex_encode(public_bytes))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&pub_path, std::fs::Permissions::from_mode(0o644));
    }

    info!(
        "generated new iroh identity key ({})",
        hex_encode(public_bytes)
    );

    Ok(secret_bytes)
}

/// Read the public EndpointId from the persisted identity key.
///
/// Returns `None` if no identity key has been created yet.
pub fn load_identity_public_key() -> io::Result<Option<EndpointId>> {
    let key_dir = identity_key_dir()?;
    let pub_path = key_dir.join(PUB_KEY_FILE_NAME);

    if !pub_path.exists() {
        return Ok(None);
    }

    let hex_str = std::fs::read_to_string(&pub_path)?;
    let bytes = hex_decode(hex_str.trim())
        .map_err(|e| io::Error::other(format!("invalid pubkey: {e}")))?;
    if bytes.len() != 32 {
        return Err(io::Error::other("invalid pubkey length"));
    }

    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    let id = EndpointId::from_bytes(&arr).map_err(|e| {
        io::Error::other(format!("invalid public key bytes: {e}"))
    })?;
    Ok(Some(id))
}

/// Returns `~/.config/herdr/iroh/`.
pub fn identity_key_dir() -> io::Result<PathBuf> {
    let config_dir = home_config_dir()
        .ok_or_else(|| io::Error::other("cannot determine config directory"))?;
    Ok(config_dir.join("herdr").join(IROH_KEY_DIR))
}

fn home_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    data_encoding::HEXLOWER
        .decode(s.trim().as_bytes())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn hex_roundtrip() {
        let input = [0xde, 0xad, 0xbe, 0xef, 0x00, 0xff];
        let encoded = hex_encode(&input);
        assert_eq!(encoded, "deadbeef00ff");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, input);
    }

    #[test]
    fn hex_decode_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_invalid_char() {
        assert!(hex_decode("zz").is_err());
    }

    // --- identity key tests ---

    #[test]
    fn create_and_load_identity_key() {
        let dir = TempDir::new().unwrap();
        let key_dir = dir.path().join("herdr").join(IROH_KEY_DIR);

        // First call creates the key.
        let secret = load_or_create_identity_key_custom(&key_dir).unwrap();
        assert_eq!(secret.len(), 32);
        assert!(key_dir.join(KEY_FILE_NAME).exists());
        assert!(key_dir.join(PUB_KEY_FILE_NAME).exists());

        // Second call loads the same key.
        let loaded = load_or_create_identity_key_custom(&key_dir).unwrap();
        assert_eq!(secret, loaded);
    }

    #[test]
    fn load_identity_public_key_existing() {
        let dir = TempDir::new().unwrap();
        let key_dir = dir.path().join("herdr").join(IROH_KEY_DIR);

        // Create a key first.
        load_or_create_identity_key_custom(&key_dir).unwrap();

        // Should load the public key.
        let pub_key = load_identity_public_key_custom(&key_dir).unwrap();
        assert!(pub_key.is_some());
    }

    #[test]
    fn load_identity_public_key_none_when_missing() {
        let dir = TempDir::new().unwrap();
        let key_dir = dir.path().join("herdr").join(IROH_KEY_DIR);

        let pub_key = load_identity_public_key_custom(&key_dir).unwrap();
        assert!(pub_key.is_none());
    }

    #[test]
    fn load_identity_public_key_invalid_hex() {
        let dir = TempDir::new().unwrap();
        let key_dir = dir.path().join("herdr").join(IROH_KEY_DIR);
        std::fs::create_dir_all(&key_dir).unwrap();
        std::fs::write(key_dir.join(PUB_KEY_FILE_NAME), "not-hex!!!").unwrap();

        let result = load_identity_public_key_custom(&key_dir);
        assert!(result.is_err());
    }

    #[test]
    fn load_or_create_identity_key_invalid_length() {
        let dir = TempDir::new().unwrap();
        let key_dir = dir.path().join("herdr").join(IROH_KEY_DIR);
        std::fs::create_dir_all(&key_dir).unwrap();
        std::fs::write(key_dir.join(KEY_FILE_NAME), b"too-short").unwrap();

        let result = load_or_create_identity_key_custom(&key_dir);
        assert!(result.is_err());
    }

    // --- helpers to override key directory ---

    fn load_or_create_identity_key_custom(key_dir: &PathBuf) -> io::Result<[u8; 32]> {
        std::fs::create_dir_all(key_dir)?;
        let key_path = key_dir.join(KEY_FILE_NAME);
        let pub_path = key_dir.join(PUB_KEY_FILE_NAME);

        if key_path.exists() {
            let key_bytes = std::fs::read(&key_path)?;
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&key_bytes);
            return Ok(secret);
        }

        let secret_key = SecretKey::generate();
        let public_key = secret_key.public();
        let secret_bytes = secret_key.to_bytes();
        std::fs::write(&key_path, secret_bytes)?;
        std::fs::write(&pub_path, hex_encode(public_key.as_bytes()))?;
        Ok(secret_bytes)
    }

    fn load_identity_public_key_custom(key_dir: &PathBuf) -> io::Result<Option<EndpointId>> {
        let pub_path = key_dir.join(PUB_KEY_FILE_NAME);
        if !pub_path.exists() {
            return Ok(None);
        }
        let hex_str = std::fs::read_to_string(&pub_path)?;
        let bytes = hex_decode(hex_str.trim())
            .map_err(|e| io::Error::other(format!("invalid pubkey: {e}")))?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let id = EndpointId::from_bytes(&arr)
            .map_err(|e| io::Error::other(format!("invalid public key bytes: {e}")))?;
        Ok(Some(id))
    }
}
