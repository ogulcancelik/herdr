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
    cleanup_test_base, client_handshake, client_handshake_with_fleet, decode_varint_u32,
    read_server_message, register_runtime_dir, register_spawned_herdr_pid, send_input,
    unregister_spawned_herdr_pid, wait_for_file, wait_for_socket,
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

/// Read frames until one contains `needle` on a row that is INDENTED as a
/// group MEMBER (>= 2 leading spaces), then return its 0-based row index.
///
/// Since the restyle's uniform `<server>:<target>` grammar (#62) the LOCAL
/// section-head row also reads `host:branch` (e.g. ` ○ mba22:main`), so a bare
/// `starts_with(whitespace)` no longer distinguishes it from the folded REMOTE
/// member (`   ○ mba22:main`). The remote row folds UNDER the group as an
/// indented member, carrying the deeper 3-space member indent — this matches on
/// that member-level indent so the click lands on the remote card, not the
/// local head.
fn wait_for_indented_peer_row(
    stream: &mut UnixStream,
    needle: &str,
    timeout: Duration,
) -> Result<usize, String> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
    let deadline = Instant::now() + timeout;
    let mut last_screen = String::new();
    while Instant::now() < deadline {
        if let Ok((VARIANT_FRAME, payload)) = read_server_message(stream) {
            if let Some(frame) = decode_frame_payload(&payload) {
                let rows = frame_rows(&frame);
                last_screen = rows.join("\n");
                if let Some(row) = rows.iter().position(|r| {
                    let indent = r.len() - r.trim_start_matches(' ').len();
                    indent >= 2 && r.contains(needle)
                }) {
                    return Ok(row);
                }
            }
        }
    }
    Err(format!(
        "timed out waiting for indented \"{needle}\"; last screen:\n{last_screen}"
    ))
}

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

/// Read frames until one satisfies every needle; return that frame's rows.
/// (Sequential single-needle waits would consume frames between checks and
/// stall when the server has no reason to re-render.)
fn wait_for_frame_matching(
    stream: &mut UnixStream,
    needles: &[&str],
    timeout: Duration,
) -> Result<Vec<String>, String> {
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
                    if needles
                        .iter()
                        .all(|needle| rows.iter().any(|row| row.contains(needle)))
                    {
                        return Ok(rows);
                    }
                }
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Err(format!(
        "timed out waiting for {needles:?} in one frame; last screen:\n{last_screen}"
    ))
}

/// Wait for a SwitchServer message and return its ssh_target plus the raw
/// trailing `fleet: Option<FleetSnapshot>` bytes. A switching client carries
/// those bytes verbatim into its next Hello, so tests can splice them too.
fn wait_for_switch_server(
    stream: &mut UnixStream,
    timeout: Duration,
) -> Result<(String, Vec<u8>), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match read_server_message(stream) {
            Ok((VARIANT_SWITCH_SERVER, payload)) => {
                let (len, offset) = decode_varint_u32(&payload, 0)?;
                let end = offset + len as usize;
                let bytes = payload
                    .get(offset..end)
                    .ok_or_else(|| "truncated SwitchServer payload".to_string())?;
                let target = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
                return Ok((target, payload[end..].to_vec()));
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }
    Err("timed out waiting for SwitchServer".into())
}

/// Splits the focus_workspace option (always the FINAL SwitchServer field)
/// off a captured payload tail, returning just the fleet option bytes —
/// what the launcher hands the next leg's Hello. None = one 0x00 byte;
/// Some = 0x01 + len + utf8 id (ids are short; single-byte varint).
fn strip_focus_suffix(tail: &[u8]) -> &[u8] {
    if tail.last() == Some(&0) {
        return &tail[..tail.len() - 1];
    }
    for n in 3..=tail.len().min(48) {
        let suffix = &tail[tail.len() - n..];
        if suffix[0] == 1
            && suffix[1] as usize == n - 2
            && std::str::from_utf8(&suffix[2..]).is_ok()
        {
            return &tail[..tail.len() - n];
        }
    }
    tail
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
    let (_, error) = client_handshake(&mut stream, 18, 90, 30).expect("handshake should complete");
    assert!(error.is_none(), "handshake rejected: {error:?}");

    // The first poll fires ~3s after A starts; allow generous slack. The
    // servers section header renders once the peer summary lands.
    wait_for_frame_row(&mut stream, "servers", Duration::from_secs(45))
        .expect("servers section should appear once the peer is polled");
    // The remote-only project leader carries the #27 owner/repo identity
    // (#62): `peer-fed-test/proj · <host>:<branch>`, truncated to the sidebar.
    let row = wait_for_frame_row(&mut stream, "peer-fed-test/proj", Duration::from_secs(45))
        .expect("peer workspace should fold into the sidebar");

    // --- Click the remote row (SGR mouse, 1-based coordinates).
    let col = 4u16;
    let sgr_row = (row as u16) + 1;
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}M").as_bytes())
        .expect("mouse press should send");
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}m").as_bytes())
        .expect("mouse release should send");

    let (target, fleet_bytes) = wait_for_switch_server(&mut stream, Duration::from_secs(10))
        .expect("click on remote row should yield SwitchServer");
    assert_eq!(target, "peerb");
    // The switch carries a fleet snapshot (down-gossip): Some(...) tag.
    assert_eq!(fleet_bytes.first(), Some(&1u8));

    drop(stream);
    drop(server_a);
    drop(server_b);
    cleanup_test_base(&base);
}

/// E2E (issue #63, the live bug): when the peer's workspace shares a
/// project with a LOCAL checkout, its remote row folds INTO that project
/// block as an INDENTED member (not a trailing `proj ·` leader row).
/// Clicking that folded row must emit the same SwitchServer the band does —
/// the live report was that folded rows silently no-op'd.
#[test]
fn folded_remote_member_row_click_switches_server() {
    let base = unique_test_dir();
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_herdr"))
        .parent()
        .unwrap()
        .to_path_buf();

    // Shared origin: A and B both host a checkout of the SAME repo, so B's
    // workspace folds under A's local project block as an indented member.
    let shared_origin = "git@github.com:peer-fed-test/shared.git";

    // --- Server B: the peer, checkout of the shared repo.
    let repo_b = base.join("shared-b");
    init_repo_with_origin(&repo_b, shared_origin);
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

    // --- Server A: a LOCAL checkout of the shared repo, federates with B.
    let repo_a = base.join("shared-a");
    init_repo_with_origin(&repo_a, shared_origin);
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
    let client_socket_a = base.join("herdr-a-client.sock");
    wait_for_file(&client_socket_a, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&client_socket_a).expect("client socket should connect");
    let (_, error) = client_handshake(&mut stream, 18, 90, 30).expect("handshake should complete");
    assert!(error.is_none(), "handshake rejected: {error:?}");

    wait_for_frame_row(&mut stream, "servers", Duration::from_secs(45))
        .expect("servers section should appear once the peer is polled");
    // The folded member row reads `   <icon> <host>:<branch>` — INDENTED under
    // the local `shared` block as a group member (>= 2 leading spaces). Since
    // the #62 uniform grammar the LOCAL head row also reads `host:branch`
    // (` ○ mba22:main`), so we key on the deeper MEMBER indent, not just any
    // leading whitespace, to land on the remote card. It appears once the
    // peer's workspace summary lands (second poll). NOTE: in this single-host
    // harness the peer reports the same `mba22` hostname as the local server,
    // so we key on the indented `host:branch` shape, not the literal name.
    let row = wait_for_indented_peer_row(&mut stream, ":main", Duration::from_secs(60))
        .expect("peer workspace should fold into the local project block as an indented member");

    let col = 4u16;
    let sgr_row = (row as u16) + 1;
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}M").as_bytes())
        .expect("mouse press should send");
    send_input(&mut stream, format!("\x1b[<0;{col};{sgr_row}m").as_bytes())
        .expect("mouse release should send");

    let (target, _fleet) = wait_for_switch_server(&mut stream, Duration::from_secs(10))
        .expect("click on folded remote member row should yield SwitchServer (#63)");
    assert_eq!(target, "peerb");

    drop(stream);
    drop(server_a);
    drop(server_b);
    cleanup_test_base(&base);
}

/// E2E (hub-and-spoke, issue #36): a switch off the hub carries a fleet
/// snapshot; re-attaching to the spoke with that snapshot in the handshake
/// renders the pinned home row plus the carried peer rows in the servers
/// band, and selecting home yields a SwitchServer with the reserved home
/// target — the way back needs zero spoke-side ssh config.
#[test]
fn switch_snapshot_renders_home_row_on_spoke_and_home_switches_back() {
    let base = unique_test_dir();
    let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_herdr"))
        .parent()
        .unwrap()
        .to_path_buf();

    // --- Server B: the spoke the client leaps into.
    let repo_b = base.join("proj");
    init_repo_with_origin(&repo_b, "git@github.com:peer-fed-home/proj.git");
    let config_home_b = base.join("config-b");
    let runtime_b = base.join("runtime-b");
    let socket_b = base.join("herdr-b.sock");
    write_config(&config_home_b, "onboarding = false\n");
    let server_b = spawn_server(&config_home_b, &runtime_b, &socket_b, &repo_b, None);
    wait_for_socket(&socket_b, Duration::from_secs(10));
    create_workspace(&socket_b, &repo_b);

    // --- Fake ssh: target "ghost" is unreachable, anything else hits B.
    let shim_dir = base.join("bin");
    fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("ssh");
    fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprev=\"\"; target=\"\"\nfor a; do target=\"$prev\"; prev=\"$a\"; done\nif [ \"$target\" = \"ghost\" ]; then exit 255; fi\nHERDR_SOCKET_PATH='{}' XDG_CONFIG_HOME='{}' PATH='{}':\"$PATH\" exec sh -c \"$prev\"\n",
            socket_b.display(),
            config_home_b.display(),
            bin_dir.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    // --- Server A: the hub. Two peers, so the snapshot still carries one
    // entry (ghost) after the hop target (peerb) is excluded.
    let repo_a = base.join("alpha");
    init_repo_with_origin(&repo_a, "git@github.com:peer-fed-home/alpha.git");
    let config_home_a = base.join("config-a");
    let runtime_a = base.join("runtime-a");
    let socket_a = base.join("herdr-a.sock");
    write_config(
        &config_home_a,
        "onboarding = false\n\n[[peers]]\nname = \"peerb\"\n\n[[peers]]\nname = \"ghost\"\n",
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
    let client_socket_a = base.join("herdr-a-client.sock");
    wait_for_file(&client_socket_a, Duration::from_secs(10));

    // --- Leap off the hub: click peerb's folded remote row. 45 rows: with
    // the pinned menu band (#41) and the half-height servers band (two
    // config peers = three 2-line slots), a 30-row frame leaves the spaces
    // body too short for the trailing remote-only project row — it falls
    // below the fold and only a scrollbar shows. Taller frame keeps the
    // whole list on screen; the folding itself is covered by unit tests.
    let mut stream = UnixStream::connect(&client_socket_a).expect("client socket should connect");
    let (_, error) = client_handshake(&mut stream, 18, 90, 45).expect("handshake should complete");
    assert!(error.is_none(), "handshake rejected: {error:?}");
    // Owner/repo identity on the remote-only project leader (#62/#27).
    let row = wait_for_frame_row(&mut stream, "peer-fed-home/proj", Duration::from_secs(45))
        .expect("peer workspace should fold into the sidebar");
    let sgr_row = (row as u16) + 1;
    send_input(&mut stream, format!("\x1b[<0;4;{sgr_row}M").as_bytes()).expect("mouse press");
    send_input(&mut stream, format!("\x1b[<0;4;{sgr_row}m").as_bytes()).expect("mouse release");

    let (target, fleet_bytes) = wait_for_switch_server(&mut stream, Duration::from_secs(10))
        .expect("click on remote row should yield SwitchServer");
    assert_eq!(target, "peerb");
    // Down-gossip: the switch carries Some(fleet) with the ghost peer in it
    // (the hop target itself is excluded from its own snapshot).
    assert_eq!(fleet_bytes.first(), Some(&1u8));
    assert!(
        fleet_bytes.windows(5).any(|w| w == b"ghost"),
        "snapshot should carry the ghost peer"
    );
    assert!(
        !fleet_bytes.windows(5).any(|w| w == b"peerb"),
        "snapshot must exclude the hop target"
    );
    // #66: the hub stamps its OWN workspaces into the snapshot (origin
    // summary), so the spoke can see the way-home spaces. The hub's workspace
    // lives in the "alpha" repo.
    assert!(
        fleet_bytes.windows(5).any(|w| w == b"alpha"),
        "snapshot should carry the hub's own (origin) workspace"
    );
    drop(stream);

    // --- Re-attach to the spoke carrying the snapshot bytes verbatim —
    // exactly what the launcher's next leg does.
    let client_socket_b = base.join("herdr-b-client.sock");
    wait_for_file(&client_socket_b, Duration::from_secs(10));
    let mut stream_b =
        UnixStream::connect(&client_socket_b).expect("spoke client socket should connect");
    let fleet_only = strip_focus_suffix(&fleet_bytes);
    let (_, error) = client_handshake_with_fleet(&mut stream_b, 18, 90, 45, fleet_only)
        .expect("spoke handshake should complete");
    assert!(error.is_none(), "spoke handshake rejected: {error:?}");

    // The spoke renders the carried fleet in one frame: the pinned home
    // row plus the carried (render-only) ghost row.
    let rows = wait_for_frame_matching(&mut stream_b, &["←", "ghost"], Duration::from_secs(30))
        .expect("home row and snapshot peer should render on the spoke");
    let home_row = rows
        .iter()
        .position(|row| row.contains('←'))
        .expect("home row present");
    assert!(
        rows[home_row].contains(" home"),
        "home row should be marked: {}",
        rows[home_row]
    );

    // --- #66: the hub's OWN workspace (alpha) folds into the spoke's spaces
    // list as a remote-only project row. Clicking it lands HOME (the spoke
    // has no ssh route to the hub) carrying the workspace as a focus target.
    let alpha_row = wait_for_frame_row(&mut stream_b, "alpha", Duration::from_secs(30))
        .expect("hub's own workspace should fold into the spoke's spaces list (#66)");
    let sgr_row = (alpha_row as u16) + 1;
    send_input(&mut stream_b, format!("\x1b[<0;4;{sgr_row}M").as_bytes()).expect("mouse press");
    send_input(&mut stream_b, format!("\x1b[<0;4;{sgr_row}m").as_bytes()).expect("mouse release");

    let (target, trailing) = wait_for_switch_server(&mut stream_b, Duration::from_secs(10))
        .expect("clicking the origin workspace row should yield SwitchServer");
    assert_eq!(target, "<home>", "origin workspace lands home, never ssh");
    // Trailing bytes = fleet (None: 0u8) then focus_workspace (Some + id).
    assert_eq!(trailing.first(), Some(&0u8), "home carries no fleet");
    // Structure: fleet None (0), then focus Some (1, len, utf8 workspace id).
    assert_eq!(trailing.get(1), Some(&1u8), "focus target present");
    let focus_len = *trailing.get(2).expect("focus length") as usize;
    let focus_id = std::str::from_utf8(&trailing[3..3 + focus_len]).expect("utf8 focus id");
    assert!(
        !focus_id.is_empty(),
        "origin workspace click carries a focus target ({trailing:?})"
    );

    // --- Select home directly: the spoke answers with the reserved home
    // target, no fleet, and no focus target — the client re-attaches locally.
    let sgr_row = (home_row as u16) + 1;
    send_input(&mut stream_b, format!("\x1b[<0;4;{sgr_row}M").as_bytes()).expect("mouse press");
    send_input(&mut stream_b, format!("\x1b[<0;4;{sgr_row}m").as_bytes()).expect("mouse release");

    let (target, trailing) = wait_for_switch_server(&mut stream_b, Duration::from_secs(10))
        .expect("selecting home should yield SwitchServer");
    assert_eq!(target, "<home>"); // protocol::HOME_SWITCH_TARGET
    assert_eq!(trailing.first(), Some(&0u8), "home carries no fleet");

    drop(stream_b);
    drop(server_a);
    drop(server_b);
    cleanup_test_base(&base);
}
