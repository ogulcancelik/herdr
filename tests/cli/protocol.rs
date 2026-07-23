use super::harness::*;

#[test]
fn cli_allows_same_protocol_different_version_and_preserves_server_error() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut ping_stream, _) = listener.accept().unwrap();
        let mut ping_line = String::new();
        BufReader::new(ping_stream.try_clone().unwrap())
            .read_line(&mut ping_line)
            .unwrap();
        let ping: serde_json::Value = serde_json::from_str(&ping_line).unwrap();
        assert_eq!(ping["method"], "ping");
        write_fake_pong(
            &mut ping_stream,
            &ping,
            "0.7.1-compatible-build",
            CURRENT_PROTOCOL,
        );

        let (mut operation_stream, _) = listener.accept().unwrap();
        let mut operation_line = String::new();
        BufReader::new(operation_stream.try_clone().unwrap())
            .read_line(&mut operation_line)
            .unwrap();
        let operation: serde_json::Value = serde_json::from_str(&operation_line).unwrap();
        assert_eq!(operation["method"], "agent.wait");
        operation_stream
            .write_all(
                br#"{"id":"cli:agent:wait","error":{"code":"not_implemented","message":"compatible server error"}}"#,
            )
            .unwrap();
        operation_stream.write_all(b"\n").unwrap();
        operation_stream.flush().unwrap();
    });

    let waited = run_cli(
        &socket_path,
        &[
            "agent",
            "wait",
            "1-1",
            "--until",
            "idle",
            "--timeout",
            "5000",
        ],
    );

    assert_eq!(waited.status.code(), Some(1));
    assert!(waited.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&waited.stderr).unwrap();
    assert_eq!(error["id"], "cli:agent:wait");
    assert_eq!(error["error"]["code"], "not_implemented");
    assert_eq!(error["error"]["message"], "compatible server error");
    server.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn server_live_handoff_bypasses_protocol_guard() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "server.live_handoff");
        stream
            .write_all(br#"{"id":"cli:server:live-handoff","result":{"type":"ok"}}"#)
            .unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    });

    let handoff = run_cli(&socket_path, &["server", "live-handoff"]);
    assert!(
        handoff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&handoff.stderr)
    );
    server.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn plugin_list_preserves_protocol_mismatch_envelope() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "ping");
        write_fake_pong(&mut stream, &request, "0.7.1", 14);
    });

    let listed = run_cli(&socket_path, &["plugin", "list", "--json"]);
    assert_eq!(listed.status.code(), Some(1));
    assert!(listed.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&listed.stderr).unwrap();
    assert_eq!(error["id"], "cli:plugin");
    assert_eq!(error["error"]["code"], "protocol_mismatch");
    server.join().unwrap();
    cleanup_test_base(&base);
}
