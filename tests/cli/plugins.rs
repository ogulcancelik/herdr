use super::harness::*;

#[test]
fn named_sessions_share_live_plugin_registry() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let first_dir = base.join("plugins").join("first");
    let second_dir = base.join("plugins").join("second");
    for (dir, id) in [
        (&first_dir, "example.first"),
        (&second_dir, "example.second"),
    ] {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("herdr-plugin.toml"),
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nmin_herdr_version = \"0.6.10\"\n\n[[actions]]\nid = \"run\"\ntitle = \"Run\"\ncommand = [\"sh\", \"-c\", \"echo run\"]\n"
            ),
        )
        .unwrap();
    }

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

    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            run_named_cli_json(
                &config_home,
                &runtime_dir,
                &[
                    "--session",
                    "alpha",
                    "plugin",
                    "link",
                    first_dir.to_str().unwrap(),
                ],
            )
        });
        let second = scope.spawn(|| {
            run_named_cli_json(
                &config_home,
                &runtime_dir,
                &[
                    "--session",
                    "beta",
                    "plugin",
                    "link",
                    second_dir.to_str().unwrap(),
                ],
            )
        });
        assert_eq!(
            first.join().unwrap()["result"]["plugin"]["plugin_id"],
            "example.first"
        );
        assert_eq!(
            second.join().unwrap()["result"]["plugin"]["plugin_id"],
            "example.second"
        );
    });

    let beta_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "plugin", "list", "--json"],
    );
    assert_eq!(beta_list["result"]["plugins"].as_array().unwrap().len(), 2);

    run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "plugin", "disable", "example.first"],
    );
    let disabled = run_named_cli(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "alpha",
            "plugin",
            "action",
            "invoke",
            "run",
            "--plugin",
            "example.first",
        ],
    );
    assert_eq!(disabled.status.code(), Some(1));
    let disabled_output = format!(
        "{}{}",
        String::from_utf8_lossy(&disabled.stdout),
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert!(disabled_output.contains("disabled"), "{disabled_output}");

    let _ = run_named_cli(&config_home, &runtime_dir, &["session", "stop", "alpha"]);
    let _ = run_named_cli(&config_home, &runtime_dir, &["session", "stop", "beta"]);
    drop(alpha);
    drop(beta);
    cleanup_test_base(&base);
}

#[test]
fn plugin_link_works_offline_and_is_global() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let state_home = base.join("state");
    let plugin_dir = base.join("plugins").join("offline");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.offline"
name = "Offline Plugin"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#,
    )
    .unwrap();

    let link_args = [
        "--session",
        "offline",
        "plugin",
        "link",
        plugin_dir.to_str().unwrap(),
        "--disabled",
    ];
    let linked = parse_cli_json_output(
        &link_args,
        run_named_cli_with_env(
            &config_home,
            &runtime_dir,
            &link_args,
            &[("XDG_STATE_HOME", &state_home)],
        ),
    );
    assert_eq!(linked["result"]["type"], "plugin_linked");
    assert_eq!(linked["result"]["plugin"]["plugin_id"], "example.offline");
    assert_eq!(linked["result"]["plugin"]["enabled"], false);
    assert_eq!(linked["result"]["plugin"]["source"]["kind"], "local");
    assert!(!named_session_socket(&config_home, "offline").exists());

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "other", "plugin", "list", "--json"],
    );
    assert_eq!(listed["result"]["plugins"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["result"]["plugins"][0]["plugin_id"],
        "example.offline"
    );
    assert_eq!(listed["result"]["plugins"][0]["enabled"], false);

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_through_named_server_is_global() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("global-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.global-plugin"
name = "Global Plugin"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#,
    )
    .unwrap();
    run_git(&source_repo, &["add", "global-plugin/herdr-plugin.toml"]);
    run_git(&source_repo, &["commit", "--quiet", "-m", "add plugin"]);

    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/example/plugins.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let alpha = spawn_named_server(&config_home, &runtime_dir, "alpha");
    wait_for_socket(
        &named_session_socket(&config_home, "alpha"),
        Duration::from_secs(5),
    );
    let install = run_named_cli_with_env(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "alpha",
            "plugin",
            "install",
            "example/plugins/global-plugin",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
    );
    assert!(
        install.status.success(),
        "install failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let beta_list = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "beta", "plugin", "list", "--json"],
    );
    assert_eq!(
        beta_list["result"]["plugins"][0]["plugin_id"],
        "example.global-plugin"
    );
    let managed_path = PathBuf::from(
        beta_list["result"]["plugins"][0]["source"]["managed_path"]
            .as_str()
            .unwrap(),
    );
    assert!(managed_path.starts_with(managed_github_plugin_dir(&config_home)));

    let _ = run_named_cli(&config_home, &runtime_dir, &["session", "stop", "alpha"]);
    drop(alpha);
    cleanup_test_base(&base);
}

#[test]
fn plugin_link_list_unlink_cli_smoke_test() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let plugin_dir = base.join("plugins").join("layout");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.layout"
name = "Layout"
version = "0.1.0"
min_herdr_version = "0.6.10"
description = "Apply a preferred Herdr layout"

[[actions]]
id = "apply"
title = "Apply layout"
contexts = ["workspace"]
command = ["sh", "-c", "echo layout"]

[[events]]
on = "worktree.created"
command = ["sh", "-c", "echo worktree"]

[[panes]]
id = "board"
title = "Board"
placement = "tab"
command = ["sh", "-c", "sleep 5"]
"#,
    )
    .unwrap();

    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let workspace = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().unwrap(),
            "--focus",
        ],
    );
    assert_eq!(workspace["result"]["type"], "workspace_created");

    let linked = run_cli_json_in_dir(&socket_path, &["plugin", "link", "plugins/layout"], &base);
    assert_eq!(linked["result"]["type"], "plugin_linked");
    assert_eq!(linked["result"]["plugin"]["plugin_id"], "example.layout");
    assert_eq!(linked["result"]["plugin"]["actions"][0]["id"], "apply");
    assert_eq!(
        linked["result"]["plugin"]["events"][0]["on"],
        "worktree.created"
    );
    assert_eq!(linked["result"]["plugin"]["panes"][0]["id"], "board");

    let listed_human = run_cli(&socket_path, &["plugin", "list"]);
    assert!(listed_human.status.success());
    assert!(String::from_utf8_lossy(&listed_human.stdout).contains("example.layout"));

    let listed = run_cli_json(&socket_path, &["plugin", "list", "--json"]);
    assert_eq!(listed["result"]["type"], "plugin_list");
    assert_eq!(
        listed["result"]["plugins"][0]["plugin_id"],
        "example.layout"
    );

    let invoked = run_cli_json(
        &socket_path,
        &[
            "plugin",
            "action",
            "invoke",
            "apply",
            "--plugin",
            "example.layout",
        ],
    );
    assert_eq!(invoked["result"]["type"], "plugin_action_invoked");
    assert_eq!(invoked["result"]["action"]["action_id"], "apply");

    let logs = run_cli_json(
        &socket_path,
        &[
            "plugin",
            "log",
            "list",
            "--plugin",
            "example.layout",
            "--limit",
            "5",
        ],
    );
    assert_eq!(logs["result"]["type"], "plugin_log_list");
    assert!(!logs["result"]["logs"].as_array().unwrap().is_empty());

    let pane = run_cli_json(
        &socket_path,
        &[
            "plugin",
            "pane",
            "open",
            "--plugin",
            "example.layout",
            "--entrypoint",
            "board",
            "--env",
            "HERDR_ROLE=board",
            "--no-focus",
        ],
    );
    assert_eq!(pane["result"]["type"], "plugin_pane_opened");
    assert_eq!(pane["result"]["plugin_pane"]["entrypoint"], "board");

    let missing_plugin_value = run_cli(&socket_path, &["plugin", "list", "--plugin"]);
    assert_eq!(missing_plugin_value.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_plugin_value.stderr)
        .contains("missing value for --plugin"));

    let invalid_limit = run_cli(
        &socket_path,
        &["plugin", "log", "list", "--limit", "not-a-number"],
    );
    assert_eq!(invalid_limit.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_limit.stderr).contains("invalid --limit value"));

    let unlinked = run_cli_json(&socket_path, &["plugin", "unlink", "example.layout"]);
    assert_eq!(unlinked["result"]["type"], "plugin_unlinked");
    assert_eq!(unlinked["result"]["removed"], true);

    let listed = run_cli_json(&socket_path, &["plugin", "list", "--json"]);
    assert!(listed["result"]["plugins"].as_array().unwrap().is_empty());

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn plugin_install_list_uninstall_offline_cli_smoke_test() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("worktree-bootstrap");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.worktree-bootstrap"
name = "Worktree Bootstrap"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["sh", "-c", "echo built > built.txt; if [ -n \"$HERDR_SESSION\" ]; then echo \"$HERDR_SESSION\" > leaked-session.txt; fi"]

[[actions]]
id = "bootstrap"
title = "Bootstrap"
command = ["sh", "-c", "echo bootstrap"]
"#,
    )
    .unwrap();
    run_git(
        &source_repo,
        &["add", "worktree-bootstrap/herdr-plugin.toml"],
    );
    run_git(&source_repo, &["commit", "--quiet", "-m", "add plugin"]);

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let install = run_named_cli_with_env(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "plugins",
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/worktree-bootstrap",
            "--yes",
        ],
        &[
            ("GIT_CONFIG_GLOBAL", &git_config),
            ("HERDR_SESSION", Path::new("leaked-session")),
        ],
    );
    assert!(
        install.status.success(),
        "install failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "other", "plugin", "list", "--json"],
    );
    let plugin = &listed["result"]["plugins"][0];
    assert_eq!(plugin["plugin_id"], "example.worktree-bootstrap");
    assert_eq!(plugin["source"]["kind"], "github");
    assert_eq!(plugin["source"]["owner"], "ogulcancelik");
    assert_eq!(plugin["source"]["repo"], "herdr-plugin-examples");
    assert_eq!(plugin["source"]["subdir"], "worktree-bootstrap");
    assert!(plugin["source"]["resolved_commit"].as_str().is_some());
    let managed_path = PathBuf::from(plugin["source"]["managed_path"].as_str().unwrap());
    assert!(managed_path.exists(), "managed checkout should exist");
    assert!(managed_path.starts_with(managed_github_plugin_dir(&config_home)));
    assert!(
        managed_path
            .join("worktree-bootstrap")
            .join("built.txt")
            .exists(),
        "build artifact should be preserved in managed checkout"
    );
    assert!(
        !managed_path
            .join("worktree-bootstrap")
            .join("leaked-session.txt")
            .exists(),
        "build command should not inherit HERDR_SESSION"
    );

    let uninstall = run_named_cli(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "third",
            "plugin",
            "uninstall",
            "example.worktree-bootstrap",
        ],
    );
    assert!(
        uninstall.status.success(),
        "uninstall failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&uninstall.stdout),
        String::from_utf8_lossy(&uninstall.stderr)
    );
    assert!(
        !managed_path.exists(),
        "managed checkout should be deleted on uninstall"
    );

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "other", "plugin", "list", "--json"],
    );
    assert!(listed["result"]["plugins"].as_array().unwrap().is_empty());

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_build_failure_does_not_register_or_create_checkout() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("build-fail");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.build-fail"
name = "Build Fail"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["sh", "-c", "echo before-fail && echo failed-build >&2 && exit 7"]

[[actions]]
id = "run"
title = "Run"
command = ["sh", "-c", "echo should-not-install"]
"#,
    )
    .unwrap();
    run_git(&source_repo, &["add", "build-fail/herdr-plugin.toml"]);
    run_git(
        &source_repo,
        &["commit", "--quiet", "-m", "add failing plugin"],
    );

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let install = run_named_cli_with_env(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "plugins",
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/build-fail",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
    );
    assert!(
        !install.status.success(),
        "install should fail when build command fails"
    );
    assert_eq!(install.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(stderr.contains("error: plugin build failed"), "{stderr}");
    assert!(stderr.contains("  plugin: example.build-fail"), "{stderr}");
    assert!(stderr.contains("  build: 1/1"), "{stderr}");
    assert!(stderr.contains("  cwd: "), "{stderr}");
    assert!(
        stderr.contains("  command: sh -c echo before-fail && echo failed-build >&2 && exit 7"),
        "{stderr}"
    );
    assert!(stderr.contains("  status: exit status: 7"), "{stderr}");
    assert!(stderr.contains("stdout:\nbefore-fail"), "{stderr}");
    assert!(stderr.contains("stderr:\nfailed-build"), "{stderr}");
    assert!(stderr.contains("Plugin was not installed."), "{stderr}");
    assert!(!stderr.contains("Error: Custom"), "{stderr}");

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "plugins", "plugin", "list", "--json"],
    );
    assert!(listed["result"]["plugins"].as_array().unwrap().is_empty());

    assert!(
        path_missing_or_empty(&managed_github_plugin_dir(&config_home)),
        "failed build should not leave managed checkouts"
    );

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_build_spawn_failure_prints_clean_error() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("missing-tool");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.missing-tool"
name = "Missing Tool"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["definitely-missing-herdr-build-tool-xyz"]

[[actions]]
id = "run"
title = "Run"
command = ["sh", "-c", "echo should-not-install"]
"#,
    )
    .unwrap();
    run_git(&source_repo, &["add", "missing-tool/herdr-plugin.toml"]);
    run_git(
        &source_repo,
        &["commit", "--quiet", "-m", "add missing tool plugin"],
    );

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let install = run_named_cli_with_env(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "plugins",
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/missing-tool",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
    );
    assert!(
        !install.status.success(),
        "install should fail when build command cannot start"
    );
    assert_eq!(install.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(stderr.contains("error: plugin build failed"), "{stderr}");
    assert!(
        stderr.contains("  plugin: example.missing-tool"),
        "{stderr}"
    );
    assert!(stderr.contains("  build: 1/1"), "{stderr}");
    assert!(
        stderr.contains("  command: definitely-missing-herdr-build-tool-xyz"),
        "{stderr}"
    );
    assert!(stderr.contains("  error: failed to start:"), "{stderr}");
    assert!(stderr.contains("Plugin was not installed."), "{stderr}");
    assert!(!stderr.contains("Error: Custom"), "{stderr}");

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "plugins", "plugin", "list", "--json"],
    );
    assert!(listed["result"]["plugins"].as_array().unwrap().is_empty());

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_rejects_manifest_changed_by_build() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("manifest-mutator");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.manifest-mutator"
name = "Manifest Mutator"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["sh", "mutate.sh"]

[[actions]]
id = "run"
title = "Run reviewed command"
command = ["sh", "-c", "echo reviewed"]
"#,
    )
    .unwrap();
    fs::write(
        plugin_dir.join("mutate.sh"),
        r#"cat > herdr-plugin.toml <<'EOF'
id = "example.manifest-mutator"
name = "Manifest Mutator"
version = "0.1.0"
min_herdr_version = "0.0.1"
platforms = ["linux", "macos", "windows"]

[[build]]
command = ["sh", "mutate.sh"]

[[actions]]
id = "run"
title = "Run reviewed command"
command = ["sh", "-c", "echo reviewed"]
EOF
"#,
    )
    .unwrap();
    run_git(&source_repo, &["add", "manifest-mutator"]);
    run_git(
        &source_repo,
        &["commit", "--quiet", "-m", "add mutating plugin"],
    );

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let install = run_named_cli_with_env(
        &config_home,
        &runtime_dir,
        &[
            "--session",
            "plugins",
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/manifest-mutator",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
    );
    assert!(
        !install.status.success(),
        "install should fail when build changes reviewed manifest"
    );
    let stderr = String::from_utf8_lossy(&install.stderr);
    assert!(
        stderr.contains("plugin build changed herdr-plugin.toml after install preview"),
        "{stderr}"
    );

    let listed = run_named_cli_json(
        &config_home,
        &runtime_dir,
        &["--session", "plugins", "plugin", "list", "--json"],
    );
    assert!(listed["result"]["plugins"].as_array().unwrap().is_empty());

    assert!(
        path_missing_or_empty(&managed_github_plugin_dir(&config_home)),
        "manifest mutation should not leave managed checkouts"
    );

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_restores_previous_checkout_when_registration_fails() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("fake-herdr.sock");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("worktree-bootstrap");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.worktree-bootstrap"
name = "Worktree Bootstrap"
version = "0.2.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "bootstrap"
title = "Bootstrap"
command = ["sh", "-c", "echo new"]
"#,
    )
    .unwrap();
    run_git(
        &source_repo,
        &["add", "worktree-bootstrap/herdr-plugin.toml"],
    );
    run_git(&source_repo, &["commit", "--quiet", "-m", "add plugin"]);

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let managed_checkout = config_home
        .join("herdr-dev")
        .join("plugins")
        .join("github")
        .join(WORKTREE_BOOTSTRAP_MANAGED_COMPONENT);
    fs::create_dir_all(&managed_checkout).unwrap();
    fs::write(managed_checkout.join("old-marker"), "old checkout\n").unwrap();

    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let listener = UnixListener::bind(&socket_path).unwrap();
    let managed_checkout_for_server = managed_checkout.clone();
    let server = thread::spawn(move || {
        let (mut first, first_line) = accept_fake_cli_operation(&listener);
        let first_request: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(first_request["method"], "plugin.list");
        writeln!(
            first,
            "{}",
            serde_json::json!({
                "id": "cli:plugin",
                "result": {
                    "type": "plugin_list",
                    "plugins": [{
                        "plugin_id": "example.worktree-bootstrap",
                        "name": "Worktree Bootstrap",
                        "version": "0.1.0",
                        "min_herdr_version": "0.6.10",
                        "manifest_path": managed_checkout_for_server.join("herdr-plugin.toml").display().to_string(),
                        "plugin_root": managed_checkout_for_server.display().to_string(),
                        "enabled": true,
                        "source": {
                            "kind": "github",
                            "owner": "ogulcancelik",
                            "repo": "herdr-plugin-examples",
                            "subdir": "worktree-bootstrap",
                            "resolved_commit": "old",
                            "managed_path": managed_checkout_for_server.display().to_string(),
                            "installed_unix_ms": 1
                        }
                    }]
                }
            })
        )
        .unwrap();
        first.flush().unwrap();

        let (mut second, second_line) = accept_fake_cli_operation(&listener);
        let second_request: serde_json::Value = serde_json::from_str(&second_line).unwrap();
        assert_eq!(second_request["method"], "plugin.link");
        second
            .write_all(
                br#"{"id":"cli:plugin","error":{"code":"plugin_registry_save_failed","message":"forced failure"}}"#,
            )
            .unwrap();
        second.write_all(b"\n").unwrap();
        second.flush().unwrap();
    });

    let install = run_named_cli_with_env_and_socket_override(
        &config_home,
        &runtime_dir,
        &[
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/worktree-bootstrap",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
        Some(&socket_path),
    );
    assert!(
        !install.status.success(),
        "install should fail when plugin.link fails"
    );
    server.join().unwrap();
    assert!(
        managed_checkout.join("old-marker").exists(),
        "old checkout should be restored after registration failure"
    );

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_rejects_server_that_drops_source_metadata() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("fake-herdr.sock");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("worktree-bootstrap");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.worktree-bootstrap"
name = "Worktree Bootstrap"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "bootstrap"
title = "Bootstrap"
command = ["sh", "-c", "echo install"]
"#,
    )
    .unwrap();
    run_git(
        &source_repo,
        &["add", "worktree-bootstrap/herdr-plugin.toml"],
    );
    run_git(&source_repo, &["commit", "--quiet", "-m", "add plugin"]);

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let managed_checkout = config_home
        .join("herdr-dev")
        .join("plugins")
        .join("github")
        .join(WORKTREE_BOOTSTRAP_MANAGED_COMPONENT);
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let listener = UnixListener::bind(&socket_path).unwrap();
    let managed_checkout_for_server = managed_checkout.clone();
    let server = thread::spawn(move || {
        let (mut first, first_line) = accept_fake_cli_operation(&listener);
        let first_request: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(first_request["method"], "plugin.list");
        first
            .write_all(br#"{"id":"cli:plugin","result":{"type":"plugin_list","plugins":[]}}"#)
            .unwrap();
        first.write_all(b"\n").unwrap();
        first.flush().unwrap();

        let (mut second, second_line) = accept_fake_cli_operation(&listener);
        let second_request: serde_json::Value = serde_json::from_str(&second_line).unwrap();
        assert_eq!(second_request["method"], "plugin.link");
        writeln!(
            second,
            "{}",
            serde_json::json!({
                "id": "cli:plugin",
                "result": {
                    "type": "plugin_linked",
                    "plugin": {
                        "plugin_id": "example.worktree-bootstrap",
                        "name": "Worktree Bootstrap",
                        "version": "0.1.0",
                        "min_herdr_version": "0.6.10",
                        "manifest_path": managed_checkout_for_server.join("herdr-plugin.toml").display().to_string(),
                        "plugin_root": managed_checkout_for_server.display().to_string(),
                        "enabled": true,
                        "source": {"kind": "local"}
                    }
                }
            })
        )
        .unwrap();
        second.flush().unwrap();

        let (mut third, third_line) = accept_fake_cli_operation(&listener);
        let third_request: serde_json::Value = serde_json::from_str(&third_line).unwrap();
        assert_eq!(third_request["method"], "plugin.unlink");
        assert_eq!(
            third_request["params"]["plugin_id"],
            "example.worktree-bootstrap"
        );
        third
            .write_all(
                br#"{"id":"cli:plugin","result":{"type":"plugin_unlinked","plugin_id":"example.worktree-bootstrap","removed":true}}"#,
            )
            .unwrap();
        third.write_all(b"\n").unwrap();
        third.flush().unwrap();
    });

    let install = run_named_cli_with_env_and_socket_override(
        &config_home,
        &runtime_dir,
        &[
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/worktree-bootstrap",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
        Some(&socket_path),
    );
    assert!(
        !install.status.success(),
        "install should fail when server drops GitHub source metadata"
    );
    server.join().unwrap();
    assert!(
        !managed_checkout.exists(),
        "new checkout should be removed after incompatible plugin.link response"
    );

    cleanup_test_base(&base);
}

#[test]
fn plugin_install_keeps_checkout_when_incompatible_server_cleanup_fails() {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("fake-herdr.sock");
    let source_repo = base.join("source-repo");
    let plugin_dir = source_repo.join("worktree-bootstrap");
    fs::create_dir_all(&plugin_dir).unwrap();
    create_committed_repo(&source_repo);
    fs::write(
        plugin_dir.join("herdr-plugin.toml"),
        r#"
id = "example.worktree-bootstrap"
name = "Worktree Bootstrap"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]

[[actions]]
id = "bootstrap"
title = "Bootstrap"
command = ["sh", "-c", "echo install"]
"#,
    )
    .unwrap();
    run_git(
        &source_repo,
        &["add", "worktree-bootstrap/herdr-plugin.toml"],
    );
    run_git(&source_repo, &["commit", "--quiet", "-m", "add plugin"]);

    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    let managed_checkout = config_home
        .join("herdr-dev")
        .join("plugins")
        .join("github")
        .join(WORKTREE_BOOTSTRAP_MANAGED_COMPONENT);
    let git_config = base.join("gitconfig");
    fs::write(
        &git_config,
        format!(
            "[url \"file://{}\"]\n    insteadOf = https://github.com/ogulcancelik/herdr-plugin-examples.git\n",
            source_repo.display()
        ),
    )
    .unwrap();

    let listener = UnixListener::bind(&socket_path).unwrap();
    let managed_checkout_for_server = managed_checkout.clone();
    let server = thread::spawn(move || {
        let (mut first, _first_line) = accept_fake_cli_operation(&listener);
        first
            .write_all(br#"{"id":"cli:plugin","result":{"type":"plugin_list","plugins":[]}}"#)
            .unwrap();
        first.write_all(b"\n").unwrap();
        first.flush().unwrap();

        let (mut second, _second_line) = accept_fake_cli_operation(&listener);
        writeln!(
            second,
            "{}",
            serde_json::json!({
                "id": "cli:plugin",
                "result": {
                    "type": "plugin_linked",
                    "plugin": {
                        "plugin_id": "example.worktree-bootstrap",
                        "name": "Worktree Bootstrap",
                        "version": "0.1.0",
                        "min_herdr_version": "0.6.10",
                        "manifest_path": managed_checkout_for_server.join("herdr-plugin.toml").display().to_string(),
                        "plugin_root": managed_checkout_for_server.display().to_string(),
                        "enabled": true,
                        "source": {"kind": "local"}
                    }
                }
            })
        )
        .unwrap();
        second.flush().unwrap();

        let (mut third, third_line) = accept_fake_cli_operation(&listener);
        let third_request: serde_json::Value = serde_json::from_str(&third_line).unwrap();
        assert_eq!(third_request["method"], "plugin.unlink");
        third
            .write_all(
                br#"{"id":"cli:plugin","error":{"code":"plugin_registry_save_failed","message":"forced unlink failure"}}"#,
            )
            .unwrap();
        third.write_all(b"\n").unwrap();
        third.flush().unwrap();
    });

    let install = run_named_cli_with_env_and_socket_override(
        &config_home,
        &runtime_dir,
        &[
            "plugin",
            "install",
            "ogulcancelik/herdr-plugin-examples/worktree-bootstrap",
            "--yes",
        ],
        &[("GIT_CONFIG_GLOBAL", &git_config)],
        Some(&socket_path),
    );
    assert!(
        !install.status.success(),
        "install should fail when source metadata is dropped and cleanup fails"
    );
    server.join().unwrap();
    assert!(
        managed_checkout.exists(),
        "checkout should stay when server cleanup fails"
    );

    cleanup_test_base(&base);
}
