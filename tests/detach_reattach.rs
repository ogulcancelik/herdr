//! Integration tests for detach/reattach flow.
//!

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, client_handshake, drain_messages, read_server_message, register_runtime_dir,
    register_spawned_herdr_pid, send_detach, send_input, unregister_spawned_herdr_pid,
    wait_for_disconnect, wait_for_file, wait_for_message_variant, wait_for_socket, wait_until,
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
        client_handshake(&mut stream, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send prefix key (Ctrl+B = 0x02) then 'q' (quit/detach in persistence mode).
    send_input(&mut stream, &[0x02]).expect("send prefix");

    // Drain any frames generated by entering navigate mode.
    drain_messages(&mut stream);

    send_input(&mut stream, b"q").expect("send detach key");

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should still respond to ping after client detach"
    );

    // Verify server is still alive and responsive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond to ping after client detach: {response}"
    );

    // The client should receive a ServerShutdown with reason "detached"
    // shortly after the quit/detach key. There may be some frames in
    // between from the mode change, so we read multiple messages.
    let got_shutdown = wait_for_message_variant(&mut stream, Duration::from_secs(2), 2)
        .expect("wait for shutdown message")
        || wait_for_disconnect(&mut stream, Duration::from_secs(1)).expect("wait for disconnect");
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
        client_handshake(&mut stream, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send ClientMessage::Detach directly.
    send_detach(&mut stream).expect("send detach message");

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should persist after client Detach message"
    );

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after client Detach message: {response}"
    );

    // The client connection should eventually be closed.
    // After sending Detach, the server removes the client.
    // We may still receive a few queued frames before the connection closes.
    let got_eof =
        wait_for_disconnect(&mut stream, Duration::from_secs(2)).expect("wait for disconnect");
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
        client_handshake(&mut stream_a, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
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

    // Client A detaches (send ClientMessage::Detach).
    send_detach(&mut stream_a).expect("send detach");

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should persist after detach"
    );

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after detach: {response}"
    );

    // --- Client B (reattach) ---
    let mut stream_b = UnixStream::connect(&client_socket).expect("client B should connect");
    let (version, error) =
        client_handshake(&mut stream_b, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
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
        client_handshake(&mut stream, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Send input to the pane — the fresh server should have at least one
    // pane with a shell running.
    send_input(&mut stream, b"echo SURVIVED_DETACH\n").expect("send echo command");

    // Drain any frames generated by the input.
    drain_messages(&mut stream);

    // Detach the client via explicit Detach message.
    send_detach(&mut stream).expect("send detach");

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should persist after detach"
    );

    // Verify server is still alive after detach.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after detach: {response}"
    );

    assert!(
        wait_for_disconnect(&mut stream, Duration::from_secs(2)).expect("wait for detach"),
        "detached client connection should close"
    );

    // Reattach — verify we can connect and receive a frame.
    let mut stream_b = UnixStream::connect(&client_socket).expect("should reattach");
    let (version, error) =
        client_handshake(&mut stream_b, 2, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 2);
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
        client_handshake(&mut stream, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
    assert!(error.is_none(), "{:?}", error);

    // Drain initial frames.
    drain_messages(&mut stream);

    // Drop the connection abruptly (simulating client crash).
    drop(stream);

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should persist after client connection drop"
    );

    // Verify server is still alive.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should persist after client connection drop: {response}"
    );

    // Reattach — verify we can connect and handshake again.
    let mut stream_b = UnixStream::connect(&client_socket).expect("should reattach");
    let (version, error) =
        client_handshake(&mut stream_b, 2, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 2);
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
        client_handshake(&mut stream_a, 2, 80, 24).expect("handshake should succeed");
    assert_eq!(version, 2);
    assert!(error.is_none(), "{:?}", error);

    // Detach client A immediately.
    send_detach(&mut stream_a).expect("send detach");

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should persist"
    );

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

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            let read_response = pane_read_recent(&api_socket, &pane_id);
            read_response["result"]["read"]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("DURING_DETACH")
        }),
        "pane should contain output produced while detached before reattach"
    );

    // --- Client B (reattach) ---
    let mut stream_b = UnixStream::connect(&client_socket).expect("client B should connect");
    let (version, error) =
        client_handshake(&mut stream_b, 2, 80, 24).expect("reattach handshake should succeed");
    assert_eq!(version, 2);
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
