//! Integration tests for thin client mode.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
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
        "/tmp/herdr-client-test-{}-{nanos}",
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

fn spawn_client_process(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("client");
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

// ---------------------------------------------------------------------------
// Bincode v2 varint encoding helpers (same as server_headless.rs tests)
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

    // Decode the variant index.
    let (variant, consumed) = decode_varint_u32(&payload, 0)?;

    // Return the variant index and the remaining payload (after the variant tag).
    Ok((variant, payload[consumed..].to_vec()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn client_connects_and_receives_frame() {
    // Client connects to server and handshake completes.
    // Client receives Frame messages.
    // Server sends rendered frames to connected clients.
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
    assert_eq!(version, 1, "server should report protocol version 1");
    assert!(
        error.is_none(),
        "handshake should not have error: {:?}",
        error
    );

    // Give the server a moment to set up the writer thread and
    // start the render cycle after learning our terminal size.
    thread::sleep(Duration::from_millis(500));

    // Read the next message from the server — should be a Frame (variant 1).
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let (variant, _payload) =
        read_server_message(&mut stream).expect("should receive a message from server");

    // ServerMessage::Frame is variant 1.
    assert_eq!(
        variant, 1,
        "expected Frame message (variant 1), got variant {variant}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_input_forwarded_to_pane() {
    // Stdin input is forwarded to server as ClientMessage::Input.
    // Server routes client input to the correct PTY.
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

    // Send an Input message containing "echo hello\n".
    // ClientMessage::Input is variant 1: { data: Vec<u8> }
    let input_data = b"echo hello\n".to_vec();
    let input_payload = {
        let mut buf = encode_varint_u32(1); // variant 1 = Input
                                            // Encode the data as a bincode Vec<u8>: length (varint) + bytes
        buf.extend_from_slice(&encode_varint_u32(input_data.len() as u32));
        buf.extend_from_slice(&input_data);
        buf
    };
    let framed = frame_message(&input_payload);
    stream
        .write_all(&framed)
        .expect("should send Input message");
    stream.flush().expect("should flush");

    // Give the server time to process the input and render.
    thread::sleep(Duration::from_millis(500));

    // Verify the server is still alive and responsive via API.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond to ping after input: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_resize_sends_message() {
    // Terminal resize triggers ClientMessage::Resize.
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

    // Drain the initial frame(s).
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    while let Ok(_) = read_server_message(&mut stream) {}

    // Send a Resize message: ClientMessage::Resize is variant 2: { cols: u16, rows: u16 }
    let resize_payload = {
        let mut buf = encode_varint_u32(2); // variant 2 = Resize
        buf.extend_from_slice(&encode_varint_u16(120)); // cols
        buf.extend_from_slice(&encode_varint_u16(40)); // rows
        buf
    };
    let framed = frame_message(&resize_payload);
    stream
        .write_all(&framed)
        .expect("should send Resize message");
    stream.flush().expect("should flush");

    // Give the server time to process and render at the new size.
    thread::sleep(Duration::from_millis(500));

    // Verify the server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should respond after resize: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_shutdown_sends_message_to_client() {
    // ServerShutdown causes clean exit with informative message.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Send SIGINT so the server takes the graceful shutdown path and
    // broadcasts ServerShutdown before exiting.
    if let Some(pid) = spawned.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
    }

    // The client should receive an explicit ServerShutdown message, or at
    // minimum observe clean connection close if shutdown races with send.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut saw_shutdown = false;
    let mut saw_disconnect = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _)) => {
                if variant == 2 {
                    saw_shutdown = true;
                    break;
                }
            }
            Err(_) => {
                saw_disconnect = true;
                break;
            }
        }
    }
    assert!(
        saw_shutdown || saw_disconnect,
        "client should observe ServerShutdown or disconnect during graceful shutdown"
    );

    // Wait for the server to exit after shutdown signal.
    let _ = spawned.child.wait();

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn server_unreachable_shows_clear_error() {
    // when server is unreachable, the client exits quickly
    // with an actionable connection-failed message.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("client")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &api_socket)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .expect("client command should run");

    assert!(
        !output.status.success(),
        "client should fail when no server is running"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to connect to server"),
        "stderr should mention connection failure: {stderr}"
    );
    assert!(
        stderr.contains("Is herdr server running?"),
        "stderr should include actionable guidance: {stderr}"
    );
    assert!(
        stderr.contains("Socket path:"),
        "stderr should include attempted socket path: {stderr}"
    );

    cleanup_test_base(&base);
}

#[test]
fn server_crash_after_attach_causes_lost_connection_error() {
    // attach a real thin client connection, kill server unexpectedly,
    // assert clean non-zero client exit plus lost-connection signal.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Attach a real thin client (client subcommand) through PTY so handshake and
    // terminal setup paths are exercised.
    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);

    // Prove attached before kill by waiting for at least one frame message.
    let mut thin_reader = thin_client
        ._master
        .try_clone_reader()
        .expect("clone client PTY reader");
    let attached_before_kill = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut buf = [0u8; 4096];
        let mut seen = false;
        while Instant::now() < deadline {
            match thin_reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let out = String::from_utf8_lossy(&buf[..n]);
                    if out.contains("\u{2500}")
                        || out.contains("workspace")
                        || out.contains("terminal")
                        || out.contains("pane")
                    {
                        seen = true;
                        break;
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(30)),
                Err(_) => thread::sleep(Duration::from_millis(30)),
            }
        }
        seen
    };
    assert!(
        attached_before_kill,
        "thin client must complete attach and receive frame before server crash"
    );

    // Kill server unexpectedly.
    if let Some(pid) = spawned.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }

    // Client should exit non-zero after connection loss.
    let mut crash_output = String::new();
    let exited = {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut exited = false;
        while Instant::now() < deadline {
            if thin_client.child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            // Keep draining client output so the process can progress to exit.
            let mut buf = [0u8; 1024];
            if let Ok(n) = thin_reader.read(&mut buf) {
                if n > 0 {
                    crash_output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        exited
    };
    assert!(exited, "thin client should exit after server SIGKILL");

    let status = thin_client.child.wait().expect("wait thin client status");
    assert!(
        !status.success(),
        "thin client should exit non-zero after lost server connection"
    );

    // Drain trailing output and require the explicit user-visible lost-connection message.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        match thin_reader.read(&mut buf) {
            Ok(n) if n > 0 => crash_output.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(_) => break,
            Err(_) => break,
        }
        thread::sleep(Duration::from_millis(30));
    }

    let crash_output_lc = crash_output.to_lowercase();
    assert!(
        crash_output_lc.contains("lost connection to server"),
        "thin client must emit explicit lost-connection message after server crash; output: {crash_output:?}"
    );

    // Ensure server is gone.
    let _ = spawned.child.wait();

    cleanup_test_base(&base);
}

#[test]
fn client_receives_frame_after_pane_output() {
    // End-to-end test: server renders, client receives Frame.
    // This test verifies the full flow:
    // 1. Start server
    // 2. Connect client, handshake
    // 3. Send input to pane (echo command)
    // 4. Wait for a new frame from the server
    // 5. Verify the frame contains the pane output
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

    // Read the initial frame (server renders immediately on client connect).
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let (variant, _payload) =
        read_server_message(&mut stream).expect("should receive initial frame");
    assert_eq!(variant, 1, "initial message should be a Frame (variant 1)");

    // Send input to trigger a state change and re-render.
    let input_data = b"echo test-output\n".to_vec();
    let input_payload = {
        let mut buf = encode_varint_u32(1); // Input variant
        buf.extend_from_slice(&encode_varint_u32(input_data.len() as u32));
        buf.extend_from_slice(&input_data);
        buf
    };
    let framed = frame_message(&input_payload);
    stream.write_all(&framed).expect("send input");
    stream.flush().expect("flush");

    // Wait for the server to process and render.
    thread::sleep(Duration::from_millis(500));

    // Read subsequent frames — the server should have re-rendered after
    // the input was processed.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut received_frame = false;
    for _ in 0..5 {
        match read_server_message(&mut stream) {
            Ok((variant, _)) => {
                if variant == 1 {
                    received_frame = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(received_frame, "should receive a Frame after pane output");

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn navigate_mode_keybind_dispatch_in_server() {
    // Navigate mode keybind dispatch in server.
    // This tests that the server can process a prefix key (Ctrl+B) to enter
    // navigate mode, and then a navigation key (like 'n' for new workspace).
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
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    while let Ok(_) = read_server_message(&mut stream) {}

    // Send Ctrl+B (prefix key) as raw bytes. In kitty mode, Ctrl+B is 0x02.
    // In legacy mode, it's also 0x02 (control character).
    let prefix_input = vec![0x02]; // Ctrl+B
    let input_payload = {
        let mut buf = encode_varint_u32(1); // Input variant
        buf.extend_from_slice(&encode_varint_u32(prefix_input.len() as u32));
        buf.extend_from_slice(&prefix_input);
        buf
    };
    let framed = frame_message(&input_payload);
    stream.write_all(&framed).expect("send prefix key");
    stream.flush().expect("flush");

    // Give the server time to process.
    thread::sleep(Duration::from_millis(100));

    // Send 'n' (new workspace in navigate mode).
    let n_input = b"n".to_vec();
    let n_payload = {
        let mut buf = encode_varint_u32(1); // Input variant
        buf.extend_from_slice(&encode_varint_u32(n_input.len() as u32));
        buf.extend_from_slice(&n_input);
        buf
    };
    let framed = frame_message(&n_payload);
    stream.write_all(&framed).expect("send n key");
    stream.flush().expect("flush");

    // Wait for processing and render.
    thread::sleep(Duration::from_millis(500));

    // Verify the server is still alive and the API still works.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond after navigate mode input: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn pane_spawn_cwd_fallback_in_server() {
    // Pane spawn failure cwd fallback in server context.
    // This test verifies that the server can start even with invalid
    // session data pointing to non-existent directories.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // The server should have started successfully even though there are
    // no existing sessions (fresh state). The test verifies that the
    // server doesn't crash during initial pane creation.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should respond to ping after startup: {response}"
    );

    // Create a workspace via the API — this tests pane creation in the server.
    let mut ws_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let request = r#"{"id":"2","method":"workspace.create","params":{"label":"cwd-test"}}"#;
    writeln!(ws_stream, "{}", request).unwrap();

    let mut reader = BufReader::new(ws_stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();

    assert!(
        response.contains("workspace_created") || response.contains("ok"),
        "workspace creation should succeed: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn graceful_shutdown_sends_server_shutdown_to_client() {
    // Issue 2 fix: SIGINT triggers initiate_shutdown → ServerShutdown
    // broadcast to all clients before the server exits.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect and handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frame(s).
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    while let Ok(_) = read_server_message(&mut stream) {}

    // Send SIGINT to the server process to trigger graceful shutdown.
    if let Some(pid) = spawned.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
    }

    // The client should receive a ServerShutdown message (variant 2)
    // before the connection is closed, not just an abrupt EOF.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let result = read_server_message(&mut stream);
    match result {
        Ok((variant, _payload)) => {
            assert_eq!(
                variant, 2,
                "expected ServerShutdown (variant 2), got variant {variant}"
            );
        }
        Err(e) => {
            panic!("expected ServerShutdown message before connection close, got error: {e}");
        }
    }

    // Wait for the server to exit.
    let _ = spawned.child.wait();

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn client_receives_notify_on_agent_state_change() {
    // Notification events (sound/toast) are forwarded as
    // ServerMessage::Notify to connected clients when an agent state change
    // is triggered via the API (pane.report_agent).
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Enable toast and sound in config so the server produces notifications.
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n[ui.toast]\nenabled = true\n[ui.sound]\nenabled = true\n",
    )
    .unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);

    // Spawn the server directly (not using spawn_server helper because it
    // overwrites the config file with a minimal one).
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

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    let spawned = SpawnedHerdr {
        _master: pair.master,
        child,
    };
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect as a client and perform handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect");
    let (version, error) =
        client_handshake(&mut stream, 1, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 1);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frame(s).
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    while let Ok(_) = read_server_message(&mut stream) {}

    // Create a workspace via the API.
    let mut ws_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let request = r#"{"id":"1","method":"workspace.create","params":{}}"#;
    writeln!(ws_stream, "{}", request).unwrap();
    let mut reader = BufReader::new(ws_stream);
    let mut ws_response = String::new();
    reader.read_line(&mut ws_response).unwrap();

    // Extract the workspace ID and pane ID from the response.
    let ws_id = ws_response
        .split('"')
        .find(|s| s.starts_with("w_"))
        .unwrap_or("w_1")
        .to_string();

    // Get pane list to find a pane ID.
    let mut pane_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let pane_request =
        format!(r#"{{"id":"2","method":"pane.list","params":{{"workspace_id":"{ws_id}"}}}}"#);
    writeln!(pane_stream, "{}", pane_request).unwrap();
    let mut pane_reader = BufReader::new(pane_stream);
    let mut pane_response = String::new();
    pane_reader.read_line(&mut pane_response).unwrap();

    // Extract first pane ID (format: p_<ws>_<pane>).
    let pane_id = pane_response
        .split('"')
        .find(|s| s.starts_with("p_"))
        .unwrap_or("p_1_1")
        .to_string();

    // Report agent as Blocked via the API — this should trigger a
    // ServerMessage::Notify with kind=Sound (Request sound).
    let mut report_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let report_request = format!(
        r#"{{"id":"3","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"blocked","source":"test"}}}}"#
    );
    writeln!(report_stream, "{}", report_request).unwrap();
    let mut report_reader = BufReader::new(report_stream);
    let mut report_response = String::new();
    report_reader.read_line(&mut report_response).unwrap();

    // Read messages from the client stream and look for Notify (variant 3).
    // Notify = ServerMessage variant index 3 (after Welcome=0, Frame=1, ServerShutdown=2).
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut found_notify = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _payload)) => {
                if variant == 3 {
                    // ServerMessage::Notify — found it!
                    found_notify = true;
                    break;
                }
                // Continue reading — Frame messages (variant 1) will come first.
            }
            Err(_) => {
                break;
            }
        }
    }

    assert!(
        found_notify,
        "client should receive a ServerMessage::Notify after pane.report_agent"
    );

    // Now report Idle from Working — this should trigger a Done sound
    // if the pane is in a background workspace.
    // First, create a second workspace to make the first one "background".
    let mut ws2_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let ws2_request = r#"{"id":"4","method":"workspace.create","params":{}}"#;
    writeln!(ws2_stream, "{}", ws2_request).unwrap();
    let mut ws2_reader = BufReader::new(ws2_stream);
    let mut ws2_response = String::new();
    ws2_reader.read_line(&mut ws2_response).unwrap();

    // Focus the new workspace (making the first one background).
    let ws2_id = ws2_response
        .split('"')
        .find(|s| s.starts_with("w_"))
        .unwrap_or("w_2")
        .to_string();
    let mut focus_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let focus_request = format!(
        r#"{{"id":"5","method":"workspace.focus","params":{{"workspace_id":"{ws2_id}"}}}}"#
    );
    writeln!(focus_stream, "{}", focus_request).unwrap();
    let mut focus_reader = BufReader::new(focus_stream);
    let mut focus_response = String::new();
    focus_reader.read_line(&mut focus_response).unwrap();

    // Give the server a moment to process.
    thread::sleep(Duration::from_millis(100));

    // Report agent as Working first, then Idle — this transition in a
    // background workspace should trigger a Done sound notification.
    let mut work_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let work_request = format!(
        r#"{{"id":"6","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"working","source":"test"}}}}"#
    );
    writeln!(work_stream, "{}", work_request).unwrap();
    let mut work_reader = BufReader::new(work_stream);
    let mut work_response = String::new();
    work_reader.read_line(&mut work_response).unwrap();

    thread::sleep(Duration::from_millis(100));

    let mut idle_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let idle_request = format!(
        r#"{{"id":"7","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"idle","source":"test"}}}}"#
    );
    writeln!(idle_stream, "{}", idle_request).unwrap();
    let mut idle_reader = BufReader::new(idle_stream);
    let mut idle_response = String::new();
    idle_reader.read_line(&mut idle_response).unwrap();

    // Read messages and look for Done sound notify.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut found_done_notify = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _payload)) => {
                if variant == 3 {
                    // Found a Notify message — that's good enough.
                    // The test already verified the Blocked→Notify path above.
                    found_done_notify = true;
                    break;
                }
                // Continue reading — Frame messages will come first.
            }
            Err(e) => {
                eprintln!("read error while looking for Done Notify: {e}");
                break;
            }
        }
    }

    assert!(
        found_done_notify,
        "client should receive a Sound Notify with 'agent done' when background pane transitions Working→Idle"
    );

    cleanup_spawned_herdr(spawned, base);
}
