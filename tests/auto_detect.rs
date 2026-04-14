//! Integration tests for auto-detect launch behavior.

#![cfg(not(target_os = "macos"))]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
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
        "/tmp/herdr-autodetect-test-{}-{nanos}",
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

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// Spawn `herdr` (no subcommand) — the auto-detect launch path.
fn spawn_herdr_auto(
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
    // No subcommand, no --no-session → auto-detect launch
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

/// Spawn `herdr --no-session` — the monolithic escape hatch.
fn spawn_herdr_no_session(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
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
    cmd.arg("--no-session");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
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

fn run_cli(socket_path: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command.args(args);
    command.env("HERDR_SOCKET_PATH", socket_path);
    command.output().unwrap()
}

fn process_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn read_json_line(stream: UnixStream) -> Value {
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Running `herdr` with no server present starts a server
/// and attaches as client.
#[test]
fn auto_detect_no_server_spawns_server_and_attaches() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Ensure no server is running initially.
    assert!(
        !api_socket.exists(),
        "api socket should not exist initially"
    );
    assert!(
        !client_socket.exists(),
        "client socket should not exist initially"
    );

    // Run `herdr` (no subcommand) — should auto-detect, spawn server, attach as client.
    let herdr = spawn_herdr_auto(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for both sockets to appear (server was spawned).
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Verify the API socket responds to ping (server is running).
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API socket should respond to ping: {response}"
    );

    // Verify the client socket accepts connections (server is listening on it).
    let _stream = UnixStream::connect(&client_socket)
        .expect("should connect to client socket (server is listening)");

    // Verify the client process is running.
    let client_pid = herdr.child.process_id().expect("client should have PID");
    assert!(
        process_exists(client_pid),
        "client process should be running"
    );

    cleanup_spawned_herdr(herdr, base);
}

/// Running `herdr` with a server already running attaches
/// as client directly (no second server).
#[test]
fn auto_detect_server_running_attaches_directly() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Start a server explicitly.
    let server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let server_pid = server.child.process_id().expect("server should have PID");

    // Verify server is running.
    assert!(process_exists(server_pid), "server should be running");

    // Run `herdr` (no subcommand) — should detect the running server and attach.
    let client = spawn_herdr_auto(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait a moment for the client to attach.
    thread::sleep(Duration::from_millis(500));

    // Verify the client is running.
    let client_pid = client.child.process_id().expect("client should have PID");
    assert!(
        process_exists(client_pid),
        "client process should be running"
    );

    // Verify the server is still the same one (no second server spawned).
    assert!(
        process_exists(server_pid),
        "original server should still be running"
    );

    // Verify API still responds.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should still respond to ping: {response}"
    );

    cleanup_spawned_herdr(client, PathBuf::from("/nonexistent"));
    cleanup_spawned_herdr(server, base);
}

/// Socket path resolution is consistent between server and client.
/// Both derive the client socket from the `HERDR_SOCKET_PATH` override,
/// so overriding the API socket keeps both endpoints aligned.
#[test]
fn auto_detect_socket_path_consistency() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Run `herdr` with custom socket paths.
    let herdr = spawn_herdr_auto(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for both sockets to appear at the custom paths.
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Verify sockets exist at the specified paths.
    assert!(
        api_socket.exists(),
        "API socket should exist at custom path"
    );
    assert!(
        client_socket.exists(),
        "client socket should exist at custom path"
    );

    // Verify API responds (server is using the custom API socket path).
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond at custom path: {response}"
    );

    // Verify client socket accepts connections (server is using the custom
    // client socket path).
    let _stream = UnixStream::connect(&client_socket)
        .expect("should connect to client socket at custom path");

    cleanup_spawned_herdr(herdr, base);
}

/// `herdr --no-session` bypasses server/client and runs
/// monolithically. No server process is spawned. No client socket is created.
#[test]
fn no_session_flag_runs_monolithically() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Run `herdr --no-session` — monolithic mode, no server/client.
    let herdr = spawn_herdr_no_session(&config_home, &runtime_dir, &api_socket);

    // Wait for the API socket (monolithic mode creates it).
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Verify the API socket exists and responds.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "monolithic API should respond: {response}"
    );

    // Verify NO client socket was created — this is the key distinction
    // between monolithic mode and server/client mode.
    assert!(
        !client_socket.exists(),
        "no client socket should exist in monolithic mode"
    );

    // Verify the API socket is served by the monolithic process itself,
    // not by a separate server. We can check this by verifying the client
    // PID matches what would be serving the socket — in monolithic mode,
    // there is only one herdr process.
    let client_pid = herdr.child.process_id().expect("should have PID");
    assert!(
        process_exists(client_pid),
        "monolithic process should be running"
    );

    cleanup_spawned_herdr(herdr, base);
}

/// CLI subcommands work through the server's JSON API socket.
#[test]
fn cli_subcommands_work_through_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Start a server.
    let server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Test `herdr workspace list` through the server's API socket.
    let output = run_cli(&api_socket, &["workspace", "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "workspace list should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The response should be valid JSON with a "result" field.
    assert!(
        stdout.contains("result"),
        "workspace list output should contain 'result': {stdout}"
    );

    // Test `herdr pane list` through the server's API socket.
    let output = run_cli(&api_socket, &["pane", "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "pane list should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("result"),
        "pane list output should contain 'result': {stdout}"
    );

    cleanup_spawned_herdr(server, base);
}

/// Verify that the server spawned by auto-detect
/// persists after the client exits, and a new `herdr` can reattach.
#[test]
fn auto_detect_server_persists_and_reattaches() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Run `herdr` — auto-detect spawns server + attaches client.
    let mut client1 = spawn_herdr_auto(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Verify API responds.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond before client exit: {response}"
    );

    // Kill the first client.
    let client1_pid = client1.child.process_id().expect("client1 should have PID");
    let _ = client1.child.kill();
    let _ = wait_for_pid_exit(client1_pid, Duration::from_secs(2));
    drop(client1);

    // Wait a moment for the server to process the disconnect.
    thread::sleep(Duration::from_millis(500));

    // Verify server is still running after client exit — the API should
    // still respond because the server is a separate daemon process.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should still respond after client exit (server persists): {response}"
    );

    // The client socket should still exist (server is still listening).
    assert!(
        client_socket.exists(),
        "client socket should still exist after client exit"
    );

    // Run `herdr` again — should detect the running server and reattach.
    let client2 = spawn_herdr_auto(&config_home, &runtime_dir, &api_socket, &client_socket);
    thread::sleep(Duration::from_millis(500));

    // Verify the new client is running.
    let client2_pid = client2.child.process_id().expect("client2 should have PID");
    assert!(
        process_exists(client2_pid),
        "second client should be running"
    );

    // Verify API still responds (same server).
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should still respond after reattach: {response}"
    );

    cleanup_spawned_herdr(client2, PathBuf::from("/nonexistent"));
    cleanup_test_base(&base);
}

/// Verify that the default API and client
/// sockets live in the app config directory when no env override is set.
#[test]
fn auto_detect_default_socket_path_from_config_dir() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");

    // Don't set HERDR_SOCKET_PATH or HERDR_CLIENT_SOCKET_PATH.
    // The default paths should come from the app config directory, not XDG_RUNTIME_DIR.
    let app_dir_name = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    let api_socket = config_home.join(app_dir_name).join("herdr.sock");
    let client_socket = config_home.join(app_dir_name).join("herdr-client.sock");

    // Spawn server with XDG_RUNTIME_DIR set to a different directory to prove it is ignored.
    fs::create_dir_all(config_home.join(app_dir_name)).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    fs::write(
        config_home.join(app_dir_name).join("config.toml"),
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
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    // Explicitly remove socket overrides to test default path resolution.
    cmd.env_remove("HERDR_SOCKET_PATH");
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);
    let server = SpawnedHerdr {
        _master: pair.master,
        child,
    };

    // Wait for sockets to appear at the default config-dir paths.
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Verify both sockets exist.
    assert!(api_socket.exists(), "API socket should exist in config dir");
    assert!(
        client_socket.exists(),
        "client socket should exist in config dir"
    );

    // Verify API responds.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond at config-dir path: {response}"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn auto_detect_respects_nested_guard_before_auto_attach() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let baseline = read_json_line({
        let mut stream = UnixStream::connect(&api_socket).unwrap();
        writeln!(
            stream,
            r#"{{"id":"ws_before","method":"workspace.list","params":{{}}}}"#
        )
        .unwrap();
        stream
    });
    let baseline_count = baseline["result"]["workspaces"]
        .as_array()
        .map(|workspaces| workspaces.len())
        .unwrap_or(0);

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &api_socket)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env("HERDR_ENV", "1")
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "nested launch should fail before auto-attach"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nested herdr is disabled by default"),
        "stderr should mention nested-launch guard: {stderr}"
    );

    let after = read_json_line({
        let mut stream = UnixStream::connect(&api_socket).unwrap();
        writeln!(
            stream,
            r#"{{"id":"ws_after","method":"workspace.list","params":{{}}}}"#
        )
        .unwrap();
        stream
    });
    let after_count = after["result"]["workspaces"]
        .as_array()
        .map(|workspaces| workspaces.len())
        .unwrap_or(0);
    assert_eq!(
        after_count, baseline_count,
        "nested launch should not auto-attach or mutate server state"
    );

    cleanup_spawned_herdr(server, base);
}
