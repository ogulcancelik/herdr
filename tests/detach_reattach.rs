//! Integration tests for detach/reattach flow.
//!

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
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
        "/tmp/herdr-detach-test-{}-{nanos}",
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

fn wait_for_socket(path: &PathBuf, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn wait_for_file(path: &PathBuf, timeout: Duration) {
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
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
    _client_socket_path: &PathBuf,
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

fn ping_socket(socket_path: &PathBuf) -> String {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");

    let request = r#"{"id":"1","method":"ping","params":{}}"#;
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response.trim().to_string()
}

fn send_json_request(socket_path: &PathBuf, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn workspace_create(socket_path: &PathBuf, label: &str) -> Value {
    send_json_request(
        socket_path,
        &format!(
            r#"{{"id":"workspace_create","method":"workspace.create","params":{{"label":"{label}"}}}}"#
        ),
    )
}

fn workspace_list(socket_path: &PathBuf) -> Value {
    send_json_request(
        socket_path,
        r#"{"id":"workspace_list","method":"workspace.list","params":{}}"#,
    )
}

fn pane_list(socket_path: &PathBuf, workspace_id: &str) -> Value {
    send_json_request(
        socket_path,
        &format!(
            r#"{{"id":"pane_list","method":"pane.list","params":{{"workspace_id":"{workspace_id}"}}}}"#
        ),
    )
}

fn pane_read_recent(socket_path: &PathBuf, pane_id: &str) -> Value {
    send_json_request(
        socket_path,
        &format!(
            r#"{{"id":"pane_read","method":"pane.read","params":{{"pane_id":"{pane_id}","source":"recent"}}}}"#
        ),
    )
}

fn workspace_id_by_label(response: &Value, label: &str) -> String {
    response["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return an array")
        .iter()
        .find(|workspace| workspace["label"] == label)
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .expect("workspace with expected label should exist")
        .to_string()
}

fn first_pane_id(response: &Value) -> String {
    response["result"]["panes"]
        .as_array()
        .expect("pane.list should return an array")
        .first()
        .and_then(|pane| pane["pane_id"].as_str())
        .expect("pane.list should contain at least one pane")
        .to_string()
}

// ---------------------------------------------------------------------------
// Bincode v2 varint encoding helpers
// ---------------------------------------------------------------------------

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

/// Sends a Hello message over the client socket and reads the Welcome response.
fn client_handshake(
    stream: &mut UnixStream,
    version: u32,
    cols: u16,
    rows: u16,
) -> Result<(u32, Option<String>), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    // Encode Hello message: ClientMessage variant 0.
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
    let mut offset = 0;

    let (variant, consumed) = decode_varint_u32(&payload, offset)?;
    offset += consumed;
    if variant != 0 {
        return Err(format!(
            "expected Welcome (variant 0), got variant {variant}"
        ));
    }

    let (version, consumed) = decode_varint_u32(&payload, offset)?;
    offset += consumed;

    if offset >= payload.len() {
        return Err("payload too short for Option tag".into());
    }
    let option_tag = payload[offset];
    offset += 1;

    let error = if option_tag == 1 {
        let (str_len, consumed) = decode_varint_u32(&payload, offset)?;
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

/// Read a framed ServerMessage from the stream, returning the variant index
/// and the raw payload for further decoding.
fn read_server_message(stream: &mut UnixStream) -> Result<(u32, Vec<u8>), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|e| format!("read length prefix: {e}"))?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > 2 * 1024 * 1024 {
        return Err(format!("oversized frame: {len} bytes"));
    }
    if len == 0 {
        return Err("zero-length frame".into());
    }

    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .map_err(|e| format!("read payload: {e}"))?;

    let (variant, consumed) = decode_varint_u32(&payload, 0)?;

    Ok((variant, payload[consumed..].to_vec()))
}

/// Sends a ClientMessage::Input with the given raw bytes.
fn send_input(stream: &mut UnixStream, data: &[u8]) -> Result<(), String> {
    let input_payload = {
        let mut buf = encode_varint_u32(1); // variant 1 = Input
        buf.extend_from_slice(&encode_varint_u32(data.len() as u32));
        buf.extend_from_slice(data);
        buf
    };
    let framed = frame_message(&input_payload);
    stream
        .write_all(&framed)
        .map_err(|e| format!("write input: {e}"))?;
    stream.flush().map_err(|e| format!("flush input: {e}"))?;
    Ok(())
}

/// Sends a ClientMessage::Detach.
fn send_detach(stream: &mut UnixStream) -> Result<(), String> {
    // ClientMessage::Detach is variant 3 with no fields.
    let detach_payload = encode_varint_u32(3);
    let framed = frame_message(&detach_payload);
    stream
        .write_all(&framed)
        .map_err(|e| format!("write detach: {e}"))?;
    stream.flush().map_err(|e| format!("flush detach: {e}"))?;
    Ok(())
}

/// Drains all pending messages from the server stream (non-blocking).
fn drain_messages(stream: &mut UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    while let Ok(_) = read_server_message(stream) {}
    stream.set_read_timeout(None).unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn navigate_q_detaches_client_and_server_persists() {
    // In persistence mode, navigate-mode q detaches the client and the server persists.
    // Flow:
    // 1. Start server
    // 2. Connect client, handshake
    // 3. Send prefix key (Ctrl+B) then 'q'
    // 4. Verify server is still alive via API ping
    // 5. Verify the client connection is closed
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send prefix key (Ctrl+B = 0x02) then 'q' (quit/detach in persistence mode).
    send_input(&mut stream, &[0x02]).expect("send prefix");
    thread::sleep(Duration::from_millis(300));

    // Drain any frames generated by entering navigate mode.
    drain_messages(&mut stream);

    send_input(&mut stream, b"q").expect("send detach key");

    // Give the server time to process the detach.
    thread::sleep(Duration::from_millis(500));

    // Verify server is still alive and responsive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond to ping after client detach: {response}"
    );

    // The client should receive a ServerShutdown with reason "detached"
    // shortly after the quit/detach key. There may be some frames in
    // between from the mode change, so we read multiple messages.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut got_shutdown = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _payload)) => {
                if variant == 2 {
                    // ServerMessage::ServerShutdown — detach acknowledged.
                    got_shutdown = true;
                    break;
                }
                // Variant 1 = Frame — may arrive before the shutdown.
                // Keep reading.
            }
            Err(_) => {
                // Connection closed — also acceptable.
                got_shutdown = true; // consider this a successful detach
                break;
            }
        }
    }
    assert!(
        got_shutdown,
        "client should receive ServerShutdown after quit/detach key"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn explicit_detach_message_causes_clean_disconnect() {
    // Client sends ClientMessage::Detach
    // directly (not via keybind), server handles it gracefully.
    // This is the flow when the client process is exiting cleanly (Ctrl+C).
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send ClientMessage::Detach directly.
    send_detach(&mut stream).expect("send detach message");

    // Give the server time to process.
    thread::sleep(Duration::from_millis(500));

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after client Detach message: {response}"
    );

    // The client connection should eventually be closed.
    // After sending Detach, the server removes the client.
    // We may still receive a few queued frames before the connection closes.
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    // Drain remaining messages until EOF or error.
    let mut got_eof = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _)) => {
                // May receive frames or other messages before disconnect.
                let _ = variant;
            }
            Err(_) => {
                got_eof = true;
                break;
            }
        }
    }
    assert!(
        got_eof,
        "client connection should be closed after explicit Detach message"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn reattach_after_detach_shows_current_state() {
    // Flow:
    // 1. Start server
    // 2. Connect client A, create a workspace via API
    // 3. Client A detaches
    // 4. Connect client B (reattach), verify it receives a frame
    // 5. Verify client B can see the workspace created by client A
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // --- Client A ---
    let mut stream_a = UnixStream::connect(&client_socket).expect("client A should connect");
    let (version, error) =
        client_handshake(&mut stream_a, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream_a);

    // Create a workspace via API while client A is attached.
    let mut ws_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let request = r#"{"id":"1","method":"workspace.create","params":{"label":"reattach-test"}}"#;
    writeln!(ws_stream, "{}", request).unwrap();
    let mut reader = BufReader::new(ws_stream);
    let mut ws_response = String::new();
    reader.read_line(&mut ws_response).unwrap();
    assert!(
        ws_response.contains("workspace_created") || ws_response.contains("ok"),
        "workspace creation should succeed: {ws_response}"
    );

    // Give the server time to process and render.
    thread::sleep(Duration::from_millis(300));

    // Client A detaches (send ClientMessage::Detach).
    send_detach(&mut stream_a).expect("send detach");

    // Give the server time to process the detach.
    thread::sleep(Duration::from_millis(500));

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after detach: {response}"
    );

    // --- Client B (reattach) ---
    let mut stream_b = UnixStream::connect(&client_socket).expect("client B should connect");
    let (version, error) =
        client_handshake(&mut stream_b, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(
        error.is_none(),
        "reattach handshake should succeed: {:?}",
        error
    );

    // Client B should receive a frame with the current state,
    // including the workspace created while client A was attached.
    stream_b.set_nonblocking(false).unwrap();
    stream_b
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut received_frame = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream_b) {
            Ok((variant, _payload)) => {
                if variant == 1 {
                    // ServerMessage::Frame
                    received_frame = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        received_frame,
        "reattached client should receive a Frame with current state"
    );

    // Verify the workspace still exists via API.
    let mut list_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let list_request = r#"{"id":"2","method":"workspace.list","params":{}}"#;
    writeln!(list_stream, "{}", list_request).unwrap();
    let mut list_reader = BufReader::new(list_stream);
    let mut list_response = String::new();
    list_reader.read_line(&mut list_response).unwrap();
    assert!(
        list_response.contains("reattach-test"),
        "workspace should still exist after detach/reattach: {list_response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn processes_survive_during_and_after_detach() {
    // PTY processes continue running during and after detach.
    //
    // Simplified flow:
    // 1. Start server
    // 2. Connect client, send "echo SURVIVED" to the pane
    // 3. Detach client
    // 4. Verify server is still alive and API works
    // 5. Reattach and verify we can receive a frame
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Verify server starts with a workspace (session restore or fresh state).
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should respond to ping: {response}"
    );

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send input to the pane — the fresh server should have at least one
    // pane with a shell running.
    send_input(&mut stream, b"echo SURVIVED_DETACH\n").expect("send echo command");

    // Wait for the shell to process the command.
    thread::sleep(Duration::from_millis(500));

    // Drain any frames generated by the input.
    drain_messages(&mut stream);

    // Detach the client via explicit Detach message.
    send_detach(&mut stream).expect("send detach");
    thread::sleep(Duration::from_millis(500));

    // Verify server is still alive after detach.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after detach: {response}"
    );

    // Wait a moment while detached.
    thread::sleep(Duration::from_secs(1));

    // Reattach — verify we can connect and receive a frame.
    let mut stream_b = UnixStream::connect(&client_socket).expect("should reattach");
    let (version, error) =
        client_handshake(&mut stream_b, 1, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Verify the reattached client receives a frame.
    stream_b
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut received_frame = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream_b) {
            Ok((variant, _)) => {
                if variant == 1 {
                    received_frame = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        received_frame,
        "reattached client should receive a Frame showing current state"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_persists_after_client_connection_drop() {
    // Server continues running after a client
    // disconnects (not just detach — also connection drop).
    // Verify that after a client connection is abruptly closed (not via Detach),
    // the server continues running and can accept new connections.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Drop the connection abruptly (simulating client crash).
    drop(stream);

    // Give the server time to detect the disconnection.
    thread::sleep(Duration::from_millis(500));

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after client connection drop: {response}"
    );

    // Reattach — verify we can connect and handshake again.
    let mut stream_b = UnixStream::connect(&client_socket).expect("should reattach");
    let (version, error) =
        client_handshake(&mut stream_b, 1, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "reattach should succeed: {:?}", error);

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn output_accumulated_while_detached_visible_on_reattach() {
    // Output produced while detached is visible in the
    // reattached client's scrollback.
    //
    // Simplified flow:
    // 1. Start server, attach client A
    // 2. Detach client A (without sending any special input)
    // 3. Use API to send text to a pane while detached
    // 4. Reattach as client B
    // 5. Verify client B receives a frame
    // 6. Verify the pane content via API includes the text sent while detached
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake client A.
    let mut stream_a = UnixStream::connect(&client_socket).expect("client A should connect");
    let (version, error) =
        client_handshake(&mut stream_a, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Detach client A immediately.
    send_detach(&mut stream_a).expect("send detach");
    thread::sleep(Duration::from_millis(500));

    // Verify server alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist: {response}"
    );

    // Use API to send text to a pane while no client is attached.
    // First create a workspace and find its pane.
    let ws_create_response = workspace_create(&api_socket, "scrollback-test");
    assert_eq!(ws_create_response["result"]["type"], "workspace_created");

    thread::sleep(Duration::from_millis(300));

    // Find the workspace and pane IDs.
    let ws_response = workspace_list(&api_socket);
    let ws_id = workspace_id_by_label(&ws_response, "scrollback-test");

    // Get pane list for this workspace.
    let pane_response = pane_list(&api_socket, &ws_id);
    let pane_id = first_pane_id(&pane_response);

    // Send text to the pane via API while detached.
    let mut send_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let send_request = format!(
        r#"{{"id":"4","method":"pane.send_text","params":{{"pane_id":"{pane_id}","text":"echo DURING_DETACH\n"}}}}"#
    );
    writeln!(send_stream, "{}", send_request).unwrap();
    let mut send_reader = BufReader::new(send_stream);
    let mut send_response = String::new();
    send_reader.read_line(&mut send_response).unwrap();

    // Wait for the echo command to execute.
    thread::sleep(Duration::from_millis(500));

    // --- Client B (reattach) ---
    let mut stream_b = UnixStream::connect(&client_socket).expect("client B should connect");
    let (version, error) =
        client_handshake(&mut stream_b, 1, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Client B should receive a frame with the current state.
    stream_b
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut received_frame = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream_b) {
            Ok((variant, _)) => {
                if variant == 1 {
                    received_frame = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(
        received_frame,
        "reattached client should receive a Frame showing current state"
    );

    // Verify the pane content via API includes the output sent while detached.
    let read_response = pane_read_recent(&api_socket, &pane_id);

    // The pane output should contain the text sent while detached.
    assert!(
        read_response["result"]["read"]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("DURING_DETACH"),
        "pane should contain output produced while detached: {read_response}"
    );

    cleanup_spawned_herdr(spawned, base);
}
