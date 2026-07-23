pub(super) use std::fs;
pub(super) use std::io::{BufRead, BufReader, Write};
pub(super) use std::os::unix::net::{UnixListener, UnixStream};
pub(super) use std::path::{Path, PathBuf};
pub(super) use std::process::{Command, Stdio};
pub(super) use std::thread;
pub(super) use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) use crate::support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, CURRENT_PROTOCOL,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub(super) const WORKTREE_BOOTSTRAP_MANAGED_COMPONENT: &str =
    "example.worktree-bootstrap-ef876653ffc3";

pub(super) fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/hcli-{}-{nanos}", std::process::id()))
}

pub(super) fn managed_github_plugin_dir(config_home: &Path) -> PathBuf {
    config_home.join("herdr-dev").join("plugins").join("github")
}

pub(super) fn path_missing_or_empty(path: &Path) -> bool {
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => panic!("failed to read {}: {err}", path.display()),
    }
}

pub(super) fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git command failed: git -C {} {}",
        repo.display(),
        args.join(" ")
    );
}

pub(super) fn create_committed_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "--quiet"]);
    run_git(path, &["config", "user.email", "herdr@example.invalid"]);
    run_git(path, &["config", "user.name", "Herdr Test"]);
    fs::write(path.join("README.md"), "test\n").unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "--quiet", "-m", "initial"]);
}

pub(super) struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    pub(super) child: Box<dyn Child + Send + Sync>,
}

pub(super) struct SpawnedServerProcess {
    child: std::process::Child,
}

impl Drop for SpawnedServerProcess {
    fn drop(&mut self) {
        let pid = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.wait();
        unregister_spawned_herdr_pid(Some(pid));
    }
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

pub(super) fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

pub(super) fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

pub(super) fn spawn_herdr(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
) -> SpawnedHerdr {
    spawn_herdr_with_config(
        config_home,
        runtime_dir,
        socket_path,
        None,
        "onboarding = false\n",
    )
}

pub(super) fn spawn_herdr_with_pane_history(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
) -> SpawnedHerdr {
    spawn_herdr_with_config(
        config_home,
        runtime_dir,
        socket_path,
        None,
        "onboarding = false\n[experimental]\npane_history = true\n",
    )
}

pub(super) fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

pub(super) fn named_session_socket(config_home: &Path, session: &str) -> PathBuf {
    config_home
        .join(app_dir_name())
        .join("sessions")
        .join(session)
        .join("herdr.sock")
}

pub(super) fn spawn_named_server(
    config_home: &Path,
    runtime_dir: &Path,
    session: &str,
) -> SpawnedServerProcess {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command
        .args(["--session", session, "server"])
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    let child = command.spawn().unwrap();
    register_spawned_herdr_pid(Some(child.id()));
    SpawnedServerProcess { child }
}

pub(super) fn run_named_cli(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
) -> std::process::Output {
    run_named_cli_with_socket_override(config_home, runtime_dir, args, None)
}

pub(super) fn run_named_cli_with_socket_override(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
    socket_override: Option<&Path>,
) -> std::process::Output {
    run_named_cli_with_env_and_socket_override(config_home, runtime_dir, args, &[], socket_override)
}

pub(super) fn run_named_cli_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
) -> std::process::Output {
    run_named_cli_with_env_and_socket_override(config_home, runtime_dir, args, envs, None)
}

pub(super) fn run_named_cli_with_env_and_socket_override(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
    socket_override: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV");
    for (key, value) in envs {
        command.env(key, value);
    }
    if let Some(socket_override) = socket_override {
        command.env("HERDR_SOCKET_PATH", socket_override);
    } else {
        command.env_remove("HERDR_SOCKET_PATH");
    }
    command.output().unwrap()
}

pub(super) fn run_named_cli_json(
    config_home: &Path,
    runtime_dir: &Path,
    args: &[&str],
) -> serde_json::Value {
    let output = run_named_cli(config_home, runtime_dir, args);
    assert!(
        output.status.success(),
        "command failed: herdr {}\nstatus: {:?}\nstderr: {}\nstdout: {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub(super) fn spawn_herdr_with_path(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    path_override: Option<&Path>,
) -> SpawnedHerdr {
    spawn_herdr_with_config(
        config_home,
        runtime_dir,
        socket_path,
        path_override,
        "onboarding = false\n",
    )
}

pub(super) fn spawn_herdr_with_config(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    path_override: Option<&Path>,
    config_toml: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        config_toml,
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

pub(super) fn run_cli(socket_path: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command.args(args);
    command.env("HERDR_SOCKET_PATH", socket_path);
    command.output().unwrap()
}

pub(super) fn run_cli_in_dir(
    socket_path: &Path,
    args: &[&str],
    current_dir: &Path,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command.args(args);
    command.current_dir(current_dir);
    command.env("HERDR_SOCKET_PATH", socket_path);
    command.output().unwrap()
}

pub(super) fn pane_topology_snapshot(list_response: &serde_json::Value) -> Vec<serde_json::Value> {
    list_response["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| {
            serde_json::json!({
                "pane_id": pane["pane_id"],
                "terminal_id": pane["terminal_id"],
                "workspace_id": pane["workspace_id"],
                "tab_id": pane["tab_id"],
                "focused": pane["focused"],
            })
        })
        .collect()
}

pub(super) fn run_cli_json(socket_path: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_cli(socket_path, args);
    parse_cli_json_output(args, output)
}

pub(super) fn run_cli_json_in_dir(
    socket_path: &Path,
    args: &[&str],
    current_dir: &Path,
) -> serde_json::Value {
    let output = run_cli_in_dir(socket_path, args, current_dir);
    parse_cli_json_output(args, output)
}

pub(super) fn parse_cli_json_output(
    args: &[&str],
    output: std::process::Output,
) -> serde_json::Value {
    assert!(
        output.status.success(),
        "command failed: herdr {}\nstatus: {:?}\nstderr: {}\nstdout: {}",
        args.join(" "),
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "failed to parse JSON response for `herdr {}`: {}\nstdout: {}\nstderr: {}",
            args.join(" "),
            err,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub(super) fn wait_until(
    timeout: Duration,
    interval: Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(interval);
    }
    false
}

pub(super) fn pane_read_recent_contains(socket_path: &Path, pane_id: &str, expected: &str) -> bool {
    let output = run_cli(
        socket_path,
        &["pane", "read", pane_id, "--source", "recent"],
    );
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).contains(expected)
}

pub(super) fn process_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub(super) fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    !process_exists(pid)
}

pub(super) fn wait_for_pid_file(pid_file: &Path, timeout: Duration) -> Result<u32, String> {
    const STABLE_PID_CONTENT_WINDOW: Duration = Duration::from_millis(250);

    let deadline = Instant::now() + timeout;
    let mut last_contents = String::new();
    let mut stable_candidate: Option<(String, u32, Instant)> = None;

    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(pid_file) {
            let trimmed = contents.trim().to_string();
            last_contents = contents;

            if let Ok(pid) = trimmed.parse::<u32>() {
                match &stable_candidate {
                    Some((candidate_text, candidate_pid, stable_since))
                        if candidate_text == &trimmed && *candidate_pid == pid =>
                    {
                        if stable_since.elapsed() >= STABLE_PID_CONTENT_WINDOW {
                            return Ok(pid);
                        }
                    }
                    _ => {
                        stable_candidate = Some((trimmed, pid, Instant::now()));
                    }
                }
            } else {
                stable_candidate = None;
            }
        }

        thread::sleep(Duration::from_millis(25));
    }

    Err(format!(
        "pid file {} did not contain stable parseable pid before timeout; last contents={:?}",
        pid_file.display(),
        last_contents
    ))
}

#[test]
fn wait_for_pid_file_retries_until_pid_is_written() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("delayed.pid");
    fs::write(&pid_file, "").unwrap();

    let writer = thread::spawn({
        let pid_file = pid_file.clone();
        move || {
            thread::sleep(Duration::from_millis(100));
            fs::write(pid_file, "424242\n").unwrap();
        }
    });

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).unwrap();
    assert_eq!(pid, 424242);

    writer.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn wait_for_pid_file_errors_when_file_never_contains_pid() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("empty.pid");
    fs::write(&pid_file, "").unwrap();

    let err = wait_for_pid_file(&pid_file, Duration::from_millis(150)).unwrap_err();
    assert!(
        err.contains("did not contain stable parseable pid"),
        "unexpected error: {err}"
    );

    cleanup_test_base(&base);
}

#[test]
fn wait_for_pid_file_rejects_unparseable_partial_write_until_stable_contents() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let pid_file = base.join("partial-race.pid");
    fs::write(&pid_file, "").unwrap();

    let writer = thread::spawn({
        let pid_file = pid_file.clone();
        move || {
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "pid=").unwrap();
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "pid=424242").unwrap();
            thread::sleep(Duration::from_millis(40));
            fs::write(&pid_file, "424242\n").unwrap();
        }
    });

    let start = Instant::now();
    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).unwrap();
    assert_eq!(pid, 424242);
    assert!(
        start.elapsed() >= Duration::from_millis(300),
        "helper should wait for stable complete contents, elapsed={:?}",
        start.elapsed()
    );

    writer.join().unwrap();
    cleanup_test_base(&base);
}

pub(super) fn send_request(socket_path: &Path, json: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

pub(super) fn write_fake_pong(
    stream: &mut UnixStream,
    request: &serde_json::Value,
    version: &str,
    protocol: u32,
) {
    writeln!(
        stream,
        "{}",
        serde_json::json!({
            "id": request["id"],
            "result": {
                "type": "pong",
                "version": version,
                "protocol": protocol,
                "capabilities": {
                    "live_handoff": true,
                    "detached_server_daemon": true
                }
            }
        })
    )
    .unwrap();
    stream.flush().unwrap();
}

pub(super) fn accept_fake_cli_operation(listener: &UnixListener) -> (UnixStream, String) {
    loop {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        reader.read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        if request["method"] != "ping" {
            return (stream, line);
        }

        write_fake_pong(
            &mut stream,
            &request,
            "different-build-same-protocol",
            CURRENT_PROTOCOL,
        );
    }
}
