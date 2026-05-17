//! Windows platform stubs.
//!
//! Implements the platform module API surface using std-only operations.
//! Agent detection that relies on Unix process group semantics returns empty
//! results; the rest of herdr continues to work as a plain multiplexer.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{ClipboardCommand, ForegroundJob, ForegroundProcess, Signal};

/// Switch the stdin console handle into Virtual Terminal input mode and
/// turn off line-buffering / echo / Ctrl+C cooking so VT mouse and key
/// sequences arrive at the raw byte reader instead of being consumed by
/// conhost. Idempotent and safe to call when stdin is redirected (returns
/// Ok in that case).
pub fn enable_vt_input() -> io::Result<()> {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_EXTENDED_FLAGS, ENABLE_MOUSE_INPUT,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_WINDOW_INPUT, STD_INPUT_HANDLE,
    };

    unsafe {
        let handle: HANDLE = GetStdHandle(STD_INPUT_HANDLE);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Ok(());
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            // Stdin isn't a console (redirected to a pipe/file). That is
            // not an error for our purposes; the byte reader still works.
            return Ok(());
        }
        // Clear ENABLE_LINE_INPUT (2), ENABLE_ECHO_INPUT (4) and
        // ENABLE_PROCESSED_INPUT (1) — same flags crossterm clears for
        // raw mode — and add the VT / mouse / window-resize bits.
        const ENABLE_LINE_INPUT: u32 = 0x0002;
        const ENABLE_ECHO_INPUT: u32 = 0x0004;
        const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
        let new_mode = (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_WINDOW_INPUT
            | ENABLE_EXTENDED_FLAGS;
        if SetConsoleMode(handle, new_mode) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Walk the process tree rooted at `child_pid` and return the deepest
/// descendant (preferring the most recently created one) as the
/// "foreground" approximation. Windows has no notion of a foreground
/// process group attached to a PTY, so we treat the youngest leaf as the
/// active agent — that matches what users observe when they run e.g.
/// `claude` inside cmd.exe.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }
    let snapshot = process_snapshot();
    let active = pick_active_descendant(&snapshot, child_pid)?;
    let entry = snapshot.iter().find(|e| e.pid == active)?;
    Some(ForegroundJob {
        process_group_id: active,
        processes: vec![ForegroundProcess {
            pid: entry.pid,
            name: entry.name.clone(),
            argv0: Some(entry.name.clone()),
            cmdline: None,
        }],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    if child_pid == 0 {
        return None;
    }
    let snapshot = process_snapshot();
    pick_active_descendant(&snapshot, child_pid)
}

#[derive(Clone)]
struct ProcessEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
}

fn process_snapshot() -> Vec<ProcessEntry> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1 {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let raw = wide_to_string(&entry.szExeFile);
                // `identify_agent` matches against bare executable stems
                // ("claude", "codex"), not the `.exe`-suffixed names that
                // Toolhelp32 returns. Strip the extension here so Unix
                // and Windows agree on the comparison key.
                let name = strip_exe_suffix(&raw).to_string();
                out.push(ProcessEntry {
                    pid: entry.th32ProcessID,
                    parent_pid: entry.th32ParentProcessID,
                    name,
                });
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    out
}

fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn strip_exe_suffix(name: &str) -> &str {
    if name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// Walk descendants of `root` breadth-first, returning the deepest leaf
/// that isn't `root` itself. When several leaves exist at the same depth
/// the higher PID wins as a cheap "most recently spawned" proxy.
fn pick_active_descendant(snapshot: &[ProcessEntry], root: u32) -> Option<u32> {
    let mut frontier = vec![root];
    let mut best: Option<u32> = None;
    let mut best_depth = 0usize;
    let mut depth = 0usize;
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for &pid in &frontier {
            let children: Vec<u32> = snapshot
                .iter()
                .filter(|e| e.parent_pid == pid && e.pid != pid)
                .map(|e| e.pid)
                .collect();
            if children.is_empty() && pid != root {
                if depth > best_depth || (depth == best_depth && best.is_some_and(|b| pid > b)) {
                    best = Some(pid);
                    best_depth = depth;
                }
            } else {
                next.extend(children);
            }
        }
        frontier = next;
        depth += 1;
        if depth > 16 {
            // Defensive cap; real shell trees rarely exceed a handful of
            // levels.
            break;
        }
    }
    best
}

/// Best-effort cwd resolution is unavailable without extra deps; callers
/// already tolerate None and fall back to the parent process cwd.
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

pub fn session_processes(_child_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Terminate the requested processes. Windows has no SIGHUP/SIGTERM; we
/// approximate with `taskkill /F /PID` which is reliable across consoles.
pub fn signal_processes(pids: &[u32], signal: Signal) {
    let force = matches!(signal, Signal::Kill | Signal::Terminate);
    for &pid in pids {
        if pid == 0 {
            continue;
        }
        let mut cmd = Command::new("taskkill");
        if force {
            cmd.arg("/F");
        }
        cmd.arg("/PID")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = cmd.status();
    }
}

/// Probe whether a PID is live by asking the OS via `tasklist`.
pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains(&format!("\"{pid}\""))
        }
        Err(_) => false,
    }
}

/// Pipe the payload through `clip.exe`, the built-in Windows clipboard helper.
pub fn write_clipboard(bytes: &[u8]) -> bool {
    use std::io::Write;
    let mut child = match Command::new("clip")
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
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Native toast notifications require an AppUserModelID and WinRT bindings;
/// no-op for now so callers gracefully fall back to in-app indicators.
pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}

/// Unused on Windows but kept to mirror the Unix module signature.
#[allow(dead_code)]
fn clipboard_commands() -> Vec<ClipboardCommand> {
    Vec::new()
}
