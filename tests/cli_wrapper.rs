#![cfg(not(target_os = "macos"))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/hcli-{}-{nanos}", std::process::id()))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

fn cleanup_spawned_herdr(mut spawned: SpawnedHerdr, base: PathBuf) {
    let pid = spawned.child.process_id();
    let _ = spawned.child.kill();

    if let Some(pid) = pid {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let mut status = 0;
            let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if result == pid as libc::pid_t || result == -1 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    drop(spawned);
    let _ = fs::remove_dir_all(base);
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && std::os::unix::net::UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_herdr(config_home: &Path, runtime_dir: &Path, socket_path: &Path) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
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
    cmd.arg("--no-session");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", socket_path);
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn run_cli(socket_path: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
    command.args(args);
    command.env("HERDR_SOCKET_PATH", socket_path);
    command.output().unwrap()
}

fn process_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    !process_exists(pid)
}

fn send_request(socket_path: &Path, json: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();

    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn pane_run_sends_one_send_text_request_with_trailing_carriage_return() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let mut first_line = String::new();
        let mut first_reader = BufReader::new(first_stream.try_clone().unwrap());
        first_reader.read_line(&mut first_line).unwrap();
        first_stream
            .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
            .unwrap();
        first_stream.write_all(b"\n").unwrap();
        first_stream.flush().unwrap();

        let mut second_line = None;
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut second_stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(second_stream.try_clone().unwrap());
                    reader.read_line(&mut line).unwrap();
                    second_stream
                        .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
                        .unwrap();
                    second_stream.write_all(b"\n").unwrap();
                    second_stream.flush().unwrap();
                    second_line = Some(line);
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("second accept failed: {err}"),
            }
        }

        (first_line, second_line)
    });

    let run = run_cli(&socket_path, &["pane", "run", "1-1", "echo hello"]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let (first_line, second_line) = server.join().unwrap();
    let first_request: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(first_request["method"], "pane.send_text");
    assert_eq!(first_request["params"]["pane_id"], "1-1");
    assert_eq!(first_request["params"]["text"], "echo hello\r");
    assert!(
        second_line.is_none(),
        "pane run sent an unexpected second request: {:?}",
        second_line
    );

    fs::remove_dir_all(base).unwrap();
}

#[test]
fn workspace_and_pane_management_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let listed = run_cli(&socket_path, &["workspace", "list"]);
    assert!(listed.status.success());
    let listed_json: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed_json["result"]["type"], "workspace_list");
    assert_eq!(
        listed_json["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let panes = run_cli(&socket_path, &["pane", "list", "--workspace", "1"]);
    assert!(panes.status.success());
    let panes_json: serde_json::Value = serde_json::from_slice(&panes.stdout).unwrap();
    assert_eq!(panes_json["result"]["panes"].as_array().unwrap().len(), 1);

    let split = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right"],
    );
    assert!(
        split.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&split.stderr)
    );
    let split_json: serde_json::Value = serde_json::from_slice(&split.stdout).unwrap();
    let split_pane_id = split_json["result"]["pane"]["pane_id"].as_str().unwrap();

    let fetched = run_cli(&socket_path, &["pane", "get", split_pane_id]);
    assert!(fetched.status.success());
    let fetched_json: serde_json::Value = serde_json::from_slice(&fetched.stdout).unwrap();
    assert_eq!(fetched_json["result"]["pane"]["pane_id"], split_pane_id);

    let closed = run_cli(&socket_path, &["pane", "close", split_pane_id]);
    assert!(closed.status.success());
    let closed_json: serde_json::Value = serde_json::from_slice(&closed.stdout).unwrap();
    assert_eq!(closed_json["result"]["type"], "ok");

    let renamed = run_cli(
        &socket_path,
        &["workspace", "rename", &workspace_id, "demo"],
    );
    assert!(renamed.status.success());
    let renamed_json: serde_json::Value = serde_json::from_slice(&renamed.stdout).unwrap();
    assert_eq!(renamed_json["result"]["workspace"]["label"], "demo");

    let focused = run_cli(&socket_path, &["workspace", "focus", &workspace_id]);
    assert!(focused.status.success());

    let closed_workspace = run_cli(&socket_path, &["workspace", "close", &workspace_id]);
    assert!(closed_workspace.status.success());
    let closed_workspace_json: serde_json::Value =
        serde_json::from_slice(&closed_workspace.stdout).unwrap();
    assert_eq!(closed_workspace_json["result"]["type"], "ok");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn tab_management_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());
    let created_json: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let workspace_id = created_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_tab_id = created_json["result"]["workspace"]["active_tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let created_tab = run_cli(
        &socket_path,
        &["tab", "create", "--workspace", &workspace_id],
    );
    assert!(created_tab.status.success());
    let created_tab_json: serde_json::Value = serde_json::from_slice(&created_tab.stdout).unwrap();
    let second_tab_id = created_tab_json["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_tab_id, format!("{workspace_id}:2"));

    let listed_tabs = run_cli(&socket_path, &["tab", "list", "--workspace", &workspace_id]);
    assert!(listed_tabs.status.success());
    let listed_tabs_json: serde_json::Value = serde_json::from_slice(&listed_tabs.stdout).unwrap();
    assert_eq!(
        listed_tabs_json["result"]["tabs"].as_array().unwrap().len(),
        2
    );

    let renamed_tab = run_cli(&socket_path, &["tab", "rename", &second_tab_id, "logs"]);
    assert!(renamed_tab.status.success());
    let renamed_tab_json: serde_json::Value = serde_json::from_slice(&renamed_tab.stdout).unwrap();
    assert_eq!(renamed_tab_json["result"]["tab"]["label"], "logs");

    let focused_tab = run_cli(&socket_path, &["tab", "focus", &first_tab_id]);
    assert!(focused_tab.status.success());
    let focused_tab_json: serde_json::Value = serde_json::from_slice(&focused_tab.stdout).unwrap();
    assert_eq!(focused_tab_json["result"]["tab"]["tab_id"], first_tab_id);

    let tab_get = run_cli(&socket_path, &["tab", "get", &second_tab_id]);
    assert!(tab_get.status.success());
    let tab_get_json: serde_json::Value = serde_json::from_slice(&tab_get.stdout).unwrap();
    assert_eq!(tab_get_json["result"]["tab"]["tab_id"], second_tab_id);

    let closed_tab = run_cli(&socket_path, &["tab", "close", &second_tab_id]);
    assert!(closed_tab.status.success());
    let closed_tab_json: serde_json::Value = serde_json::from_slice(&closed_tab.stdout).unwrap();
    assert_eq!(closed_tab_json["result"]["type"], "ok");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_run_read_and_wait_commands_work() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());

    let create = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            "echo alpha && echo beta && printf 'ready\\n'",
        ],
    );
    assert!(create.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "output",
            "1-1",
            "--match",
            "ready",
            "--source",
            "recent",
            "--lines",
            "40",
            "--timeout",
            "5000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["type"], "output_matched");

    let read = run_cli(
        &socket_path,
        &["pane", "read", "1-1", "--source", "recent", "--lines", "40"],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains("alpha"));
    assert!(text.contains("ready"));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn wait_output_matches_recent_unwrapped_text() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let token = "WRAP_WAIT_TEST_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789_ABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789";
    let script = base.join("emit-long-token.sh");
    std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{token}'\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }

    let run = run_cli(
        &socket_path,
        &["pane", "run", "1-1", &format!("sh {}", script.display())],
    );
    assert!(run.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "output",
            "1-1",
            "--match",
            token,
            "--source",
            "recent",
            "--lines",
            "80",
            "--timeout",
            "5000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&waited.stderr),
        String::from_utf8_lossy(&waited.stdout)
    );

    let read = run_cli(
        &socket_path,
        &[
            "pane",
            "read",
            "1-1",
            "--source",
            "recent-unwrapped",
            "--lines",
            "80",
        ],
    );
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains(token));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn closing_pane_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let split = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right"],
    );
    assert!(split.status.success());
    let split_json: serde_json::Value = serde_json::from_slice(&split.stdout).unwrap();
    let pane_id = split_json["result"]["pane"]["pane_id"].as_str().unwrap();

    let pid_file = base.join("pane-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", pane_id, &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["pane", "close", pane_id]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived pane close"
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn closing_workspace_terminates_processes_inside_it() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(created.status.success());

    let pid_file = base.join("workspace-close.pid");
    let command = format!(
        "python3 -c 'import os,time,pathlib; pathlib.Path(r\"{}\").write_text(str(os.getpid())); time.sleep(1000)'",
        pid_file.display()
    );
    let ran = run_cli(&socket_path, &["pane", "run", "1-1", &command]);
    assert!(
        ran.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ran.stderr)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !pid_file.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(pid_file.exists(), "pid file was not created");

    let pid: u32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(process_exists(pid), "child process was not running");

    let closed = run_cli(&socket_path, &["workspace", "close", "1"]);
    assert!(
        closed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "process {pid} survived workspace close"
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn workspace_ids_are_stable_and_pane_numbers_stay_compact() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let ws1 = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    assert!(ws1.status.success());
    let ws1_json: serde_json::Value = serde_json::from_slice(&ws1.stdout).unwrap();
    let ws1_id = ws1_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split_12 = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "right", "--no-focus"],
    );
    let split_12_json: serde_json::Value = serde_json::from_slice(&split_12.stdout).unwrap();
    assert_eq!(
        split_12_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}-2")
    );

    let split_13 = run_cli(
        &socket_path,
        &["pane", "split", "1-1", "--direction", "down", "--no-focus"],
    );
    let split_13_json: serde_json::Value = serde_json::from_slice(&split_13.stdout).unwrap();
    assert_eq!(
        split_13_json["result"]["pane"]["pane_id"],
        format!("{ws1_id}-3")
    );

    let ws2 = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", "/tmp", "--no-focus"],
    );
    let ws2_json: serde_json::Value = serde_json::from_slice(&ws2.stdout).unwrap();
    let ws2_id = ws2_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws2_id, ws1_id);

    let ws2_focus = run_cli(&socket_path, &["workspace", "focus", &ws2_id]);
    assert!(ws2_focus.status.success());
    let ws2_split = run_cli(
        &socket_path,
        &["pane", "split", "2-1", "--direction", "right", "--no-focus"],
    );
    let ws2_split_json: serde_json::Value = serde_json::from_slice(&ws2_split.stdout).unwrap();
    assert_eq!(
        ws2_split_json["result"]["pane"]["pane_id"],
        format!("{ws2_id}-2")
    );

    let ws3 = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", "/", "--no-focus"],
    );
    let ws3_json: serde_json::Value = serde_json::from_slice(&ws3.stdout).unwrap();
    let ws3_id = ws3_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ws3_id, ws1_id);
    assert_ne!(ws3_id, ws2_id);

    let close_ws2 = run_cli(&socket_path, &["workspace", "close", &ws2_id]);
    assert!(close_ws2.status.success());

    let workspaces = run_cli(&socket_path, &["workspace", "list"]);
    let workspaces_json: serde_json::Value = serde_json::from_slice(&workspaces.stdout).unwrap();
    let ids: Vec<String> = workspaces_json["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ws| ws["workspace_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![ws1_id.clone(), ws3_id.clone()]);

    let new_ws = run_cli(
        &socket_path,
        &["workspace", "create", "--cwd", "/var/tmp", "--no-focus"],
    );
    let new_ws_json: serde_json::Value = serde_json::from_slice(&new_ws.stdout).unwrap();
    let new_ws_id = new_ws_json["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(new_ws_id, ws1_id);
    assert_ne!(new_ws_id, ws2_id);
    assert_ne!(new_ws_id, ws3_id);

    let ws3_panes = run_cli(&socket_path, &["pane", "list", "--workspace", &ws3_id]);
    let ws3_panes_json: serde_json::Value = serde_json::from_slice(&ws3_panes.stdout).unwrap();
    assert_eq!(
        ws3_panes_json["result"]["panes"][0]["pane_id"],
        format!("{ws3_id}-1")
    );

    let close_middle = run_cli(&socket_path, &["pane", "close", &format!("{ws1_id}-2")]);
    assert!(close_middle.status.success());
    let ws1_panes = run_cli(&socket_path, &["pane", "list", "--workspace", &ws1_id]);
    let ws1_panes_json: serde_json::Value = serde_json::from_slice(&ws1_panes.stdout).unwrap();
    let pane_ids: Vec<String> = ws1_panes_json["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["pane_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(pane_ids, vec![format!("{ws1_id}-1"), format!("{ws1_id}-2")]);

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn pane_shell_gets_herdr_socket_and_pane_env() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_env_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());

    let env_capture = base.join("pane-env.txt");
    let ran = run_cli(
        &socket_path,
        &[
            "pane",
            "run",
            "1-1",
            &format!(
                "printf '%s\\n%s\\n' \"$HERDR_SOCKET_PATH\" \"$HERDR_PANE_ID\" > {}",
                env_capture.display()
            ),
        ],
    );
    assert!(ran.status.success());

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !env_capture.exists() {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(env_capture.exists(), "env capture file was not created");
    let text = fs::read_to_string(&env_capture).unwrap();
    assert!(
        text.contains(&socket_path.display().to_string()),
        "env file was: {text:?}"
    );
    assert!(text.contains("p_"), "env file was: {text:?}");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn wait_agent_state_exits_when_state_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nprintf 'Working...\\n'\nsleep 1\nprintf '\\033[2J\\033[Hdone\\n'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("--no-session");
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", &socket_path);
    cmd.env_remove("HERDR_ENV");
    cmd.env(
        "PATH",
        format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let child = pair.slave.spawn_command(cmd).unwrap();
    let herdr = SpawnedHerdr {
        _master: pair.master,
        child,
    };

    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_2","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());

    let start_pi = run_cli(&socket_path, &["pane", "run", "1-1", "pi"]);
    assert!(start_pi.status.success());

    let waited = run_cli(
        &socket_path,
        &[
            "wait",
            "agent-state",
            "1-1",
            "--state",
            "idle",
            "--timeout",
            "5000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["event"], "pane.agent_state_changed");
    assert_eq!(waited_json["data"]["state"], "idle");
    assert_eq!(waited_json["data"]["agent"], "pi");

    cleanup_spawned_herdr(herdr, base);
}
