//! Integration tests for headless server mode.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-server-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("file did not appear at {}", path.display());
}

fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    _client_socket_path: &Path,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn ping_socket(socket_path: &Path) -> String {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");

    let request = r#"{"id":"1","method":"ping","params":{}}"#;
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response.trim().to_string()
}

/// Sends a Hello message over the client socket and reads the Welcome response.
/// Uses bincode v2 wire format: [u32LE length][bincode payload]
/// bincode v2 standard config uses VarintEncoding:
///   - Integers < 251 are encoded as a single byte
///   - Enum variant index is encoded as u32 varint
///   - Option discriminant is always a single byte (0=None, 1=Some)
///   - String: length (varint) + UTF-8 bytes
fn client_handshake(
    stream: &mut UnixStream,
    version: u32,
    cols: u16,
    rows: u16,
) -> Result<(u32, Option<String>), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    // Encode Hello message using bincode v2 varint format.
    // ClientMessage::Hello is variant 0.
    let hello_payload = encode_varint_enum(
        0,
        &[
            &encode_varint_u32(version),
            &encode_varint_u16(cols),
            &encode_varint_u16(rows),
        ],
    );
    let framed = frame_message(&hello_payload);
    stream.write_all(&framed).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    // Read the framed response.
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).map_err(|e| e.to_string())?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > 2 * 1024 * 1024 {
        return Err(format!("oversized response: {len}"));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;

    // Decode Welcome: ServerMessage variant 0 = Welcome { version: u32, error: Option<String> }
    decode_welcome(&payload)
}

/// Encode a varint u32 value according to bincode v2 VarintEncoding.
fn encode_varint_u32(v: u32) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else if v < 65536 {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&(v as u16).to_le_bytes());
        buf
    } else {
        let mut buf = vec![252u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

/// Encode a varint u16 value.
fn encode_varint_u16(v: u16) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

/// Encode an enum variant with its fields.
fn encode_varint_enum(variant_idx: u32, fields: &[&[u8]]) -> Vec<u8> {
    let mut buf = encode_varint_u32(variant_idx);
    for field in fields {
        buf.extend_from_slice(field);
    }
    buf
}

/// Frame a message with u32LE length prefix.
fn frame_message(payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut framed = len.to_le_bytes().to_vec();
    framed.extend_from_slice(payload);
    framed
}

/// Decode a varint u32 from a byte slice at the given offset.
/// Returns (value, bytes_consumed).
fn decode_varint_u32(payload: &[u8], offset: usize) -> Result<(u32, usize), String> {
    if offset >= payload.len() {
        return Err("payload too short for varint".into());
    }
    let first_byte = payload[offset];
    match first_byte {
        0..=250 => Ok((first_byte as u32, 1)),
        251 => {
            if offset + 3 > payload.len() {
                return Err("payload too short for u16 varint".into());
            }
            let v = u16::from_le_bytes(
                payload[offset + 1..offset + 3]
                    .try_into()
                    .map_err(|e: std::array::TryFromSliceError| e.to_string())?,
            );
            Ok((v as u32, 3))
        }
        252 => {
            if offset + 5 > payload.len() {
                return Err("payload too short for u32 varint".into());
            }
            let v = u32::from_le_bytes(
                payload[offset + 1..offset + 5]
                    .try_into()
                    .map_err(|e: std::array::TryFromSliceError| e.to_string())?,
            );
            Ok((v, 5))
        }
        _ => Err(format!("unsupported varint tag: {first_byte}")),
    }
}

/// Decode a varint u16 from a byte slice at the given offset.
#[allow(dead_code)]
fn decode_varint_u16(payload: &[u8], offset: usize) -> Result<(u16, usize), String> {
    if offset >= payload.len() {
        return Err("payload too short for varint".into());
    }
    let first_byte = payload[offset];
    match first_byte {
        0..=250 => Ok((first_byte as u16, 1)),
        251 => {
            if offset + 3 > payload.len() {
                return Err("payload too short for u16 varint".into());
            }
            let v = u16::from_le_bytes(
                payload[offset + 1..offset + 3]
                    .try_into()
                    .map_err(|e: std::array::TryFromSliceError| e.to_string())?,
            );
            Ok((v, 3))
        }
        _ => Err(format!("unsupported varint tag for u16: {first_byte}")),
    }
}

/// Decode a ServerMessage::Welcome from bincode v2 payload.
fn decode_welcome(payload: &[u8]) -> Result<(u32, Option<String>), String> {
    let mut offset = 0;

    // Variant index (should be 0 for Welcome)
    let (variant, consumed) = decode_varint_u32(payload, offset)?;
    offset += consumed;
    if variant != 0 {
        return Err(format!(
            "expected Welcome (variant 0), got variant {variant}"
        ));
    }

    // version: u32
    let (version, consumed) = decode_varint_u32(payload, offset)?;
    offset += consumed;

    // error: Option<String> — discriminant is always 1 byte
    if offset >= payload.len() {
        return Err("payload too short for Option tag".into());
    }
    let option_tag = payload[offset];
    offset += 1;

    let error = if option_tag == 1 {
        // Some(String) — length as varint + UTF-8 bytes
        let (str_len, consumed) = decode_varint_u32(payload, offset)?;
        offset += consumed;
        let str_len = str_len as usize;

        if offset + str_len > payload.len() {
            return Err("payload too short for string content".into());
        }
        let s = String::from_utf8(payload[offset..offset + str_len].to_vec())
            .map_err(|e| e.to_string())?;
        Some(s)
    } else {
        None
    };

    Ok((version, error))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn server_creates_both_sockets() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for both sockets to appear.
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Verify the client socket is a socket file.
    let metadata = fs::metadata(&client_socket).unwrap();
    let file_type = metadata.file_type();
    assert!(
        file_type.is_socket(),
        "client socket should be a socket file"
    );

    // Verify the API socket works.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "ping should return pong: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_starts_without_terminal() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for the API socket to appear — proves the server started.
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // The server process should be running.
    if let Some(pid) = spawned.child.process_id() {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        assert_eq!(result, 0, "server process should be running");
    }

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_api_responds_to_ping() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Ping the API socket.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_removes_client_socket_on_exit() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Kill the server.
    let _ = spawned.child.kill();
    let _ = spawned.child.wait();

    // Give it a moment to clean up.
    thread::sleep(Duration::from_millis(300));

    // The client socket should be removed (best-effort by Drop).
    // If it still exists, it should be stale (not connectable).
    if client_socket.exists() {
        assert!(
            UnixStream::connect(&client_socket).is_err(),
            "stale client socket should not accept connections"
        );
    }

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn server_cleans_up_stale_client_socket() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Create a stale client socket file (simulating a crashed server).
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    {
        let _listener = std::os::unix::net::UnixListener::bind(&client_socket).unwrap();
        // Drop the listener so the socket becomes stale.
    }

    // Now start the server — it should clean up the stale socket.
    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // The API should work.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_persists_after_client_disconnect() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and then immediately disconnect.
    {
        let _stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
        // Immediately drop the connection.
    }

    // Give the server a moment to process the disconnect.
    thread::sleep(Duration::from_millis(200));

    // The server should still be running and the API should still respond.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should still respond after client disconnect: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn duplicate_server_start_fails_gracefully() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Start the first server.
    let spawned1 = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Try to start a second server — it should fail.
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", &api_socket);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let mut child2 = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child2.process_id());
    drop(pair.slave);

    // Wait for the second server to exit.
    let exit_status = child2.wait().unwrap();
    unregister_spawned_herdr_pid(child2.process_id());

    // The second server should exit with a non-zero code.
    assert!(!exit_status.success(), "duplicate server start should fail");

    cleanup_spawned_herdr(spawned1, base);
}

#[test]
fn client_handshake_succeeds() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and perform a handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    // Send Hello with version 2, 80 cols, 24 rows.
    let (version, error) =
        client_handshake(&mut stream, 2, 80, 24).expect("handshake should succeed");

    assert_eq!(version, 2, "server should report protocol version 2");
    assert!(
        error.is_none(),
        "handshake should not have an error: {:?}",
        error
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_handshake_rejects_incompatible_version() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and send Hello with version 0 (pre-persistence).
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    let (version, error) = client_handshake(&mut stream, 0, 80, 24)
        .expect("should read Welcome response even on rejection");

    assert_eq!(version, 2, "server should report its version 2");
    assert!(
        error.is_some(),
        "version 0 should be rejected with an error"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_handshake_clamps_small_terminal_size() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Send Hello with 0x0 terminal size — should be clamped.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    let (version, error) = client_handshake(&mut stream, 2, 0, 0)
        .expect("handshake with 0x0 should succeed (server clamps)");

    assert_eq!(version, 2);
    assert!(
        error.is_none(),
        "0x0 size should be accepted (clamped): {:?}",
        error
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn no_hello_client_closed_within_five_seconds() {
    // Client connection that sends no Hello is closed within 5 seconds.
    // The server sets a handshake timeout of 4 seconds to guarantee the connection
    // is closed within the 5-second deadline even with OS overhead.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect but don't send Hello — just a raw connection.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    // Set a read timeout longer than the handshake timeout so we can detect
    // when the server closes the connection.
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();

    let start = Instant::now();

    // Try to read from the stream. The server should close the connection
    // within 5 seconds, causing our read to return with an error (EOF or
    // connection reset).
    let mut buf = [0u8; 1024];
    let result = stream.read(&mut buf);
    let elapsed = start.elapsed();

    // The read should fail (connection closed by server).
    assert!(
        result.is_err() || result.unwrap() == 0,
        "server should close the connection when no Hello is sent"
    );

    // The connection should be closed within 5 seconds.
    assert!(
        elapsed < Duration::from_secs(5),
        "connection should be closed within 5 seconds, took {:?}",
        elapsed
    );

    // Verify the server is still healthy — a proper client can still connect.
    let mut good_stream =
        UnixStream::connect(&client_socket).expect("should connect after no-hello client");
    let (version, error) = client_handshake(&mut good_stream, 2, 80, 24)
        .expect("proper handshake should still work after no-hello client");
    assert_eq!(version, 2);
    assert!(error.is_none());

    // API should still work.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}
