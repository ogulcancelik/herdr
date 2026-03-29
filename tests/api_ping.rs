use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-api-test-{}-{nanos}", std::process::id()))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
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
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
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

fn open_subscription(socket_path: &Path, json: &str) -> (UnixStream, BufReader<UnixStream>) {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream.write_all(json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();

    let reader = BufReader::new(stream.try_clone().unwrap());
    (stream, reader)
}

fn read_json_line(reader: &mut BufReader<UnixStream>, timeout: Duration) -> serde_json::Value {
    reader.get_ref().set_read_timeout(Some(timeout)).unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn wait_for_event(
    reader: &mut BufReader<UnixStream>,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let value = read_json_line(reader, remaining.max(Duration::from_millis(1)));
        if value["event"] == expected {
            return value;
        }
    }
}

#[test]
fn ping_over_socket_returns_version() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let mut child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let value = send_request(
        &socket_path,
        r#"{"id":"req_1","method":"ping","params":{}}"#,
    );
    assert_eq!(value["id"], "req_1");
    assert_eq!(value["result"]["type"], "pong");
    assert_eq!(value["result"]["version"], env!("CARGO_PKG_VERSION"));

    let _ = child.child.kill();
    let _ = child.child.wait();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn workspace_list_and_create_round_trip() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let mut child = spawn_herdr(&config_home, &runtime_dir, &socket_path);
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
    assert_eq!(created["result"]["type"], "workspace_info");
    assert_eq!(created["result"]["workspace"]["workspace_id"], "1");
    assert_eq!(created["result"]["workspace"]["number"], 1);
    assert_eq!(created["result"]["workspace"]["focused"], true);

    let listed = send_request(
        &socket_path,
        r#"{"id":"req_4","method":"workspace.list","params":{}}"#,
    );
    let workspaces = listed["result"]["workspaces"].as_array().unwrap();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0]["workspace_id"], "1");

    let fetched = send_request(
        &socket_path,
        r#"{"id":"req_5","method":"workspace.get","params":{"workspace_id":"1"}}"#,
    );
    assert_eq!(fetched["result"]["workspace"]["workspace_id"], "1");

    let panes = send_request(
        &socket_path,
        r#"{"id":"req_6","method":"pane.list","params":{}}"#,
    );
    let panes = panes["result"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["workspace_id"], "1");
    let pane_id = panes[0]["pane_id"].as_str().unwrap().to_string();

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

    let _ = child.child.kill();
    let _ = child.child.wait();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn events_subscribe_streams_lifecycle_and_agent_events() {
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
    let mut child = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let (_stream, mut reader) = open_subscription(
        &socket_path,
        r#"{"id":"sub_life","method":"events.subscribe","params":{"subscriptions":[{"type":"workspace.created"},{"type":"workspace.focused"},{"type":"pane.created"},{"type":"pane.focused"},{"type":"pane.agent_detected"},{"type":"pane.closed"},{"type":"workspace.closed"}]}}"#,
    );

    let ack = read_json_line(&mut reader, Duration::from_secs(2));
    assert_eq!(ack["id"], "sub_life");
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

    let split = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l4","method":"pane.split","params":{{"target_pane_id":"{}","direction":"right","focus":true}}}}"#,
            pane_id
        ),
    );
    let split_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split_created = wait_for_event(&mut reader, "pane_created", Duration::from_secs(2));
    assert_eq!(split_created["data"]["pane"]["pane_id"], split_pane_id);

    let closed = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l5","method":"pane.close","params":{{"pane_id":"{}"}}}}"#,
            split_pane_id
        ),
    );
    assert_eq!(closed["result"]["type"], "ok");
    let pane_closed = wait_for_event(&mut reader, "pane_closed", Duration::from_secs(2));
    assert_eq!(pane_closed["data"]["pane_id"], split_pane_id);

    let closed_ws = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_l6","method":"workspace.close","params":{{"workspace_id":"{}"}}}}"#,
            workspace_id
        ),
    );
    assert_eq!(closed_ws["result"]["type"], "ok");
    let workspace_closed = wait_for_event(&mut reader, "workspace_closed", Duration::from_secs(2));
    assert_eq!(workspace_closed["data"]["workspace_id"], workspace_id);

    let _ = child.child.kill();
    let _ = child.child.wait();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn events_subscribe_streams_output_and_agent_state_events() {
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
    let mut child = spawn_herdr_with_path(
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
    assert_eq!(created["result"]["workspace"]["workspace_id"], "1");

    let panes = send_request(
        &socket_path,
        r#"{"id":"req_21","method":"pane.list","params":{}}"#,
    );
    let pane_id = panes["result"]["panes"][0]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_stream, mut reader) = open_subscription(
        &socket_path,
        &format!(
            r#"{{"id":"sub_1","method":"events.subscribe","params":{{"subscriptions":[{{"type":"pane.output_matched","pane_id":"{}","source":"recent","lines":40,"match":{{"type":"substring","value":"hello from socket"}}}},{{"type":"pane.agent_state_changed","pane_id":"{}","state":"idle"}}]}}}}"#,
            pane_id, pane_id,
        ),
    );

    let ack = read_json_line(&mut reader, Duration::from_secs(2));
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

    let output_event = read_json_line(&mut reader, Duration::from_secs(3));
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

    let agent_idle = read_json_line(&mut reader, Duration::from_secs(8));
    assert_eq!(agent_idle["event"], "pane.agent_state_changed");
    assert_eq!(agent_idle["data"]["pane_id"], pane_id);
    assert_eq!(agent_idle["data"]["state"], "idle");
    assert_eq!(agent_idle["data"]["agent"], "pi");

    let _ = child.child.kill();
    let _ = child.child.wait();
    let _ = fs::remove_dir_all(base);
}
