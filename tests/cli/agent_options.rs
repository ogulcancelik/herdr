use super::harness::*;

#[test]
fn agent_wait_accepts_repeated_until_and_exits_when_one_status_matches() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin_dir = base.join("bin");

    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(
        &fake_pi,
        "#!/bin/sh\nprintf 'starting\\n'\nsleep 4\nprintf 'Working...\\n'\nsleep 1\nprintf '\\033[2J\\033[Hdone\\n'\n",
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
    let herdr = spawn_herdr_with_path(
        &config_home,
        &runtime_dir,
        &socket_path,
        Some(Path::new(&path_override)),
    );

    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = send_request(
        &socket_path,
        &format!(
            r#"{{"id":"req_cli_2","method":"workspace.create","params":{{"cwd":"{}","focus":true}}}}"#,
            base.display()
        ),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let start_pi = run_cli(&socket_path, &["pane", "run", &pane_id, "pi"]);
    assert!(start_pi.status.success());
    assert!(wait_until(
        Duration::from_secs(3),
        Duration::from_millis(25),
        || run_cli(&socket_path, &["agent", "get", &pane_id])
            .status
            .success()
    ));

    let waited = run_cli(
        &socket_path,
        &[
            "agent",
            "wait",
            &pane_id,
            "--until",
            "blocked",
            "--until",
            "idle",
            "--timeout",
            "10000",
        ],
    );
    assert!(
        waited.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&waited.stderr)
    );
    let waited_json: serde_json::Value = serde_json::from_slice(&waited.stdout).unwrap();
    assert_eq!(waited_json["result"]["agent"]["agent_status"], "idle");
    assert_eq!(waited_json["result"]["agent"]["agent"], "pi");

    cleanup_spawned_herdr(herdr, base);
}
