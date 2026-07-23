use super::harness::*;

#[test]
fn cli_rejects_protocol_mismatch_before_agent_wait_request() {
    let base = unique_test_dir();
    fs::create_dir_all(&base).unwrap();
    let socket_path = base.join("herdr.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn(move || {
        let (mut first_stream, _) = listener.accept().unwrap();
        let mut first_line = String::new();
        let mut first_reader = BufReader::new(first_stream.try_clone().unwrap());
        first_reader.read_line(&mut first_line).unwrap();
        let first_request: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        if first_request["method"] == "ping" {
            write_fake_pong(&mut first_stream, &first_request, "0.7.1", 14);
        } else {
            first_stream
                .write_all(
                    br#"{"id":"cli:agent:wait","error":{"code":"not_implemented","message":"method not implemented yet"}}"#,
                )
                .unwrap();
        }
        if first_request["method"] != "ping" {
            first_stream.write_all(b"\n").unwrap();
            first_stream.flush().unwrap();
        }

        let mut second_line = None;
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((second_stream, _)) => {
                    let mut line = String::new();
                    let mut reader = BufReader::new(second_stream);
                    reader.read_line(&mut line).unwrap();
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
    assert_eq!(error["error"]["code"], "protocol_mismatch");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("client protocol {CURRENT_PROTOCOL}")),
        "message: {message}"
    );
    assert!(message.contains("server protocol 14"), "message: {message}");
    assert!(message.contains("restart"), "message: {message}");

    let (first_line, second_line) = server.join().unwrap();
    let first_request: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(first_request["method"], "ping");
    assert!(
        second_line.is_none(),
        "mismatched CLI sent an operational request: {second_line:?}"
    );

    cleanup_test_base(&base);
}
