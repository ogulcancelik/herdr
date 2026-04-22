use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

use super::{ClipboardCommand, ForegroundJob, ForegroundProcess, Signal};

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let tpgid = foreground_pgid(child_pid)? as u32;
    let mut processes = Vec::new();

    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let file_name = entry.file_name();
        let pid_str = file_name.to_str()?;
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let pid: u32 = match pid_str.parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };

        let Some((pgrp, name)) = process_pgrp_and_comm(pid) else {
            continue;
        };
        if pgrp as u32 != tpgid {
            continue;
        }

        processes.push(ForegroundProcess {
            pid,
            name,
            argv0: None,
            cmdline: process_cmdline(pid),
        });
    }

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: tpgid,
        processes,
    })
}

fn foreground_pgid(child_pid: u32) -> Option<i32> {
    // /proc/<pid>/stat format: "pid (comm) state ppid pgrp session tty_nr tpgid ..."
    // The (comm) field can contain spaces and parens, so we find the last ')' first.
    let stat = std::fs::read_to_string(format!("/proc/{child_pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid)
}

fn process_pgrp_and_comm(pid: u32) -> Option<(i32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let comm = stat.get(1 + stat.find('(')?..close)?.to_string();
    let rest = stat.get(close + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgrp: i32 = fields.get(2)?.parse().ok()?;
    Some((pgrp, comm))
}

fn process_cmdline(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Get the current working directory of a process.
/// Uses /proc/<pid>/cwd symlink.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    let Some(session_id) = process_session_id(child_pid) else {
        return Vec::new();
    };

    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if process_session_id(pid) == Some(session_id) {
            pids.push(pid);
        }
    }
    pids
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    let sig = match signal {
        Signal::Hangup => libc::SIGHUP,
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };

    for &pid in pids {
        if pid == 0 {
            continue;
        }
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    for command in clipboard_commands() {
        if run_clipboard_command(&command, bytes) {
            return true;
        }
    }
    false
}

fn clipboard_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-copy",
            args: &["--type", "text/plain;charset=utf-8"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-in"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--input"],
        });
    }

    commands
}

fn run_clipboard_command(command: &ClipboardCommand, bytes: &[u8]) -> bool {
    let mut child = match Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    if stdin.write_all(bytes).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    drop(stdin);

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn process_session_id(pid: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn clipboard_commands_prefer_wayland_when_available() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::remove_var("DISPLAY");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "wl-copy");
    }

    #[test]
    fn clipboard_commands_include_x11_fallbacks() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, "xclip");
        assert_eq!(commands[1].program, "xsel");
    }
}
