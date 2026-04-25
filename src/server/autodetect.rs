//! Auto-detect launch behavior for the `herdr` command.
//!
//! When the user runs `herdr` with no subcommand:
//! 1. Check if a server is already listening on the client socket
//! 2. If no server → spawn one as a background daemon → wait for socket readiness (up to 5s)
//! 3. Attach as a thin client to the server
//!
//! The `--no-session` flag bypasses server/client entirely and runs monolithically
//! (escape hatch for users who want the traditional single-process behavior).

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use tracing::{info, warn};

use super::headless::client_socket_path;

/// Maximum time to wait for the server's client socket to become ready
/// after spawning the server process.
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval when waiting for the server socket to appear.
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Server detection
// ---------------------------------------------------------------------------

/// Checks whether a herdr server is currently listening on the client socket.
///
/// This works by attempting to connect to the client socket. If the connection
/// succeeds, a server is running. If the socket file doesn't exist or the
/// connection is refused, no server is running. Stale sockets (from a crashed
/// server) are detected because `UnixStream::connect` returns `ConnectionRefused`
/// when nobody is listening.
#[allow(dead_code)] // Public API for external use and testing
pub fn is_server_listening() -> bool {
    is_server_listening_at(&client_socket_path())
}

/// Checks whether a herdr server is listening at a specific socket path.
fn is_server_listening_at(socket_path: &Path) -> bool {
    if !socket_path.exists() {
        return false;
    }

    match UnixStream::connect(socket_path) {
        Ok(_) => {
            // Server is listening. Close the test connection immediately.
            // The server's handshake handler will time out on this connection
            // since we don't send Hello, which is fine.
            true
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
            ) =>
        {
            // Socket file exists but nobody is listening — stale socket.
            false
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Socket file disappeared between exists() and connect().
            false
        }
        Err(err) => {
            // Other errors (permission denied, etc.) — assume not listening.
            warn!(err = %err, "unexpected error checking server socket");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Server spawning
// ---------------------------------------------------------------------------

/// Spawns the herdr server as a background daemon process.
///
/// The server process is fully detached:
/// - Runs in its own session (setsid) so it survives the client exiting
/// - Stdin/stdout/stderr are redirected to /dev/null
/// - Inherits relevant environment variables (`XDG_CONFIG_HOME`, `HERDR_SESSION`,
///   socket overrides, etc.), except inherited socket overrides are cleared when
///   this CLI invocation explicitly selected a session.
///
/// Returns the PID of the spawned server process.
pub fn spawn_server_daemon() -> io::Result<u32> {
    let exe = std::env::current_exe().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to determine herdr executable path: {err}"),
        )
    })?;

    info!(exe = %exe.display(), "spawning server daemon");

    let mut command = build_server_daemon_command(exe);

    let child = command.spawn().map_err(|err: io::Error| {
        io::Error::new(err.kind(), format!("failed to spawn herdr server: {err}"))
    })?;

    let pid = child.id();
    info!(pid, "server daemon spawned");

    Ok(pid)
}

fn build_server_daemon_command(exe: PathBuf) -> Command {
    let mut command = Command::new(&exe);
    command
        .arg("server")
        // Create a new process group so the server survives the parent's exit
        // and doesn't receive SIGHUP when the client's terminal closes.
        .process_group(0)
        // Redirect stdio to /dev/null
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    if crate::session::explicit_session_requested() {
        command
            .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
            .env_remove("HERDR_CLIENT_SOCKET_PATH");
    }

    command
}

// ---------------------------------------------------------------------------
// Socket readiness
// ---------------------------------------------------------------------------

/// Waits for the server's client socket to become ready for connections.
///
/// Polls the socket path at regular intervals until a connection succeeds
/// or the timeout elapses. Returns an error if the server doesn't become
/// ready within the timeout.
pub fn wait_for_server_socket(socket_path: &Path, timeout: Duration) -> io::Result<()> {
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        if is_server_listening_at(socket_path) {
            info!(path = %socket_path.display(), "server socket ready");
            return Ok(());
        }
        std::thread::sleep(SOCKET_POLL_INTERVAL);
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "server did not become ready within {}s (socket: {})",
            timeout.as_secs(),
            socket_path.display()
        ),
    ))
}

// ---------------------------------------------------------------------------
// Auto-detect launch
// ---------------------------------------------------------------------------

/// Performs auto-detect launch: check for server, spawn if needed, then
/// attach as a thin client.
///
/// This is the entry point called from `main.rs` when the user runs `herdr`
/// without `--no-session` and without a subcommand.
///
/// Flow:
/// 1. Check if a server is listening on the client socket
/// 2. If no server → spawn server daemon → wait for socket readiness
/// 3. Run the thin client (which connects to the server)
pub fn auto_detect_launch() -> io::Result<()> {
    let socket_path = client_socket_path();
    info!(path = %socket_path.display(), "auto-detect launch starting");

    if is_server_listening_at(&socket_path) {
        info!("server already running, attaching as client");
    } else {
        info!("no server running, spawning server daemon");
        spawn_server_daemon()?;
        wait_for_server_socket(&socket_path, SERVER_READY_TIMEOUT)?;
        info!("server ready, attaching as client");
    }

    // Now attach as a thin client.
    crate::client::run_client()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::net::UnixListener;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::path::PathBuf::from(format!("/tmp/ha-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn is_server_listening_returns_false_for_nonexistent_path() {
        let dir = unique_test_dir("nonexistent");
        let path = dir.join("s.sock");
        assert!(!is_server_listening_at(&path));
    }

    #[test]
    fn server_daemon_command_clears_socket_overrides_for_explicit_session() {
        let _guard = env_lock().lock().unwrap();
        std::env::set_var(crate::api::SOCKET_PATH_ENV_VAR, "/tmp/inherited.sock");
        std::env::set_var("HERDR_CLIENT_SOCKET_PATH", "/tmp/inherited-client.sock");
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let args = vec![
            "herdr".to_string(),
            "--session".to_string(),
            "work".to_string(),
        ];
        crate::session::configure_from_args(&args).unwrap();

        let command = build_server_daemon_command(PathBuf::from("/tmp/herdr-test"));
        let envs: Vec<_> = command.get_envs().collect();

        assert!(envs.iter().any(|(key, value)| {
            *key == OsStr::new(crate::api::SOCKET_PATH_ENV_VAR) && value.is_none()
        }));
        assert!(envs.iter().any(|(key, value)| {
            *key == OsStr::new("HERDR_CLIENT_SOCKET_PATH") && value.is_none()
        }));
        std::env::remove_var(crate::api::SOCKET_PATH_ENV_VAR);
        std::env::remove_var("HERDR_CLIENT_SOCKET_PATH");
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
    }

    #[test]
    fn is_server_listening_returns_true_for_live_socket() {
        let dir = unique_test_dir("live");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        let _listener = UnixListener::bind(&path).unwrap();
        assert!(is_server_listening_at(&path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn is_server_listening_returns_false_for_stale_socket() {
        let dir = unique_test_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Create a socket and immediately drop the listener.
        // This leaves a stale socket file with nobody listening.
        {
            let _listener = UnixListener::bind(&path).unwrap();
        }

        // The socket file exists but nobody is listening.
        assert!(!is_server_listening_at(&path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn is_server_listening_returns_false_when_listener_dropped() {
        let dir = unique_test_dir("dropped");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Bind and immediately drop the listener.
        drop(UnixListener::bind(&path).unwrap());

        // Socket is stale — should return false.
        assert!(!is_server_listening_at(&path));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_succeeds_immediately() {
        let dir = unique_test_dir("wait-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        let _listener = UnixListener::bind(&path).unwrap();

        // Should succeed immediately (socket is already ready).
        let result = wait_for_server_socket(&path, Duration::from_millis(100));
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_times_out() {
        let dir = unique_test_dir("wait-timeout");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // No listener — should time out.
        let result = wait_for_server_socket(&path, Duration::from_millis(50));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wait_for_server_socket_succeeds_after_delay() {
        let dir = unique_test_dir("wait-delay");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");

        // Spawn a thread that will create the listener after a short delay.
        let path_clone = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _listener = UnixListener::bind(&path_clone).unwrap();
            // Keep the listener alive for a bit.
            std::thread::sleep(Duration::from_secs(1));
        });

        // Wait with a generous timeout — should succeed.
        let result = wait_for_server_socket(&path, Duration::from_secs(2));
        assert!(result.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
