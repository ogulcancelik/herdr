use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;
use serde_json::{json, Map, Value};

use crate::layout::PaneId;

pub(crate) const HERDR_PANE_ID_ENV_VAR: &str = "HERDR_PANE_ID";
const PI_EXTENSION_INSTALL_NAME: &str = "herdr-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/herdr-agent-state.ts");
const CLAUDE_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const CLAUDE_HOOK_ASSET: &str = include_str!("assets/claude/herdr-agent-state.sh");
const CODEX_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const CODEX_HOOK_ASSET: &str = include_str!("assets/codex/herdr-agent-state.sh");
const OPENCODE_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/herdr-agent-state.js");

#[derive(Debug)]
pub(crate) struct ClaudeInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CodexInstallPaths {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct OpenCodeInstallPaths {
    pub plugin_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PiUninstallResult {
    pub extension_path: PathBuf,
    pub removed_extension: bool,
}

#[derive(Debug)]
pub(crate) struct ClaudeUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct CodexUninstallResult {
    pub hook_path: PathBuf,
    pub hooks_path: PathBuf,
    pub config_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_hooks: bool,
}

#[derive(Debug)]
pub(crate) struct OpenCodeUninstallResult {
    pub plugin_path: PathBuf,
    pub removed_plugin: bool,
}

pub(crate) fn apply_pane_env(cmd: &mut CommandBuilder, pane_id: PaneId) {
    cmd.env(crate::api::SOCKET_PATH_ENV_VAR, crate::api::socket_path());
    cmd.env(HERDR_PANE_ID_ENV_VAR, format!("p_{}", pane_id.raw()));
}

pub(crate) fn install_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let path = install_pi()?;
            vec![format!("installed pi integration to {}", path.display())]
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let installed = install_claude()?;
            vec![
                format!(
                    "installed claude integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured claude settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let installed = install_codex()?;
            vec![
                format!(
                    "installed codex integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("ensured codex hooks at {}", installed.hooks_path.display()),
                format!(
                    "ensured codex config at {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let installed = install_opencode()?;
            vec![format!(
                "installed opencode integration plugin to {}",
                installed.plugin_path.display()
            )]
        }
    };

    crate::logging::integration_action("install", integration_target_label(target), "ok");
    Ok(messages)
}

pub(crate) fn uninstall_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let messages = match target {
        crate::api::schema::IntegrationTarget::Pi => {
            let result = uninstall_pi()?;
            if result.removed_extension {
                vec![format!(
                    "removed pi integration extension at {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "no pi integration extension found at {}",
                    result.extension_path.display()
                )]
            }
        }
        crate::api::schema::IntegrationTarget::Claude => {
            let result = uninstall_claude()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed claude hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no claude hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed herdr claude hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr claude hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Codex => {
            let result = uninstall_codex()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed codex hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no codex hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed herdr codex hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr codex hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            messages.push(format!(
                "left codex config unchanged at {}",
                result.config_path.display()
            ));
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let result = uninstall_opencode()?;
            if result.removed_plugin {
                vec![format!(
                    "removed opencode integration plugin at {}",
                    result.plugin_path.display()
                )]
            } else {
                vec![format!(
                    "no opencode integration plugin found at {}",
                    result.plugin_path.display()
                )]
            }
        }
    };

    crate::logging::integration_action("uninstall", integration_target_label(target), "ok");
    Ok(messages)
}

fn integration_target_label(target: crate::api::schema::IntegrationTarget) -> &'static str {
    match target {
        crate::api::schema::IntegrationTarget::Pi => "pi",
        crate::api::schema::IntegrationTarget::Claude => "claude",
        crate::api::schema::IntegrationTarget::Codex => "codex",
        crate::api::schema::IntegrationTarget::Opencode => "opencode",
    }
}

pub(crate) fn install_pi() -> io::Result<PathBuf> {
    let dir = pi_extension_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "pi extension directory not found at {}. install pi and create the extensions directory first",
            dir.display()
        )));
    }

    let path = dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&path, PI_EXTENSION_ASSET)?;
    Ok(path)
}

pub(crate) fn install_claude() -> io::Result<ClaudeInstallPaths> {
    let dir = claude_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "claude directory not found at {}. install claude code first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CLAUDE_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let settings_path = dir.join("settings.json");
    let mut settings = if settings_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?).map_err(|err| {
            io::Error::other(format!(
                "failed to parse {}: {err}",
                settings_path.display()
            ))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut settings,
        &settings_path,
        "claude settings",
        "claude settings hooks",
    )?;
    let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
    ensure_command_hook(
        hooks,
        "UserPromptSubmit",
        format!("bash {quoted_hook_path} working"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "PreToolUse",
        format!("bash {quoted_hook_path} working"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "PermissionRequest",
        format!("bash {quoted_hook_path} blocked"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "PostToolUse",
        format!("bash {quoted_hook_path} working"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "PostToolUseFailure",
        format!("bash {quoted_hook_path} working"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "SubagentStop",
        format!("bash {quoted_hook_path} working"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "Stop",
        format!("bash {quoted_hook_path} idle"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "SessionEnd",
        format!("bash {quoted_hook_path} release"),
        10,
        Some("*"),
    )?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(ClaudeInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn install_codex() -> io::Result<CodexInstallPaths> {
    let dir = codex_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "codex config directory not found at {}. install codex first",
            dir.display()
        )));
    }

    let hook_path = dir.join(CODEX_HOOK_INSTALL_NAME);
    fs::write(&hook_path, CODEX_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let hooks_path = dir.join("hooks.json");
    let mut hooks_file = if hooks_path.is_file() {
        serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?).map_err(|err| {
            io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
        })?
    } else {
        json!({})
    };

    let hooks = ensure_hooks_object(
        &mut hooks_file,
        &hooks_path,
        "codex hooks file",
        "codex hooks file hooks",
    )?;
    let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
    ensure_command_hook(
        hooks,
        "SessionStart",
        format!("bash {quoted_hook_path} idle"),
        10,
        None,
    )?;
    ensure_command_hook(
        hooks,
        "UserPromptSubmit",
        format!("bash {quoted_hook_path} working"),
        10,
        None,
    )?;
    ensure_command_hook(
        hooks,
        "PreToolUse",
        format!("bash {quoted_hook_path} working"),
        10,
        None,
    )?;
    ensure_command_hook(
        hooks,
        "Stop",
        format!("bash {quoted_hook_path} idle"),
        10,
        None,
    )?;

    fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;

    let config_path = dir.join("config.toml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = build_codex_config_with_hooks(&existing_config);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }

    Ok(CodexInstallPaths {
        hook_path,
        hooks_path,
        config_path,
    })
}

pub(crate) fn install_opencode() -> io::Result<OpenCodeInstallPaths> {
    let dir = opencode_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "opencode config directory not found at {}. install opencode first",
            dir.display()
        )));
    }

    let plugins_dir = dir.join("plugins");
    fs::create_dir_all(&plugins_dir)?;

    let plugin_path = plugins_dir.join(OPENCODE_PLUGIN_INSTALL_NAME);
    fs::write(&plugin_path, OPENCODE_PLUGIN_ASSET)?;

    Ok(OpenCodeInstallPaths { plugin_path })
}

pub(crate) fn uninstall_pi() -> io::Result<PiUninstallResult> {
    let extension_path = pi_extension_dir()?.join(PI_EXTENSION_INSTALL_NAME);
    let removed_extension = remove_file_if_exists(&extension_path)?;

    Ok(PiUninstallResult {
        extension_path,
        removed_extension,
    })
}

pub(crate) fn uninstall_claude() -> io::Result<ClaudeUninstallResult> {
    let hook_path = claude_dir()?.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME);
    let settings_path = claude_dir()?.join("settings.json");
    let mut updated_settings = false;

    if settings_path.is_file() {
        let mut settings = serde_json::from_str::<Value>(&fs::read_to_string(&settings_path)?)
            .map_err(|err| {
                io::Error::other(format!(
                    "failed to parse {}: {err}",
                    settings_path.display()
                ))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut settings,
            &settings_path,
            "claude settings",
            "claude settings hooks",
        )? {
            let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
            updated_settings |= remove_command_hook(
                hooks,
                "UserPromptSubmit",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "PreToolUse",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "PermissionRequest",
                &format!("bash {quoted_hook_path} blocked"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "PostToolUse",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "PostToolUseFailure",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "SubagentStop",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_settings |=
                remove_command_hook(hooks, "Stop", &format!("bash {quoted_hook_path} idle"))?;
            updated_settings |= remove_command_hook(
                hooks,
                "SessionEnd",
                &format!("bash {quoted_hook_path} release"),
            )?;
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file = remove_file_if_exists(&hook_path)?;

    Ok(ClaudeUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
    })
}

pub(crate) fn uninstall_codex() -> io::Result<CodexUninstallResult> {
    let codex_dir = codex_dir()?;
    let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
    let hooks_path = codex_dir.join("hooks.json");
    let config_path = codex_dir.join("config.toml");
    let mut updated_hooks = false;

    if hooks_path.is_file() {
        let mut hooks_file = serde_json::from_str::<Value>(&fs::read_to_string(&hooks_path)?)
            .map_err(|err| {
                io::Error::other(format!("failed to parse {}: {err}", hooks_path.display()))
            })?;

        if let Some(hooks) = hooks_object_if_present(
            &mut hooks_file,
            &hooks_path,
            "codex hooks file",
            "codex hooks file hooks",
        )? {
            let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
            updated_hooks |= remove_command_hook(
                hooks,
                "SessionStart",
                &format!("bash {quoted_hook_path} idle"),
            )?;
            updated_hooks |= remove_command_hook(
                hooks,
                "UserPromptSubmit",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_hooks |= remove_command_hook(
                hooks,
                "PreToolUse",
                &format!("bash {quoted_hook_path} working"),
            )?;
            updated_hooks |=
                remove_command_hook(hooks, "Stop", &format!("bash {quoted_hook_path} idle"))?;
        }

        if updated_hooks {
            fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_file)?)?;
        }
    }

    let removed_hook_file = remove_file_if_exists(&hook_path)?;

    Ok(CodexUninstallResult {
        hook_path,
        hooks_path,
        config_path,
        removed_hook_file,
        updated_hooks,
    })
}

pub(crate) fn uninstall_opencode() -> io::Result<OpenCodeUninstallResult> {
    let plugin_path = opencode_dir()?
        .join("plugins")
        .join(OPENCODE_PLUGIN_INSTALL_NAME);
    let removed_plugin = remove_file_if_exists(&plugin_path)?;

    Ok(OpenCodeUninstallResult {
        plugin_path,
        removed_plugin,
    })
}

fn ensure_hooks_object<'a>(
    settings: &'a mut Value,
    settings_path: &Path,
    root_description: &str,
    hooks_description: &str,
) -> io::Result<&'a mut Map<String, Value>> {
    let root = settings.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{root_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })?;

    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    hooks.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{hooks_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })
}

fn hooks_object_if_present<'a>(
    settings: &'a mut Value,
    settings_path: &Path,
    root_description: &str,
    hooks_description: &str,
) -> io::Result<Option<&'a mut Map<String, Value>>> {
    let root = settings.as_object_mut().ok_or_else(|| {
        io::Error::other(format!(
            "{root_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })?;

    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(None);
    };

    hooks.as_object_mut().map(Some).ok_or_else(|| {
        io::Error::other(format!(
            "{hooks_description} at {} must be a JSON object",
            settings_path.display()
        ))
    })
}

fn ensure_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: String,
    timeout: u64,
    matcher: Option<&str>,
) -> io::Result<()> {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let already_installed = entries.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hook_entries| {
                hook_entries.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str) == Some(command.as_str())
                })
            })
    });
    if already_installed {
        return Ok(());
    }

    let mut entry = Map::new();
    if let Some(matcher) = matcher {
        entry.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    entry.insert(
        "hooks".to_string(),
        json!([
            {
                "type": "command",
                "command": command,
                "timeout": timeout,
            }
        ]),
    );

    entries.push(Value::Object(entry));
    Ok(())
}

fn remove_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: &str,
) -> io::Result<bool> {
    let Some(entries_value) = hooks.get_mut(event) else {
        return Ok(false);
    };

    let entries = entries_value
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    let mut removed = false;
    entries.retain_mut(|entry| {
        let Some(entry_object) = entry.as_object_mut() else {
            return true;
        };
        let Some(hook_entries) = entry_object.get_mut("hooks") else {
            return true;
        };
        let Some(hook_entries) = hook_entries.as_array_mut() else {
            return true;
        };

        let before = hook_entries.len();
        hook_entries.retain(|hook| !is_matching_command_hook(hook, command));
        if hook_entries.len() != before {
            removed = true;
        }

        !hook_entries.is_empty()
    });

    let remove_event = entries.is_empty();
    if remove_event {
        hooks.remove(event);
    }

    Ok(removed)
}

fn is_matching_command_hook(hook: &Value, command: &str) -> bool {
    hook.get("type").and_then(Value::as_str) == Some("command")
        && hook.get("command").and_then(Value::as_str) == Some(command)
}

fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn build_codex_config_with_hooks(content: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let trailing_newline = content.ends_with('\n');

    if let Some(index) = lines
        .iter()
        .position(|line| is_toml_key(line, "codex_hooks"))
    {
        lines[index] = "codex_hooks = true".to_string();
        let mut result = lines.join("\n");
        if trailing_newline || result.is_empty() {
            result.push('\n');
        }
        return result;
    }

    if let Some(index) = lines.iter().position(|line| line.trim() == "[features]") {
        lines.insert(index + 1, "codex_hooks = true".to_string());
        let mut result = lines.join("\n");
        if trailing_newline || result.is_empty() {
            result.push('\n');
        }
        return result;
    }

    let mut result = content.trim_end_matches('\n').to_string();
    if !result.is_empty() {
        result.push('\n');
        result.push('\n');
    }
    result.push_str("[features]\ncodex_hooks = true\n");
    result
}

fn is_toml_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || !trimmed.starts_with(key) {
        return false;
    }

    trimmed[key.len()..].trim_start().starts_with('=')
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

fn pi_extension_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".pi/agent/extensions"))
}

fn claude_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".claude"))
}

fn codex_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".codex"))
}

fn opencode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/opencode"))
}

fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::other("HOME is not set; cannot locate home directory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn unique_base() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-integration-install-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn install_pi_writes_embedded_asset_to_pi_extensions_dir() {
        let _lock = env_lock();
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
    fn uninstall_pi_removes_embedded_extension_when_present() {
        let _lock = env_lock();
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
    fn install_pi_errors_when_extension_dir_missing() {
        let _lock = env_lock();
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
        let _lock = env_lock();
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
        assert_eq!(settings["hooks"]["UserPromptSubmit"][0]["matcher"], "*");
        assert!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains(" working")
        );
        assert!(settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" working"));
        assert!(
            settings["hooks"]["PermissionRequest"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains(" blocked")
        );
        assert!(settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" working"));
        assert!(
            settings["hooks"]["PostToolUseFailure"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains(" working")
        );
        assert!(settings["hooks"]["SubagentStop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" working"));
        assert!(settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" idle"));
        assert!(settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" release"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_claude_is_idempotent_for_hook_entries() {
        let _lock = env_lock();
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
            settings["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            settings["hooks"]["PermissionRequest"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            settings["hooks"]["PostToolUse"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            settings["hooks"]["PostToolUseFailure"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            settings["hooks"]["SubagentStop"].as_array().unwrap().len(),
            1
        );
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(settings["hooks"]["SessionEnd"].as_array().unwrap().len(), 1);

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_claude_removes_herdr_hooks_and_preserves_others() {
        let _lock = env_lock();
        let base = unique_base();
        let home = base.join("home");
        let claude_dir = home.join(".claude");
        let hooks_dir = claude_dir.join("hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let hook_path = hooks_dir.join(CLAUDE_HOOK_INSTALL_NAME);
        fs::write(&hook_path, CLAUDE_HOOK_ASSET).unwrap();
        fs::write(
            claude_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"UserPromptSubmit":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"PermissionRequest":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' blocked","timeout":10}}]}}],"PostToolUse":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"PostToolUseFailure":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"SubagentStop":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"Stop":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}],"SessionEnd":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' release","timeout":10}}]}}]}}}}"#,
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
            ),
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
        let _lock = env_lock();
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
    fn install_codex_writes_hook_and_updates_hooks_and_config() {
        let _lock = env_lock();
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
            .contains(" idle"));
        assert!(hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" working"));
        assert!(hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" working"));
        assert!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(" idle"));
        assert!(config.contains("model = \"gpt-5.4\""));
        assert!(config.contains("[features]"));
        assert!(config.contains("codex_hooks = true"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_codex_is_idempotent_for_hook_entries_and_feature_flag() {
        let _lock = env_lock();
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
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
        let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

        assert_eq!(hooks["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(
            hooks["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1
        );
        assert_eq!(hooks["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(hooks["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(config.matches("codex_hooks = true").count(), 1);
        assert!(config.contains("other = true"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_codex_removes_herdr_hooks_and_leaves_config_alone() {
        let _lock = env_lock();
        let base = unique_base();
        let home = base.join("home");
        let codex_dir = home.join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let hook_path = codex_dir.join(CODEX_HOOK_INSTALL_NAME);
        fs::write(&hook_path, CODEX_HOOK_ASSET).unwrap();
        fs::write(
            codex_dir.join("hooks.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}],"UserPromptSubmit":[{{"hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"Stop":[{{"hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}]}}}}"#,
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
            ),
        )
        .unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "[features]\ncodex_hooks = true\nother = true\n",
        )
        .unwrap();
        std::env::set_var("HOME", &home);

        let result = uninstall_codex().unwrap();
        let hooks: Value =
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
        let config = fs::read_to_string(codex_dir.join("config.toml")).unwrap();

        assert!(result.removed_hook_file);
        assert!(result.updated_hooks);
        assert!(!result.hook_path.exists());
        assert!(hooks["hooks"].get("SessionStart").is_none());
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
        assert!(config.contains("codex_hooks = true"));
        assert!(config.contains("other = true"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_codex_errors_when_config_dir_missing() {
        let _lock = env_lock();
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
    fn install_opencode_writes_plugin_to_plugins_dir() {
        let _lock = env_lock();
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
        let _lock = env_lock();
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
        let _lock = env_lock();
        let base = unique_base();
        let home = base.join("home");
        fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        let err = install_opencode().unwrap_err().to_string();

        assert!(err.contains("opencode config directory not found"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }
}
