use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

const PROC_PGRP_ONLY: u32 = 2;
const SERVER_NOFILE_LIMIT_TARGET: libc::rlim_t = 8192;

pub fn raise_server_nofile_limit() {
    match raise_nofile_limit(SERVER_NOFILE_LIMIT_TARGET) {
        Ok(None) => {}
        Ok(Some((previous, target))) => {
            tracing::info!(previous, target, "raised server file descriptor soft limit")
        }
        Err(err) => tracing::warn!(err = %err, "failed to raise server file descriptor limit"),
    }
}

fn raise_nofile_limit(
    target: libc::rlim_t,
) -> std::io::Result<Option<(libc::rlim_t, libc::rlim_t)>> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut limit = unsafe { limit.assume_init() };
    let Some(target) = target_nofile_soft_limit(limit.rlim_cur, limit.rlim_max, target) else {
        return Ok(None);
    };

    let previous = limit.rlim_cur;
    limit.rlim_cur = target;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(Some((previous, target)))
}

fn target_nofile_soft_limit(
    current: libc::rlim_t,
    hard: libc::rlim_t,
    target: libc::rlim_t,
) -> Option<libc::rlim_t> {
    let target = if hard == libc::RLIM_INFINITY {
        target
    } else {
        target.min(hard)
    };

    (current < target).then_some(target)
}

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }

    let fg_pgid = foreground_process_group_id(child_pid)?;
    let mut processes = Vec::new();

    for pid in process_group_pids(fg_pgid) {
        let Some(info) = process_bsdinfo(pid) else {
            continue;
        };
        if info.pbi_pgid != fg_pgid {
            continue;
        }

        let Some(name) = comm_from_bsdinfo(&info) else {
            continue;
        };
        let argv = process_argv(pid);
        processes.push(ForegroundProcess {
            pid,
            name,
            argv0: process_argv0_name(pid),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        });
    }

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: fg_pgid,
        processes,
    })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let info = process_bsdinfo(process_group_id)?;
    if info.pbi_pgid != process_group_id {
        return None;
    }

    let name = comm_from_bsdinfo(&info)?;
    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: process_argv0_name(process_group_id),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

fn process_group_pids(process_group_id: u32) -> Vec<u32> {
    let mut capacity = 16usize;

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let buffer_bytes = pids.len() * std::mem::size_of::<libc::pid_t>();
        let returned_bytes = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                process_group_id,
                pids.as_mut_ptr() as *mut libc::c_void,
                buffer_bytes as libc::c_int,
            )
        };
        if returned_bytes <= 0 {
            return Vec::new();
        }

        let returned_bytes = returned_bytes as usize;
        let count = returned_bytes / std::mem::size_of::<libc::pid_t>();
        if returned_bytes < buffer_bytes {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

/// Read `e_tpgid` (foreground process group of the controlling terminal)
/// for the given PID.
pub fn foreground_process_group_id(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    let fg = info.e_tpgid;
    if fg > 0 {
        #[allow(clippy::unnecessary_cast)] // info.e_tpgid (pid_t) type is platform-dependent
        Some(fg as u32)
    } else {
        None
    }
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

/// Get the effective process name from `argv[0]` via `sysctl(KERN_PROCARGS2)`.
///
/// This is the macOS equivalent of reading `/proc/{pid}/cmdline` on Linux.
/// It reflects runtime title changes like Node.js `process.title = "pi"`.
fn process_argv0_name(pid: u32) -> Option<String> {
    let buf = kern_procargs2(pid)?;

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] [argv[1]\0] ...
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Skip past exec_path and null padding to reach argv[0]
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    if pos >= rest.len() {
        return None;
    }

    // Read argv[0]
    let argv0_end = rest[pos..]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(rest.len() - pos);
    let argv0 = std::str::from_utf8(&rest[pos..pos + argv0_end]).ok()?;

    if argv0.is_empty() {
        return None;
    }

    // Return basename (argv[0] may be a full path like "/usr/bin/node")
    let basename = Path::new(argv0).file_name()?.to_str()?;

    // Strip leading dash (login shells show as "-zsh")
    let name = basename.strip_prefix('-').unwrap_or(basename);
    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

/// Raw `sysctl(KERN_PROCARGS2)` call. Returns the full buffer.
fn kern_procargs2(pid: u32) -> Option<Vec<u8>> {
    unsafe {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];

        // First call: query required buffer size
        let mut size: libc::size_t = 0;
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 || size == 0 {
            return None;
        }

        // Second call: read data
        let mut buf = vec![0u8; size];
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return None;
        }
        buf.truncate(size);
        Some(buf)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    run_clipboard_command(
        &ClipboardCommand {
            program: "pbcopy",
            args: &[],
        },
        bytes,
    )
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    let path = std::env::temp_dir().join(format!(
        "herdr-clipboard-image-{}-{}.png",
        std::process::id(),
        unique_timestamp_nanos()
    ));
    let script = format!(
        "set png_data to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{}\" with write permission\nwrite png_data to fp\nclose access fp",
        path.display()
    );

    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }

    let bytes = match std::fs::File::open(&path).ok().and_then(|file| {
        read_limited_reader(file, crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()
    }) {
        Some(LimitedRead::Complete(bytes)) => bytes,
        Some(LimitedRead::Empty | LimitedRead::Oversized) | None => {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    let _ = std::fs::remove_file(&path);
    Some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

fn unique_timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// Show a native macOS notification.
///
/// Prefer `terminal-notifier` when it is installed because it can activate the
/// hosting terminal on click. Fall back to built-in AppleScript notifications
/// when it is not available.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if show_terminal_notifier_notification(title, body, &mut command).unwrap_or(false) {
        return Ok(true);
    }

    show_osascript_notification(title, body, &mut command)
}

fn show_terminal_notifier_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let activate_bundle_id = verified_terminal_bundle_identifier(command);
    show_terminal_notifier_notification_with_options(
        title,
        body,
        activate_bundle_id.as_deref(),
        command,
    )
}

fn show_terminal_notifier_notification_with_options(
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("terminal-notifier");
    build_terminal_notifier_command(&mut cmd, title, body, activate_bundle_id);
    run_notification_command(cmd)
}

fn build_terminal_notifier_command(
    cmd: &mut Command,
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
) {
    cmd.arg("-title").arg(title);
    cmd.arg("-message").arg(body.unwrap_or_default());
    if let Some(bundle_id) = activate_bundle_id {
        cmd.arg("-activate").arg(bundle_id);
    }
}

fn show_osascript_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("/usr/bin/osascript");
    cmd.arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("display notification (item 2 of argv) with title (item 1 of argv)")
        .arg("-e")
        .arg("end run")
        .arg(title)
        .arg(body.unwrap_or_default());
    run_notification_command(cmd)
}

fn verified_terminal_bundle_identifier(
    command: &mut impl FnMut(&str) -> Command,
) -> Option<String> {
    static BUNDLE_ID: OnceLock<Option<String>> = OnceLock::new();
    BUNDLE_ID
        .get_or_init(|| {
            let bundle_id = detected_terminal_bundle_identifier()?;
            bundle_identifier_available(bundle_id, command).then(|| bundle_id.to_owned())
        })
        .clone()
}

fn bundle_identifier_available(bundle_id: &str, command: &mut impl FnMut(&str) -> Command) -> bool {
    let query = format!("kMDItemCFBundleIdentifier == '{bundle_id}'");
    let output = command("mdfind")
        .arg(query)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        _ => false,
    }
}

fn detected_terminal_bundle_identifier() -> Option<&'static str> {
    terminal_bundle_identifier_from_env(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        std::env::var_os("ALACRITTY_WINDOW_ID").is_some(),
    )
}

fn terminal_bundle_identifier_from_env(
    term_program: Option<&str>,
    term: Option<&str>,
    has_kitty_window_id: bool,
    has_alacritty_window_id: bool,
) -> Option<&'static str> {
    match term_program {
        Some("ghostty") => return Some("com.mitchellh.ghostty"),
        Some("iTerm.app") => return Some("com.googlecode.iterm2"),
        Some("WezTerm") => return Some("com.github.wez.wezterm"),
        Some("Apple_Terminal") => return Some("com.apple.Terminal"),
        _ => {}
    }

    if has_kitty_window_id || term == Some("xterm-kitty") {
        return Some("net.kovidgoyal.kitty");
    }
    if has_alacritty_window_id {
        return Some("org.alacritty");
    }

    None
}

fn run_notification_command(mut command: Command) -> std::io::Result<bool> {
    let status = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
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

fn process_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    (ret == size).then_some(info)
}

fn comm_from_bsdinfo(info: &libc::proc_bsdinfo) -> Option<String> {
    let end = info
        .pbi_comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(info.pbi_comm.len());
    if end == 0 {
        return None;
    }

    let bytes: Vec<u8> = info.pbi_comm[..end].iter().map(|&b| b as u8).collect();
    String::from_utf8(bytes).ok()
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let buf = kern_procargs2(pid)?;
    procargs2_argv(&buf)
}

fn procargs2_argv(buf: &[u8]) -> Option<Vec<String>> {
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] [argv[1]\0] ... [env\0] ...
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    if pos >= rest.len() {
        return None;
    }

    let mut argv = Vec::with_capacity(argc as usize);
    let mut current = pos;
    for _ in 0..argc {
        if current >= rest.len() {
            return None;
        }
        let end = rest[current..]
            .iter()
            .position(|&b| b == 0)
            .map(|offset| current + offset)
            .unwrap_or(rest.len());
        if end == current {
            return None;
        }
        argv.push(String::from_utf8_lossy(&rest[current..end]).into_owned());
        current = end + 1;
    }

    Some(argv)
}

/// Get the current working directory of a process.
///
/// Uses `proc_pidinfo(PROC_PIDVNODEPATHINFO)` to read `pvi_cdir.vip_path`.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }

    let mut pathinfo: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut pathinfo as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    // vip_path is [[c_char; 32]; 32] in libc (workaround for old Rust const generics).
    // Reinterpret as flat bytes (total MAXPATHLEN = 1024).
    let vip_path = unsafe {
        std::slice::from_raw_parts(
            pathinfo.pvi_cdir.vip_path.as_ptr() as *const u8,
            libc::MAXPATHLEN as usize,
        )
    };

    let nul = vip_path.iter().position(|&b| b == 0)?;
    if nul == 0 {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&vip_path[..nul])))
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    if child_pid == 0 {
        return Vec::new();
    }

    let target_session = unsafe { libc::getsid(child_pid as libc::c_int) };
    if target_session <= 0 {
        return Vec::new();
    }

    all_pids()
        .into_iter()
        .filter(|pid| unsafe { libc::getsid(*pid as libc::pid_t) } == target_session)
        .collect()
}

fn all_pids() -> Vec<u32> {
    let initial_count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = if initial_count > 0 {
        initial_count as usize + 128
    } else {
        4096
    };

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr() as *mut libc::c_void,
                (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if count <= 0 {
            return Vec::new();
        }

        let count = count as usize;
        if count < capacity {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

fn collect_positive_pids(pids: Vec<libc::pid_t>, count: usize) -> Vec<u32> {
    pids.into_iter()
        .take(count)
        .filter(|pid| *pid > 0)
        .map(|pid| pid as u32)
        .collect()
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
            libc::kill(pid as libc::c_int, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::c_int, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nofile_target_raises_low_soft_limit_to_cap_when_hard_is_unlimited() {
        assert_eq!(
            target_nofile_soft_limit(256, libc::RLIM_INFINITY, 8192),
            Some(8192)
        );
    }

    #[test]
    fn nofile_target_respects_finite_hard_limit() {
        assert_eq!(target_nofile_soft_limit(256, 4096, 8192), Some(4096));
    }

    #[test]
    fn nofile_target_does_not_lower_existing_soft_limit() {
        assert_eq!(
            target_nofile_soft_limit(16_384, libc::RLIM_INFINITY, 8192),
            None
        );
    }

    fn build_procargs2(exec_path: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(argv.len() as i32).to_ne_bytes());
        buf.extend_from_slice(exec_path.as_bytes());
        buf.push(0);
        buf.push(0);
        for arg in argv {
            buf.extend_from_slice(arg.as_bytes());
            buf.push(0);
        }
        for entry in env {
            buf.extend_from_slice(entry.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn procargs2_argv_excludes_environment_entries() {
        let buf = build_procargs2(
            "/usr/bin/node",
            &["node", "/Users/can/.local/bin/pi"],
            &[
                "PATH=/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin",
                "TERM=tmux-256color",
            ],
        );

        let argv = procargs2_argv(&buf).expect("expected argv");
        assert_eq!(argv, vec!["node", "/Users/can/.local/bin/pi"]);
        assert_eq!(argv.join(" "), "node /Users/can/.local/bin/pi");
        assert!(!argv.join(" ").contains("codex.system"));
    }

    #[test]
    fn terminal_bundle_identifier_maps_known_terminal_env() {
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("ghostty"), None, false, false),
            Some("com.mitchellh.ghostty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("iTerm.app"), None, false, false),
            Some("com.googlecode.iterm2")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("WezTerm"), None, false, false),
            Some("com.github.wez.wezterm")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("Apple_Terminal"), None, false, false),
            Some("com.apple.Terminal")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, Some("xterm-kitty"), false, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, true, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, true),
            Some("org.alacritty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, false),
            None
        );
    }

    #[test]
    fn terminal_notifier_command_includes_icon_and_activation() {
        let mut cmd = Command::new("terminal-notifier");
        build_terminal_notifier_command(
            &mut cmd,
            "pi finished",
            Some("workspace 1"),
            Some("com.mitchellh.ghostty"),
        );
        let args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-title",
                "pi finished",
                "-message",
                "workspace 1",
                "-activate",
                "com.mitchellh.ghostty"
            ]
        );
    }

    #[test]
    fn terminal_notifier_success_skips_osascript() {
        let path = std::env::temp_dir().join(format!(
            "herdr-terminal-notifier-args-{}",
            std::process::id()
        ));
        let script = "printf '%s:%s\\n' \"$0\" \"$*\" >> \"$HERDR_NOTIFY_ARGS\"";
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };

        let shown = show_terminal_notifier_notification_with_options(
            "title",
            Some("body"),
            Some("com.mitchellh.ghostty"),
            &mut command,
        )
        .expect("terminal-notifier command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert!(args.starts_with("terminal-notifier:"), "{args}");
        assert!(args.contains("-activate com.mitchellh.ghostty"), "{args}");
        assert!(!args.contains("osascript"), "{args}");
    }

    #[test]
    fn desktop_notification_falls_back_to_osascript_when_terminal_notifier_fails() {
        let path =
            std::env::temp_dir().join(format!("herdr-osascript-args-{}", std::process::id()));
        let script = r#"
if [ "$0" = "terminal-notifier" ]; then
  exit 1
fi
printf '%s\n' "$@" > "$HERDR_NOTIFY_ARGS"
"#;
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };
        let shown = show_desktop_notification_with_command("title", Some("body"), &mut command)
            .expect("osascript fallback should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            args,
            "-e\non run argv\n-e\ndisplay notification (item 2 of argv) with title (item 1 of argv)\n-e\nend run\ntitle\nbody\n"
        );
    }
}
