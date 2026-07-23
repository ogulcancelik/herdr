use super::harness::*;

#[test]
fn named_sessions_use_separate_servers_and_workspace_state() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");

    let alpha = spawn_named_server(&config_home, &runtime_dir, "alpha");
    let beta = spawn_named_server(&config_home, &runtime_dir, "beta");

    wait_for_socket(
        &named_session_socket(&config_home, "alpha"),
        Duration::from_secs(5),
    );
    wait_for_socket(
        &named_session_socket(&config_home, "beta"),
        Duration::from_secs(5),
    );

    run_named_cli_json(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "alpha",
            "workspace",
            "create",
            "--label",
            "alpha-ws",
            "--no-focus",
        ],
    );
    run_named_cli_json(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "beta",
            "workspace",
            "create",
            "--label",
            "beta-ws",
            "--no-focus",
        ],
    );

    let alpha_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "alpha", "workspace", "list"],
    );
    let beta_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "workspace", "list"],
    );

    let alpha_labels: Vec<_> = alpha_list["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();
    let beta_labels: Vec<_> = beta_list["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();

    assert_eq!(alpha_labels, vec!["alpha-ws"]);
    assert_eq!(beta_labels, vec!["beta-ws"]);

    let beta_via_explicit_session = run_named_cli_with_socket_override(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "workspace", "list"],
        Some(&named_session_socket(&config_home, "alpha")),
    );
    assert!(
        beta_via_explicit_session.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&beta_via_explicit_session.stderr)
    );
    let beta_via_explicit_session: serde_json::Value =
        serde_json::from_slice(&beta_via_explicit_session.stdout).unwrap();
    let labels_via_explicit: Vec<_> = beta_via_explicit_session["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| workspace["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels_via_explicit, vec!["beta-ws"]);

    let human_sessions = run_named_cli(&config_home, &runtime_dir, &["session", "list"]);
    assert!(human_sessions.status.success());
    let human_sessions = String::from_utf8_lossy(&human_sessions.stdout);
    assert!(human_sessions.contains("name"), "stdout: {human_sessions}");
    assert!(
        human_sessions.contains("status"),
        "stdout: {human_sessions}"
    );
    assert!(human_sessions.contains("alpha"), "stdout: {human_sessions}");
    assert!(
        human_sessions.contains("running"),
        "stdout: {human_sessions}"
    );
    assert!(
        human_sessions.contains("/sessions/beta"),
        "stdout: {human_sessions}"
    );

    let sessions = run_named_cli_json(&config_home, &runtime_dir, &["session", "list", "--json"]);
    let sessions = sessions["sessions"].as_array().unwrap();
    let default_session = sessions
        .iter()
        .find(|session| session["name"] == "default")
        .unwrap();
    let alpha_session = sessions
        .iter()
        .find(|session| session["name"] == "alpha")
        .unwrap();
    let beta_session = sessions
        .iter()
        .find(|session| session["name"] == "beta")
        .unwrap();
    assert_eq!(default_session["default"], true);
    assert_eq!(default_session["running"], false);
    assert_eq!(alpha_session["running"], true);
    assert_eq!(beta_session["running"], true);
    assert!(alpha_session["socket_path"]
        .as_str()
        .unwrap()
        .ends_with("/sessions/alpha/herdr.sock"));
    assert!(beta_session["session_dir"]
        .as_str()
        .unwrap()
        .ends_with("/sessions/beta"));

    let delete_running = run_named_cli(&config_home, &runtime_dir, &["session", "delete", "alpha"]);
    assert_eq!(delete_running.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&delete_running.stderr).contains("stop it before deleting"),
        "stderr: {}",
        String::from_utf8_lossy(&delete_running.stderr)
    );

    let delete_default = run_named_cli(
        &config_home,
        &runtime_dir,
        &["session", "delete", "default"],
    );
    assert_eq!(delete_default.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&delete_default.stderr).contains("default session"),
        "stderr: {}",
        String::from_utf8_lossy(&delete_default.stderr)
    );

    let stopped_alpha = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["session", "stop", "alpha", "--json"],
    );
    assert_eq!(stopped_alpha["stopped"], true);
    assert_eq!(stopped_alpha["session"]["running"], false);

    let deleted_alpha = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["session", "delete", "alpha", "--json"],
    );
    assert_eq!(deleted_alpha["deleted"], true);
    assert!(!config_home
        .join(app_dir_name())
        .join("sessions")
        .join("alpha")
        .exists());

    let _ = run_named_cli(&config_home, &runtime_dir, &["session", "stop", "beta"]);
    drop(alpha);
    drop(beta);
    cleanup_test_base(&base);
}

#[test]
fn integration_commands_run_locally_when_server_is_missing() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    let extensions_dir = home_dir.join(".pi/agent/extensions");
    fs::create_dir_all(&extensions_dir).unwrap();

    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let expected_extension = extensions_dir.join("herdr-agent-state.ts");
    assert!(
        !expected_extension.exists(),
        "test setup should start without extension file"
    );

    let workspace_list = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["workspace", "list"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(workspace_list.status.code(), Some(1));

    let integration_install = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "install", "pi"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_install.status.code(), Some(0));
    assert!(
        expected_extension.exists(),
        "integration install should write local files without a server"
    );

    let integration_status = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "status"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_status.status.code(), Some(0));
    let status_stdout = String::from_utf8_lossy(&integration_status.stdout);
    assert!(status_stdout.contains("pi: current (v6)"));
    assert!(status_stdout.contains("claude: not installed"));

    let integration_uninstall = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "uninstall", "pi"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();
    assert_eq!(integration_uninstall.status.code(), Some(0));
    assert!(
        !expected_extension.exists(),
        "integration uninstall should remove local files without a server"
    );

    cleanup_test_base(&base);
}

#[test]
fn integration_status_outdated_only_prints_action_for_legacy_install() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    let extensions_dir = home_dir.join(".pi/agent/extensions");
    fs::create_dir_all(&extensions_dir).unwrap();
    fs::write(
        extensions_dir.join("herdr-agent-state.ts"),
        "// legacy herdr integration\n",
    )
    .unwrap();

    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "status", "--outdated-only"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("installed herdr integrations need updating"));
    assert!(stderr.contains("herdr integration install pi"));

    cleanup_test_base(&base);
}

#[test]
fn integration_status_rejects_unknown_flags() {
    let base = unique_test_dir();
    let home_dir = base.join("home");
    fs::create_dir_all(&home_dir).unwrap();
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let missing_socket = runtime_dir.join("missing.sock");

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["integration", "status", "--wat"])
        .env("HERDR_SOCKET_PATH", &missing_socket)
        .env("HOME", &home_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));

    cleanup_test_base(&base);
}

#[test]
fn status_commands_report_client_and_server_versions() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let full = run_cli(&socket_path, &["status"]);
    assert!(
        full.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&full.stderr)
    );
    let full_stdout = String::from_utf8_lossy(&full.stdout);
    assert!(full_stdout.contains("client:\n"), "stdout: {full_stdout}");
    assert!(
        full_stdout.contains(&format!("  version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  protocol: 18"),
        "stdout: {full_stdout}"
    );
    assert!(full_stdout.contains("server:\n"), "stdout: {full_stdout}");
    assert!(
        full_stdout.contains("  status: running"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  compatible: yes"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains("  restart_needed: no"),
        "stdout: {full_stdout}"
    );
    assert!(
        full_stdout.contains(&socket_path.display().to_string()),
        "stdout: {full_stdout}"
    );

    let server = run_cli(&socket_path, &["status", "server"]);
    assert!(server.status.success());
    let server_stdout = String::from_utf8_lossy(&server.stdout);
    assert!(
        server_stdout.contains("status: running"),
        "stdout: {server_stdout}"
    );
    assert!(
        server_stdout.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {server_stdout}"
    );
    assert!(
        server_stdout.contains("protocol: 18"),
        "stdout: {server_stdout}"
    );

    let client = run_cli(&socket_path, &["status", "client"]);
    assert!(client.status.success());
    let client_stdout = String::from_utf8_lossy(&client.stdout);
    assert!(
        client_stdout.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
        "stdout: {client_stdout}"
    );
    assert!(
        client_stdout.contains("protocol: 18"),
        "stdout: {client_stdout}"
    );
    assert!(
        client_stdout.contains("binary: "),
        "stdout: {client_stdout}"
    );

    let full_json = run_cli_json(&socket_path, &["status", "--json"]);
    assert_eq!(full_json["client"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(full_json["client"]["protocol"], 18);
    assert_eq!(full_json["server"]["status"], "running");
    assert_eq!(full_json["server"]["running"], true);
    assert_eq!(full_json["server"]["compatible"], true);
    assert_eq!(
        full_json["server"]["socket"],
        socket_path.display().to_string()
    );
    assert_eq!(full_json["server"]["restart_needed"], false);
    assert_eq!(full_json["update"]["restart_needed"], false);

    let server_json = run_cli_json(&socket_path, &["status", "server", "--json"]);
    assert_eq!(server_json["status"], "running");
    assert_eq!(server_json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(server_json["protocol"], 18);
    assert_eq!(server_json["compatible"], true);

    let client_json = run_cli_json(&socket_path, &["status", "client", "--json"]);
    assert_eq!(client_json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(client_json["protocol"], 18);
    assert!(client_json["binary"]
        .as_str()
        .is_some_and(|path| !path.is_empty()));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn status_reports_not_running_when_server_socket_is_missing() {
    let base = unique_test_dir();
    let runtime_dir = base.join("runtime");
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    let socket_path = runtime_dir.join("missing.sock");

    let status = run_cli(&socket_path, &["status"]);
    assert!(status.status.success());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("  status: not running"), "stdout: {stdout}");
    assert!(stdout.contains("  restart_needed: no"), "stdout: {stdout}");
    assert!(
        stdout.contains(&socket_path.display().to_string()),
        "stdout: {stdout}"
    );

    let status_json = run_cli_json(&socket_path, &["status", "--json"]);
    assert_eq!(status_json["server"]["status"], "not_running");
    assert_eq!(status_json["server"]["running"], false);
    assert_eq!(
        status_json["server"]["socket"],
        socket_path.display().to_string()
    );
    assert_eq!(status_json["server"]["restart_needed"], false);
    assert_eq!(status_json["update"]["restart_needed"], false);

    cleanup_test_base(&base);
}

#[test]
fn server_stop_command_shuts_down_running_server() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let stopped = run_cli(&socket_path, &["server", "stop"]);
    assert!(
        stopped.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        stopped.stdout.is_empty(),
        "server stop should not print stdout: {}",
        String::from_utf8_lossy(&stopped.stdout)
    );
    assert!(
        !socket_path.exists() || UnixStream::connect(&socket_path).is_err(),
        "api socket should be removed or stale before server stop returns"
    );
    assert!(
        !client_socket.exists() || UnixStream::connect(&client_socket).is_err(),
        "client socket should be removed or stale before server stop returns"
    );

    let pid = herdr.child.process_id();
    let exit_status = herdr.child.wait().unwrap();
    unregister_spawned_herdr_pid(pid);
    assert!(exit_status.success(), "server stop should exit cleanly");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn server_stop_then_restart_restores_pane_history() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let marker = "PERSISTED_HISTORY_AFTER_STOP";

    let mut herdr = spawn_herdr_with_pane_history(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().expect("test path should be utf-8"),
            "--label",
            "history-restart",
        ],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("workspace create should return root pane id")
        .to_string();
    let sent = run_cli(
        &socket_path,
        &["pane", "send-text", &pane_id, &format!("echo {marker}\n")],
    );
    assert!(
        sent.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&sent.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(3), Duration::from_millis(25), || {
            pane_read_recent_contains(&socket_path, &pane_id, marker)
        }),
        "pane should contain marker before server stop"
    );

    let stopped = run_cli(&socket_path, &["server", "stop"]);
    assert!(
        stopped.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );

    let pid = herdr.child.process_id();
    let exit_status = herdr.child.wait().unwrap();
    unregister_spawned_herdr_pid(pid);
    assert!(exit_status.success(), "server stop should exit cleanly");
    drop(herdr);

    let restarted = spawn_herdr_with_pane_history(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let workspace_id = workspaces["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces")
        .iter()
        .find(|workspace| workspace["label"] == "history-restart")
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .expect("restored workspace should exist")
        .to_string();
    let panes = run_cli_json(
        &socket_path,
        &["pane", "list", "--workspace", &workspace_id],
    );
    let restored_pane_id = panes["result"]["panes"]
        .as_array()
        .expect("pane.list should return panes")
        .first()
        .and_then(|pane| pane["pane_id"].as_str())
        .expect("restored pane should exist")
        .to_string();

    assert!(
        wait_until(Duration::from_secs(3), Duration::from_millis(25), || {
            pane_read_recent_contains(&socket_path, &restored_pane_id, marker)
        }),
        "restarted server should restore saved pane history"
    );

    cleanup_spawned_herdr(restarted, base);
}

#[test]
fn server_start_restores_legacy_session_through_api_identity() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let data_dir = config_home.join(app_dir_name());
    let pion_cwd = base.join("legacy-pion");
    let herdr_cwd = base.join("legacy-herdr");

    fs::create_dir_all(&pion_cwd).unwrap();
    fs::create_dir_all(&herdr_cwd).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    let pion_cwd = pion_cwd.to_str().expect("test cwd should be UTF-8");
    let herdr_cwd = herdr_cwd.to_str().expect("test cwd should be UTF-8");
    let legacy_session = include_str!("../fixtures/session/legacy-pre-tabs-v2.json")
        .replace("/tmp/pion", pion_cwd)
        .replace("/tmp/herdr", herdr_cwd);
    fs::write(data_dir.join("session.json"), legacy_session).unwrap();

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let workspaces = run_cli_json(&socket_path, &["workspace", "list"]);
    let restored_workspace = workspaces["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces")
        .iter()
        .find(|workspace| workspace["label"] == "legacy")
        .expect("legacy workspace should restore");
    let workspace_id = restored_workspace["workspace_id"]
        .as_str()
        .expect("restored workspace should have public id")
        .to_string();
    assert_eq!(restored_workspace["pane_count"], 2);
    assert_eq!(restored_workspace["tab_count"], 1);
    assert_eq!(
        restored_workspace["active_tab_id"],
        format!("{workspace_id}:t1")
    );

    let panes = run_cli_json(
        &socket_path,
        &["pane", "list", "--workspace", &workspace_id],
    );
    let panes = panes["result"]["panes"]
        .as_array()
        .expect("pane.list should return panes");
    assert_eq!(panes.len(), 2);
    let root_pane_id = format!("{workspace_id}:p1");
    let focused_pane_id = format!("{workspace_id}:p2");
    assert!(panes.iter().any(|pane| {
        pane["pane_id"] == root_pane_id
            && pane["tab_id"] == format!("{workspace_id}:t1")
            && pane["cwd"] == pion_cwd
            && pane["focused"] == false
    }));
    assert!(panes.iter().any(|pane| {
        pane["pane_id"] == focused_pane_id
            && pane["tab_id"] == format!("{workspace_id}:t1")
            && pane["cwd"] == herdr_cwd
            && pane["focused"] == true
    }));

    let reported = run_cli(
        &socket_path,
        &[
            "pane",
            "report-agent",
            &focused_pane_id,
            "--source",
            "test",
            "--agent",
            "pi",
            "--state",
            "working",
        ],
    );
    assert!(
        reported.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&reported.stderr)
    );

    let agents = run_cli_json(&socket_path, &["agent", "list"]);
    let agents = agents["result"]["agents"]
        .as_array()
        .expect("agent.list should return agents");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["pane_id"], focused_pane_id);
    assert_eq!(agents[0]["workspace_id"], workspace_id);
    assert_eq!(agents[0]["agent"], "pi");
    assert_eq!(agents[0]["agent_status"], "working");

    cleanup_spawned_herdr(herdr, base);
}
