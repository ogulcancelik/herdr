//! Host integration helpers used by core modules that would otherwise need
//! `#[cfg(target_os = "…")]` branches.

#[cfg(unix)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Per-user config directory to use when neither `XDG_CONFIG_HOME` nor
/// `HOME` is set. On Windows we prefer `%APPDATA%`, then
/// `%USERPROFILE%\.config`, then the system temp directory. On Unix this
/// returns `None` and the caller falls back to `/tmp/<app>`.
pub fn fallback_config_dir(app_dir_name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Some(PathBuf::from(appdata).join(app_dir_name));
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return Some(PathBuf::from(profile).join(".config").join(app_dir_name));
        }
        return Some(std::env::temp_dir().join(app_dir_name));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app_dir_name;
        None
    }
}

/// Default interactive shell to spawn in a pane when the user hasn't
/// overridden `$SHELL`. POSIX systems use `/bin/sh`; Windows uses
/// `%ComSpec%` (cmd.exe) since the herdr pane reader expects a shell that
/// produces VT-escape output via ConPTY.
pub fn default_login_shell() -> String {
    if let Ok(value) = std::env::var("SHELL") {
        return value;
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(value) = std::env::var("ComSpec") {
            return value;
        }
        "cmd.exe".to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        "/bin/sh".to_string()
    }
}

/// Returns `(shell, flag)` suitable for running a single command string
/// via `shell <flag> "command"`. POSIX shells use `-c`; `cmd.exe` uses
/// `/C`.
pub fn shell_command_runner() -> (String, &'static str) {
    #[cfg(target_os = "windows")]
    {
        let shell = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        (shell, "/C")
    }
    #[cfg(not(target_os = "windows"))]
    {
        ("/bin/sh".to_string(), "-c")
    }
}

/// Classify whether `err` indicates "no server listening on the herdr
/// socket" so the CLI can print a friendly message instead of a raw
/// OS errno. On Windows the `uds_windows` AF_UNIX wrapper surfaces a
/// missing socket as several different winsock codes (WSAENETDOWN,
/// WSAECONNREFUSED, WSAENOTSOCK); on Unix only the standard error
/// kinds are produced.
/// Tighten the file-mode of a Unix-domain socket so only the current
/// user can connect. On Unix this calls `chmod` with the supplied mode.
/// Windows AF_UNIX sockets inherit ACLs from the parent directory, so
/// callers should place the socket inside a per-user config dir and this
/// becomes a no-op.
pub fn restrict_socket_permissions(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Configure `command` so that it runs as a detached background daemon
/// that survives the parent terminal closing. On Unix this puts the
/// child in its own process group so it doesn't receive `SIGHUP`. On
/// Windows it requests a hidden console (`CREATE_NO_WINDOW`) and a fresh
/// console process group, which keeps later `CreatePseudoConsole` calls
/// from flashing a real conhost window onto the desktop while still
/// isolating the daemon from Ctrl+C/Ctrl+Break delivered to the
/// launching shell.
pub fn detach_daemon_command(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = command;
    }
}

pub fn is_server_not_running_error(err: &io::Error) -> bool {
    if matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(code) = err.raw_os_error() {
            if matches!(code, 10050 | 10061 | 10038 | 2 | 3) {
                return true;
            }
        }
    }
    false
}
