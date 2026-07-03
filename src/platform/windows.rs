use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    mem::{size_of, MaybeUninit},
    path::PathBuf,
    ptr::null_mut,
};

use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
    Win32::{
        Foundation::{
            CloseHandle, LocalFree, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, STATUS_SUCCESS,
            UNICODE_STRING,
        },
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
            },
            Threading::{
                GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_BASIC_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
            },
        },
        UI::Shell::{CommandLineToArgvW, ShellExecuteW},
    },
};

use super::{ClipboardImage, ForegroundJob, Signal};

const STILL_ACTIVE: u32 = 259;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsProcessEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
    argv0: Option<String>,
    argv: Option<Vec<String>>,
    cmdline: Option<String>,
}

pub fn raise_server_nofile_limit() {}

pub(crate) fn scrollback_editor_argv(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    scrollback_editor_argv_with_env(path, editor.as_deref())
}

fn scrollback_editor_argv_with_env(
    path: &std::path::Path,
    editor: Option<&str>,
) -> std::io::Result<Vec<String>> {
    let mut argv = match editor.filter(|value| !value.trim().is_empty()) {
        Some(editor) => command_line_to_argv(editor).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("failed to parse editor command {editor:?}"),
            )
        })?,
        None => vec!["notepad.exe".to_string()],
    };
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "editor command must not be empty",
        ));
    }
    argv.push(path.display().to_string());
    Ok(argv)
}

pub fn detach_server_daemon_command(_command: &mut std::process::Command) {}

pub fn current_process_is_detached_server_daemon() -> bool {
    false
}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let entries = snapshot_processes();
    select_pane_foreground_job(child_pid, &entries)
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let entries = snapshot_processes();
    let entry = entries.iter().find(|entry| entry.pid == process_group_id)?;
    Some(ForegroundJob {
        process_group_id,
        processes: vec![foreground_process_from_entry(entry)],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    foreground_job(child_pid).map(|job| job.process_group_id)
}

pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let process_parameters = read_process_parameters(process.0)?;
    read_unicode_string(process.0, process_parameters.current_directory.dos_path)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn select_pane_foreground_job(
    shell_pid: u32,
    entries: &[WindowsProcessEntry],
) -> Option<ForegroundJob> {
    let shell = entries.iter().find(|entry| entry.pid == shell_pid)?;
    let shell_job = || ForegroundJob {
        process_group_id: shell_pid,
        processes: vec![foreground_process_from_entry(shell)],
    };

    let descendants = descendant_entries(shell_pid, entries);
    let mut candidates = Vec::new();
    for entry in &descendants {
        let process = foreground_process_from_entry(entry);
        let job = ForegroundJob {
            process_group_id: entry.pid,
            processes: vec![process],
        };
        if let Some((agent, _)) = crate::detect::identify_agent_in_job(&job) {
            candidates.push((*entry, agent));
        }
    }

    match candidates.len() {
        1 => candidates
            .pop()
            .map(|(entry, _)| foreground_job_from_entry(entry)),
        _ => select_single_agent_chain_candidate(&candidates, entries).map_or_else(
            || Some(shell_job()),
            |entry| Some(foreground_job_from_entry(entry)),
        ),
    }
}

fn foreground_job_from_entry(entry: &WindowsProcessEntry) -> ForegroundJob {
    ForegroundJob {
        process_group_id: entry.pid,
        processes: vec![foreground_process_from_entry(entry)],
    }
}

fn select_single_agent_chain_candidate<'a>(
    candidates: &[(&'a WindowsProcessEntry, crate::detect::Agent)],
    entries: &[WindowsProcessEntry],
) -> Option<&'a WindowsProcessEntry> {
    let (_, first_agent) = candidates.first()?;
    if !candidates.iter().all(|(_, agent)| agent == first_agent) {
        return None;
    }

    let parent_by_pid: HashMap<u32, u32> = entries
        .iter()
        .map(|entry| (entry.pid, entry.parent_pid))
        .collect();

    candidates.iter().map(|(entry, _)| *entry).find(|entry| {
        candidates.iter().all(|(other, _)| {
            entry.pid == other.pid || process_is_ancestor(entry.pid, other.pid, &parent_by_pid)
        })
    })
}

fn process_is_ancestor(
    ancestor_pid: u32,
    descendant_pid: u32,
    parent_by_pid: &HashMap<u32, u32>,
) -> bool {
    let mut current = descendant_pid;
    while let Some(parent) = parent_by_pid.get(&current).copied() {
        if parent == ancestor_pid {
            return true;
        }
        if parent == 0 || parent == current {
            return false;
        }
        current = parent;
    }

    false
}

fn descendant_entries(root_pid: u32, entries: &[WindowsProcessEntry]) -> Vec<&WindowsProcessEntry> {
    let mut children: HashMap<u32, Vec<&WindowsProcessEntry>> = HashMap::new();
    for entry in entries {
        children.entry(entry.parent_pid).or_default().push(entry);
    }

    let mut output = Vec::new();
    let mut queue = VecDeque::new();
    if let Some(root_children) = children.get(&root_pid) {
        queue.extend(root_children.iter().copied());
    }
    while let Some(entry) = queue.pop_front() {
        output.push(entry);
        if let Some(next) = children.get(&entry.pid) {
            queue.extend(next.iter().copied());
        }
    }
    output
}

fn foreground_process_from_entry(entry: &WindowsProcessEntry) -> super::ForegroundProcess {
    super::ForegroundProcess {
        pid: entry.pid,
        name: entry.name.clone(),
        argv0: entry.argv0.clone(),
        argv: entry.argv.clone(),
        cmdline: entry.cmdline.clone(),
    }
}

fn snapshot_processes() -> Vec<WindowsProcessEntry> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let _snapshot = ProcessHandle(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut output = Vec::new();
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while ok {
        let pid = entry.th32ProcessID;
        let name = nul_terminated_utf16_to_string(&entry.szExeFile);
        let cmdline = process_command_line(pid);
        let argv = cmdline.as_deref().and_then(command_line_to_argv);
        let argv0 = argv
            .as_ref()
            .and_then(|argv| argv.first().cloned())
            .or_else(|| (!name.is_empty()).then(|| name.clone()));
        output.push(WindowsProcessEntry {
            pid,
            parent_pid: entry.th32ParentProcessID,
            name,
            argv0,
            argv,
            cmdline,
        });
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    output
}

fn process_command_line(pid: u32) -> Option<String> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let parameters = read_process_parameters(process.0)?;
    read_unicode_string(process.0, parameters.command_line)
}

fn read_process_parameters(process: HANDLE) -> Option<RtlUserProcessParameters> {
    let mut basic_info = MaybeUninit::<PROCESS_BASIC_INFORMATION>::uninit();
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessBasicInformation,
            basic_info.as_mut_ptr().cast::<c_void>(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            null_mut(),
        )
    };
    if status != STATUS_SUCCESS as NTSTATUS {
        return None;
    }

    let basic_info = unsafe { basic_info.assume_init() };
    if basic_info.PebBaseAddress.is_null() {
        return None;
    }

    let peb = read_process_value::<Peb>(process, basic_info.PebBaseAddress.cast::<c_void>())?;
    if peb.process_parameters.is_null() {
        return None;
    }

    read_process_value::<RtlUserProcessParameters>(process, peb.process_parameters.cast())
}

fn command_line_to_argv(command_line: &str) -> Option<Vec<String>> {
    let wide: Vec<u16> = command_line
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut argc = 0;
    let argv_ptr = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv_ptr.is_null() || argc <= 0 {
        return None;
    }

    let argv_slice = unsafe { std::slice::from_raw_parts(argv_ptr, argc as usize) };
    let mut argv = Vec::with_capacity(argc as usize);
    for &arg in argv_slice {
        if arg.is_null() {
            continue;
        }
        let mut len = 0;
        unsafe {
            while *arg.add(len) != 0 {
                len += 1;
            }
            argv.push(String::from_utf16_lossy(std::slice::from_raw_parts(
                arg, len,
            )));
        }
    }
    unsafe {
        LocalFree(argv_ptr.cast());
    }
    Some(argv)
}

fn nul_terminated_utf16_to_string(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    if child_pid == 0 {
        return Vec::new();
    }

    let entries = snapshot_processes();
    session_processes_from_entries(child_pid, &entries)
}

fn session_processes_from_entries(child_pid: u32, entries: &[WindowsProcessEntry]) -> Vec<u32> {
    if !entries.iter().any(|entry| entry.pid == child_pid) {
        return Vec::new();
    }

    let mut pids = vec![child_pid];
    pids.extend(
        descendant_entries(child_pid, entries)
            .into_iter()
            .map(|entry| entry.pid),
    );
    pids
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    if signal == Signal::Hangup {
        return;
    }

    for &pid in pids {
        let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            continue;
        };
        unsafe {
            TerminateProcess(process.0, 1);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };

    let mut exit_code = 0;
    let ok = unsafe { GetExitCodeProcess(process.0, &mut exit_code) } != 0;
    ok && exit_code == STILL_ACTIVE
}

pub fn write_clipboard(_bytes: &[u8]) -> bool {
    false
}

pub fn read_clipboard_text() -> Option<String> {
    None
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    let operation = wide_null("open");
    let url = wide_null(url);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "failed to open URL with ShellExecuteW: code {}",
            result as isize
        )))
    }
}

// Windows does not wire clipboard-image bridging into semantic input yet.
#[cfg_attr(windows, allow(dead_code))]
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

struct ProcessHandle(HANDLE);

impl ProcessHandle {
    fn open(pid: u32, access: u32) -> Option<Self> {
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Peb {
    reserved1: [u8; 2],
    being_debugged: u8,
    reserved2: [u8; 1],
    reserved3: [*mut c_void; 2],
    ldr: *mut c_void,
    process_parameters: *mut RtlUserProcessParameters,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CurDir {
    dos_path: UNICODE_STRING,
    handle: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RtlUserProcessParameters {
    maximum_length: u32,
    length: u32,
    flags: u32,
    debug_flags: u32,
    console_handle: HANDLE,
    console_flags: u32,
    standard_input: HANDLE,
    standard_output: HANDLE,
    standard_error: HANDLE,
    current_directory: CurDir,
    dll_path: UNICODE_STRING,
    image_path_name: UNICODE_STRING,
    command_line: UNICODE_STRING,
}

fn read_process_value<T: Copy>(process: HANDLE, address: *const c_void) -> Option<T> {
    if address.is_null() {
        return None;
    }

    let mut value = MaybeUninit::<T>::uninit();
    let mut bytes_read = 0;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address,
            value.as_mut_ptr().cast::<c_void>(),
            size_of::<T>(),
            &mut bytes_read,
        )
    } != 0;

    (ok && bytes_read == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn read_unicode_string(process: HANDLE, unicode: UNICODE_STRING) -> Option<String> {
    if unicode.Buffer.is_null() || unicode.Length == 0 || !unicode.Length.is_multiple_of(2) {
        return None;
    }

    let char_len = usize::from(unicode.Length / 2);
    let mut buffer = vec![0_u16; char_len];
    let mut bytes_read = 0;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            unicode.Buffer.cast::<c_void>(),
            buffer.as_mut_ptr().cast::<c_void>(),
            usize::from(unicode.Length),
            &mut bytes_read,
        )
    } != 0;

    if !ok || bytes_read != usize::from(unicode.Length) {
        return None;
    }

    String::from_utf16(&buffer).ok()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn windows_process_cwd_reads_child_launch_directory() {
        let cwd = std::env::temp_dir().join(format!("herdr-cwd-test-{}", std::process::id()));
        fs::create_dir_all(&cwd).expect("create cwd fixture");

        let shell =
            std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
        let mut child = Command::new(shell)
            .args(["/D", "/Q", "/C", "ping -n 11 127.0.0.1 > NUL"])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = None;
        while Instant::now() < deadline {
            observed = super::process_cwd(child.id());
            if observed.as_deref() == Some(cwd.as_path()) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_dir_all(&cwd);

        assert_eq!(observed.as_deref(), Some(cwd.as_path()));
    }

    #[test]
    fn windows_process_tree_selects_direct_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes.len(), 1);
        assert_eq!(job.processes[0].name, "codex.exe");
    }

    #[test]
    fn windows_process_tree_selects_wrapped_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "cmd.exe", &["cmd.exe"]),
            test_entry(
                20,
                10,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\codex\\bin\\codex.js",
                ],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "node.exe");
    }

    #[test]
    fn windows_process_tree_selects_cmd_wrapped_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(
                20,
                10,
                "cmd.exe",
                &[
                    "cmd.exe",
                    "/D",
                    "/S",
                    "/C",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\codex.cmd --model gpt-5",
                ],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "cmd.exe");
    }

    #[test]
    fn windows_process_tree_selects_topmost_codex_process_in_single_agent_chain() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(
                20,
                10,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\bin\\codex.js",
                ],
            ),
            test_entry(
                30,
                20,
                "codex.exe",
                &["C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\node_modules\\@openai\\codex-win32-x64\\vendor\\x86_64-pc-windows-msvc\\bin\\codex.exe"],
            ),
            test_entry(40, 30, "node_repl.exe", &["node_repl.exe"]),
            test_entry(
                50,
                40,
                "codex.exe",
                &["codex.exe", "app-server", "--listen", "stdio://"],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "node.exe");
    }

    #[test]
    fn windows_process_tree_selects_topmost_claude_process_in_single_agent_chain() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "claude.exe", &["claude.exe"]),
            test_entry(30, 20, "claude.exe", &["claude.exe", "mcp-server"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "claude.exe");
    }

    #[test]
    fn windows_process_tree_returns_shell_for_same_agent_siblings() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "codex.exe", &["codex.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_process_tree_returns_shell_for_plain_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "git.exe", &["git.exe", "status"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_process_tree_returns_shell_for_multiple_agent_descendants() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "claude.exe", &["claude.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_session_processes_collects_shell_and_descendants() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "cmd.exe", &["cmd.exe"]),
            test_entry(30, 20, "node.exe", &["node.exe"]),
            test_entry(40, 1, "unrelated.exe", &["unrelated.exe"]),
        ];

        let mut pids = super::session_processes_from_entries(10, &entries);
        pids.sort_unstable();

        assert_eq!(pids, vec![10, 20, 30]);
    }

    #[test]
    fn scrollback_editor_argv_uses_editor_env_and_appends_path() {
        let path = std::path::Path::new(r"C:\Users\User\AppData\Local\Temp\herdr scrollback.txt");
        let argv = super::scrollback_editor_argv_with_env(
            path,
            Some(r#""C:\Program Files\Microsoft VS Code\Code.exe" --wait"#),
        )
        .unwrap();

        assert_eq!(argv[0], r"C:\Program Files\Microsoft VS Code\Code.exe");
        assert_eq!(argv[1], "--wait");
        assert_eq!(argv[2], path.display().to_string());
    }

    #[test]
    fn scrollback_editor_argv_falls_back_to_notepad() {
        let path = std::path::Path::new(r"C:\Temp\herdr-scrollback.txt");
        let argv = super::scrollback_editor_argv_with_env(path, None).unwrap();

        assert_eq!(
            argv,
            vec!["notepad.exe".to_string(), path.display().to_string()]
        );
    }

    fn test_entry(
        pid: u32,
        parent_pid: u32,
        name: &str,
        argv: &[&str],
    ) -> super::WindowsProcessEntry {
        super::WindowsProcessEntry {
            pid,
            parent_pid,
            name: name.to_string(),
            argv0: argv.first().map(|value| (*value).to_string()),
            argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
            cmdline: Some(argv.join(" ")),
        }
    }
}
