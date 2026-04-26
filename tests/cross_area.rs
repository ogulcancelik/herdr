//! Cross-area integration tests for end-to-end persistence flows.

mod support;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
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
        "/tmp/herdr-cross-area-test-{}-{nanos}",
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

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket_path: &Path) -> SpawnedHerdr {
    spawn_server_with_path(config_home, runtime_dir, api_socket_path, None)
}

fn spawn_server_with_path(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    path_override: Option<&Path>,
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
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_client_process(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
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

fn send_json_request(socket_path: &Path, id: &str, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    let request = json!({
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn ping_socket(socket_path: &Path) -> String {
    let response = send_json_request(socket_path, "ping", "ping", json!({}));
    response.to_string()
}

fn workspace_create(socket_path: &Path, label: &str) -> Value {
    send_json_request(
        socket_path,
        "workspace_create",
        "workspace.create",
        json!({ "label": label }),
    )
}

fn workspace_list(socket_path: &Path) -> Value {
    send_json_request(socket_path, "workspace_list", "workspace.list", json!({}))
}

fn workspace_count(socket_path: &Path) -> usize {
    workspace_list(socket_path)["result"]["workspaces"]
        .as_array()
        .map(|workspaces| workspaces.len())
        .unwrap_or(0)
}

fn workspace_id_by_label(response: &Value, label: &str) -> String {
    response["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces array")
        .iter()
        .find(|workspace| workspace["label"] == label)
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .expect("workspace with matching label should exist")
        .to_string()
}

fn wait_for_child_exit(child: &mut Box<dyn Child + Send + Sync>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn pane_send_input(socket_path: &Path, pane_id: &str, text: &str) {
    let response = send_json_request(
        socket_path,
        "pane_send_input",
        "pane.send_input",
        json!({
            "pane_id": pane_id,
            "text": text,
            "keys": ["Enter"]
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.send_input should succeed: {response}"
    );
}

fn pane_send_text(socket_path: &Path, pane_id: &str, text: &str) {
    let response = send_json_request(
        socket_path,
        "pane_send_text",
        "pane.send_text",
        json!({
            "pane_id": pane_id,
            "text": text
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.send_text should succeed: {response}"
    );
}

fn pane_read_recent(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json_request(
        socket_path,
        "pane_read",
        "pane.read",
        json!({
            "pane_id": pane_id,
            "source": "recent",
            "lines": 200
        }),
    );

    response["result"]["read"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn pane_read_recent_contains(
    socket_path: &Path,
    pane_id: &str,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let text = pane_read_recent(socket_path, pane_id);
        if text.contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn pane_report_agent(socket_path: &Path, pane_id: &str, agent: &str, state: &str, source: &str) {
    let response = send_json_request(
        socket_path,
        "pane_report_agent",
        "pane.report_agent",
        json!({
            "pane_id": pane_id,
            "agent": agent,
            "state": state,
            "source": source,
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent should succeed: {response}"
    );
}

fn pane_agent_status(socket_path: &Path, pane_id: &str) -> Option<String> {
    let response = send_json_request(
        socket_path,
        "pane_get",
        "pane.get",
        json!({ "pane_id": pane_id }),
    );
    response["result"]["pane"]["agent_status"]
        .as_str()
        .map(|status| status.to_string())
}

fn wait_for_agent_status(
    socket_path: &Path,
    pane_id: &str,
    expected: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_agent_status(socket_path, pane_id).as_deref() == Some(expected) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

// ---------------------------------------------------------------------------
// Minimal protocol helpers (bincode v2 varint + framing)
// ---------------------------------------------------------------------------

fn encode_varint_u32(v: u32) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else if v < 65_536 {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&(v as u16).to_le_bytes());
        buf
    } else {
        let mut buf = vec![252u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

fn encode_varint_u16(v: u16) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

fn frame_message(payload: &[u8]) -> Vec<u8> {
    let mut framed = (payload.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(payload);
    framed
}

fn decode_varint_u32(payload: &[u8], offset: usize) -> Result<(u32, usize), String> {
    if offset >= payload.len() {
        return Err("payload too short for varint".into());
    }
    let first = payload[offset];
    match first {
        0..=250 => Ok((first as u32, 1)),
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
        _ => Err(format!("unsupported varint tag: {first}")),
    }
}

fn client_handshake(stream: &mut UnixStream, version: u32, cols: u16, rows: u16) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    // ClientMessage::Hello = variant 0
    let mut payload = encode_varint_u32(0);
    payload.extend_from_slice(&encode_varint_u32(version));
    payload.extend_from_slice(&encode_varint_u16(cols));
    payload.extend_from_slice(&encode_varint_u16(rows));

    stream
        .write_all(&frame_message(&payload))
        .expect("write hello");
    stream.flush().expect("flush hello");

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .expect("read welcome length");
    let len = u32::from_le_bytes(len_buf) as usize;
    assert!(len > 0 && len <= 2 * 1024 * 1024, "unexpected welcome size");

    let mut welcome_payload = vec![0u8; len];
    stream
        .read_exact(&mut welcome_payload)
        .expect("read welcome payload");

    let mut offset = 0;
    let (variant, consumed) = decode_varint_u32(&welcome_payload, offset).expect("decode variant");
    offset += consumed;
    assert_eq!(variant, 0, "expected ServerMessage::Welcome variant");

    let (_server_version, consumed) =
        decode_varint_u32(&welcome_payload, offset).expect("decode version");
    offset += consumed;

    let option_tag = *welcome_payload
        .get(offset)
        .expect("welcome payload should contain Option tag");
    if option_tag == 1 {
        let (str_len, consumed) =
            decode_varint_u32(&welcome_payload, offset + 1).expect("decode error length");
        let start = offset + 1 + consumed;
        let end = start + str_len as usize;
        let err = String::from_utf8(welcome_payload[start..end].to_vec()).expect("utf8 error");
        panic!("handshake rejected: {err}");
    }
}

fn send_client_input(stream: &mut UnixStream, data: &[u8]) {
    // ClientMessage::Input = variant 1
    let mut payload = encode_varint_u32(1);
    payload.extend_from_slice(&encode_varint_u32(data.len() as u32));
    payload.extend_from_slice(data);
    stream
        .write_all(&frame_message(&payload))
        .expect("write input");
    stream.flush().expect("flush input");
}

fn send_client_detach(stream: &mut UnixStream) {
    // ClientMessage::Detach = variant 3
    let payload = encode_varint_u32(3);
    stream
        .write_all(&frame_message(&payload))
        .expect("write detach");
    stream.flush().expect("flush detach");
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[derive(Debug, Deserialize)]
struct FrameWire {
    cells: Vec<CellWire>,
    width: u16,
    height: u16,
    cursor: Option<CursorWire>,
}

#[derive(Debug, Deserialize)]
struct CellWire {
    symbol: String,
    fg: u32,
    bg: u32,
    modifier: u16,
    skip: bool,
}

#[derive(Debug, Deserialize)]
struct CursorWire {
    x: u16,
    y: u16,
    visible: bool,
}

fn decode_frame_payload(payload: &[u8]) -> io::Result<FrameWire> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .map(|(frame, consumed)| (frame, consumed))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
        .and_then(|(frame, consumed): (FrameWire, usize)| {
            if consumed != payload.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "frame payload had trailing bytes: consumed={}, len={}",
                        consumed,
                        payload.len()
                    ),
                ));
            }
            Ok(frame)
        })
}

fn frame_contains_text(frame: &FrameWire, needle: &str) -> bool {
    if frame.cells.is_empty() {
        return false;
    }

    let width = frame.width.max(1) as usize;
    let mut text = String::new();
    for row in frame.cells.chunks(width) {
        for cell in row {
            let _ = (cell.fg, cell.bg, cell.modifier, cell.skip);
            text.push_str(&cell.symbol);
        }
        text.push('\n');
    }
    let _ = frame.height;
    if let Some(cursor) = frame.cursor.as_ref() {
        let _ = (cursor.x, cursor.y, cursor.visible);
    }

    text.contains(needle)
}

fn read_server_variant(stream: &mut UnixStream, timeout: Duration) -> io::Result<u32> {
    stream.set_read_timeout(Some(timeout))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length payload",
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;

    let (variant, _consumed) = decode_varint_u32(&payload, 0)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(variant)
}

fn read_server_message_payload(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<(u32, Vec<u8>)> {
    stream.set_read_timeout(Some(timeout))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length payload",
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;

    let (variant, consumed) = decode_varint_u32(&payload, 0)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((variant, payload[consumed..].to_vec()))
}

fn wait_for_frame_matching(
    stream: &mut UnixStream,
    timeout: Duration,
    predicate: impl Fn(&FrameWire) -> bool,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_message_payload(stream, slice) {
            Ok((1, payload)) => {
                let frame = decode_frame_payload(&payload)?;
                if predicate(&frame) {
                    return Ok(true);
                }
            }
            Ok((_variant, _payload)) => {}
            Err(err) if is_timeout(&err) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(false)
}

fn wait_for_frame(stream: &mut UnixStream, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_variant(stream, slice) {
            Ok(1) => return true, // ServerMessage::Frame
            Ok(_) => {}
            Err(err) if is_timeout(&err) => {}
            Err(_) => return false,
        }
    }
    false
}

fn drain_server_messages(stream: &mut UnixStream, max_drain: Duration) {
    let deadline = Instant::now() + max_drain;
    while Instant::now() < deadline {
        match read_server_variant(stream, Duration::from_millis(40)) {
            Ok(_) => {}
            Err(err) if is_timeout(&err) => break,
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-area tests
// ---------------------------------------------------------------------------

#[test]
fn cross_area_detach_and_reattach_preserves_state() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Local attach (client A).
    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, 2, 100, 30);
    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));

    // Use herdr: create a workspace and write output into its pane.
    let create = workspace_create(&api_socket, "cross-ssh-state");
    let workspace_id = create["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let pane_id = create["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    pane_send_input(&api_socket, &pane_id, "echo LOCAL_BEFORE_DETACH");
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "LOCAL_BEFORE_DETACH",
        Duration::from_secs(5)
    ));

    // Detach local client.
    send_client_detach(&mut client_a);
    drop(client_a);

    // Simulate activity while detached.
    pane_send_text(&api_socket, &pane_id, "echo DETACHED_UPDATE\n");
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "DETACHED_UPDATE",
        Duration::from_secs(5)
    ));

    // Reattach from another terminal/session (client B).
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, 2, 80, 24);
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(5)),
        "reattached client should receive frame"
    );

    let listed = workspace_list(&api_socket);
    assert_eq!(
        workspace_id,
        workspace_id_by_label(&listed, "cross-ssh-state"),
        "reattached session should see same workspace"
    );

    let readback = pane_read_recent(&api_socket, &pane_id);
    assert!(
        readback.contains("DETACHED_UPDATE"),
        "pane output should include detached-period output: {readback}"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_agent_process_survives_detach_and_reattach() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let bin_dir = base.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(&fake_pi, "#!/bin/sh\nprintf 'Working...\\n'\nsleep 8\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);

    let server = spawn_server_with_path(
        &config_home,
        &runtime_dir,
        &api_socket,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, 2, 100, 30);
    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));

    let created = workspace_create(&api_socket, "agent-persist");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    // Ensure detected agent surface is populated by running fake `pi`.
    pane_send_text(&api_socket, &pane_id, "pi");
    pane_send_input(&api_socket, &pane_id, "");
    let detected_before_hook = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut detected = false;
        while Instant::now() < deadline {
            let response = send_json_request(
                &api_socket,
                "pane_get",
                "pane.get",
                json!({ "pane_id": &pane_id }),
            );
            if response["result"]["pane"]["agent"].as_str() == Some("pi") {
                detected = true;
                break;
            }
            thread::sleep(Duration::from_millis(60));
        }
        detected
    };
    assert!(
        detected_before_hook,
        "expected fake pi process to be detected before hook status assertions"
    );

    // Use agent status surfaces directly instead of a generic sleep command.
    pane_report_agent(&api_socket, &pane_id, "pi", "working", "cross-area-test");
    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "working", Duration::from_secs(3)),
        "pane agent status should become working before detach"
    );

    // Detach and ensure status persists through API while detached.
    send_client_detach(&mut client_a);
    drop(client_a);

    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "working", Duration::from_secs(3)),
        "agent status should remain working while detached"
    );

    // Reattach and ensure client-side state reflects the persisted working status.
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, 2, 80, 24);
    let saw_working_on_client =
        wait_for_frame_matching(&mut client_b, Duration::from_secs(5), |frame| {
            frame_contains_text(frame, "working")
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_working_on_client,
        "reattached client frame should expose persisted agent working status"
    );

    // Transition to idle and verify API + client surfaces both observe it.
    pane_report_agent(&api_socket, &pane_id, "pi", "idle", "cross-area-test");
    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "idle", Duration::from_secs(3)),
        "pane agent status should transition to idle"
    );

    let saw_idle_on_client =
        wait_for_frame_matching(&mut client_b, Duration::from_secs(5), |frame| {
            frame_contains_text(frame, "idle")
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_idle_on_client,
        "reattached client frame should show idle status after transition"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_client_and_api_workspace_views_are_consistent() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, 2, 100, 30);
    assert!(wait_for_frame(&mut client, Duration::from_secs(2)));
    drain_server_messages(&mut client, Duration::from_millis(300));

    let before = workspace_count(&api_socket);

    // Create a workspace via API while the client is attached.
    let created = workspace_create(&api_socket, "api-visible-workspace");
    let created_workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace.create should return workspace_id")
        .to_string();

    // The attached client must receive a frame that includes the new workspace
    // label, proving client-side state reflects the API surface.
    let saw_workspace_on_client =
        wait_for_frame_matching(&mut client, Duration::from_secs(3), |frame| {
            frame_contains_text(frame, "api-visible-workspace")
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_workspace_on_client,
        "client-side frame should include the newly created workspace label"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut count_reached = false;
    while Instant::now() < deadline {
        if workspace_count(&api_socket) == before + 1 {
            count_reached = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        count_reached,
        "API workspace list should include the created workspace"
    );

    let listed = workspace_list(&api_socket);
    let listed_workspace_id = workspace_id_by_label(&listed, "api-visible-workspace");
    assert_eq!(
        listed_workspace_id, created_workspace_id,
        "API and client-side state should reference the same created workspace"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_two_clients_shared_view_and_single_detach_stability() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, 2, 110, 30);
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, 2, 100, 30);

    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));
    assert!(wait_for_frame(&mut client_b, Duration::from_secs(2)));
    drain_server_messages(&mut client_a, Duration::from_millis(250));
    drain_server_messages(&mut client_b, Duration::from_millis(250));

    let created = workspace_create(&api_socket, "shared-view");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    // Input from client A should update shared state visible to client B.
    send_client_input(&mut client_a, b"echo SHARED_VIEW\n");
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(2)),
        "client B should receive update from client A"
    );
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "SHARED_VIEW",
        Duration::from_secs(5)
    ));

    // Detach client A; client B should keep working.
    send_client_detach(&mut client_a);
    drop(client_a);

    send_client_input(&mut client_b, b"echo AFTER_A_DETACH\n");
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(2)),
        "remaining client should still receive frames after other client detaches"
    );
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "AFTER_A_DETACH",
        Duration::from_secs(5)
    ));

    let ping = ping_socket(&api_socket);
    assert!(
        ping.contains("pong"),
        "server and remaining client flow should stay healthy: {ping}"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_server_kill_then_restart_and_reconnect() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Attach a real thin client process and prove it reached attached state
    // by observing an incoming frame on its PTY stream.
    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);
    let mut thin_reader = thin_client
        ._master
        .try_clone_reader()
        .expect("clone thin client reader");

    let attached_before_kill = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut observed = false;
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            match thin_reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let out = String::from_utf8_lossy(&buf[..n]);
                    if out.contains("\u{2500}")
                        || out.contains("workspace")
                        || out.contains("pane")
                        || out.contains("terminal")
                    {
                        observed = true;
                        break;
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(30)),
                Err(_) => thread::sleep(Duration::from_millis(30)),
            }
        }
        observed
    };
    assert!(
        attached_before_kill,
        "thin client should complete attach before server SIGKILL"
    );

    // Kill server abruptly and verify thin client exits with lost-connection messaging.
    let server_pid = server.child.process_id().expect("server pid should exist");
    unsafe {
        libc::kill(server_pid as libc::pid_t, libc::SIGKILL);
    }
    assert!(
        wait_for_child_exit(&mut server.child, Duration::from_secs(5)),
        "server should exit after SIGKILL"
    );
    drop(server);

    let mut crash_output = String::new();
    let thin_exited = {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut exited = false;
        let mut buf = [0u8; 1024];
        while Instant::now() < deadline {
            if thin_client.child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            if let Ok(n) = thin_reader.read(&mut buf) {
                if n > 0 {
                    crash_output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        exited
    };
    assert!(thin_exited, "thin client should exit after server SIGKILL");

    let thin_status = thin_client
        .child
        .wait()
        .expect("wait for thin client exit status");
    assert!(
        !thin_status.success(),
        "thin client should exit non-zero after unexpected server crash"
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
        "thin client output must include explicit lost-connection message after server kill; output: {crash_output:?}"
    );

    // Restart server and verify new client can connect (stale socket cleaned).
    let server2 = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut reconnect_client =
        UnixStream::connect(&client_socket).expect("new client should connect after restart");
    client_handshake(&mut reconnect_client, 2, 80, 24);
    assert!(
        wait_for_frame(&mut reconnect_client, Duration::from_secs(5)),
        "new client should receive frame after restart"
    );

    let ping = ping_socket(&api_socket);
    assert!(
        ping.contains("pong"),
        "restarted server should respond over API: {ping}"
    );

    cleanup_spawned_herdr(server2, base);
}
