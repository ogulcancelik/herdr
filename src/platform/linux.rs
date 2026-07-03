use std::{
    collections::HashMap,
    io::Write,
    os::fd::RawFd,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

const FOREGROUND_MEMBERS_CACHE_TTL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcGroupMember {
    pid: u32,
    comm: String,
}

type ForegroundMembersByGroup = HashMap<u32, Vec<ProcGroupMember>>;

#[derive(Debug, Clone)]
struct CachedForegroundMembers {
    built_at: Instant,
    by_group: ForegroundMembersByGroup,
}

#[derive(Debug, Default)]
struct ForegroundMembersCache {
    cached: Option<CachedForegroundMembers>,
}

static FOREGROUND_MEMBERS_CACHE: Mutex<ForegroundMembersCache> =
    Mutex::new(ForegroundMembersCache { cached: None });

pub fn raise_server_nofile_limit() {}

pub(crate) fn scrollback_editor_argv(path: &std::path::Path) -> std::io::Result<Vec<String>> {
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

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let tpgid = foreground_process_group_id(child_pid)?;
    let members = foreground_process_group_members(tpgid)?;
    let processes = members
        .into_iter()
        .map(|member| {
            let argv = process_argv(member.pid);
            ForegroundProcess {
                pid: member.pid,
                name: member.comm,
                argv0: None,
                cmdline: argv.as_ref().map(|parts| parts.join(" ")),
                argv,
            }
        })
        .collect::<Vec<_>>();

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: tpgid,
        processes,
    })
}

fn foreground_process_group_members(process_group_id: u32) -> Option<Vec<ProcGroupMember>> {
    let mut cache = FOREGROUND_MEMBERS_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    cache.members(
        process_group_id,
        Instant::now(),
        FOREGROUND_MEMBERS_CACHE_TTL,
        build_foreground_members_by_group,
        live_process_group_member,
    )
}

fn live_process_group_member(process_group_id: u32, pid: u32) -> Option<ProcGroupMember> {
    let (pgrp, comm) = process_pgrp_and_comm(pid)?;
    (pgrp > 0 && pgrp as u32 == process_group_id).then_some(ProcGroupMember { pid, comm })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let (pgrp, name) = process_pgrp_and_comm(process_group_id)?;
    if pgrp as u32 != process_group_id {
        return None;
    }

    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: None,
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    // /proc/<pid>/stat format: "pid (comm) state ppid pgrp session tty_nr tpgid ..."
    // The (comm) field can contain spaces and parens, so we find the last ')' first.
    let stat = std::fs::read_to_string(format!("/proc/{child_pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid as u32)
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

fn process_pgrp_and_comm(pid: u32) -> Option<(i32, String)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    process_pgrp_and_comm_from_stat(&stat)
}

fn process_pgrp_and_comm_from_stat(stat: &str) -> Option<(i32, String)> {
    let close = stat.rfind(')')?;
    let comm = stat.get(1 + stat.find('(')?..close)?.to_string();
    let rest = stat.get(close + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgrp: i32 = fields.get(2)?.parse().ok()?;
    Some((pgrp, comm))
}

fn build_foreground_members_by_group() -> ForegroundMembersByGroup {
    let entries = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let pid_str = file_name.to_str()?;
            if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let pid = pid_str.parse::<u32>().ok()?;
            let (pgrp, comm) = process_pgrp_and_comm(pid)?;
            Some(ProcStatEntry { pid, pgrp, comm })
        });
    foreground_members_by_group_from_entries(entries)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcStatEntry {
    pid: u32,
    pgrp: i32,
    comm: String,
}

fn foreground_members_by_group_from_entries(
    entries: impl IntoIterator<Item = ProcStatEntry>,
) -> ForegroundMembersByGroup {
    let mut by_group = ForegroundMembersByGroup::default();
    for entry in entries {
        if entry.pgrp <= 0 {
            continue;
        }
        by_group
            .entry(entry.pgrp as u32)
            .or_default()
            .push(ProcGroupMember {
                pid: entry.pid,
                comm: entry.comm,
            });
    }
    by_group
}

impl ForegroundMembersCache {
    fn members(
        &mut self,
        process_group_id: u32,
        now: Instant,
        max_age: Duration,
        build: impl FnMut() -> ForegroundMembersByGroup,
        mut validate: impl FnMut(u32, u32) -> Option<ProcGroupMember>,
    ) -> Option<Vec<ProcGroupMember>> {
        if let Some(cached) = &self.cached {
            if now.duration_since(cached.built_at) < max_age {
                if let Some(members) = cached.by_group.get(&process_group_id) {
                    let members = members
                        .iter()
                        .filter_map(|member| validate(process_group_id, member.pid))
                        .collect::<Vec<_>>();
                    if !members.is_empty() {
                        return Some(members);
                    }
                }
                return self.refresh_and_get(process_group_id, now, build);
            }
        }
        self.refresh_and_get(process_group_id, now, build)
    }

    fn refresh_and_get(
        &mut self,
        process_group_id: u32,
        now: Instant,
        build: impl FnOnce() -> ForegroundMembersByGroup,
    ) -> Option<Vec<ProcGroupMember>> {
        self.cached = Some(CachedForegroundMembers {
            built_at: now,
            by_group: build(),
        });
        self.cached
            .as_ref()?
            .by_group
            .get(&process_group_id)
            .cloned()
    }
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Get the current working directory of a process.
/// Uses /proc/<pid>/cwd symlink.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read a Herdr agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    parse_agent_env_hint(&environ)
}

fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        let value = std::str::from_utf8(value).ok()?;
        return crate::detect::parse_agent_label(value);
    }
    None
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

pub fn read_clipboard_text() -> Option<String> {
    for command in read_clipboard_text_commands() {
        if let Some(text) = read_clipboard_text_with_command(&command) {
            return Some(text);
        }
    }
    None
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/jpg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(image) =
                read_validated_clipboard_image("wl-paste", &["--type", mime], extension)
            {
                return Some(image);
            }
        }

        if std::env::var_os("DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                extension,
            ) {
                return Some(image);
            }
        }
    }

    None
}

fn read_validated_clipboard_image(
    program: &str,
    args: &[&str],
    extension: &'static str,
) -> Option<ClipboardImage> {
    let bytes = read_clipboard_image_with_command(program, args)?;
    if !bytes_match_image_signature(extension, &bytes) {
        return None;
    }
    Some(ClipboardImage { bytes, extension })
}

fn bytes_match_image_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP",
        "bmp" => {
            if bytes.len() < 26 || !bytes.starts_with(b"BM") {
                return false;
            }
            let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
            (26..=bytes.len()).contains(&offset)
        }
        _ => false,
    }
}

/// Show a native desktop notification through libnotify's command-line helper.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(false);
    }

    let mut cmd = command("notify-send");
    cmd.arg("--").arg(title);
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        cmd.arg(body);
    }
    run_notification_command(cmd)
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

fn read_clipboard_image_with_command(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command.args(args);
    read_clipboard_image_with_spawned_command(command)
}

fn read_clipboard_image_with_spawned_command(command: Command) -> Option<Vec<u8>> {
    read_clipboard_image_with_spawned_command_max(
        command,
        crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD,
    )
}

fn read_clipboard_image_with_spawned_command_max(
    mut command: Command,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    let read = match read_limited_reader(stdout, max_bytes) {
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    if read == LimitedRead::Oversized {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }

    match read {
        LimitedRead::Complete(bytes) => Some(bytes),
        LimitedRead::Empty | LimitedRead::Oversized => None,
    }
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

fn read_clipboard_text_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain;charset=utf-8"],
        });
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-out"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--output"],
        });
    }

    commands
}

fn read_clipboard_text_with_command(command: &ClipboardCommand) -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new(command.program)
        .args(command.args)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn proc_entry(pid: u32, pgrp: i32, comm: &str) -> ProcStatEntry {
        ProcStatEntry {
            pid,
            pgrp,
            comm: comm.to_string(),
        }
    }

    fn foreground_members(groups: &[(u32, &str, i32)]) -> ForegroundMembersByGroup {
        foreground_members_by_group_from_entries(
            groups
                .iter()
                .map(|(pid, comm, pgrp)| proc_entry(*pid, *pgrp, comm)),
        )
    }

    fn validate_from<'a>(
        groups: &'a [(u32, &'a str, i32)],
    ) -> impl FnMut(u32, u32) -> Option<ProcGroupMember> + 'a {
        move |process_group_id, pid| {
            groups.iter().find_map(|(member_pid, comm, pgrp)| {
                (*member_pid == pid && *pgrp > 0 && *pgrp as u32 == process_group_id).then(|| {
                    ProcGroupMember {
                        pid,
                        comm: (*comm).to_string(),
                    }
                })
            })
        }
    }

    fn member_names(by_group: &ForegroundMembersByGroup, process_group_id: u32) -> Vec<String> {
        by_group
            .get(&process_group_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|member| member.comm)
            .collect()
    }

    #[test]
    fn foreground_members_indexes_by_process_group() {
        let by_group = foreground_members(&[
            (10, "shell", 10),
            (11, "claude", 11),
            (12, "node", 11),
            (13, "ignored", -1),
        ]);

        assert_eq!(member_names(&by_group, 10), vec!["shell"]);
        assert_eq!(member_names(&by_group, 11), vec!["claude", "node"]);
        assert_eq!(by_group.get(&13), None);
    }

    #[test]
    fn foreground_members_cache_reuses_snapshot_inside_ttl() {
        let mut cache = ForegroundMembersCache::default();
        let now = Instant::now();
        let builds = AtomicUsize::new(0);

        let first = cache.members(
            10,
            now,
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(10, "shell", 10)])
            },
            validate_from(&[(10, "shell", 10)]),
        );
        let second = cache.members(
            10,
            now + Duration::from_millis(100),
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(20, "new", 20)])
            },
            validate_from(&[(10, "shell-live", 10)]),
        );

        assert_eq!(
            first
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["shell"]
        );
        assert_eq!(
            second
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["shell-live"]
        );
        assert_eq!(builds.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn foreground_members_cache_rebuilds_after_ttl() {
        let mut cache = ForegroundMembersCache::default();
        let now = Instant::now();
        let builds = AtomicUsize::new(0);

        let _ = cache.members(
            10,
            now,
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(10, "shell", 10)])
            },
            validate_from(&[(10, "shell", 10)]),
        );
        let second = cache.members(
            20,
            now + FOREGROUND_MEMBERS_CACHE_TTL,
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(20, "new", 20)])
            },
            validate_from(&[(20, "new", 20)]),
        );

        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert_eq!(
            second
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["new"]
        );
    }

    #[test]
    fn foreground_members_cache_refreshes_reused_snapshot_on_group_miss() {
        let mut cache = ForegroundMembersCache::default();
        let now = Instant::now();
        let builds = AtomicUsize::new(0);

        let stale = cache.members(
            10,
            now,
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(10, "shell", 10)])
            },
            validate_from(&[(10, "shell", 10)]),
        );
        assert_eq!(
            stale
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["shell"]
        );

        let refreshed = cache.members(
            42,
            now + Duration::from_millis(10),
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(42, "claude", 42)])
            },
            validate_from(&[(42, "claude", 42)]),
        );

        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert_eq!(
            refreshed
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["claude"]
        );
    }

    #[test]
    fn foreground_members_cache_refreshes_existing_group_when_cached_members_are_stale() {
        let mut cache = ForegroundMembersCache::default();
        let now = Instant::now();
        let builds = AtomicUsize::new(0);

        let stale = cache.members(
            42,
            now,
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(10, "old", 42)])
            },
            validate_from(&[(10, "old", 42)]),
        );
        assert_eq!(
            stale
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["old"]
        );

        let refreshed = cache.members(
            42,
            now + Duration::from_millis(10),
            FOREGROUND_MEMBERS_CACHE_TTL,
            || {
                builds.fetch_add(1, Ordering::Relaxed);
                foreground_members(&[(20, "new", 42)])
            },
            validate_from(&[(10, "old", 7), (20, "new", 42)]),
        );

        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert_eq!(
            refreshed
                .unwrap()
                .into_iter()
                .map(|member| member.comm)
                .collect::<Vec<_>>(),
            vec!["new"]
        );
    }

    #[test]
    fn proc_stat_parsing_keeps_group_leader_inputs_live() {
        assert_eq!(
            process_pgrp_and_comm_from_stat("123 (name with ) paren) S 1 456 789 0 456"),
            Some((456, "name with ) paren".to_string()))
        );
    }

    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
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

    #[test]
    fn read_clipboard_text_commands_include_session_backends() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
        }

        let commands = read_clipboard_text_commands();
        assert_eq!(commands[0].program, "wl-paste");
        assert_eq!(commands[1].program, "wl-paste");
        assert_eq!(commands[2].program, "xclip");
        assert_eq!(commands[3].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_with_command_reads_utf8() {
        let command = ClipboardCommand {
            program: "printf",
            args: &["feature/linear-302"],
        };

        assert_eq!(
            read_clipboard_text_with_command(&command).as_deref(),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn read_clipboard_text_with_command_rejects_oversized_output() {
        let command = ClipboardCommand {
            program: "sh",
            args: &["-c", "yes x | head -c 1048578"],
        };

        assert_eq!(read_clipboard_text_with_command(&command), None);
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_reads_under_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf image");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 16),
            Some(b"image".to_vec())
        );
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_rejects_over_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf oversized");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 4),
            None
        );
    }

    #[test]
    fn read_clipboard_image_rejects_xclip_text_served_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&fake_xclip)
                .expect("fake xclip metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&fake_xclip, permissions)
                .expect("fake xclip should be executable");
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_clipboard_image_rejects_wayland_xclip_fallback_text_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-wayland-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_wl_paste = temp_dir.join("wl-paste");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_wl_paste, "#!/bin/sh\nexit 1\n")
            .expect("fake wl-paste should be written");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            for command in [&fake_wl_paste, &fake_xclip] {
                let mut permissions = std::fs::metadata(command)
                    .expect("fake clipboard command metadata")
                    .permissions();
                permissions.set_mode(0o700);
                std::fs::set_permissions(command, permissions)
                    .expect("fake clipboard command should be executable");
            }
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_wl_paste);
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_validated_clipboard_image_accepts_real_png_payload() {
        assert_eq!(
            read_validated_clipboard_image(
                "sh",
                &["-c", "printf '\\211PNG\\r\\n\\032\\nrest-of-image'"],
                "png"
            ),
            Some(ClipboardImage {
                bytes: b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
                extension: "png",
            })
        );
    }

    #[test]
    fn image_signatures_match_only_their_format() {
        assert!(bytes_match_image_signature("png", b"\x89PNG\r\n\x1a\n..."));
        assert!(bytes_match_image_signature(
            "jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0]
        ));
        assert!(bytes_match_image_signature("gif", b"GIF87a..."));
        assert!(bytes_match_image_signature("gif", b"GIF89a..."));
        assert!(bytes_match_image_signature(
            "webp",
            b"RIFF\x10\x00\x00\x00WEBPVP8 "
        ));

        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 26;
        assert!(bytes_match_image_signature("bmp", &bmp));

        assert!(!bytes_match_image_signature("png", b"# Tasks"));
        assert!(!bytes_match_image_signature("jpg", b"plain clipboard text"));
        assert!(!bytes_match_image_signature("gif", b""));
        assert!(!bytes_match_image_signature("webp", b"RIFF but not webp"));
        assert!(!bytes_match_image_signature("bmp", b"\x89PNG\r\n\x1a\n"));
        assert!(!bytes_match_image_signature(
            "bmp",
            b"BM text is not a bitmap"
        ));
        assert!(!bytes_match_image_signature("svg", b"<svg></svg>"));
    }

    #[test]
    fn desktop_notification_separates_option_like_titles() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }

        let path =
            std::env::temp_dir().join(format!("herdr-notify-send-args-{}", std::process::id()));
        let script = "printf '%s\\n' \"$@\" > \"$HERDR_NOTIFY_ARGS\"";
        let shown = show_desktop_notification_with_command("-danger", Some("body"), |_| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg("notify-send")
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        })
        .expect("notification command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(args, "--\n-danger\nbody\n");
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
