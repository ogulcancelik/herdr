//! E2E: two sandboxed servers federate via the [[peers]] summary poll (over
//! a fake-ssh shim), the poller folds the peer's workspaces into the spaces
//! sidebar, and clicking a remote row yields ServerMessage::SwitchServer.

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use support::{
    cleanup_test_base, client_handshake, decode_varint_u32, read_server_message,
    register_runtime_dir, register_spawned_herdr_pid, send_input, unregister_spawned_herdr_pid,
    wait_for_file, wait_for_socket,
};

/// ServerMessage bincode variant indices (declaration order in wire.rs).
const VARIANT_FRAME: u32 = 1;
const VARIANT_SWITCH_SERVER: u32 = 9;

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-peer-federation-test-{}-{nanos}",
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

fn init_repo_with_origin(path: &Path, origin: &str) {
    fs::create_dir_all(path).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git init failed for {}", path.display());
    let status = std::process::Command::new("git")
        .args(["remote", "add", "origin", origin])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git remote add failed");
}

/// Create a workspace over the JSON API socket (fresh servers have none).
fn create_workspace(api_socket: &Path, cwd: &Path) {
    let mut stream = UnixStream::connect(api_socket).expect("API socket should connect");
    let request = format!(
        "{{\"id\":\"test:ws\",\"method\":\"workspace.create\",\"params\":{{\"cwd\":\"{}\",\"focus\":true}}}}\n",
        cwd.display()
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut response = String::new();
    std::io::BufRead::read_line(&mut reader, &mut response).unwrap();
    assert!(
        response.contains("\"result\""),
        "workspace.create failed: {response}"
    );
}

fn write_config(config_home: &Path, contents: &str) {
    // Debug builds read the herdr-dev app dir; release builds read herdr.
    for app_dir in ["herdr", "herdr-dev"] {
        let dir = config_home.join(app_dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), contents).unwrap();
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    cwd: &Path,
    extra_path: Option<&Path>,
) -> SpawnedHerdr {
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 90,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.cwd(cwd);
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    cmd.env("HERDR_DISABLE_SOUND", "1");
    if let Some(extra) = extra_path {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{}", extra.display(), path));
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct FrameWire {
    cells: Vec<CellWire>,
    width: u16,
    height: u16,
    cursor: Option<CursorWire>,
    hyperlinks: Vec<String>,
    graphics: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CellWire {
    symbol: String,
    fg: u32,
    bg: u32,
    modifier: u16,
    skip: bool,
    hyperlink: Option<u32>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CursorWire {
    x: u16,
    y: u16,
    visible: bool,
    shape: u8,
}

fn decode_frame_payload(payload: &[u8]) -> Option<FrameWire> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .ok()
        .map(|(frame, _): (FrameWire, usize)| frame)
}

fn frame_rows(frame: &FrameWire) -> Vec<String> {
    let width = frame.width.max(1) as usize;
    frame
        .cells
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol.as_str()).collect())
        .collect()
}

/// Read frames until one contains `needle`; return its 0-based row index.
fn wait_for_frame_row(
    stream: &mut UnixStream,
    needle: &str,
    timeout: Duration,
) -> Result<usize, String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let mut last_screen = String::new();
    while Instant::now() < deadline {
        match read_server_message(stream) {
            Ok((VARIANT_FRAME, payload)) => {
                if let Some(frame) = decode_frame_payload(&payload) {
                    let rows = frame_rows(&frame);
                    last_screen = rows.join("\n");
                    if let Some(row) = rows.iter().position(|row| row.contains(needle)) {
                        return Ok(row);
                    }
                }
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Err(format!(
        "timed out waiting for \"{needle}\" in a frame; last screen:\n{last_screen}"
    ))
}

/// Wait for a SwitchServer message and return its ssh_target.
fn wait_for_switch_server(stream: &mut UnixStream, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match read_server_message(stream) {
            Ok((VARIANT_SWITCH_SERVER, payload)) => {
                let (len, offset) = decode_varint_u32(&payload, 0)?;
                let end = offset + len as usize;
                let bytes = payload
                    .get(offset..end)
                    .ok_or_else(|| "truncated SwitchServer payload".to_string())?;
                return String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string());
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Err("timed out waiting for SwitchServer".into())
}

#[test]
fn peer_summary_folds_into_sidebar_and_click_switches_server() {
    let base = unique_test_dir();
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_herdr"))
        .parent()
        .unwrap()
        .to_path_buf();

    // --- Server B: the peer. Its workspace lives in a repo with an origin
    // unknown to A, so A renders it as a trailing remote-only project.
    let repo_b = base.join("proj");
    init_repo_with_origin(&repo_b, "git@github.com:peer-fed-test/proj.git");
    let config_home_b = base.join("config-b");
    let runtime_b = base.join("runtime-b");
    let socket_b = base.join("herdr-b.sock");
    write_config(&config_home_b, "onboarding = false\n");
    let server_b = spawn_server(&config_home_b, &runtime_b, &socket_b, &repo_b, None);
    wait_for_socket(&socket_b, Duration::from_secs(10));
    create_workspace(&socket_b, &repo_b);

    // --- Fake ssh: ignores the target, runs the summary command against B.
    let shim_dir = base.join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ssh");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor last; do :; done\nHERDR_SOCKET_PATH='{}' XDG_CONFIG_HOME='{}' PATH='{}':\"$PATH\" exec sh -c \"$last\"\n",
            socket_b.display(),
            config_home_b.display(),
            bin_dir.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    // --- Server A: federates with B.
    let repo_a = base.join("alpha");
    init_repo_with_origin(&repo_a, "git@github.com:peer-fed-test/alpha.git");
    let config_home_a = base.join("config-a");
    let runtime_a = base.join("runtime-a");
    let socket_a = base.join("herdr-a.sock");
    write_config(
        &config_home_a,
        "onboarding = false\n\n[[peers]]\nname = \"peerb\"\n",
    );
    let server_a = spawn_server(
        &config_home_a,
        &runtime_a,
        &socket_a,
        &repo_a,
        Some(&shim_dir),
    );
    wait_for_socket(&socket_a, Duration::from_secs(10));
    create_workspace(&socket_a, &repo_a);
    // Derived from HERDR_SOCKET_PATH: `-client` inserted before `.sock`.
    let client_socket_a = base.join("herdr-a-client.sock");
    wait_for_file(&client_socket_a, Duration::from_secs(10));

    // --- Attach a protocol client to A and wait for the folded remote row.
    let mut stream = UnixStream::connect(&client_socket_a).expect("client socket should connect");
    let (_, error) = client_handshake(&mut stream, 13, 90, 30).expect("handshake should complete");
    assert!(error.is_none(), "handshake rejected: {error:?}");

    // The first poll fires ~3s after A starts; allow generous slack.
    let row = wait_for_frame_row(&mut stream, "proj · ", Duration::from_secs(45))
        .expect("peer workspace should fold into the sidebar");

    // --- Click the remote row (SGR mouse, 1-based coordinates).
    let col = 4u16;
    let sgr_row = (row as u16) + 1;
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}M").as_bytes())
        .expect("mouse press should send");
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}m").as_bytes())
        .expect("mouse release should send");

    let target = wait_for_switch_server(&mut stream, Duration::from_secs(10))
        .expect("click on remote row should yield SwitchServer");
    assert_eq!(target, "peerb");

    drop(stream);
    drop(server_a);
    drop(server_b);
    cleanup_test_base(&base);
}
