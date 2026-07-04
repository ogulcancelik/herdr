//! Two-server host-link integration coverage (Task 11 of the multi-host
//! plan: `host.attach`/`host.list`/reconnect/offline/restart against a real
//! second server, plus the homelab manual checklist).
//!
//! ## Where the real host.attach/reconnect/offline/restart test actually lives
//!
//! The plan's guess was that the server-under-test could point a
//! `UnixSocketTransport` at a second real server through a test-only env
//! override (something like `HERDR_TEST_HOST_SOCKET_DIR`). That hook does
//! not exist. What Task 9 actually built is
//! `HeadlessServer::host_transport_override_for_test`: a private field (set
//! by direct field assignment, not a public setter) that makes `host.attach`
//! build its transport from an in-process closure instead of `SshTransport`.
//! It is declared with `#[cfg(unix)]`, not `#[cfg(test)]`, so it compiles
//! into every unix build -- but being a *private* field, "reachable" is
//! purely a question of module visibility, and that is where this plan
//! step's premise breaks down:
//!
//! `herdr`'s `Cargo.toml` has a `[[bin]]` target (`src/main.rs`) and NO
//! `[lib]` target at all. Every file under `tests/` (this crate's
//! integration tests, this file included) is therefore its own separate
//! crate that links against... nothing of `herdr`'s internals. Confirmed by
//! grep: no `tests/*.rs` file anywhere in this repo has ever written `use
//! herdr::`. Every existing integration test instead drives the *compiled
//! binary* as a child process (`env!("CARGO_BIN_EXE_herdr")`) and speaks to
//! it purely over the JSON API / client wire protocol (see
//! `tests/server_headless.rs`'s `spawn_server` + `ping_socket`, which this
//! file's `spawn_server` below mirrors). There is no Rust-level seam from a
//! file in `tests/` into `src/` at all -- `host_transport_override_for_test`
//! is exactly as unreachable here as every other private item in the crate,
//! and adding one (a `[lib]` target, or a `#[cfg(feature = "test-support")]`
//! public setter) would be a real architecture change, not a one-line test
//! fixture.
//!
//! (`CARGO_BIN_EXE_herdr`, which every other integration test relies on to
//! find the binary, makes the same point from the other direction: it is
//! only defined for genuine integration-test/bench/example targets, not for
//! a bin crate's own `#[cfg(test)]` unit tests. A probe placed directly in
//! `src/server/headless.rs` and compiled with `cargo test --bin herdr`
//! failed with "environment variable `CARGO_BIN_EXE_herdr` not defined at
//! compile time" -- confirming empirically that even the child-process
//! pattern above cannot be turned around and used from inside the crate's
//! own unit tests either. The two worlds -- in-crate unit tests with access
//! to `host_transport_override_for_test`, and `tests/*.rs` integration tests
//! with access to `CARGO_BIN_EXE_herdr` -- are mutually exclusive.)
//!
//! So the genuine, transport-swapped two-server test lives in-crate, next to
//! the fake-remote host-link tests it strengthens:
//! `src/server/headless.rs`, `mod tests::host_lifecycle`, test fn
//! `attach_reconnect_offline_and_restart_against_a_real_second_server`.
//! Unlike every other test in that module (which scripts a bare
//! `UnixListener` to play canned wire-format lines), it spins up a second,
//! REAL `HeadlessServer` -- the exact `HeadlessServer::new` + `run()` path
//! `run_server()` drives in production, with one real workspace/pane -- on
//! its own real API + client sockets, and attaches the server-under-test to
//! it via a real `UnixSocketTransport`. It:
//! - attaches and asserts `host.list` shows `connected` with the real pane
//!   count from the real second server's `pane.list`/`events.subscribe`;
//! - kills the real second server (a full, real teardown of its run loop
//!   and both real sockets -- not a dropped scripted connection) and asserts
//!   the link degrades through `reconnecting` to the terminal `offline`
//!   state;
//! - restarts a fresh real second server on the exact same socket paths and
//!   confirms the link becomes `connected` again. Since
//!   `HostLinkRegistry::on_disconnect` documents `Offline` as terminal for
//!   *automatic* retries ("manual retry is modeled as detach + attach"),
//!   this last step goes through a fresh `host.detach` + `host.attach`,
//!   exactly like a homelab user manually reattaching after noticing a host
//!   went offline -- not an unattended auto-reconnect, which the state
//!   machine does not do once a link is fully offline.
//!
//! Run it with:
//! ```text
//! cargo nextest run --bin herdr host_lifecycle::attach_reconnect_offline_and_restart_against_a_real_second_server
//! ```
//!
//! ## What this file DOES cover
//!
//! A genuine two-*process* sanity check, fully black-box (no Rust-level
//! access to `src/`, matching every other file in `tests/`): two real
//! `herdr server` child processes, each with its own socket pair, running
//! concurrently and independently. It does not (and structurally cannot,
//! per the above) exercise `host.attach`/ssh/`UnixSocketTransport` -- for
//! that, see the in-crate test named above. What it does prove at the
//! process level: two herdr servers can run side by side without
//! interfering with each other, and killing one has no effect on the other
//! (no shared global socket/lock/state at the OS level) -- a real-process
//! complement to the in-crate link-lifecycle coverage.
//!
//! ## Homelab manual checklist
//!
//! Everything above is automated coverage in a headless CI-like
//! environment. The following steps need a real second machine (a laptop
//! plus the Debian homelab server, linked over Tailscale) and cannot be run
//! here -- do them by hand before relying on multi-host in production:
//!
//! ```text
//! [ ] scp target/release/herdr to the server OR let bootstrap install it
//!     (HERDR_REMOTE_BINARY=target/release/herdr for the local build)
//! [ ] on laptop: herdr  ->  herdr host attach <server-alias>
//! [ ] server's agents appear grouped in sidebar with live status
//! [ ] open/focus a remote pane: frames render, typing reaches the agent
//! [ ] pull ethernet/VPN: only that host's group degrades; local panes fine
//! [ ] restore link: panes resume without restart
//! [ ] herdr server stop + restart client: hosts reattach from session
//! ```
//!
//! IMPORTANT caveat on "open/focus a remote pane: frames render, typing
//! reaches the agent": this depends on terminal-frame streaming that is
//! DEFERRED. Task 8 built `RemotePaneAttach`, but Task 9 explicitly left it
//! unwired from the emulator/visibility hook (tracked as "Task 9b", not done
//! on this branch). `HeadlessServer::handle_host_event`'s
//! `HostEvent::TerminalBytes` and `HostEvent::AttachFailed` arms are both
//! literally no-ops today with a `// Seam for Task 9b` comment -- nothing
//! spawns a `RemotePaneAttach` for a focused remote pane yet. So on this
//! branch, expect the sidebar/status/attach/detach/persist checklist items
//! above to work; do NOT expect the "frames render, typing reaches the
//! agent" item to work until Task 9b lands -- a homelab tester hitting a
//! blank/unresponsive remote pane is not a regression, it is exactly the
//! documented current state.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, wait_for_socket,
};

fn unique_test_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-multi-host-{label}-{}-{nanos}",
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
        unregister_spawned_herdr_pid(pid);
    }
}

fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

/// Serializes the two servers' construction against any other test in this
/// binary that might spawn a `herdr server` of its own -- mirrors
/// `tests/server_headless.rs`'s `test_lock`, defense in depth for a plain
/// `cargo test` run (nextest, which `just check`/`just test` use, already
/// isolates each test in its own process).
fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawns a real `herdr server` child process on its own socket pair --
/// mirrors `tests/server_headless.rs`'s `spawn_server` exactly (same launch
/// pattern: a PTY slave as the controlling terminal so the daemon doesn't
/// need one of its own, `HERDR_SOCKET_PATH` pointing at a fresh runtime
/// dir).
fn spawn_server(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
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

fn ping_socket(socket_path: &std::path::Path) -> String {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket_path).expect("connect to API socket");
    let request = r#"{"id":"1","method":"ping","params":{}}"#;
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response.trim().to_string()
}

/// Two real `herdr server` processes, each with its own socket pair, run
/// concurrently without interfering with each other, and killing one has no
/// observable effect on the other. This is a real-OS-process complement to
/// the in-crate `attach_reconnect_offline_and_restart_against_a_real_second_server`
/// test (see this file's module doc): that test proves the host-link state
/// machine against a real second server's real API; this test proves two
/// real servers are genuinely independent at the process/socket level, with
/// no shared global state to accidentally leak between a homelab laptop
/// server and a homelab remote server.
#[test]
fn two_independent_real_servers_do_not_share_state_and_survive_each_others_death() {
    let _lock = test_lock();

    let base_a = unique_test_dir("a");
    let config_a = base_a.join("config");
    let runtime_a = base_a.join("runtime");
    let api_a = runtime_a.join("herdr.sock");

    let base_b = unique_test_dir("b");
    let config_b = base_b.join("config");
    let runtime_b = base_b.join("runtime");
    let api_b = runtime_b.join("herdr.sock");

    let server_a = spawn_server(&config_a, &runtime_a, &api_a);
    let server_b = spawn_server(&config_b, &runtime_b, &api_b);

    wait_for_socket(&api_a, Duration::from_secs(10));
    wait_for_socket(&api_b, Duration::from_secs(10));

    let response_a = ping_socket(&api_a);
    let response_b = ping_socket(&api_b);
    assert!(
        response_a.contains("pong"),
        "server A should pong: {response_a}"
    );
    assert!(
        response_b.contains("pong"),
        "server B should pong: {response_b}"
    );

    // Kill server A only; server B (a fully independent process with its
    // own socket pair) must be completely unaffected.
    cleanup_spawned_herdr(server_a, base_a);

    let response_b_after = ping_socket(&api_b);
    assert!(
        response_b_after.contains("pong"),
        "server B must survive server A's death untouched: {response_b_after}"
    );

    cleanup_spawned_herdr(server_b, base_b);
}
