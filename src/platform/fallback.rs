use std::path::PathBuf;
use std::process::Command;

use super::{ClipboardImage, ForegroundJob, Signal};

/// Unsupported platform stub.
pub fn raise_server_nofile_limit() {}

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    false
}

fn raw_command_argv(command: &str, flag: &str) -> Vec<std::ffi::OsString> {
    vec!["/bin/sh".into(), flag.into(), command.into()]
}

pub(crate) fn detached_custom_command_process_platform(command: &str) -> std::process::Command {
    let argv = raw_command_argv(command, "-lc");
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

pub(crate) fn pane_custom_command_pty_builder_platform(
    command: &str,
) -> portable_pty::CommandBuilder {
    portable_pty::CommandBuilder::from_argv(raw_command_argv(command, "-c"))
}

pub(crate) fn interactive_shell_command(_argv: &[String], _shell_name: &str) -> Option<String> {
    None
}

/// Unsupported platform stub.
pub(crate) fn scrollback_editor_argv(_path: &std::path::Path) -> std::io::Result<Vec<String>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening scrollback in an editor is not supported on this platform",
    ))
}

/// Unsupported platform stub.
pub fn detach_server_daemon_command(_command: &mut Command) {}

/// Unsupported platform stub.
pub fn current_process_is_detached_server_daemon() -> bool {
    false
}

pub(crate) fn available_pane_shell(_child_pid: u32) -> Option<String> {
    None
}

/// Unsupported platform stub.
pub fn foreground_job(_child_pid: u32) -> Option<ForegroundJob> {
    None
}

/// Unsupported platform stub.
pub fn foreground_group_leader_job(_process_group_id: u32) -> Option<ForegroundJob> {
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
pub fn open_url(_url: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening URLs is not supported on this platform",
    ))
}

/// Unsupported platform stub.
// Windows does not wire clipboard-image bridging into semantic input yet.
#[cfg_attr(windows, allow(dead_code))]
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

/// Unsupported platform stub.
pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}
