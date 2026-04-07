use std::path::PathBuf;

use super::{ForegroundJob, Signal};

/// Unsupported platform stub.
pub fn foreground_job(_child_pid: u32) -> Option<ForegroundJob> {
    None
}

/// Unsupported platform stub.
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// Unsupported platform stub.
pub fn session_processes(_child_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Unsupported platform stub.
pub fn signal_processes(_pids: &[u32], _signal: Signal) {}

/// Unsupported platform stub.
pub fn process_exists(_pid: u32) -> bool {
    false
}

pub async fn wait_for_shutdown_request() -> std::io::Result<()> {
    // Unsupported platforms have no reliable app-level shutdown hook yet.
    // Do not intercept process-level Ctrl-C here; that must keep reaching the
    // foreground program running inside the pane.
    std::future::pending::<std::io::Result<()>>().await
}
