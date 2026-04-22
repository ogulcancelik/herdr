use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

static PID_REGISTRY: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
static RUNTIME_DIR_REGISTRY: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static INIT: Once = Once::new();
static CLEANUP_GUARD: OnceLock<CleanupGuard> = OnceLock::new();
const WATCHDOG_SCAN_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_OWNER_MARKER: &str = ".herdr-test-owner-pid";

pub fn register_spawned_herdr_pid(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };

    ensure_cleanup_hooks();
    let mut registry = pid_registry_lock();
    registry.insert(pid);
}

pub fn unregister_spawned_herdr_pid(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };

    if let Some(registry) = PID_REGISTRY.get() {
        let mut guard = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&pid);
    }
}

pub fn register_runtime_dir(path: &Path) {
    ensure_cleanup_hooks();

    let _ = fs::create_dir_all(path);
    let _ = fs::write(
        path.join(RUNTIME_OWNER_MARKER),
        std::process::id().to_string(),
    );

    let mut runtime_dirs = runtime_dir_registry_lock();
    runtime_dirs.insert(path.to_path_buf());
}

pub fn unregister_runtime_dir(path: &Path) {
    if let Some(registry) = RUNTIME_DIR_REGISTRY.get() {
        let mut guard = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(path);
    }
}

pub fn cleanup_test_base(base: &Path) {
    let runtime_dir = base.join("runtime");
    let runtime_dirs = HashSet::from([runtime_dir.clone()]);

    terminate_servers_for_runtime_dirs(&runtime_dirs);
    unregister_runtime_dir(&runtime_dir);
    let _ = fs::remove_dir_all(base);
}

pub fn cleanup_registered_herdr_pids() {
    let pids: Vec<u32> = {
        let mut registry = pid_registry_lock();
        registry.drain().collect()
    };

    for pid in pids {
        terminate_pid(pid);
    }

    let runtime_dirs: HashSet<PathBuf> = {
        let mut runtime_dirs = runtime_dir_registry_lock();
        runtime_dirs.drain().collect()
    };

    terminate_servers_for_runtime_dirs(&runtime_dirs);
    let _ = cleanup_servers_with_missing_runtime_dir();
}

fn ensure_cleanup_hooks() {
    INIT.call_once(|| {
        let _ = cleanup_servers_with_missing_runtime_dir();
        start_global_watchdog();

        let _ = CLEANUP_GUARD.set(CleanupGuard);

        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            cleanup_registered_herdr_pids();
            previous_hook(panic_info);
        }));

        let _ = ctrlc::set_handler(|| {
            cleanup_registered_herdr_pids();
            std::process::exit(130);
        });

        unsafe {
            libc::atexit(run_atexit_cleanup);
        }
    });
}

fn pid_registry_lock() -> std::sync::MutexGuard<'static, HashSet<u32>> {
    PID_REGISTRY
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn runtime_dir_registry_lock() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    RUNTIME_DIR_REGISTRY
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn registered_runtime_dirs_snapshot() -> HashSet<PathBuf> {
    if let Some(runtime_dirs) = RUNTIME_DIR_REGISTRY.get() {
        runtime_dirs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    } else {
        HashSet::new()
    }
}

fn should_terminate_runtime_dir(
    runtime_dir: &Path,
    registered_runtime_dirs: &HashSet<PathBuf>,
) -> bool {
    if !registered_runtime_dirs.contains(runtime_dir) {
        return false;
    }

    if !runtime_dir.exists() {
        return true;
    }

    !runtime_dir_owner_alive(runtime_dir)
}

fn start_global_watchdog() {
    thread::spawn(|| loop {
        thread::sleep(WATCHDOG_SCAN_INTERVAL);

        if let Err(err) = cleanup_servers_with_missing_runtime_dir() {
            eprintln!("herdr test cleanup watchdog error: {err}");
        }
    });
}

fn cleanup_servers_with_missing_runtime_dir() -> std::io::Result<()> {
    let registered_runtime_dirs = registered_runtime_dirs_snapshot();
    if registered_runtime_dirs.is_empty() {
        return Ok(());
    }

    for pid in iter_worktree_server_pids()? {
        let Some(runtime_dir) = process_runtime_dir(pid)? else {
            continue;
        };

        if should_terminate_runtime_dir(&runtime_dir, &registered_runtime_dirs) {
            terminate_pid(pid);
        }
    }

    Ok(())
}

fn terminate_servers_for_runtime_dirs(runtime_dirs: &HashSet<PathBuf>) {
    if runtime_dirs.is_empty() {
        return;
    }

    let Ok(pids) = iter_worktree_server_pids() else {
        return;
    };

    for pid in pids {
        let Ok(runtime_dir) = process_runtime_dir(pid) else {
            continue;
        };

        let Some(runtime_dir) = runtime_dir else {
            continue;
        };

        if runtime_dirs.contains(&runtime_dir) {
            terminate_pid(pid);
        }
    }
}

fn iter_worktree_server_pids() -> std::io::Result<Vec<u32>> {
    let own_pid = std::process::id();
    let mut pids = Vec::new();

    let proc_entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    for entry in proc_entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };

        if pid == own_pid {
            continue;
        }

        if is_test_herdr_server_process(pid) {
            pids.push(pid);
        }
    }

    Ok(pids)
}

fn is_test_herdr_server_process(pid: u32) -> bool {
    let Some(exe_path) = proc_link_target(pid, "exe") else {
        return false;
    };

    if !is_test_herdr_binary(&exe_path) {
        return false;
    }

    let Ok(cmdline) = read_cmdline(pid) else {
        return false;
    };

    cmdline.iter().any(|arg| arg == "server")
}

fn proc_link_target(pid: u32, link: &str) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/{link}")).ok()
}

fn read_cmdline(pid: u32) -> std::io::Result<Vec<String>> {
    let cmdline = fs::read(format!("/proc/{pid}/cmdline"))?;
    Ok(cmdline
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect())
}

fn process_runtime_dir(pid: u32) -> std::io::Result<Option<PathBuf>> {
    let environ = fs::read(format!("/proc/{pid}/environ"))?;

    let mut socket_path: Option<PathBuf> = None;

    for entry in environ.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }

        let kv = String::from_utf8_lossy(entry);
        if let Some(value) = kv.strip_prefix("XDG_RUNTIME_DIR=") {
            return Ok(Some(PathBuf::from(value)));
        }

        if let Some(value) = kv.strip_prefix("HERDR_SOCKET_PATH=") {
            socket_path = Some(PathBuf::from(value));
        }
    }

    Ok(socket_path.and_then(|path| path.parent().map(Path::to_path_buf)))
}

fn runtime_dir_owner_alive(runtime_dir: &Path) -> bool {
    let marker = runtime_dir.join(RUNTIME_OWNER_MARKER);
    let Ok(contents) = fs::read_to_string(marker) else {
        return false;
    };

    let Ok(owner_pid) = contents.trim().parse::<libc::pid_t>() else {
        return false;
    };

    process_exists(owner_pid)
}

fn current_checkout_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn is_test_herdr_binary(path: &Path) -> bool {
    path.ends_with("target/debug/herdr") && path.starts_with(current_checkout_root())
}

extern "C" fn run_atexit_cleanup() {
    cleanup_registered_herdr_pids();
}

struct CleanupGuard;

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        cleanup_registered_herdr_pids();
    }
}

fn terminate_pid(pid: u32) {
    let pid_t = pid as libc::pid_t;

    if process_exists(pid_t) {
        unsafe {
            libc::kill(pid_t, libc::SIGTERM);
        }
    }

    if wait_for_pid_exit(pid_t, Duration::from_millis(400)) {
        return;
    }

    if process_exists(pid_t) {
        unsafe {
            libc::kill(pid_t, libc::SIGKILL);
        }
    }

    let _ = wait_for_pid_exit(pid_t, Duration::from_secs(2));
}

fn wait_for_pid_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }

        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result == pid {
            return true;
        }

        if result == -1 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ECHILD) => {
                    // Not our child (or already reaped elsewhere). Poll /proc existence
                    // until the process is truly gone.
                    if !process_exists(pid) {
                        return true;
                    }
                }
                Some(libc::ESRCH) => return true,
                _ => {
                    if !process_exists(pid) {
                        return true;
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(20));
    }

    !process_exists(pid)
}

fn process_exists(pid: libc::pid_t) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_missing_runtime_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "herdr-watchdog-scoping-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn watchdog_scoping_does_not_terminate_missing_unregistered_runtime_dir() {
        let runtime_dir = unique_missing_runtime_dir("unregistered");
        let registered_runtime_dirs = HashSet::new();

        assert!(
            !should_terminate_runtime_dir(&runtime_dir, &registered_runtime_dirs),
            "missing runtime dirs must not be killable until they are proven session-owned"
        );
    }

    #[test]
    fn watchdog_scoping_terminates_missing_registered_runtime_dir() {
        let runtime_dir = unique_missing_runtime_dir("registered");
        let mut registered_runtime_dirs = HashSet::new();
        registered_runtime_dirs.insert(runtime_dir.clone());

        assert!(
            should_terminate_runtime_dir(&runtime_dir, &registered_runtime_dirs),
            "missing runtime dirs that are session-owned should be considered killable"
        );
    }

    #[test]
    fn test_binary_matcher_accepts_current_checkout_debug_binary() {
        let binary = current_checkout_root().join("target/debug/herdr");
        assert!(
            is_test_herdr_binary(&binary),
            "current checkout debug binary should be considered test-owned"
        );
    }

    #[test]
    fn test_binary_matcher_rejects_installed_binary() {
        assert!(
            !is_test_herdr_binary(Path::new("/home/can/.local/bin/herdr")),
            "installed binaries must not be considered test-owned"
        );
    }
}
