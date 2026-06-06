use std::io;
use std::path::{Path, PathBuf};

/// Legacy environment variable for overriding the client socket path.
///
/// Contractual override behavior for auto-detect uses `HERDR_SOCKET_PATH`.
/// This variable is kept as a fallback for callers that explicitly need a
/// client-only override when `HERDR_SOCKET_PATH` is not set.
pub const CLIENT_SOCKET_PATH_ENV_VAR: &str = "HERDR_CLIENT_SOCKET_PATH";

/// Socket permission mode (owner read/write only).
const SOCKET_PERMISSION_MODE: u32 = 0o600;

/// Returns the path for the client protocol socket.
///
/// Contract-aligned override behavior:
/// 1. If CLI `--session <name>` is active, use that session's client socket.
/// 2. If `HERDR_SOCKET_PATH` is set, derive the client socket path from it by
///    inserting `-client` before `.sock` (e.g. `herdr.sock` -> `herdr-client.sock`).
///    This keeps JSON API and client socket overrides consistent.
/// 3. Otherwise, honor `HERDR_CLIENT_SOCKET_PATH` (legacy/testing fallback).
/// 4. Otherwise, use the active session data directory.
pub fn client_socket_path() -> PathBuf {
    if crate::session::explicit_session_requested() {
        return crate::session::client_socket_path_for(crate::session::active_name().as_deref());
    }
    client_socket_path_from_overrides(
        std::env::var(crate::api::SOCKET_PATH_ENV_VAR)
            .ok()
            .as_deref(),
        std::env::var(CLIENT_SOCKET_PATH_ENV_VAR).ok().as_deref(),
    )
}

pub(crate) fn client_socket_path_from_overrides(
    api_socket_override: Option<&str>,
    client_socket_override: Option<&str>,
) -> PathBuf {
    if let Some(api_socket_override) = api_socket_override {
        return derive_client_socket_from_api_socket(Path::new(api_socket_override));
    }

    if let Some(client_socket_override) = client_socket_override {
        return PathBuf::from(client_socket_override);
    }

    crate::session::client_socket_path_for(crate::session::active_name().as_deref())
}

pub(crate) fn derive_client_socket_from_api_socket(api_socket_path: &Path) -> PathBuf {
    let stem = api_socket_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("herdr");
    let parent = api_socket_path.parent().unwrap_or_else(|| Path::new(""));

    if api_socket_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext == "sock")
    {
        return parent.join(format!("{stem}-client.sock"));
    }

    parent.join(format!("{stem}-client.sock"))
}

/// Prepares a socket path for binding: creates parent directories,
/// removes stale socket files where no server is listening, and rejects live
/// sockets that are already in use.
pub(crate) fn prepare_socket_path(path: &Path) -> io::Result<()> {
    crate::ipc::prepare_socket_path(path, |path| {
        format!(
            "herdr server is already running (socket busy at {})",
            path.display()
        )
    })
}

/// Restricts socket file permissions to owner-only (0o600).
pub(crate) fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    crate::ipc::restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    #[test]
    fn client_socket_path_derived_from_api_socket_override() {
        let path = client_socket_path_from_overrides(Some("/tmp/test-herdr.sock"), None);
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_api_override_takes_precedence_over_legacy_client_override() {
        let path = client_socket_path_from_overrides(
            Some("/tmp/test-herdr.sock"),
            Some("/tmp/legacy-client.sock"),
        );
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_respects_legacy_client_override_without_api_override() {
        let path = client_socket_path_from_overrides(None, Some("/tmp/test-herdr-client.sock"));
        assert_eq!(path, PathBuf::from("/tmp/test-herdr-client.sock"));
    }

    #[test]
    fn client_socket_path_defaults_to_config_dir() {
        std::env::remove_var(crate::session::SESSION_ENV_VAR);
        crate::session::clear_explicit_session_for_test();
        let path = client_socket_path_from_overrides(None, None);
        assert_eq!(path, crate::config::config_dir().join("herdr-client.sock"));
    }

    #[test]
    fn derive_client_socket_from_api_socket_without_sock_extension() {
        let derived = derive_client_socket_from_api_socket(Path::new("/tmp/custom-api"));
        assert_eq!(derived, PathBuf::from("/tmp/custom-api-client.sock"));
    }

    #[test]
    fn prepare_socket_path_removes_stale_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("stale.sock");

        {
            let _listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let result = prepare_socket_path(&socket_path);
        assert!(result.is_ok(), "should remove stale socket: {result:?}");
        assert!(!socket_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_socket_path_rejects_live_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("live.sock");

        let _listener = UnixListener::bind(&socket_path).expect("bind");

        let result = prepare_socket_path(&socket_path);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AddrInUse);

        let _ = fs::remove_dir_all(&dir);
    }
}
