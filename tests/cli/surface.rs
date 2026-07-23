use super::harness::*;

#[test]
fn pane_run_sends_one_send_input_request_with_enter_key() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, first_line) = accept_fake_cli_operation(&listener);
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
    assert_eq!(first_request["method"], "pane.send_input");
    assert_eq!(first_request["params"]["pane_id"], "1-1");
    assert_eq!(first_request["params"]["text"], "echo hello");
    assert_eq!(
        first_request["params"]["keys"],
        serde_json::json!(["Enter"])
    );
    assert!(
        second_line.is_none(),
        "pane run sent an unexpected second request: {:?}",
        second_line
    );

    cleanup_test_base(&base);
}

#[test]
fn workspace_report_metadata_sends_token_patch() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, line) = accept_fake_cli_operation(&listener);
        stream
            .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        line
    });

    let run = run_cli(
        &socket_path,
        &[
            "workspace",
            "report-metadata",
            "2",
            "--source",
            "user:jj",
            "--token",
            "jj_status=2 changes",
            "--token",
            "summary=clean",
            "--clear-token",
            "old",
            "--ttl-ms",
            "5000",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let request: serde_json::Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert_eq!(request["method"], "workspace.report_metadata");
    assert_eq!(request["params"]["workspace_id"], "2");
    assert_eq!(request["params"]["tokens"]["jj_status"], "2 changes");
    assert_eq!(request["params"]["tokens"]["summary"], "clean");
    assert!(request["params"]["tokens"]["old"].is_null());
    assert_eq!(request["params"]["ttl_ms"], 5000);

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_sends_presentation_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, line) = accept_fake_cli_operation(&listener);
        stream
            .write_all(br#"{"id":"cli:request","result":{"type":"ok"}}"#)
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        line
    });

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "user:claude-title",
            "--agent",
            "claude",
            "--title",
            "Refactor auth",
            "--display-agent",
            "Claude auth",
            "--state-label",
            "working=deep in the mines",
            "--token",
            "summary=reviewing auth",
            "--token",
            "model=opus",
            "--clear-token",
            "old",
            "--ttl-ms",
            "3600000",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let line = server.join().unwrap();
    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], "pane.report_metadata");
    assert_eq!(request["params"]["pane_id"], "1-1");
    assert_eq!(request["params"]["source"], "user:claude-title");
    assert_eq!(request["params"]["agent"], "claude");
    assert!(request["params"]["applies_to_source"].is_null());
    assert_eq!(request["params"]["title"], "Refactor auth");
    assert_eq!(request["params"]["display_agent"], "Claude auth");
    assert_eq!(
        request["params"]["state_labels"]["working"],
        "deep in the mines"
    );
    assert_eq!(request["params"]["tokens"]["summary"], "reviewing auth");
    assert_eq!(request["params"]["tokens"]["model"], "opus");
    assert!(request["params"]["tokens"]["old"].is_null());
    assert_eq!(request["params"]["ttl_ms"], 3_600_000);

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_rejects_blank_source_before_socket_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("missing.sock");

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "   ",
            "--token",
            "summary=middleware",
        ],
    );

    assert_eq!(run.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("missing required --source"),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    cleanup_test_base(&base);
}

#[test]
fn pane_report_metadata_rejects_blank_applies_to_source_before_socket_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("missing.sock");

    let run = run_cli(
        &socket_path,
        &[
            "pane",
            "report-metadata",
            "1-1",
            "--source",
            "user:claude-title",
            "--applies-to-source",
            "   ",
            "--token",
            "summary=middleware",
        ],
    );

    assert_eq!(run.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("missing value for --applies-to-source"),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    cleanup_test_base(&base);
}

#[test]
fn help_commands_exit_successfully() {
    let help_cases: &[&[&str]] = &[
        &["--help"],
        &["agent", "wait", "--help"],
        &["terminal", "session", "control", "-h"],
    ];

    for args in help_cases {
        let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(*args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "herdr {} failed: status={:?} stdout={} stderr={}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn subcommand_help_explains_automation_semantics_without_a_server() {
    let cases: &[(&[&str], &str)] = &[
        (&["agent", "wait", "--help"], "Without --until"),
        (
            &["agent", "prompt", "--help"],
            "first matching state observed after submission",
        ),
        (&["agent", "start", "--help"], "ready for input"),
        (
            &["agent", "send-keys", "--help"],
            "canonical Escape key name",
        ),
        (
            &["pane", "wait-output", "--help"],
            "including existing output",
        ),
        (
            &["pane", "send-keys", "--help"],
            "canonical Escape key name",
        ),
    ];

    for (args, expected) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(*args)
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_ENV")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "herdr {} failed: status={:?} stdout={} stderr={}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(expected),
            "herdr {} help did not contain {expected:?}: {stdout}",
            args.join(" ")
        );
    }
}

#[test]
fn removed_wait_and_agent_send_commands_are_rejected() {
    let wait = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["wait", "output", "w1:p1", "--match", "ready"])
        .output()
        .unwrap();
    assert_eq!(wait.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&wait.stderr).contains("unknown command: wait"));
    let help = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("herdr wait <subcommand>"));

    let send = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["agent", "send", "reviewer", "hello"])
        .output()
        .unwrap();
    assert_eq!(send.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&send.stderr);
    assert!(stderr.contains("herdr agent send-keys"));
    assert!(!stderr.contains("herdr agent send <"));
}

#[test]
fn agent_cli_rejects_invalid_wait_and_rename_grammar_locally() {
    for args in [
        &["agent", "wait", "reviewer", "--until", "finished"][..],
        &["agent", "wait", "reviewer", "--timeout", "later"][..],
        &[
            "agent", "prompt", "reviewer", "work", "--wait", "--until", "finished",
        ][..],
        &[
            "agent",
            "prompt",
            "reviewer",
            "work",
            "--wait",
            "--timeout",
            "later",
        ][..],
        &[
            "agent",
            "start",
            "reviewer",
            "--kind",
            "pi",
            "--pane",
            "w1:p1",
            "--timeout",
            "later",
        ][..],
        &["agent", "rename", "reviewer"][..],
        &["agent", "rename", "reviewer", "worker", "--clear"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(args)
            .env("HERDR_SOCKET_PATH", "/nonexistent/herdr.sock")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "herdr {}: stdout={} stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn completion_command_prints_zsh_script_without_session_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["completion", "zsh"])
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("#compdef herdr"), "stdout: {stdout}");
    assert!(
        !stdout.contains("--cwd=[]"),
        "zsh completions should not suggest equals-style values unsupported by most manual parsers: {stdout}"
    );
    assert!(
        !stdout.contains("--direction=[]"),
        "zsh completions should not suggest equals-style direction values: {stdout}"
    );
    assert!(
        !stdout.contains("live-handoff"),
        "internal server handoff command should not be completed: {stdout}"
    );
}

#[test]
fn root_help_hides_explicit_client_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("herdr client"),
        "root help should not advertise the internal client command: {stdout}"
    );
}

#[test]
fn root_help_advertises_api_schema_command_group() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("herdr api <subcommand>"),
        "root help should advertise the api command group: {stdout}"
    );
}

#[test]
fn api_schema_default_output_is_a_short_summary() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["api", "schema"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Herdr API schema"), "stdout: {stdout}");
    assert!(
        stdout.contains("Use `herdr api schema --json`"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.len() < 400,
        "summary should stay small enough for terminal output: {stdout}"
    );
}

#[test]
fn api_schema_json_prints_bundled_schema() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["api", "schema", "--json"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(schema
        .get("protocol")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|protocol| protocol > 0));
    assert_eq!(
        schema
            .get("schemas")
            .and_then(serde_json::Value::as_object)
            .map(serde_json::Map::len),
        Some(5)
    );
}

#[test]
fn api_snapshot_prints_live_session_snapshot() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let (mut stream, line) = accept_fake_cli_operation(&listener);
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "session.snapshot");
            assert_eq!(request["id"], "cli:api:snapshot");

            let response = serde_json::json!({
                "id": "cli:api:snapshot",
                "result": {
                    "type": "ok",
                    "marker": "snapshot-passthrough"
                }
            });
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
            let _ = fs::remove_file(socket_path);
        }
    });

    let value = run_cli_json(&socket_path, &["api", "snapshot"]);

    assert_eq!(value["result"]["marker"], "snapshot-passthrough");
    server.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn api_schema_output_writes_bundled_schema_to_file() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let schema_path = base.join("herdr-api.schema.json");

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["api", "schema", "--output"])
        .arg(&schema_path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("wrote API schema"),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let schema: serde_json::Value =
        serde_json::from_slice(&fs::read(&schema_path).unwrap()).unwrap();
    assert!(schema
        .get("protocol")
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|protocol| protocol > 0));

    cleanup_test_base(&base);
}

#[test]
fn explicit_client_command_respects_nested_guard() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("client")
        .env("HERDR_ENV", "1")
        .env("XDG_CONFIG_HOME", &base)
        .env_remove("HERDR_CONFIG_PATH")
        .output()
        .unwrap();

    cleanup_test_base(&base);

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nested herdr is disabled by default"),
        "client should fail at the nested guard before connecting: {stderr}"
    );
}

#[test]
fn removed_show_changelog_flag_fails_before_nested_guard() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--show-changelog")
        .env("HERDR_ENV", "1")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown option: --show-changelog"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("nested herdr"),
        "unknown flag should be rejected before nested guard: {stderr}"
    );
}
