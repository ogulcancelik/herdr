use std::path::PathBuf;

use super::{ClipboardImage, ForegroundJob, Signal};

/// Unsupported platform stub.
pub fn foreground_job(_child_pid: u32) -> Option<ForegroundJob> {
    None
}

/// Unsupported platform stub.
pub fn foreground_process_group_id(_child_pid: u32) -> Option<u32> {
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

/// Unsupported platform stub.
pub fn write_clipboard(_bytes: &[u8]) -> bool {
    false
}

/// Unsupported platform stub.
pub fn read_clipboard_text() -> Option<String> {
    None
}

/// Unsupported platform stub.
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

/// Unsupported platform stub.
pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}
