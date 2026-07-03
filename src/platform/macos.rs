use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::sync::OnceLock;

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

const PROC_PGRP_ONLY: u32 = 2;
const SERVER_NOFILE_LIMIT_TARGET: libc::rlim_t = 8192;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

pub(crate) fn scrollback_editor_argv(path: &Path) -> std::io::Result<Vec<String>> {
    let quoted_path = shell_quote(&path.display().to_string());
    let command = format!(
        r#"scrollback_file={quoted_path}; eval "${{EDITOR:-vi}} \"\$scrollback_file\""; status=$?; rm -f "$scrollback_file"; exit $status"#
    );
    Ok(vec!["/bin/sh".to_string(), "-c".to_string(), command])
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[repr(C)]
struct TisInputSource {
    _private: [u8; 0],
}

type TisInputSourceRef = *const TisInputSource;
type CfTypeRef = *const libc::c_void;
type CfStringRef = *const libc::c_void;
type OsStatus = libc::c_int;
type Boolean = libc::c_uchar;
type CfIndex = isize;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    #[link_name = "kTISPropertyInputSourceID"]
    static TIS_PROPERTY_INPUT_SOURCE_ID: CfStringRef;

    #[link_name = "TISCopyCurrentKeyboardInputSource"]
    fn tis_copy_current_keyboard_input_source() -> TisInputSourceRef;

    #[link_name = "TISCopyCurrentASCIICapableKeyboardLayoutInputSource"]
    fn tis_copy_current_ascii_capable_keyboard_layout_input_source() -> TisInputSourceRef;

    #[link_name = "TISGetInputSourceProperty"]
    fn tis_get_input_source_property(
        input_source: TisInputSourceRef,
        property_key: CfStringRef,
    ) -> CfTypeRef;

    #[link_name = "TISSelectInputSource"]
    fn tis_select_input_source(input_source: TisInputSourceRef) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "CFRelease"]
    fn cf_release(value: CfTypeRef);

    #[link_name = "CFEqual"]
    fn cf_equal(left: CfTypeRef, right: CfTypeRef) -> Boolean;

    #[link_name = "CFStringGetCStringPtr"]
    fn cf_string_get_cstring_ptr(value: CfStringRef, encoding: u32) -> *const libc::c_char;

    #[link_name = "CFStringGetLength"]
    fn cf_string_get_length(value: CfStringRef) -> CfIndex;

    #[link_name = "CFStringGetMaximumSizeForEncoding"]
    fn cf_string_get_maximum_size_for_encoding(length: CfIndex, encoding: u32) -> CfIndex;

    #[link_name = "CFStringGetCString"]
    fn cf_string_get_cstring(
        value: CfStringRef,
        buffer: *mut libc::c_char,
        buffer_size: CfIndex,
        encoding: u32,
    ) -> Boolean;
}

#[derive(Debug)]
pub(crate) struct InputSourceRestore {
    previous: NonNull<TisInputSource>,
}

impl Drop for InputSourceRestore {
    fn drop(&mut self) {
        // SAFETY: `previous` is a retained TIS input source created by
        // `TISCopyCurrentKeyboardInputSource`; selecting it and releasing that
        // retain follows the Carbon Input Source Services ownership contract.
        unsafe {
            let previous = self.previous.as_ptr();
            let status = tis_select_input_source(previous);
            cf_release(previous.cast());
            if status != 0 {
                tracing::debug!(
                    status,
                    "failed to restore host input source after prefix mode"
                );
            }
        }
    }
}

pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    // SAFETY: TISCopy* functions return retained references or null. Each
    // retained reference is either transferred into `InputSourceRestore` or
    // released before returning; TISSelectInputSource accepts live TIS refs.
    unsafe {
        let current =
            NonNull::new(tis_copy_current_keyboard_input_source() as *mut TisInputSource)?;
        let Some(ascii) = NonNull::new(
            tis_copy_current_ascii_capable_keyboard_layout_input_source() as *mut TisInputSource,
        ) else {
            cf_release(current.as_ptr().cast());
            return None;
        };

        if input_source_ids_equal(current.as_ptr(), ascii.as_ptr()) {
            cf_release(current.as_ptr().cast());
            cf_release(ascii.as_ptr().cast());
            return None;
        }

        let debug_ids = tracing::enabled!(tracing::Level::DEBUG).then(|| {
            (
                input_source_id(current.as_ptr()),
                input_source_id(ascii.as_ptr()),
            )
        });

        let status = tis_select_input_source(ascii.as_ptr());
        cf_release(ascii.as_ptr().cast());
        if status != 0 {
            cf_release(current.as_ptr().cast());
            tracing::debug!(status, "failed to switch host input source for prefix mode");
            return None;
        }

        if let Some((Some(from), Some(to))) = debug_ids {
            tracing::debug!(from, to, "switched host input source for prefix mode");
        }

        Some(InputSourceRestore { previous: current })
    }
}

unsafe fn input_source_ids_equal(left: TisInputSourceRef, right: TisInputSourceRef) -> bool {
    let left_property = tis_get_input_source_property(left, TIS_PROPERTY_INPUT_SOURCE_ID);
    let right_property = tis_get_input_source_property(right, TIS_PROPERTY_INPUT_SOURCE_ID);
    !left_property.is_null()
        && !right_property.is_null()
        && cf_equal(left_property, right_property) != 0
}

unsafe fn input_source_id(input_source: TisInputSourceRef) -> Option<String> {
    let property =
        tis_get_input_source_property(input_source, TIS_PROPERTY_INPUT_SOURCE_ID) as CfStringRef;
    cf_string_to_string(property)
}

unsafe fn cf_string_to_string(value: CfStringRef) -> Option<String> {
    if value.is_null() {
        return None;
    }

    let direct = cf_string_get_cstring_ptr(value, CF_STRING_ENCODING_UTF8);
    if !direct.is_null() {
        return std::ffi::CStr::from_ptr(direct)
            .to_str()
            .ok()
            .map(str::to_owned);
    }

    let length = cf_string_get_length(value);
    if length < 0 {
        return None;
    }
    let max_bytes = cf_string_get_maximum_size_for_encoding(length, CF_STRING_ENCODING_UTF8);
    if max_bytes < 0 {
        return None;
    }
    let buffer_len = usize::try_from(max_bytes).ok()?.checked_add(1)?;
    let mut buffer = vec![0 as libc::c_char; buffer_len];
    let buffer_size = CfIndex::try_from(buffer.len()).ok()?;
    if cf_string_get_cstring(
        value,
        buffer.as_mut_ptr(),
        buffer_size,
        CF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }

    std::ffi::CStr::from_ptr(buffer.as_ptr())
        .to_str()
        .ok()
        .map(str::to_owned)
}

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

pub fn read_clipboard_text() -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new("pbpaste")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let read = match read_limited_reader(stdout, MAX_CLIPBOARD_TEXT_BYTES) {
        Ok(LimitedRead::Oversized) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    match read {
        LimitedRead::Complete(bytes) => String::from_utf8(bytes).ok(),
        LimitedRead::Empty => None,
        LimitedRead::Oversized => unreachable!("oversized clipboard text is handled before wait"),
    }
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

    #[test]
    fn scrollback_editor_argv_preserves_unix_editor_shell_semantics() {
        let path = std::path::Path::new("/tmp/herdr scrollback.txt");
        let argv = scrollback_editor_argv(path).unwrap();

        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].contains("EDITOR:-vi"));
        assert!(argv[2].contains("/tmp/herdr scrollback.txt"));
    }
}
