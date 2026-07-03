use super::command::*;
use super::config_edit::*;
use super::env::*;
use super::file_ops::*;
use super::registry::*;
use super::targets::*;
use super::types::*;
use super::version::*;
use super::*;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

#[test]
fn extract_version_triple_parses_common_outputs() {
    assert_eq!(extract_version_triple("0.14.0"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("v1.2.3"), Some((1, 2, 3)));
    assert_eq!(
        extract_version_triple("kimi-code 0.14.0 (linux/x64)"),
        Some((0, 14, 0))
    );
    assert_eq!(extract_version_triple("0.14"), Some((0, 14, 0)));
    assert_eq!(extract_version_triple("0.14.1-beta.2"), Some((0, 14, 1)));
    assert_eq!(extract_version_triple("no version here"), None);
    assert_eq!(extract_version_triple(""), None);
}

#[test]
fn extract_version_triple_orders_versions() {
    let old = extract_version_triple("0.12.1").unwrap();
    let min = extract_version_triple(KIMI_MIN_VERSION).unwrap();
    let new = extract_version_triple("0.15.0").unwrap();
    assert!(old < min);
    assert!(min <= min);
    assert!(min < new);
}

#[test]
fn agent_version_requirement_only_set_for_kimi() {
    let requirement = agent_version_requirement(crate::api::schema::IntegrationTarget::Kimi)
        .expect("kimi must have a version requirement");
    assert_eq!(requirement.binary, "kimi");
    assert_eq!(requirement.min_version, KIMI_MIN_VERSION);
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Claude).is_none());
    assert!(agent_version_requirement(crate::api::schema::IntegrationTarget::Codex).is_none());
}

#[test]
fn enforce_agent_version_warns_when_binary_missing() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "herdr-test-binary-that-does-not-exist",
        args: &["--version"],
        min_version: "0.14.0",
    };
    let warning = enforce_agent_version(&requirement)
        .expect("missing binary must not fail the install")
        .expect("missing binary must produce a warning");
    assert!(warning.contains("could not run"));
    assert!(warning.contains("0.14.0"));
}

#[cfg(unix)]
#[test]
fn enforce_agent_version_rejects_old_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.12.1"],
        min_version: "0.14.0",
    };
    let err = enforce_agent_version(&requirement).expect_err("old version must fail the install");
    let message = err.to_string();
    assert!(message.contains("0.12.1"));
    assert!(message.contains("0.14.0"));
    assert!(message.contains("upgrade"));
}

#[cfg(unix)]
#[test]
fn enforce_agent_version_accepts_current_version() {
    let requirement = AgentVersionRequirement {
        label: "kimi code",
        binary: "echo",
        args: &["0.14.0"],
        min_version: "0.14.0",
    };
    let result =
        enforce_agent_version(&requirement).expect("matching version must not fail the install");
    assert!(result.is_none(), "matching version must not warn");
}

fn clear_integration_path_env() {
    std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
    std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CODEX_HOME_ENV_VAR);
    std::env::remove_var(COPILOT_HOME_ENV_VAR);
    std::env::remove_var(KIMI_CODE_HOME_ENV_VAR);
    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
}

fn kimi_hook_command(hook_path: &Path, action: &str) -> String {
    hook_command(hook_path, Some(action))
}

fn kimi_config_hooks(config: &str) -> Vec<toml::Value> {
    let parsed: toml::Value = toml::from_str(config).unwrap();
    parsed
        .get("hooks")
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn assert_kimi_hook(config: &str, hook_path: &Path, event: &str, action: &str) {
    let command = kimi_hook_command(hook_path, action);
    let hooks = kimi_config_hooks(config);
    assert!(
        hooks.iter().any(|hook| {
            hook.get("event").and_then(toml::Value::as_str) == Some(event)
                && hook.get("command").and_then(toml::Value::as_str) == Some(command.as_str())
                && hook.get("timeout").and_then(toml::Value::as_integer) == Some(10)
        }),
        "missing kimi hook for {event} -> {action}"
    );
}

fn unique_base() -> PathBuf {
    clear_integration_path_env();
    std::env::temp_dir().join(format!(
        "herdr-integration-install-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[cfg(windows)]
#[test]
fn home_dir_uses_userprofile_when_home_is_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let previous_home = std::env::var_os("HOME");
    let previous_userprofile = std::env::var_os("USERPROFILE");
    std::env::remove_var("HOME");
    std::env::set_var("USERPROFILE", &base);

    assert_eq!(home_dir().unwrap(), base);

    if let Some(home) = previous_home {
        std::env::set_var("HOME", home);
    }
    if let Some(userprofile) = previous_userprofile {
        std::env::set_var("USERPROFILE", userprofile);
    } else {
        std::env::remove_var("USERPROFILE");
    }
}

#[cfg(windows)]
#[test]
fn windows_supports_only_cli_hook_integrations() {
    use crate::api::schema::IntegrationTarget;

    assert!(!integration_target_supported(IntegrationTarget::Pi));
    assert!(!integration_target_supported(IntegrationTarget::Omp));
    assert!(!integration_target_supported(IntegrationTarget::Opencode));
    assert!(!integration_target_supported(IntegrationTarget::Kilo));
    assert!(!integration_target_supported(IntegrationTarget::Hermes));
    assert!(!integration_target_supported(IntegrationTarget::Cursor));
    assert!(!integration_target_supported(IntegrationTarget::Devin));
    assert!(!integration_target_supported(IntegrationTarget::Mastracode));

    assert!(integration_target_supported(IntegrationTarget::Claude));
    assert!(integration_target_supported(IntegrationTarget::Codex));
    assert!(integration_target_supported(IntegrationTarget::Copilot));
    assert!(integration_target_supported(IntegrationTarget::Droid));
    assert!(integration_target_supported(IntegrationTarget::Kimi));
    assert!(integration_target_supported(IntegrationTarget::Qodercli));
}

#[cfg(windows)]
#[test]
fn windows_does_not_offer_unsupported_integrations_even_when_commands_exist() {
    use crate::api::schema::IntegrationTarget;

    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("pi.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("omp.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("opencode.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("kilo.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("hermes.exe"), "").unwrap();
    fs::write(bin.join("cursor-agent.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("devin.cmd"), "@echo off\r\n").unwrap();
    fs::write(bin.join("mastracode.cmd"), "@echo off\r\n").unwrap();

    assert!(!integration_target_available(IntegrationTarget::Pi));
    assert!(!integration_target_available(IntegrationTarget::Omp));
    assert!(!integration_target_available(IntegrationTarget::Opencode));
    assert!(!integration_target_available(IntegrationTarget::Kilo));
    assert!(!integration_target_available(IntegrationTarget::Hermes));
    assert!(!integration_target_available(IntegrationTarget::Cursor));
    assert!(!integration_target_available(IntegrationTarget::Devin));
    assert!(!integration_target_available(IntegrationTarget::Mastracode));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[cfg(windows)]
#[test]
fn windows_install_rejects_unsupported_integration_before_config_lookup() {
    use crate::api::schema::IntegrationTarget;

    let _lock = integration_env_lock();
    let original_home = std::env::var_os("HOME");
    let original_userprofile = std::env::var_os("USERPROFILE");
    let original_homedrive = std::env::var_os("HOMEDRIVE");
    let original_homepath = std::env::var_os("HOMEPATH");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    std::env::remove_var("HOMEDRIVE");
    std::env::remove_var("HOMEPATH");

    let err = install_target(IntegrationTarget::Pi).unwrap_err();
    assert_eq!(
        err.to_string(),
        "pi integration is not supported on Windows"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    }
    if let Some(userprofile) = original_userprofile {
        std::env::set_var("USERPROFILE", userprofile);
    }
    if let Some(homedrive) = original_homedrive {
        std::env::set_var("HOMEDRIVE", homedrive);
    }
    if let Some(homepath) = original_homepath {
        std::env::set_var("HOMEPATH", homepath);
    }
}

#[test]
#[cfg(unix)]
fn command_available_requires_executable_file_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    let command = bin.join("claude");
    fs::write(&command, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!command_available("claude"));

    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(command_available("claude"));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn command_available_finds_windows_command_shims_on_path() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("claude.cmd"), "@echo off\r\n").unwrap();
    assert!(command_available("claude"));

    fs::write(bin.join("codex.exe"), "").unwrap();
    assert!(command_available("codex"));

    assert!(!command_available("missing-agent"));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn qodercli_availability_checks_windows_aliases() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin);

    fs::write(bin.join("qoder.cmd"), "@echo off\r\n").unwrap();

    assert!(integration_target_available(
        crate::api::schema::IntegrationTarget::Qodercli
    ));

    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
#[cfg(windows)]
fn hermes_layout_can_exist_without_making_unsupported_target_available() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let local_app_data = base.join("local-app-data");
    let hermes_bin = local_app_data.join("hermes").join("bin");
    fs::create_dir_all(&hermes_bin).unwrap();
    fs::write(hermes_bin.join("hermes.exe"), "").unwrap();
    let original_local_app_data = std::env::var_os("LOCALAPPDATA");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("LOCALAPPDATA", &local_app_data);
    std::env::set_var("PATH", "");

    assert!(hermes_install_layout_available());
    assert!(!integration_target_available(
        crate::api::schema::IntegrationTarget::Hermes
    ));

    if let Some(local_app_data) = original_local_app_data {
        std::env::set_var("LOCALAPPDATA", local_app_data);
    } else {
        std::env::remove_var("LOCALAPPDATA");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn codex_availability_finds_standalone_binary_under_codex_home() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    assert!(integration_target_available(
        crate::api::schema::IntegrationTarget::Codex
    ));

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendations_mark_standalone_codex_available() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let bin = home
        .join(".codex/packages/standalone/releases/0.137.0-test")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let binary = bin.join(codex_executable_name());
    fs::write(&binary, "").unwrap();
    make_executable(&binary).unwrap();
    let original_home = std::env::var_os("HOME");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("HOME", &home);
    std::env::set_var("PATH", "");

    let codex = integration_recommendations()
        .into_iter()
        .find(|recommendation| {
            recommendation.target == crate::api::schema::IntegrationTarget::Codex
        })
        .expect("codex recommendation should be present");

    assert!(codex.available);
    assert_eq!(codex.state, IntegrationStatusKind::NotInstalled);
    assert!(codex.needs_install());

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(path) = original_path {
        std::env::set_var("PATH", path);
    } else {
        std::env::remove_var("PATH");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn integration_recommendation_installs_available_or_outdated_targets() {
    let mut recommendation = IntegrationRecommendation {
        target: crate::api::schema::IntegrationTarget::Claude,
        label: "claude",
        command: "claude",
        available: false,
        path: PathBuf::from("/tmp/herdr-agent-state.sh"),
        state: IntegrationStatusKind::NotInstalled,
    };
    assert!(!recommendation.needs_install());

    recommendation.available = true;
    assert!(recommendation.needs_install());

    recommendation.available = false;
    recommendation.state = IntegrationStatusKind::Outdated;
    assert!(recommendation.needs_install());

    recommendation.available = true;
    recommendation.state = IntegrationStatusKind::Current;
    assert!(!recommendation.needs_install());
}

#[test]
fn install_pi_writes_embedded_asset_to_pi_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);

    let path = install_pi().unwrap();
    let content = fs::read_to_string(&path).unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));
    assert_eq!(content, PI_EXTENSION_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_uses_pi_coding_agent_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agent_dir = base.join("custom-pi-agent");
    let ext_dir = agent_dir.join("extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, &agent_dir);

    let path = install_pi().unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_expands_tilde_in_pi_coding_agent_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join("custom-pi-agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, "~/custom-pi-agent");

    let path = install_pi().unwrap();

    assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));

    std::env::remove_var("HOME");
    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_writes_embedded_asset_to_omp_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();
    let content = fs::read_to_string(&installed.extension_path).unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);
    assert_eq!(content, OMP_EXTENSION_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_removes_legacy_pi_integration_from_omp_extensions_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let legacy_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&legacy_path, PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(installed.removed_legacy_pi_extension);
    assert!(!legacy_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_preserves_non_herdr_file_with_pi_install_name() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let user_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&user_path, "// user extension\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);
    assert_eq!(
        fs::read_to_string(user_path).unwrap(),
        "// user extension\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_uses_pi_coding_agent_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let agent_dir = base.join("custom-omp-agent");
    let ext_dir = agent_dir.join("extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    std::env::set_var(PI_CODING_AGENT_DIR_ENV_VAR, &agent_dir);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(!installed.removed_legacy_pi_extension);

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_creates_extensions_dir_when_agent_dir_exists() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let agent_dir = home.join(".omp/agent");
    let ext_dir = agent_dir.join("extensions");
    fs::create_dir_all(&agent_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_omp().unwrap();

    assert_eq!(
        installed.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(ext_dir.is_dir());
    assert!(!installed.removed_legacy_pi_extension);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_omp_removes_embedded_extension_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".omp/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME),
        OMP_EXTENSION_ASSET,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_omp().unwrap();

    assert_eq!(
        result.extension_path,
        ext_dir.join(OMP_EXTENSION_INSTALL_NAME)
    );
    assert!(result.removed_extension);
    assert!(!result.extension_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_omp_errors_when_extension_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_omp().unwrap_err().to_string();

    assert!(err.contains("omp extension directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_pi_removes_embedded_extension_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(ext_dir.join(PI_EXTENSION_INSTALL_NAME), PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_pi().unwrap();

    assert_eq!(
        result.extension_path,
        ext_dir.join(PI_EXTENSION_INSTALL_NAME)
    );
    assert!(result.removed_extension);
    assert!(!result.extension_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_treat_missing_version_marker_as_legacy() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    let extension_path = ext_dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&extension_path, "// installed by herdr\n").unwrap();
    std::env::set_var("HOME", &home);

    let outdated = outdated_installed_integrations();

    assert_eq!(outdated.len(), 1);
    assert_eq!(
        outdated[0].target,
        crate::api::schema::IntegrationTarget::Pi
    );
    assert_eq!(outdated[0].path, extension_path);
    assert_eq!(outdated[0].installed_version, None);
    assert_eq!(outdated[0].expected_version, PI_INTEGRATION_VERSION);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn outdated_integrations_accept_current_version_marker() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let ext_dir = home.join(".pi/agent/extensions");
    fs::create_dir_all(&ext_dir).unwrap();
    fs::write(ext_dir.join(PI_EXTENSION_INSTALL_NAME), PI_EXTENSION_ASSET).unwrap();
    std::env::set_var("HOME", &home);

    assert!(outdated_installed_integrations().is_empty());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_pi_errors_when_extension_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_pi().unwrap_err().to_string();

    assert!(err.contains("pi extension directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("settings.json"),
        r#"{"permissions":{"allow":["Read"]},"hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_claude().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        claude_dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)
    );
    assert_eq!(hook_content, CLAUDE_HOOK_ASSET);
    assert!(settings["permissions"]["allow"].is_array());
    assert_eq!(settings["hooks"]["SessionStart"][0]["matcher"], "*");
    assert!(settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" session"));
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_uses_claude_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let claude_dir = base.join("custom-claude");
    fs::create_dir_all(&claude_dir).unwrap();
    std::env::set_var(CLAUDE_CONFIG_DIR_ENV_VAR, &claude_dir);

    let installed = install_claude().unwrap();

    assert_eq!(installed.settings_path, claude_dir.join("settings.json"));
    assert_eq!(
        installed.hook_path,
        claude_dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)
    );

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_claude().unwrap();
    install_claude().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["SessionStart"].as_array().unwrap().len(),
        1
    );
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_removes_deprecated_completion_hooks_and_preserves_user_hooks() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    let settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-post", "timeout": 10}
                ]
            }],
            "PostToolUseFailure": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-failure", "timeout": 10}
                ]
            }],
            "SubagentStop": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-subagent", "timeout": 10}
                ]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' release", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep-session-end", "timeout": 10}
                ]
            }]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_claude().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "echo keep-post"
    );
    assert_eq!(
        settings["hooks"]["PostToolUseFailure"][0]["hooks"][0]["command"],
        "echo keep-failure"
    );
    assert_eq!(
        settings["hooks"]["SubagentStop"][0]["hooks"][0]["command"],
        "echo keep-subagent"
    );
    assert_eq!(
        settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
        "echo keep-session-end"
    );
    assert!(settings["hooks"].get("UserPromptSubmit").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("Stop").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn claude_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(1));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn claude_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_hooks_dir = home.join(".claude").join("hooks");
    fs::create_dir_all(&claude_hooks_dir).unwrap();
    let hook_path = claude_hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=claude\n# HERDR_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let claude = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Claude)
        .unwrap();

    assert_eq!(claude.path, hook_path);
    assert_eq!(claude.installed_version, Some(2));
    assert_eq!(claude.expected_version, 7);
    assert_eq!(claude.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_claude_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let claude_dir = home.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CLAUDE_HOOK_ASSET).unwrap();
    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]
            }],
            "UserPromptSubmit": [{
                "matcher": "*",
                "hooks": [
                    {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                    {"type": "command", "command": "echo keep", "timeout": 10}
                ]
            }],
            "PermissionRequest": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' blocked", hook_path.display()), "timeout": 10}]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "PostToolUseFailure": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "SubagentStop": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("bash '{}' release", hook_path.display()), "timeout": 10}]
            }]
        }
    });
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_claude().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("PostToolUseFailure").is_none());
    assert!(settings["hooks"].get("SubagentStop").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_claude_errors_when_claude_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_claude().unwrap_err().to_string();

    assert!(err.contains("claude directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn codex_v2_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=codex\n# HERDR_INTEGRATION_VERSION=2\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let codex = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Codex)
        .unwrap();

    assert_eq!(codex.path, hook_path);
    assert_eq!(codex.installed_version, Some(2));
    assert_eq!(codex.expected_version, 6);
    assert_eq!(codex.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_writes_hook_and_updates_hooks_and_config() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_codex().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(&installed.hooks_path).unwrap()).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();

    assert_eq!(installed.hook_path, codex_dir.join(CODEX_HOOK_INSTALL_NAME));
    assert_eq!(installed.hooks_path, codex_dir.join("hooks.json"));
    assert_eq!(installed.config_path, codex_dir.join("config.toml"));
    assert_eq!(hook_content, CODEX_HOOK_ASSET);
    assert!(hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(" session"));
    assert!(hooks["hooks"].get("UserPromptSubmit").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert!(config.contains("model = \"gpt-5.4\""));
    assert!(config.contains("[features]"));
    assert!(config.contains("hooks = true"));
    assert!(!config.contains("codex_hooks"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_uses_codex_home_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let codex_dir = base.join("custom-codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("config.toml"), "model = \"gpt-5.4\"\n").unwrap();
    std::env::set_var(CODEX_HOME_ENV_VAR, &codex_dir);

    let installed = install_codex().unwrap();

    assert_eq!(installed.hook_path, codex_dir.join(CODEX_HOOK_INSTALL_NAME));
    assert_eq!(installed.hooks_path, codex_dir.join("hooks.json"));
    assert_eq!(installed.config_path, codex_dir.join("config.toml"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_is_idempotent_for_hook_entries_and_feature_flag() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "[features]\ncodex_hooks = false\nother = true\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_codex().unwrap();
    install_codex().unwrap();

    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap()).unwrap();
    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert_eq!(hooks["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert!(hooks["hooks"].get("UserPromptSubmit").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert_eq!(config.matches("hooks = true").count(), 1);
    assert!(!config.contains("codex_hooks"));
    assert!(config.contains("other = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_only_migrates_top_level_feature_flags() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(
            codex_dir.join("config.toml"),
            "profile = \"work\"\n\n[profiles.work.features]\nhooks = false\ncodex_hooks = false\n\n[features]\ncodex_hooks = true\nother = true\n",
        )
        .unwrap();
    std::env::set_var("HOME", &home);

    install_codex().unwrap();

    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert!(config.contains("[profiles.work.features]\nhooks = false\ncodex_hooks = false"));
    assert!(config.contains("[features]\nhooks = true\nother = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_codex_removes_herdr_hooks_and_leaves_config_alone() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let codex_dir = home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CODEX_HOOK_ASSET).unwrap();
    let hooks = serde_json::json!({
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]}],
            "UserPromptSubmit": [{"hooks": [
                {"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10},
                {"type": "command", "command": "echo keep", "timeout": 10}
            ]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": format!("bash '{}' working", hook_path.display()), "timeout": 10}]}],
            "PermissionRequest": [{"hooks": [{"type": "command", "command": format!("bash '{}' blocked", hook_path.display()), "timeout": 10}]}],
            "Stop": [{"hooks": [{"type": "command", "command": format!("bash '{}' idle", hook_path.display()), "timeout": 10}]}]
        }
    });
    fs::write(
        codex_dir.join("hooks.json"),
        serde_json::to_string(&hooks).unwrap(),
    )
    .unwrap();
    fs::write(
        codex_dir.join("config.toml"),
        "[features]\nhooks = true\nother = true\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_codex().unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap()).unwrap();
    let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!result.hook_path.exists());
    assert!(hooks["hooks"].get("SessionStart").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());
    assert!(hooks["hooks"].get("PermissionRequest").is_none());
    assert!(hooks["hooks"].get("Stop").is_none());
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(config.contains("hooks = true"));
    assert!(config.contains("other = true"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_codex_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_codex().unwrap_err().to_string();

    assert!(err.contains("codex config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_writes_hook_and_updates_config() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    fs::write(
            kimi_dir.join("config.toml"),
            "default_model = \"moonshot\"\n\n[[hooks]]\nevent = \"Notification\"\nmatcher = \"task.completed\"\ncommand = \"echo keep\"\ntimeout = 3\n",
        )
        .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kimi().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert_eq!(
        installed.hook_path,
        kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.config_path, kimi_dir.join("config.toml"));
    assert_eq!(hook_content, KIMI_HOOK_ASSET);
    assert_eq!(hooks.len(), KIMI_HOOK_EVENTS.len() + 1);
    assert!(config.contains("default_model = \"moonshot\""));
    assert!(config.contains("command = \"echo keep\""));
    assert!(config.contains(KIMI_CONFIG_BLOCK_BEGIN));
    assert!(config.contains(KIMI_CONFIG_BLOCK_END));
    for (event, action) in KIMI_HOOK_EVENTS {
        assert_kimi_hook(&config, &installed.hook_path, event, action);
    }

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_uses_kimi_code_home_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let kimi_dir = base.join("custom-kimi");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var(KIMI_CODE_HOME_ENV_VAR, &kimi_dir);

    let installed = install_kimi().unwrap();

    assert_eq!(
        installed.hook_path,
        kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.config_path, kimi_dir.join("config.toml"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_is_idempotent_for_config_block() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_kimi().unwrap();
    install_kimi().unwrap();

    let config = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert_eq!(config.matches(KIMI_CONFIG_BLOCK_BEGIN).count(), 1);
    assert_eq!(config.matches(KIMI_CONFIG_BLOCK_END).count(), 1);
    assert_eq!(hooks.len(), KIMI_HOOK_EVENTS.len());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_kimi_removes_hook_and_config_block_preserves_other_hooks() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kimi_dir = home.join(".kimi-code");
    fs::create_dir_all(&kimi_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kimi().unwrap();
    fs::write(
            &installed.config_path,
            format!(
                "default_model = \"moonshot\"\n\n[[hooks]]\nevent = \"Notification\"\ncommand = \"echo keep\"\n\n{}",
                fs::read_to_string(&installed.config_path).unwrap()
            ),
        )
        .unwrap();

    let result = uninstall_kimi().unwrap();
    let config = fs::read_to_string(kimi_dir.join("config.toml")).unwrap();
    let hooks = kimi_config_hooks(&config);

    assert!(result.removed_hook_file);
    assert!(result.updated_config);
    assert!(!result.hook_path.exists());
    assert!(config.contains("default_model = \"moonshot\""));
    assert!(config.contains("command = \"echo keep\""));
    assert!(!config.contains(KIMI_CONFIG_BLOCK_BEGIN));
    assert!(!config.contains(KIMI_CONFIG_BLOCK_END));
    assert_eq!(hooks.len(), 1);
    assert_eq!(
        hooks[0].get("event").and_then(toml::Value::as_str),
        Some("Notification")
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kimi_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_kimi().unwrap_err().to_string();

    assert!(err.contains("kimi code config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_dir = home.join(".copilot");
    fs::create_dir_all(&copilot_dir).unwrap();
    let hook_path = copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME);
    let stale_session_start_command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    fs::write(
            copilot_dir.join("settings.json"),
            format!(
                r#"{{"theme":"dark","hooks":{{"PreToolUse":[{{"type":"command","command":"echo keep","timeoutSec":10}}],"sessionStart":[{{"type":"command","bash":{},"timeoutSec":10}}]}}}}"#,
                serde_json::to_string(&stale_session_start_command).unwrap()
            ),
        )
        .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_copilot().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.settings_path, copilot_dir.join("settings.json"));
    assert_eq!(hook_content, COPILOT_HOOK_ASSET);
    assert_eq!(settings["theme"], "dark");
    assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(settings["hooks"]["PreToolUse"][0]["command"], "echo keep");
    assert!(settings["hooks"]["SessionStart"][0][direct_command_field()]
        .as_str()
        .unwrap()
        .contains(COPILOT_HOOK_INSTALL_NAME));
    for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
        if let Some(entries) = settings["hooks"].get(event) {
            assert!(
                !entries.to_string().contains(COPILOT_HOOK_INSTALL_NAME),
                "expected herdr hooks.{event} entries to be removed"
            );
        }
    }
    assert!(settings["hooks"].get("sessionStart").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn copilot_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_hooks_dir = home.join(".copilot").join("hooks");
    fs::create_dir_all(&copilot_hooks_dir).unwrap();
    let hook_path = copilot_hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=copilot\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let copilot = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Copilot)
        .unwrap();

    assert_eq!(copilot.path, hook_path);
    assert_eq!(copilot.installed_version, Some(1));
    assert_eq!(copilot.expected_version, COPILOT_INTEGRATION_VERSION);
    assert_eq!(copilot.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_uses_copilot_home_env_and_is_idempotent() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let copilot_dir = base.join("custom-copilot");
    fs::create_dir_all(&copilot_dir).unwrap();
    std::env::set_var(COPILOT_HOME_ENV_VAR, &copilot_dir);

    let installed = install_copilot().unwrap();
    install_copilot().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(copilot_dir.join("settings.json")).unwrap())
            .unwrap();

    assert_eq!(
        installed.hook_path,
        copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)
    );
    assert_eq!(
        settings["hooks"]["SessionStart"].as_array().unwrap().len(),
        1
    );
    for event in COPILOT_REMOVED_LIFECYCLE_HOOK_EVENTS {
        assert!(
            settings["hooks"].get(event).is_none(),
            "expected hooks.{event} to be absent"
        );
    }
    assert!(settings["hooks"].get("sessionStart").is_none());

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_copilot_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let copilot_dir = home.join(".copilot");
    let hooks_dir = copilot_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(&hook_path, COPILOT_HOOK_ASSET).unwrap();
    let command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {"type": "command", direct_command_field(): command, "timeoutSec": 10},
                {"type": "command", "command": "echo keep", "timeoutSec": 10}
            ],
            "PostToolUse": [{"type": "command", direct_command_field(): command, "timeoutSec": 10}],
            "notification": [{
                "type": "command",
                "matcher": "permission_prompt|elicitation_dialog|agent_idle",
                direct_command_field(): command,
                "timeoutSec": 10
            }]
        }
    });
    fs::write(
        copilot_dir.join("settings.json"),
        serde_json::to_string(&settings).unwrap(),
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_copilot().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(copilot_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(settings["hooks"]["PreToolUse"][0]["command"], "echo keep");
    assert!(settings["hooks"].get("PostToolUse").is_none());
    assert!(settings["hooks"].get("notification").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_copilot_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_copilot().unwrap_err().to_string();

    assert!(err.contains("copilot config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    fs::write(
        devin_dir.join("config.json"),
        r#"{"theme_mode":"dark","hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let installed = install_devin().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();

    assert_eq!(installed.hook_path, devin_dir.join(DEVIN_HOOK_INSTALL_NAME));
    assert_eq!(installed.settings_path, devin_dir.join("config.json"));
    assert_eq!(hook_content, DEVIN_HOOK_ASSET);
    assert_eq!(settings["theme_mode"], "dark");
    for (event, action) in DEVIN_HOOK_EVENTS {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.contains(DEVIN_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected devin {event} hook command to end with {action}, got {command}"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    install_devin().unwrap();
    install_devin().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    for (event, _) in DEVIN_HOOK_EVENTS {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "expected hooks.{event} to be idempotent"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_removes_legacy_lifecycle_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let hook_path = devin_dir.join(DEVIN_HOOK_INSTALL_NAME);
    let mut hooks = Map::new();
    for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            json!([
                {
                    "hooks": [{
                        "type": "command",
                        "command": hook_command(&hook_path, Some(action)),
                        "timeout": 10
                    }]
                }
            ]),
        );
    }
    fs::write(
        devin_dir.join("config.json"),
        serde_json::to_string_pretty(&json!({ "hooks": hooks })).unwrap(),
    )
    .unwrap();

    install_devin().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    for (event, action) in DEVIN_REMOVED_LIFECYCLE_HOOK_EVENTS {
        let legacy_command = hook_command(&hook_path, Some(action));
        let entries = settings["hooks"][event].as_array();
        assert!(
            entries.is_none_or(|entries| {
                entries.iter().all(|entry| {
                    entry
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_none_or(|hooks| {
                            hooks.iter().all(|hook| {
                                hook.get("command").and_then(Value::as_str)
                                    != Some(legacy_command.as_str())
                            })
                        })
                })
            }),
            "expected legacy devin {event} -> {action} hook to be removed"
        );

        if !DEVIN_HOOK_EVENTS
            .iter()
            .any(|(installed_event, _)| installed_event == &event)
        {
            continue;
        }

        let session_command = hook_command(&hook_path, Some("session"));
        let entries = entries.unwrap();
        assert!(
            entries.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command").and_then(Value::as_str)
                                == Some(session_command.as_str())
                        })
                    })
            }),
            "expected devin {event} session hook to be installed"
        );
    }

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_devin_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    let devin_dir = xdg_config.join("devin");
    fs::create_dir_all(&devin_dir).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    install_devin().unwrap();

    let hook_path = devin_dir.join(DEVIN_HOOK_INSTALL_NAME);
    let mut settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();
    settings["hooks"]["UserPromptSubmit"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": "echo keep",
                "timeout": 10
            }]
        }));
    fs::write(
        devin_dir.join("config.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    let result = uninstall_devin().unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(devin_dir.join("config.json")).unwrap()).unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_settings);
    assert!(!hook_path.exists());
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert!(settings["hooks"].get("PreToolUse").is_none());
    assert!(settings["hooks"].get("PermissionRequest").is_none());
    assert!(settings["hooks"].get("Stop").is_none());
    assert!(settings["hooks"].get("SessionEnd").is_none());

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_devin_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let xdg_config = base.join("xdg");
    fs::create_dir_all(&xdg_config).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
    std::env::set_var("HOME", base.join("home"));

    let err = install_devin().unwrap_err().to_string();
    assert!(err.contains("devin config directory not found"));

    clear_integration_path_env();
    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_writes_hook_to_settings_and_cleans_legacy_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    let legacy_hook_path = droid_dir.join("hooks").join(DROID_HOOK_INSTALL_NAME);
    fs::create_dir_all(legacy_hook_path.parent().unwrap()).unwrap();
    fs::create_dir_all(&droid_dir).unwrap();
    let legacy_command = format!(
        "bash {}",
        shell_single_quote(&legacy_hook_path.display().to_string())
    );
    fs::write(
            droid_dir.join("hooks.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}}]}}],"PreToolUse":[{{"matcher":"Read","hooks":[{{"type":"command","command":"echo keep","timeout":10}}]}}]}}}}"#,
                legacy_command,
            ),
        )
        .unwrap();
    fs::write(
        droid_dir.join("settings.json"),
        r#"{"theme":"factory-dark"}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_droid().unwrap();
    let hook_content = fs::read_to_string(&installed.hook_path).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();
    let legacy_hooks: Value =
        serde_json::from_str(&fs::read_to_string(&installed.hooks_path).unwrap()).unwrap();

    assert_eq!(
        installed.hook_path,
        droid_dir.join("hooks").join(DROID_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, droid_dir.join("hooks.json"));
    assert_eq!(installed.settings_path, droid_dir.join("settings.json"));
    assert!(installed.updated_legacy_hooks);
    assert_eq!(hook_content, DROID_HOOK_ASSET);
    assert_eq!(settings["theme"], "factory-dark");
    assert!(settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains(DROID_HOOK_INSTALL_NAME));
    assert!(settings["hooks"]["SessionStart"][0]
        .get("matcher")
        .is_none());
    for (event, action) in DROID_HOOK_EVENTS {
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.contains(DROID_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected droid {event} hook command to end with {action}, got {command}"
        );
    }
    assert_eq!(legacy_hooks["hooks"]["PreToolUse"][0]["matcher"], "Read");
    assert!(legacy_hooks["hooks"].get("SessionStart").is_none());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    fs::create_dir_all(&droid_dir).unwrap();
    std::env::set_var("HOME", &home);

    install_droid().unwrap();
    install_droid().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("settings.json")).unwrap())
            .unwrap();
    for (event, _) in DROID_HOOK_EVENTS {
        assert_eq!(
            settings["hooks"][event].as_array().unwrap().len(),
            1,
            "expected hooks.{event} to be idempotent"
        );
    }

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn droid_v1_integration_status_is_outdated() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_hooks_dir = home.join(".factory").join("hooks");
    fs::create_dir_all(&droid_hooks_dir).unwrap();
    let hook_path = droid_hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=droid\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let statuses = installed_integration_statuses();
    let droid = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Droid)
        .unwrap();

    assert_eq!(droid.path, hook_path);
    assert_eq!(droid.installed_version, Some(1));
    assert_eq!(droid.expected_version, DROID_INTEGRATION_VERSION);
    assert_eq!(droid.state, IntegrationStatusKind::Outdated);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_droid_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let droid_dir = home.join(".factory");
    let hooks_dir = droid_dir.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join(DROID_HOOK_INSTALL_NAME);
    fs::write(&hook_path, DROID_HOOK_ASSET).unwrap();
    let command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    fs::write(
            droid_dir.join("hooks.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"PreToolUse":[{{"matcher":"Read","hooks":[{{"type":"command","command":"echo read","timeout":10}}]}}]}}}}"#,
                command,
            ),
        )
        .unwrap();
    fs::write(
            droid_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{}","timeout":10}}]}}],"PostToolUse":[{{"matcher":"Edit","hooks":[{{"type":"command","command":"echo post","timeout":10}}]}}]}}}}"#,
                command,
            ),
        )
        .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_droid().unwrap();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("hooks.json")).unwrap()).unwrap();
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(droid_dir.join("settings.json")).unwrap())
            .unwrap();

    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(result.updated_settings);
    assert!(!result.hook_path.exists());
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo keep"
    );
    assert_eq!(hooks["hooks"]["PreToolUse"][0]["matcher"], "Read");
    assert!(settings["hooks"].get("SessionStart").is_none());
    assert_eq!(settings["hooks"]["PostToolUse"][0]["matcher"], "Edit");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_droid_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_droid().unwrap_err().to_string();

    assert!(err.contains("droid config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_opencode_writes_plugin_to_plugins_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_opencode().unwrap();
    let plugin_content = fs::read_to_string(&installed.plugin_path).unwrap();

    assert_eq!(
        installed.plugin_path,
        opencode_dir
            .join("plugins")
            .join(OPENCODE_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(plugin_content, OPENCODE_PLUGIN_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_opencode_removes_plugin_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let opencode_dir = home.join(".config/opencode/plugins");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(
        opencode_dir.join(OPENCODE_PLUGIN_INSTALL_NAME),
        OPENCODE_PLUGIN_ASSET,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_opencode().unwrap();

    assert!(result.removed_plugin);
    assert!(!result.plugin_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_opencode_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_opencode().unwrap_err().to_string();

    assert!(err.contains("opencode config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kilo_writes_plugin_to_plugin_dir() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kilo_dir = home.join(".config/kilo");
    fs::create_dir_all(&kilo_dir).unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_kilo().unwrap();
    let plugin_content = fs::read_to_string(&installed.plugin_path).unwrap();

    assert_eq!(
        installed.plugin_path,
        kilo_dir.join("plugin").join(KILO_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(plugin_content, KILO_PLUGIN_ASSET);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_kilo_removes_plugin_when_present() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let kilo_plugin_dir = home.join(".config/kilo/plugin");
    fs::create_dir_all(&kilo_plugin_dir).unwrap();
    fs::write(
        kilo_plugin_dir.join(KILO_PLUGIN_INSTALL_NAME),
        KILO_PLUGIN_ASSET,
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_kilo().unwrap();

    assert!(result.removed_plugin);
    assert!(!result.plugin_path.exists());

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_kilo_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_kilo().unwrap_err().to_string();

    assert!(err.contains("kilo config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_writes_plugin_and_enables_it() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(hermes_dir.join("config.yaml"), "model:\n  provider: auto\n").unwrap();
    std::env::set_var("HOME", &home);

    let installed = install_hermes().unwrap();
    let manifest = fs::read_to_string(
        installed
            .plugin_dir
            .join(HERMES_PLUGIN_MANIFEST_INSTALL_NAME),
    )
    .unwrap();
    let init =
        fs::read_to_string(installed.plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME)).unwrap();
    let config = fs::read_to_string(&installed.config_path).unwrap();

    assert_eq!(
        installed.plugin_dir,
        hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME)
    );
    assert_eq!(manifest, HERMES_PLUGIN_MANIFEST_ASSET);
    assert_eq!(init, HERMES_PLUGIN_INIT_ASSET);
    assert!(config.contains("plugins:\n  enabled:\n    - herdr-agent-state"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_is_idempotent_for_enabled_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled:\n    - herdr-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();
    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(config.matches("herdr-agent-state").count(), 1);

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_preserves_flat_plugin_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - platforms/discord\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - herdr-agent-state\n  - platforms/discord\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_converts_flow_plugin_list_to_block_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins: [platforms/discord]\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - herdr-agent-state\n  - platforms/discord\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_is_idempotent_for_quoted_flat_plugin_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - \"herdr-agent-state\" # installed by herdr\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    install_hermes().unwrap();

    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config,
        "plugins:\n  - \"herdr-agent-state\" # installed by herdr\n"
    );

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_plugin_and_enabled_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled:\n    - other-plugin\n    - herdr-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert!(!plugin_dir.exists());
    assert!(config.contains("    - other-plugin"));
    assert!(!config.contains("herdr-agent-state"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_preserves_flat_plugin_list() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - other-plugin\n  - herdr-agent-state\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_flow_plugin_list_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins: [other-plugin, herdr-agent-state]\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_hermes_removes_commented_flat_plugin_entry() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    let hermes_dir = home.join(".hermes");
    let plugin_dir = hermes_dir.join("plugins").join(HERMES_PLUGIN_INSTALL_NAME);
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )
    .unwrap();
    fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  - other-plugin\n  - herdr-agent-state # installed by herdr\n",
    )
    .unwrap();
    std::env::set_var("HOME", &home);

    let result = uninstall_hermes().unwrap();
    let config = fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();

    assert!(result.removed_plugin_dir);
    assert!(result.updated_config);
    assert_eq!(config, "plugins:\n  - other-plugin\n");

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_hermes_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let err = install_hermes().unwrap_err().to_string();

    assert!(err.contains("hermes config directory not found"));

    std::env::remove_var("HOME");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn bundled_integration_assets_report_session_refs() {
    assert!(PI_EXTENSION_ASSET.contains("agent_session_path"));
    assert!(PI_EXTENSION_ASSET.contains("agent_session_id"));
    assert!(PI_EXTENSION_ASSET.contains("ctx?.hasUI !== true"));
    assert!(PI_EXTENSION_ASSET.contains("pane.report_agent_session"));
    assert!(PI_EXTENSION_ASSET.contains("pane.report_agent\""));
    assert!(PI_EXTENSION_ASSET.contains("pi.on(\"agent_start\""));
    assert!(PI_EXTENSION_ASSET.contains("pi.on(\"agent_end\""));
    assert!(PI_EXTENSION_ASSET.contains("pane.release_agent"));
    assert!(PI_EXTENSION_ASSET.contains("pi.on(\"session_shutdown\""));
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_path"));
    assert!(OMP_EXTENSION_ASSET.contains("agent_session_id"));
    assert!(OMP_EXTENSION_ASSET.contains("ctx?.hasUI !== true"));
    assert!(OMP_EXTENSION_ASSET.contains("pane.report_agent_session"));
    assert!(OMP_EXTENSION_ASSET.contains("pane.report_agent\""));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"agent_start\""));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"agent_end\""));
    assert!(OMP_EXTENSION_ASSET.contains("pane.release_agent"));
    assert!(OMP_EXTENSION_ASSET.contains("pi.on(\"session_shutdown\""));
    assert!(
        CLAUDE_HOOK_ASSET.contains("agent_session_id")
            || CLAUDE_HOOK_ASSET.contains("--agent-session-id")
    );
    assert!(
        CLAUDE_HOOK_ASSET.contains("agent_session_path")
            || CLAUDE_HOOK_ASSET.contains("--agent-session-path")
    );
    assert!(CLAUDE_HOOK_ASSET.contains("agent_id"));
    assert!(
        CLAUDE_HOOK_ASSET.contains("session_start_source")
            || CLAUDE_HOOK_ASSET.contains("--session-start-source")
    );
    assert!(
        CLAUDE_HOOK_ASSET.contains("pane.report_agent_session")
            || CLAUDE_HOOK_ASSET.contains("report-agent-session")
    );
    assert!(!CLAUDE_HOOK_ASSET.contains("\"state\": action"));
    assert!(!CLAUDE_HOOK_ASSET.contains("pane.release_agent"));
    assert!(
        CODEX_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE")
            || CODEX_HOOK_ASSET.contains("In.ReadToEnd")
    );
    assert!(
        CODEX_HOOK_ASSET.contains("agent_session_id")
            || CODEX_HOOK_ASSET.contains("--agent-session-id")
    );
    assert!(
        CODEX_HOOK_ASSET.contains("session_start_source")
            || CODEX_HOOK_ASSET.contains("--session-start-source")
    );
    assert!(
        CODEX_HOOK_ASSET.contains("pane.report_agent_session")
            || CODEX_HOOK_ASSET.contains("report-agent-session")
    );
    assert!(!CODEX_HOOK_ASSET.contains("\"state\": action"));
    assert!(!CODEX_HOOK_ASSET.contains("pane.release_agent"));
    assert!(KIMI_HOOK_ASSET.contains("source = \"herdr:kimi\""));
    assert!(KIMI_HOOK_ASSET.contains("agent_session_id"));
    assert!(KIMI_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(KIMI_HOOK_ASSET.contains("\"state\": action"));
    assert!(!KIMI_HOOK_ASSET.contains("pane.release_agent"));
    assert!(COPILOT_HOOK_ASSET.contains("agent_session_id"));
    assert!(COPILOT_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!COPILOT_HOOK_ASSET.contains("\"state\":"));
    assert!(!COPILOT_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("HERDR_DEVIN_LIST_JSON"));
    assert!(DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent_session\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"method\": \"pane.report_agent\""));
    assert!(!DEVIN_HOOK_ASSET.contains("\"state\":"));
    assert!(!DEVIN_HOOK_ASSET.contains("pane.release_agent"));
    assert!(DEVIN_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("agent_session_id"));
    assert!(DROID_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!DROID_HOOK_ASSET.contains("\"state\": action"));
    assert!(!DROID_HOOK_ASSET.contains("pane.release_agent"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("properties?.sessionID"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("params.agent_session_id = sessionID"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("pane.report_agent_session"));
    assert!(OPENCODE_PLUGIN_ASSET.contains("reportState"));
    assert!(!OPENCODE_PLUGIN_ASSET.contains("pane.release_agent"));
    assert!(KILO_PLUGIN_ASSET.contains("SOURCE = \"herdr:kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("AGENT = \"kilo\""));
    assert!(KILO_PLUGIN_ASSET.contains("pane.report_agent_session"));
    assert!(KILO_PLUGIN_ASSET.contains("reportState"));
    assert!(!KILO_PLUGIN_ASSET.contains("pane.release_agent"));
    assert!(HERMES_PLUGIN_INIT_ASSET.contains("session_id = _session_id(kwargs)"));
    assert!(HERMES_PLUGIN_INIT_ASSET.contains("agent_session_id"));
    assert!(HERMES_PLUGIN_INIT_ASSET.contains("pane.report_agent\","));
    assert!(HERMES_PLUGIN_INIT_ASSET.contains("on_session_end"));
    assert!(!HERMES_PLUGIN_INIT_ASSET.contains("on_session_finalize"));
    assert!(!HERMES_PLUGIN_INIT_ASSET.contains("pane.release_agent"));
    assert!(QODERCLI_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE"));
    assert!(QODERCLI_HOOK_ASSET.contains("agent_session_id"));
    assert!(QODERCLI_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(!QODERCLI_HOOK_ASSET.contains("\"state\": action"));
    assert!(!QODERCLI_HOOK_ASSET.contains("pane.release_agent"));
    assert!(!QODERCLI_HOOK_ASSET.contains("QODER_HOOK_EVENT"));
    assert!(CURSOR_HOOK_ASSET.contains("HERDR_INTEGRATION_ID=cursor"));
    assert!(CURSOR_HOOK_ASSET.contains("conversation_id"));
    assert!(CURSOR_HOOK_ASSET.contains("conversationId"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionId"));
    assert!(CURSOR_HOOK_ASSET.contains("agent_session_id"));
    assert!(CURSOR_HOOK_ASSET.contains("pane.report_agent_session"));
    assert!(CURSOR_HOOK_ASSET.contains("hook_event_name"));
    assert!(CURSOR_HOOK_ASSET.contains("sessionStart"));
    assert!(!CURSOR_HOOK_ASSET.contains("\"state\":"));
    assert!(!CURSOR_HOOK_ASSET.contains("pane.release_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("HERDR_INTEGRATION_ID=mastracode"));
    assert!(MASTRACODE_HOOK_ASSET.contains("HERDR_INTEGRATION_VERSION=1"));
    assert!(MASTRACODE_HOOK_ASSET.contains("session_id"));
    assert!(!MASTRACODE_HOOK_ASSET.contains("run_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("agent_session_id"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.report_agent"));
    assert!(MASTRACODE_HOOK_ASSET.contains("pane.release_agent"));
}

#[test]
fn pi_extension_releases_only_for_quit_session_shutdown() {
    let release_policy = PI_EXTENSION_ASSET
        .find("function shouldReleaseOnSessionShutdown")
        .expect("pi extension should centralize session shutdown release policy");
    let quit_check = PI_EXTENSION_ASSET
        .find("reason === \"quit\"")
        .expect("pi extension should release only for true quit shutdowns");
    let shutdown_handler = PI_EXTENSION_ASSET
        .find("pi.on(\"session_shutdown\", async (event)")
        .expect("pi extension should inspect the session_shutdown event");
    let guarded_release = PI_EXTENSION_ASSET[shutdown_handler..]
        .find("if (shouldReleaseOnSessionShutdown(event))")
        .expect("pi extension should guard releaseAgent by shutdown reason");

    assert!(release_policy < shutdown_handler);
    assert!(release_policy < quit_check);
    assert!(quit_check < shutdown_handler);
    assert!(guarded_release > 0);
}

#[test]
fn pi_extension_refreshes_session_ref_before_agent_start_state() {
    let agent_start = PI_EXTENSION_ASSET
        .find("pi.on(\"agent_start\", (_event, ctx)")
        .expect("pi extension should receive agent_start context");
    let handler = &PI_EXTENSION_ASSET[agent_start..];
    let update_session = handler
        .find("updateSessionRef(ctx);")
        .expect("pi extension should refresh the active session on agent_start");
    let report_session = handler
        .find("void reportSession();")
        .expect("pi extension should report the refreshed session before state");
    let publish_state = handler
        .find("publishState();")
        .expect("pi extension should publish working state after refreshing session");

    assert!(update_session < report_session);
    assert!(report_session < publish_state);
}

#[test]
fn omp_extension_releases_only_for_quit_session_shutdown() {
    let release_policy = OMP_EXTENSION_ASSET
        .find("function shouldReleaseOnSessionShutdown")
        .expect("omp extension should centralize session shutdown release policy");
    let quit_check = OMP_EXTENSION_ASSET
        .find("reason === \"quit\"")
        .expect("omp extension should release only for true quit shutdowns");
    let shutdown_handler = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_shutdown\", async (event)")
        .expect("omp extension should inspect the session_shutdown event");
    let guarded_release = OMP_EXTENSION_ASSET[shutdown_handler..]
        .find("if (shouldReleaseOnSessionShutdown(event))")
        .expect("omp extension should guard releaseAgent by shutdown reason");

    assert!(release_policy < shutdown_handler);
    assert!(release_policy < quit_check);
    assert!(quit_check < shutdown_handler);
    assert!(guarded_release > 0);
}

#[test]
fn omp_extension_refreshes_session_ref_before_agent_start_state() {
    let agent_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"agent_start\", (_event, ctx)")
        .expect("omp extension should receive agent_start context");
    let handler = &OMP_EXTENSION_ASSET[agent_start..];
    let update_session = handler
        .find("updateSessionRef(ctx);")
        .expect("omp extension should refresh the active session on agent_start");
    let report_session = handler
        .find("void reportSession();")
        .expect("omp extension should report the refreshed session before state");
    let publish_state = handler
        .find("publishState();")
        .expect("omp extension should publish working state after refreshing session");

    assert!(update_session < report_session);
    assert!(report_session < publish_state);
}

fn omp_handler(event: &str) -> &'static str {
    let start = OMP_EXTENSION_ASSET
        .find(&format!("pi.on(\"{event}\""))
        .unwrap_or_else(|| panic!("omp extension registers {event} handler"));
    let rest = &OMP_EXTENSION_ASSET[start..];
    let end = rest[1..]
        .find("\n\n  pi.")
        .map(|offset| offset + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn omp_root_activation_requires_ui_context() {
    let activator = OMP_EXTENSION_ASSET
        .find("function activateRootSession(ctx: any, sessionStartSource = \"startup\"): boolean")
        .expect("omp extension should centralize root session activation");
    let helper = &OMP_EXTENSION_ASSET[activator..];
    let non_ui_guard = helper
        .find("ctx?.hasUI !== true")
        .expect("omp extension checks UI context before activating");
    let root_session = helper
        .find("rootSession = true;")
        .expect("omp extension activates root session after UI guard");
    let session_report = helper
        .find("void reportSession(sessionStartSource);")
        .expect("omp extension reports root session");

    assert!(non_ui_guard < root_session);
    assert!(root_session < session_report);
}

#[test]
fn omp_session_start_and_switch_use_root_activation() {
    let session_start = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_start\", (_event, ctx)")
        .expect("omp extension registers session_start handler");
    let session_start_handler = &OMP_EXTENSION_ASSET[session_start..];
    session_start_handler
        .find("if (!activateRootSession(ctx))")
        .expect("omp session_start handler should activate root session");

    let session_switch = OMP_EXTENSION_ASSET
        .find("pi.on(\"session_switch\", (event, ctx)")
        .expect("omp extension registers session_switch handler");
    let session_switch_handler = &OMP_EXTENSION_ASSET[session_switch..];
    session_switch_handler
        .find("if (!activateRootSession(ctx, event?.reason || \"resume\"))")
        .expect("omp session_switch handler should activate root session with switch reason");
}

#[test]
fn omp_session_reports_include_start_source() {
    let report_session = OMP_EXTENSION_ASSET
        .find("function reportSession(sessionStartSource = \"startup\"): Promise<void>")
        .expect("omp extension should label session reports with a lifecycle source");
    let helper = &OMP_EXTENSION_ASSET[report_session..];
    let session_source = helper
        .find("session_start_source: sessionStartSource")
        .expect("omp session reports should include the lifecycle source");
    let session_ref = helper
        .find("...sessionRef")
        .expect("omp session reports should include the native session ref");

    assert!(session_source < session_ref);
}

#[test]
fn omp_socket_requests_are_serialized() {
    let queue = OMP_EXTENSION_ASSET
        .find("let requestQueue = Promise.resolve();")
        .expect("omp extension should keep socket reports ordered");
    let send_request = OMP_EXTENSION_ASSET[queue..]
        .find("function sendRequest(request: unknown): Promise<void>")
        .expect("omp extension should wrap socket sends in an ordered queue");
    let queued_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("requestQueue = requestQueue.then(")
        .expect("omp extension should serialize socket requests through the queue");
    let raw_send = OMP_EXTENSION_ASSET[queue + send_request..]
        .find("sendRequestNow(request)")
        .expect("omp extension should enqueue the raw socket send");

    assert!(queued_send < raw_send);
}

#[test]
fn omp_runtime_events_can_activate_root_session_after_resume() {
    for event in [
        "agent_start",
        "tool_approval_requested",
        "tool_approval_resolved",
        "tool_execution_start",
        "tool_execution_end",
    ] {
        let handler = omp_handler(event);
        handler
            .find("!rootSession && !activateRootSession(ctx)")
            .unwrap_or_else(|| panic!("omp {event} handler should recover missing root session"));
    }
}

#[test]
fn omp_ask_and_approval_events_report_blocked_state() {
    let approval_handler = omp_handler("tool_approval_requested");
    approval_handler
        .find("activateBlocked(label);")
        .expect("approval requests should block the pane");

    let approval_resolved = omp_handler("tool_approval_resolved");
    approval_resolved
        .find("deactivateBlocked();")
        .expect("approval resolution should unblock the pane");

    let ask_handler = omp_handler("tool_execution_start");
    ask_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution handler should only treat Ask as blocked");
    ask_handler
        .find("activateBlocked(askBlockedMessage(event.args));")
        .expect("Ask start should block the pane");

    let ask_end_handler = omp_handler("tool_execution_end");
    ask_end_handler
        .find("event?.toolName !== \"ask\"")
        .expect("tool execution end should only treat Ask as blocked");
    ask_end_handler
        .find("deactivateBlocked();")
        .expect("Ask end should unblock the pane");
}

#[test]
fn install_qodercli_writes_hook_and_updates_settings() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    fs::write(
        qoder_dir.join("settings.json"),
        r#"{"permissions":{"allow":["Read"]},"hooks":{}}"#,
    )
    .unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    let installed = install_qodercli().unwrap();

    assert_eq!(
        installed.hook_path,
        qoder_dir.join("hooks").join(QODERCLI_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.settings_path, qoder_dir.join("settings.json"));
    assert!(installed.hook_path.is_file());

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(&installed.settings_path).unwrap()).unwrap();
    let hooks = settings
        .get("hooks")
        .and_then(Value::as_object)
        .expect("hooks should be present");
    for (event, action) in QODERCLI_HOOK_EVENTS {
        assert!(
            hooks.contains_key(event),
            "expected hooks.{event} to be registered"
        );
        let command = hooks[event][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            command.contains(QODERCLI_HOOK_INSTALL_NAME) && command.ends_with(action),
            "expected qodercli {event} hook command to end with {action}, got {command}"
        );
    }
    // Pre-existing settings keys must be preserved.
    assert!(settings.get("permissions").is_some());

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_qodercli_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    install_qodercli().unwrap();
    install_qodercli().unwrap();

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    let hooks = settings.get("hooks").and_then(Value::as_object).unwrap();
    for (event, _) in QODERCLI_HOOK_EVENTS {
        let entries = hooks.get(event).and_then(Value::as_array).unwrap();
        assert_eq!(
            entries.len(),
            1,
            "expected hooks.{event} to contain exactly one entry, got {entries:?}"
        );
    }

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_qodercli_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let qoder_dir = base.join(".qoder");
    fs::create_dir_all(&qoder_dir).unwrap();
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &qoder_dir);

    install_qodercli().unwrap();
    // Inject a foreign hook entry the user might have configured by hand.
    let mut settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    settings["hooks"]["SessionStart"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "matcher": "*",
            "hooks": [{"type": "command", "command": "echo user-defined"}],
        }));
    fs::write(
        qoder_dir.join("settings.json"),
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    let result = uninstall_qodercli().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_settings);

    let settings: Value =
        serde_json::from_str(&fs::read_to_string(qoder_dir.join("settings.json")).unwrap())
            .unwrap();
    let hooks = settings.get("hooks").and_then(Value::as_object).unwrap();
    let remaining = hooks.get("SessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(remaining.len(), 1);
    let cmd = remaining[0]["hooks"][0]["command"].as_str().unwrap();
    assert_eq!(cmd, "echo user-defined");

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_qodercli_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let missing = base.join(".qoder");
    std::env::set_var(QODERCLI_CONFIG_DIR_ENV_VAR, &missing);

    let err = install_qodercli().unwrap_err().to_string();
    assert!(
        err.contains("qodercli config directory not found"),
        "unexpected error: {err}"
    );

    std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_writes_hook_and_updates_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    fs::write(
        cursor_dir.join("hooks.json"),
        r#"{"version":1,"hooks":{"stop":[{"command":"echo keep-me"}]}}"#,
    )
    .unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let installed = install_cursor().unwrap();

    assert_eq!(
        installed.hook_path,
        cursor_dir.join(CURSOR_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, cursor_dir.join("hooks.json"));
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        CURSOR_HOOK_ASSET
    );

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    let session_start = hooks.get("sessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(session_start.len(), 1);
    assert!(session_start[0]
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| {
            command.starts_with("bash ")
                && command.contains("herdr-agent-state.sh")
                && command.ends_with(" session")
        }));
    assert!(hooks.get("beforeSubmitPrompt").is_none());
    assert!(hooks.get("beforeShellExecution").is_none());
    let stop = hooks.get("stop").and_then(Value::as_array).unwrap();
    assert_eq!(stop.len(), 1);
    assert_eq!(
        stop[0].get("command").and_then(Value::as_str),
        Some("echo keep-me")
    );

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    install_cursor().unwrap();
    install_cursor().unwrap();

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    let session_start = hooks.get("sessionStart").and_then(Value::as_array).unwrap();
    assert_eq!(session_start.len(), 1);

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_cursor_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    install_cursor().unwrap();
    let mut hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    hooks_file["hooks"]["beforeSubmitPrompt"] = json!([{ "command": "echo user-defined" }]);
    fs::write(
        cursor_dir.join("hooks.json"),
        serde_json::to_string_pretty(&hooks_file).unwrap(),
    )
    .unwrap();

    let result = uninstall_cursor().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!cursor_dir.join(CURSOR_HOOK_INSTALL_NAME).is_file());

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(cursor_dir.join("hooks.json")).unwrap()).unwrap();
    let hooks = hooks_file.get("hooks").and_then(Value::as_object).unwrap();
    assert!(!hooks.contains_key("sessionStart"));
    assert!(hooks.contains_key("beforeSubmitPrompt"));

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_uses_cursor_config_dir_env() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join("custom-cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let installed = install_cursor().unwrap();

    assert_eq!(
        installed.hook_path,
        cursor_dir.join(CURSOR_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, cursor_dir.join("hooks.json"));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn cursor_v1_integration_status_is_current() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let cursor_dir = base.join(".cursor");
    fs::create_dir_all(&cursor_dir).unwrap();
    let hook_path = cursor_dir.join(CURSOR_HOOK_INSTALL_NAME);
    fs::write(
        &hook_path,
        "#!/bin/sh\n# HERDR_INTEGRATION_ID=cursor\n# HERDR_INTEGRATION_VERSION=1\n",
    )
    .unwrap();
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &cursor_dir);

    let statuses = installed_integration_statuses();
    let cursor = statuses
        .iter()
        .find(|status| status.target == crate::api::schema::IntegrationTarget::Cursor)
        .expect("cursor integration status");
    assert_eq!(cursor.state, IntegrationStatusKind::Current);
    assert_eq!(cursor.installed_version, Some(CURSOR_INTEGRATION_VERSION));

    clear_integration_path_env();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_cursor_errors_when_config_dir_missing() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let missing = base.join(".cursor");
    std::env::set_var(CURSOR_CONFIG_DIR_ENV_VAR, &missing);

    let err = install_cursor().unwrap_err().to_string();
    assert!(
        err.contains("cursor config directory not found"),
        "unexpected error: {err}"
    );

    std::env::remove_var(CURSOR_CONFIG_DIR_ENV_VAR);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_writes_hook_and_updates_hooks_json() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(
        mastracode_dir.join("hooks.json"),
        r#"{"PostToolUse":[{"type":"command","command":"echo keep-me"}]}"#,
    )
    .unwrap();
    std::env::set_var("HOME", &base);

    let installed = install_mastracode().unwrap();

    assert_eq!(
        installed.hook_path,
        mastracode_dir
            .join("hooks")
            .join(MASTRACODE_HOOK_INSTALL_NAME)
    );
    assert_eq!(installed.hooks_path, mastracode_dir.join("hooks.json"));
    assert_eq!(
        fs::read_to_string(&installed.hook_path).unwrap(),
        MASTRACODE_HOOK_ASSET
    );

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(mastracode_dir.join("hooks.json")).unwrap())
            .unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, action) in MASTRACODE_HOOK_EVENTS {
        let entries = hooks.get(event).and_then(Value::as_array).unwrap();
        assert_eq!(entries.len(), 1, "{event} should have one Herdr hook");
        let command = entries[0].get("command").and_then(Value::as_str).unwrap();
        assert!(command.starts_with("bash "));
        assert!(command.contains(MASTRACODE_HOOK_INSTALL_NAME));
        assert!(command.ends_with(action));
        assert_eq!(
            entries[0].get("type").and_then(Value::as_str),
            Some("command")
        );
        assert_eq!(
            entries[0].get("timeout").and_then(Value::as_u64),
            Some(MASTRACODE_HOOK_TIMEOUT_MS)
        );
    }
    assert_eq!(
        hooks["PostToolUse"][0]
            .get("command")
            .and_then(Value::as_str),
        Some("echo keep-me")
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_is_idempotent_for_hook_entries() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &base);

    install_mastracode().unwrap();
    install_mastracode().unwrap();

    let hooks_file: Value = serde_json::from_str(
        &fs::read_to_string(base.join(".mastracode").join("hooks.json")).unwrap(),
    )
    .unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, _) in MASTRACODE_HOOK_EVENTS {
        assert_eq!(hooks.get(event).and_then(Value::as_array).unwrap().len(), 1);
    }

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_mastracode_removes_herdr_hooks_and_preserves_others() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &base);

    install_mastracode().unwrap();
    let hooks_path = base.join(".mastracode").join("hooks.json");
    let mut hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    hooks_file["UserPromptSubmit"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "type": "command", "command": "echo user-defined" }));
    fs::write(
        &hooks_path,
        serde_json::to_string_pretty(&hooks_file).unwrap(),
    )
    .unwrap();

    let result = uninstall_mastracode().unwrap();
    assert!(result.removed_hook_file);
    assert!(result.updated_hooks);
    assert!(!base
        .join(".mastracode")
        .join("hooks")
        .join(MASTRACODE_HOOK_INSTALL_NAME)
        .is_file());

    let hooks_file: Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    let hooks = hooks_file.as_object().unwrap();
    for (event, _) in MASTRACODE_HOOK_EVENTS {
        if event == "UserPromptSubmit" {
            continue;
        }
        assert!(!hooks.contains_key(event), "{event} should be removed");
    }
    let user_prompt_submit = hooks
        .get("UserPromptSubmit")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(user_prompt_submit.len(), 1);
    assert_eq!(
        user_prompt_submit[0].get("command").and_then(Value::as_str),
        Some("echo user-defined")
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn install_mastracode_errors_when_event_value_not_array() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(mastracode_dir.join("hooks.json"), r#"{"SessionStart":{}}"#).unwrap();
    std::env::set_var("HOME", &base);

    let err = install_mastracode().unwrap_err().to_string();
    assert!(
        err.contains("hook entries for SessionStart must be an array"),
        "unexpected error: {err}"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn uninstall_mastracode_errors_when_event_value_not_array() {
    let _lock = integration_env_lock();
    let base = unique_base();
    let original_home = std::env::var_os("HOME");
    let mastracode_dir = base.join(".mastracode");
    fs::create_dir_all(&mastracode_dir).unwrap();
    fs::write(mastracode_dir.join("hooks.json"), r#"{"SessionStart":{}}"#).unwrap();
    std::env::set_var("HOME", &base);

    let err = uninstall_mastracode().unwrap_err().to_string();
    assert!(
        err.contains("hook entries for SessionStart must be an array"),
        "unexpected error: {err}"
    );

    if let Some(home) = original_home {
        std::env::set_var("HOME", home);
    } else {
        std::env::remove_var("HOME");
    }
    let _ = fs::remove_dir_all(base);
}
