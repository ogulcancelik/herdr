mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/hapi-{}-{nanos}", std::process::id()))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
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

fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_herdr(config_home: &Path, runtime_dir: &Path, socket_path: &Path) -> SpawnedHerdr {
    spawn_herdr_with_path(config_home, runtime_dir, socket_path, None)
}

fn spawn_herdr_with_path(
    config_home: &Path,
    runtime_dir: &Path,
    socket_path: &Path,
    path_override: Option<&Path>,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
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

struct JsonLineReader {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl JsonLineReader {
    fn connect(socket_path: &Path) -> Self {
        Self {
            stream: UnixStream::connect(socket_path).unwrap(),
            buf: Vec::new(),
        }
    }

    fn send_line(&mut self, json: &str) {
        self.stream.write_all(json.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
        self.stream.flush().unwrap();
    }

    fn read_json_line(&mut self, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        self.stream.set_nonblocking(true).unwrap();

        loop {
            assert!(Instant::now() < deadline, "timed out waiting for json line");

            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8(self.buf.drain(..=pos).collect()).unwrap();
                self.stream.set_nonblocking(false).unwrap();
                return serde_json::from_str(&line).unwrap();
            }

            let mut bytes = [0u8; 256];
            match self.stream.read(&mut bytes) {
                Ok(0) => panic!("stream closed while waiting for json line"),
                Ok(n) => self.buf.extend_from_slice(&bytes[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("failed to read json line: {err}"),
            }
        }
    }
}

fn send_request(socket_path: &Path, json: &str) -> serde_json::Value {
    let mut reader = JsonLineReader::connect(socket_path);
    reader.send_line(json);
    reader.read_json_line(Duration::from_secs(5))
}

fn open_subscription(socket_path: &Path, json: &str) -> JsonLineReader {
    let mut reader = JsonLineReader::connect(socket_path);
    reader.send_line(json);
    reader
}

fn wait_for_event(
    reader: &mut JsonLineReader,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    wait_for_event_matching(reader, expected, timeout, |_| true)
}

fn wait_for_event_matching<F>(
    reader: &mut JsonLineReader,
    expected: &str,
    timeout: Duration,
    mut matches: F,
) -> serde_json::Value
where
    F: FnMut(&serde_json::Value) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let value = reader.read_json_line(remaining.max(Duration::from_millis(1)));
        if value["event"] == expected && matches(&value) {
            return value;
        }
    }
}

#[test]
fn ping_over_socket_returns_version() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let value = send_request(
        &socket_path,
        r#"{"id":"req_1","method":"ping","params":{}}"#,
    );
    assert_eq!(value["id"], "req_1");
    assert_eq!(value["result"]["type"], "pong");
    assert_eq!(value["result"]["version"], env!("CARGO_PKG_VERSION"));

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn workspace_list_and_create_round_trip() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let empty = send_request(
        &socket_path,
        r#"{"id":"req_2","method":"workspace.list","params":{}}"#,
    );
    assert_eq!(empty["id"], "req_2");
    assert_eq!(empty["result"]["type"], "workspace_list");
    assert_eq!(empty["result"]["workspaces"].as_array().unwrap().len(), 0);

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_3","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert_eq!(created["id"], "req_3");
    assert_eq!(created["result"]["type"], "workspace_created");
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let active_tab_id = created["result"]["workspace"]["active_tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    let root_pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created["result"]["workspace"]["number"], 1);
    assert_eq!(created["result"]["workspace"]["focused"], true);
    assert_eq!(created["result"]["workspace"]["tab_count"], 1);
    assert_eq!(created["result"]["tab"]["tab_id"], active_tab_id);
    assert_eq!(created["result"]["root_pane"]["tab_id"], active_tab_id);
    assert_eq!(active_tab_id, format!("{workspace_id}:1"));

    let listed = send_request(
        &socket_path,
        r#"{"id":"req_4","method":"workspace.list","params":{}}"#,
    );
    let workspaces = listed["result"]["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0]["workspace_id"], workspace_id);

    let fetched = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_5","method":"workspace.get","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(fetched["result"]["workspace"]["workspace_id"], workspace_id);

    let panes = send_request(
        &socket_path,
        r#"{"id":"req_6","method":"pane.list","params":{}}"#,
    );
    let panes = panes["result"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["workspace_id"], workspace_id);
    assert_eq!(panes[0]["tab_id"], active_tab_id);
    let pane_id = panes[0]["pane_id"].as_str().unwrap().to_string();
    assert_eq!(pane_id, root_pane_id);

    let pane = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_7","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(pane["result"]["pane"]["pane_id"], pane_id);

    let read = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_8","method":"pane.read","params":{{"pane_id":"{}","source":"visible"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(read["result"]["read"]["pane_id"], pane_id);
    assert_eq!(read["result"]["read"]["tab_id"], active_tab_id);
    assert!(read["result"]["read"]["text"].is_string());

    let send_text = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_9","method":"pane.send_text","params":{{"pane_id":"{}","text":"echo alpha; echo beta; echo gamma"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_text["result"]["type"], "ok");

    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_10","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    std::thread::sleep(Duration::from_millis(300));

    let recent = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_11","method":"pane.read","params":{{"pane_id":"{}","source":"recent","lines":20}}}}"#,
            pane_id
        ),
    );
    let recent_text = recent["result"]["read"]["text"].as_str().unwrap();
    assert!(recent_text.contains("beta") || recent_text.contains("gamma"));

    let waited = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_12","method":"pane.wait_for_output","params":{{"pane_id":"{}","source":"recent","lines":40,"match":{{"type":"substring","value":"gamma"}},"timeout_ms":2000}}}}"#,
            pane_id
        ),
    );
    assert_eq!(waited["result"]["type"], "output_matched");
    assert!(waited["result"]["matched_line"]
        .as_str()
        .unwrap()
        .contains("gamma"));
    assert!(waited["result"]["read"]["text"]
        .as_str()
        .unwrap()
        .contains("gamma"));

    let send_input = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_12b","method":"pane.send_input","params":{{"pane_id":"{}","text":"echo delta","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_input["result"]["type"], "ok");

    let waited_delta = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_12c","method":"pane.wait_for_output","params":{{"pane_id":"{}","source":"recent","lines":40,"match":{{"type":"substring","value":"delta"}},"timeout_ms":2000}}}}"#,
            pane_id
        ),
    );
    assert_eq!(waited_delta["result"]["type"], "output_matched");
    assert!(waited_delta["result"]["matched_line"]
        .as_str()
        .unwrap()
        .contains("delta"));

    let waited_regex = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_13","method":"pane.wait_for_output","params":{{"pane_id":"{}","source":"recent","lines":40,"match":{{"type":"regex","value":"alp.*gamma"}},"timeout_ms":2000}}}}"#,
            pane_id
        ),
    );
    assert_eq!(waited_regex["result"]["type"], "output_matched");
    assert!(waited_regex["result"]["matched_line"]
        .as_str()
        .unwrap()
        .contains("alpha"));

    let timeout = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_14","method":"pane.wait_for_output","params":{{"pane_id":"{}","source":"recent","lines":10,"match":{{"type":"substring","value":"definitely-not-there"}},"timeout_ms":200}}}}"#,
            pane_id
        ),
    );
    assert_eq!(timeout["error"]["code"], "timeout");

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn tab_methods_round_trip_over_socket() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_tab_id = created["result"]["workspace"]["active_tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(first_tab_id, format!("{workspace_id}:1"));

    let tab_created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t2","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(tab_created["result"]["type"], "tab_created");
    let second_tab_id = tab_created["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    let second_root_pane_id = tab_created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_tab_id, format!("{workspace_id}:2"));
    assert_eq!(tab_created["result"]["tab"]["focused"], true);
    assert_eq!(tab_created["result"]["root_pane"]["tab_id"], second_tab_id);

    let tab_list = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t3","method":"tab.list","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    let tabs = tab_list["result"]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0]["tab_id"], first_tab_id);

    let panes = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t3b","method":"pane.list","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    let panes = panes["result"]["panes"].as_array().unwrap();
    assert!(panes
        .iter()
        .any(|pane| pane["pane_id"] == second_root_pane_id));
    assert_eq!(tabs[1]["tab_id"], second_tab_id);

    let tab_get = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t4","method":"tab.get","params":{{"tab_id":"{}"}}}}"#,
            second_tab_id
        ),
    );
    assert_eq!(tab_get["result"]["tab"]["tab_id"], second_tab_id);

    let renamed = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t5","method":"tab.rename","params":{{"tab_id":"{}","label":"logs"}}}}"#,
            second_tab_id
        ),
    );
    assert_eq!(renamed["result"]["tab"]["label"], "logs");

    let focused = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t6","method":"tab.focus","params":{{"tab_id":"{}"}}}}"#,
            first_tab_id
        ),
    );
    assert_eq!(focused["result"]["tab"]["tab_id"], first_tab_id);
    assert_eq!(focused["result"]["tab"]["focused"], true);

    let closed = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_t7","method":"tab.close","params":{{"tab_id":"{}"}}}}"#,
            second_tab_id
        ),
    );
    assert_eq!(closed["result"]["type"], "ok");

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn tab_create_with_no_focus_preserves_active_tab() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_nf_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_tab_id = created["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let tab_created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_nf_2","method":"tab.create","params":{{"workspace_id":"{}","focus":false}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(tab_created["result"]["type"], "tab_created");
    let second_tab_id = tab_created["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_tab_id, format!("{workspace_id}:2"));
    assert_eq!(tab_created["result"]["tab"]["focused"], false);

    let tab_list = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_nf_3","method":"tab.list","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    let tabs = tab_list["result"]["tabs"].as_array().unwrap();
    assert_eq!(tabs[0]["tab_id"], first_tab_id);
    assert_eq!(tabs[0]["focused"], true);
    assert_eq!(tabs[1]["tab_id"], second_tab_id);
    assert_eq!(tabs[1]["focused"], false);

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn events_subscribe_streams_workspace_tab_and_agent_events() {
    let _lock = test_lock();
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

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let mut reader = open_subscription(
        &socket_path,
        r#"{"id":"sub_life_a","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"},{"type":"workspace.focused"},{"type":"tab.created"},{"type":"tab.focused"},{"type":"tab.renamed"},{"type":"pane.created"},{"type":"pane.focused"},{"type":"pane.agent_detected"}]}}"#,
    );

    let ack = reader.read_json_line(Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_life_a");
    assert_eq!(ack["result"]["type"], "subscription_started");

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let workspace_created =
        wait_for_event(&mut reader, "workspace_created", Duration::from_secs(2));
    assert_eq!(
        workspace_created["data"]["workspace"]["workspace_id"],
        workspace_id
    );
    let workspace_focused =
        wait_for_event(&mut reader, "workspace_focused", Duration::from_secs(2));
    assert_eq!(workspace_focused["data"]["workspace_id"], workspace_id);

    let first_tab_id = format!("{workspace_id}:1");
    let tab_created = wait_for_event(&mut reader, "tab_created", Duration::from_secs(2));
    assert_eq!(tab_created["data"]["tab"]["tab_id"], first_tab_id);
    let tab_focused = wait_for_event(&mut reader, "tab_focused", Duration::from_secs(2));
    assert_eq!(tab_focused["data"]["tab_id"], first_tab_id);

    let pane_created = wait_for_event(&mut reader, "pane_created", Duration::from_secs(2));
    let pane_id = pane_created["data"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane_focused = wait_for_event(&mut reader, "pane_focused", Duration::from_secs(2));
    assert_eq!(pane_focused["data"]["pane_id"], pane_id);

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l2","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l3","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let agent_detected = wait_for_event(&mut reader, "pane_agent_detected", Duration::from_secs(3));
    assert_eq!(agent_detected["data"]["pane_id"], pane_id);
    assert_eq!(agent_detected["data"]["agent"], "pi");

    let new_tab = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l4","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    let second_tab_id = new_tab["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(second_tab_id, format!("{workspace_id}:2"));

    let created_tab_event = wait_for_event(&mut reader, "tab_created", Duration::from_secs(2));
    assert_eq!(created_tab_event["data"]["tab"]["tab_id"], second_tab_id);
    let focused_tab_event = wait_for_event(&mut reader, "tab_focused", Duration::from_secs(2));
    assert_eq!(focused_tab_event["data"]["tab_id"], second_tab_id);

    let renamed_tab = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l5","method":"tab.rename","params":{{"tab_id":"{}","label":"logs"}}}}"#,
            second_tab_id
        ),
    );
    assert_eq!(renamed_tab["result"]["tab"]["label"], "logs");
    let renamed_event = wait_for_event(&mut reader, "tab_renamed", Duration::from_secs(2));
    assert_eq!(renamed_event["data"]["tab_id"], second_tab_id);
    assert_eq!(renamed_event["data"]["label"], "logs");

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn events_subscribe_streams_pane_split_and_close_events() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_pc_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pane_id = format!("{workspace_id}-1");

    let mut reader = open_subscription(
        &socket_path,
        r#"{"id":"sub_life_b","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.created"},{"type":"pane.closed"}]}}"#,
    );

    let ack = reader.read_json_line(Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_life_b");
    assert_eq!(ack["result"]["type"], "subscription_started");

    let split = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_pc_2","method":"pane.split","params":{{"target_pane_id":"{}","direction":"right","focus":true}}}}"#,
            pane_id
        ),
    );
    let split_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split_created = wait_for_event_matching(
        &mut reader,
        "pane_created",
        Duration::from_secs(2),
        |value| value["data"]["pane"]["pane_id"] == split_pane_id,
    );
    assert_eq!(split_created["data"]["pane"]["pane_id"], split_pane_id);

    let closed = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_pc_3","method":"pane.close","params":{{"pane_id":"{}"}}}}"#,
            split_pane_id
        ),
    );
    assert_eq!(closed["result"]["type"], "ok");

    let pane_closed = wait_for_event_matching(
        &mut reader,
        "pane_closed",
        Duration::from_secs(2),
        |value| value["data"]["pane_id"] == split_pane_id,
    );
    assert_eq!(pane_closed["data"]["pane_id"], split_pane_id);

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn events_subscribe_streams_tab_and_workspace_close_events() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_tc_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let new_tab = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_tc_2","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    let second_tab_id = new_tab["result"]["tab"]["tab_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut reader = open_subscription(
        &socket_path,
        r#"{"id":"sub_life_c","method":"events.subscribe","params":{"subscriptions":[{"type":"tab.closed"},{"type":"workspace.closed"}]}}"#,
    );

    let ack = reader.read_json_line(Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_life_c");
    assert_eq!(ack["result"]["type"], "subscription_started");

    let closed_tab = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_tc_3","method":"tab.close","params":{{"tab_id":"{}"}}}}"#,
            second_tab_id
        ),
    );
    assert_eq!(closed_tab["result"]["type"], "ok");

    let tab_closed = wait_for_event(&mut reader, "tab_closed", Duration::from_secs(2));
    assert_eq!(tab_closed["data"]["tab_id"], second_tab_id);

    let closed_ws = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_tc_4","method":"workspace.close","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(closed_ws["result"]["type"], "ok");

    let workspace_closed = wait_for_event(&mut reader, "workspace_closed", Duration::from_secs(2));
    assert_eq!(workspace_closed["data"]["workspace_id"], workspace_id);

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[cfg(not(target_os = "macos"))]
#[test]
fn pane_report_agent_updates_effective_state() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(&fake_pi, "#!/bin/sh\nprintf 'Working...\\n'\nsleep 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .map(|workspace_id| format!("{}-1", workspace_id))
        .unwrap();

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_2","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_3","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let pane = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"req_hook_detect","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
                pane_id
            ),
        );
        if pane["result"]["pane"]["agent"] == "pi" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pi agent was never detected: {pane}"
        );
        thread::sleep(Duration::from_millis(100));
    }

    let hook = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_5","method":"pane.report_agent","params":{{"pane_id":"{}","source":"herdr:pi","agent":"pi","state":"working","message":"thinking"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(hook["result"]["type"], "ok");

    let pane = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_6","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(pane["result"]["pane"]["agent"], "pi");
    assert_eq!(pane["result"]["pane"]["agent_status"], "working");

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn pane_report_agent_accepts_unknown_agent_labels() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_generic_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .map(|workspace_id| format!("{}-1", workspace_id))
        .unwrap();

    let hook = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_generic_2","method":"pane.report_agent","params":{{"pane_id":"{}","source":"custom:hermes","agent":"hermes","state":"working"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(hook["result"]["type"], "ok");

    let pane = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_hook_generic_3","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(pane["result"]["pane"]["agent"], "hermes");
    assert_eq!(pane["result"]["pane"]["agent_status"], "working");

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn pane_release_agent_suppresses_reacquire_during_graceful_exit() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    let stop_file = base.join("pi-stop");
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nprintf 'Working...\\n'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\n",
            stop_file.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_release_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .map(|workspace_id| format!("{}-1", workspace_id))
        .unwrap();

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_release_2","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_release_3","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let pane = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"req_release_detect","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
                pane_id
            ),
        );
        if pane["result"]["pane"]["agent"] == "pi" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pi agent was never detected: {pane}"
        );
        thread::sleep(Duration::from_millis(100));
    }

    let hook = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_release_4","method":"pane.report_agent","params":{{"pane_id":"{}","source":"herdr:pi","agent":"pi","state":"working"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(hook["result"]["type"], "ok");

    let released = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_release_5","method":"pane.release_agent","params":{{"pane_id":"{}","source":"herdr:pi","agent":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(released["result"]["type"], "ok");

    let suppression_deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < suppression_deadline {
        let pane = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"req_release_6","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
                pane_id
            ),
        );
        assert!(
            pane["result"]["pane"]["agent"].is_null(),
            "pane reacquired pi during graceful release: {pane}"
        );
        assert_eq!(pane["result"]["pane"]["agent_status"], "unknown");
        thread::sleep(Duration::from_millis(50));
    }

    fs::write(&stop_file, "stop").unwrap();

    let cleared_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let pane = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"req_release_7","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
                pane_id
            ),
        );
        if pane["result"]["pane"]["agent"].is_null()
            && pane["result"]["pane"]["agent_status"] == "unknown"
        {
            break;
        }
        assert!(
            Instant::now() < cleared_deadline,
            "pi agent was not cleared promptly after release: {pane}"
        );
        thread::sleep(Duration::from_millis(50));
    }

    cleanup_spawned_herdr(child, base);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn pane_clear_agent_authority_restores_fallback_state() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(&fake_pi, "#!/bin/sh\nprintf 'Working...\\n'\nsleep 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .map(|workspace_id| format!("{}-1", workspace_id))
        .unwrap();

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_2","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_3","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let pane = send_request(
            &socket_path,
            &format!(
                r#"{{"id":"req_clear_detect","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
                pane_id
            ),
        );
        if pane["result"]["pane"]["agent"] == "pi" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pi agent was never detected: {pane}"
        );
        thread::sleep(Duration::from_millis(100));
    }

    let fallback_before_hook = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_fallback","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            pane_id
        ),
    );
    let fallback_status = fallback_before_hook["result"]["pane"]["agent_status"]
        .as_str()
        .unwrap()
        .to_string();

    let hook = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_4","method":"pane.report_agent","params":{{"pane_id":"{}","source":"herdr:pi","agent":"pi","state":"idle"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(hook["result"]["type"], "ok");

    let cleared = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_5","method":"pane.clear_agent_authority","params":{{"pane_id":"{}","source":"herdr:pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(cleared["result"]["type"], "ok");

    let pane = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_clear_6","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(pane["result"]["pane"]["agent"], "pi");
    assert_eq!(pane["result"]["pane"]["agent_status"], fallback_status);

    cleanup_spawned_herdr(child, base);
}

#[test]
fn events_subscribe_streams_output_and_agent_status_events() {
    let _lock = test_lock();
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

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_20","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    assert!(created["result"]["workspace"]["workspace_id"].is_string());

    let panes = send_request(
        &socket_path,
        r#"{"id":"req_21","method":"pane.list","params":{}}"#,
    );
    let pane_id = panes["result"]["panes"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut reader = open_subscription(
        &socket_path,
        &format!(
            r#"{{"id":"sub_1","method":"events.subscribe","params":{{"subscriptions":[{{"type":"pane.output_matched","pane_id":"{}","source":"recent","lines":40,"match":{{"type":"substring","value":"hello from socket"}}}},{{"type":"pane.agent_status_changed","pane_id":"{}","agent_status":"idle"}}]}}}}"#,
            pane_id, pane_id,
        ),
    );

    let ack = reader.read_json_line(Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_1");
    assert_eq!(ack["result"]["type"], "subscription_started");

    let send_text = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_22","method":"pane.send_text","params":{{"pane_id":"{}","text":"echo hello from socket"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_text["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_23","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let output_event = reader.read_json_line(Duration::from_secs(3));
    assert_eq!(output_event["event"], "pane.output_matched");
    assert_eq!(output_event["data"]["pane_id"], pane_id);
    assert!(output_event["data"]["matched_line"]
        .as_str()
        .unwrap()
        .contains("hello from socket"));
    assert!(output_event["data"]["read"]["text"]
        .as_str()
        .unwrap()
        .contains("hello from socket"));

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_24","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_25","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let agent_idle = reader.read_json_line(Duration::from_secs(8));
    assert_eq!(agent_idle["event"], "pane.agent_status_changed");
    assert_eq!(agent_idle["data"]["pane_id"], pane_id);
    assert_eq!(agent_idle["data"]["agent_status"], "idle");
    assert_eq!(agent_idle["data"]["agent"], "pi");

    cleanup_spawned_herdr(child, base);
}

#[test]
fn pane_info_and_subscriptions_expose_done_agent_status() {
    let _lock = test_lock();
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

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);
    let child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_1","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let background_pane_id = format!("{}-1", workspace_id);

    let tab_created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_2","method":"tab.create","params":{{"workspace_id":"{}","focus":true}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(tab_created["result"]["type"], "tab_created");

    let mut reader = open_subscription(
        &socket_path,
        &format!(
            r#"{{"id":"sub_status","method":"events.subscribe","params":{{"subscriptions":[{{"type":"pane.agent_status_changed","pane_id":"{}","agent_status":"done"}}]}}}}"#,
            background_pane_id,
        ),
    );
    let ack = reader.read_json_line(Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_status");
    assert_eq!(ack["result"]["type"], "subscription_started");

    let send_pi = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_3","method":"pane.send_text","params":{{"pane_id":"{}","text":"pi"}}}}"#,
            background_pane_id
        ),
    );
    assert_eq!(send_pi["result"]["type"], "ok");
    let send_enter = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_4","method":"pane.send_keys","params":{{"pane_id":"{}","keys":["Enter"]}}}}"#,
            background_pane_id
        ),
    );
    assert_eq!(send_enter["result"]["type"], "ok");

    let status_event = reader.read_json_line(Duration::from_secs(8));
    assert_eq!(status_event["event"], "pane.agent_status_changed");
    assert_eq!(status_event["data"]["pane_id"], background_pane_id);
    assert_eq!(status_event["data"]["agent_status"], "done");
    assert_eq!(status_event["data"]["agent"], "pi");

    let pane = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_5","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            background_pane_id
        ),
    );
    assert_eq!(pane["result"]["pane"]["agent_status"], "done");

    let focused_tab_id = created["result"]["workspace"]["active_tab_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tab_focus = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_6","method":"tab.focus","params":{{"tab_id":"{}"}}}}"#,
            focused_tab_id
        ),
    );
    assert_eq!(tab_focus["result"]["type"], "tab_info");

    let pane_after_focus = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_status_7","method":"pane.get","params":{{"pane_id":"{}"}}}}"#,
            background_pane_id
        ),
    );
    assert_eq!(pane_after_focus["result"]["pane"]["agent_status"], "idle");

    cleanup_spawned_herdr(child, base);
}
