use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

use portable_pty::CommandBuilder;
use serde_json::{json, Map, Value};

use crate::layout::PaneId;

pub(crate) const HERDR_PANE_ID_ENV_VAR: &str = "HERDR_PANE_ID";
const PI_EXTENSION_INSTALL_NAME: &str = "herdr-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/herdr-agent-state.ts");
const PI_INTEGRATION_VERSION: u32 = 2;
const OMP_EXTENSION_INSTALL_NAME: &str = "herdr-omp-agent-state.ts";
const OMP_EXTENSION_ASSET: &str = include_str!("assets/omp/herdr-agent-state.ts");
const OMP_INTEGRATION_VERSION: u32 = 2;
const PI_CODING_AGENT_DIR_ENV_VAR: &str = "PI_CODING_AGENT_DIR";
const CLAUDE_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const CLAUDE_HOOK_ASSET: &str = include_str!("assets/claude/herdr-agent-state.sh");
const CLAUDE_INTEGRATION_VERSION: u32 = 6;
const CLAUDE_CONFIG_DIR_ENV_VAR: &str = "CLAUDE_CONFIG_DIR";
const CODEX_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const CODEX_HOOK_ASSET: &str = include_str!("assets/codex/herdr-agent-state.sh");
const CODEX_INTEGRATION_VERSION: u32 = 5;
const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
const KIMI_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const KIMI_HOOK_ASSET: &str = include_str!("assets/kimi/herdr-agent-state.sh");
const KIMI_INTEGRATION_VERSION: u32 = 1;
const KIMI_CODE_HOME_ENV_VAR: &str = "KIMI_CODE_HOME";
const KIMI_CONFIG_BLOCK_BEGIN: &str = "# >>> herdr kimi integration";
const KIMI_CONFIG_BLOCK_END: &str = "# <<< herdr kimi integration";
const KIMI_MIN_VERSION: &str = "0.8.0";
const KIMI_HOOK_EVENTS: [(&str, &str); 10] = [
    ("SessionStart", "idle"),
    ("UserPromptSubmit", "working"),
    ("PreToolUse", "working"),
    ("PermissionRequest", "blocked"),
    ("PermissionResult", "working"),
    ("PostToolUse", "working"),
    ("PostToolUseFailure", "working"),
    ("Stop", "idle"),
    ("StopFailure", "idle"),
    ("SessionEnd", "release"),
];
const COPILOT_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const COPILOT_HOOK_ASSET: &str = include_str!("assets/copilot/herdr-agent-state.sh");
const COPILOT_INTEGRATION_VERSION: u32 = 1;
const COPILOT_HOME_ENV_VAR: &str = "COPILOT_HOME";
const OPENCODE_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state.js";
const OPENCODE_PLUGIN_ASSET: &str = include_str!("assets/opencode/herdr-agent-state.js");
const OPENCODE_INTEGRATION_VERSION: u32 = 4;
const HERMES_PLUGIN_INSTALL_NAME: &str = "herdr-agent-state";
const HERMES_PLUGIN_MANIFEST_INSTALL_NAME: &str = "plugin.yaml";
const HERMES_PLUGIN_INIT_INSTALL_NAME: &str = "__init__.py";
const HERMES_PLUGIN_MANIFEST_ASSET: &str = include_str!("assets/hermes/plugin.yaml");
const HERMES_PLUGIN_INIT_ASSET: &str = include_str!("assets/hermes/__init__.py");
const HERMES_INTEGRATION_VERSION: u32 = 2;
const QODERCLI_HOOK_INSTALL_NAME: &str = "herdr-agent-state.sh";
const QODERCLI_HOOK_ASSET: &str = include_str!("assets/qodercli/herdr-agent-state.sh");
const QODERCLI_INTEGRATION_VERSION: u32 = 1;
const QODERCLI_CONFIG_DIR_ENV_VAR: &str = "QODER_CONFIG_DIR";
const INTEGRATION_VERSION_MARKER: &str = "HERDR_INTEGRATION_VERSION=";

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
pub(crate) struct KimiInstallPaths {
    pub hook_path: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct CopilotInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct OpenCodeInstallPaths {
    pub plugin_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct OmpInstallPaths {
    pub extension_path: PathBuf,
    pub removed_legacy_pi_extension: bool,
}

#[derive(Debug)]
pub(crate) struct HermesInstallPaths {
    pub plugin_dir: PathBuf,
    pub config_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct QodercliInstallPaths {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct QodercliUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationStatus {
    pub target: crate::api::schema::IntegrationTarget,
    pub path: PathBuf,
    pub state: IntegrationStatusKind,
    pub installed_version: Option<u32>,
    pub expected_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrationStatusKind {
    NotInstalled,
    Current,
    Outdated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationRecommendation {
    pub target: crate::api::schema::IntegrationTarget,
    pub label: &'static str,
    pub command: &'static str,
    pub available: bool,
    pub path: PathBuf,
    pub state: IntegrationStatusKind,
}

impl IntegrationRecommendation {
    pub fn needs_install(&self) -> bool {
        self.state == IntegrationStatusKind::Outdated
            || (self.available && self.state == IntegrationStatusKind::NotInstalled)
    }

    pub fn status_label(&self) -> &'static str {
        match (self.available, self.state) {
            (_, IntegrationStatusKind::Current) => "installed",
            (_, IntegrationStatusKind::Outdated) => "update available",
            (true, IntegrationStatusKind::NotInstalled) => "available",
            (false, IntegrationStatusKind::NotInstalled) => "not found",
        }
    }
}

#[derive(Debug)]
pub(crate) struct PiUninstallResult {
    pub extension_path: PathBuf,
    pub removed_extension: bool,
}

#[derive(Debug)]
pub(crate) struct OmpUninstallResult {
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
pub(crate) struct KimiUninstallResult {
    pub hook_path: PathBuf,
    pub config_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_config: bool,
}

#[derive(Debug)]
pub(crate) struct CopilotUninstallResult {
    pub hook_path: PathBuf,
    pub settings_path: PathBuf,
    pub removed_hook_file: bool,
    pub updated_settings: bool,
}

#[derive(Debug)]
pub(crate) struct OpenCodeUninstallResult {
    pub plugin_path: PathBuf,
    pub removed_plugin: bool,
}

#[derive(Debug)]
pub(crate) struct HermesUninstallResult {
    pub plugin_dir: PathBuf,
    pub config_path: PathBuf,
    pub removed_plugin_dir: bool,
    pub updated_config: bool,
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
        crate::api::schema::IntegrationTarget::Omp => {
            let installed = install_omp()?;
            let mut messages = Vec::new();
            if installed.removed_legacy_pi_extension {
                messages.push(format!(
                    "removed legacy pi integration from omp extension directory at {}",
                    installed
                        .extension_path
                        .with_file_name(PI_EXTENSION_INSTALL_NAME)
                        .display()
                ));
            }
            messages.push(format!(
                "installed omp integration to {}",
                installed.extension_path.display()
            ));
            messages
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
        crate::api::schema::IntegrationTarget::Copilot => {
            let installed = install_copilot()?;
            vec![
                format!(
                    "installed copilot integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured copilot settings at {}",
                    installed.settings_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let installed = install_kimi()?;
            vec![
                format!(
                    "installed kimi integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("ensured kimi config at {}", installed.config_path.display()),
                format!("requires kimi code {KIMI_MIN_VERSION} or newer"),
            ]
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let installed = install_opencode()?;
            vec![format!(
                "installed opencode integration plugin to {}",
                installed.plugin_path.display()
            )]
        }
        crate::api::schema::IntegrationTarget::Hermes => {
            let installed = install_hermes()?;
            vec![
                format!(
                    "installed hermes integration plugin to {}",
                    installed.plugin_dir.display()
                ),
                format!(
                    "enabled hermes plugin in {}",
                    installed.config_path.display()
                ),
            ]
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let installed = install_qodercli()?;
            vec![
                format!(
                    "installed qodercli integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured qodercli settings at {}",
                    installed.settings_path.display()
                ),
            ]
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
        crate::api::schema::IntegrationTarget::Omp => {
            let result = uninstall_omp()?;
            if result.removed_extension {
                vec![format!(
                    "removed omp integration extension at {}",
                    result.extension_path.display()
                )]
            } else {
                vec![format!(
                    "no omp integration extension found at {}",
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
        crate::api::schema::IntegrationTarget::Copilot => {
            let result = uninstall_copilot()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed copilot hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no copilot hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed herdr copilot hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr copilot hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Kimi => {
            let result = uninstall_kimi()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed kimi hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no kimi hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "removed herdr kimi hook entries from {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr kimi hook entries found in {}",
                    result.config_path.display()
                ));
            }
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
        crate::api::schema::IntegrationTarget::Hermes => {
            let result = uninstall_hermes()?;
            let mut messages = Vec::new();
            if result.removed_plugin_dir {
                messages.push(format!(
                    "removed hermes integration plugin at {}",
                    result.plugin_dir.display()
                ));
            } else {
                messages.push(format!(
                    "no hermes integration plugin found at {}",
                    result.plugin_dir.display()
                ));
            }
            if result.updated_config {
                messages.push(format!(
                    "disabled hermes plugin in {}",
                    result.config_path.display()
                ));
            } else {
                messages.push(format!(
                    "no hermes plugin entry found in {}",
                    result.config_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Qodercli => {
            let result = uninstall_qodercli()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed qodercli hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no qodercli hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed herdr qodercli hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr qodercli hook entries found in {}",
                    result.settings_path.display()
                ));
            }
            messages
        }
    };

    crate::logging::integration_action("uninstall", integration_target_label(target), "ok");
    Ok(messages)
}

pub(crate) fn integration_target_label(
    target: crate::api::schema::IntegrationTarget,
) -> &'static str {
    match target {
        crate::api::schema::IntegrationTarget::Pi => "pi",
        crate::api::schema::IntegrationTarget::Omp => "omp",
        crate::api::schema::IntegrationTarget::Claude => "claude",
        crate::api::schema::IntegrationTarget::Codex => "codex",
        crate::api::schema::IntegrationTarget::Copilot => "copilot",
        crate::api::schema::IntegrationTarget::Kimi => "kimi",
        crate::api::schema::IntegrationTarget::Opencode => "opencode",
        crate::api::schema::IntegrationTarget::Hermes => "hermes",
        crate::api::schema::IntegrationTarget::Qodercli => "qodercli",
    }
}

fn integration_target_command(target: crate::api::schema::IntegrationTarget) -> &'static str {
    match target {
        crate::api::schema::IntegrationTarget::Pi => "pi",
        crate::api::schema::IntegrationTarget::Omp => "omp",
        crate::api::schema::IntegrationTarget::Claude => "claude",
        crate::api::schema::IntegrationTarget::Codex => "codex",
        crate::api::schema::IntegrationTarget::Copilot => "copilot",
        crate::api::schema::IntegrationTarget::Kimi => "kimi",
        crate::api::schema::IntegrationTarget::Opencode => "opencode",
        crate::api::schema::IntegrationTarget::Hermes => "hermes",
        crate::api::schema::IntegrationTarget::Qodercli => "qodercli",
    }
}

fn integration_target_available(target: crate::api::schema::IntegrationTarget) -> bool {
    command_available(integration_target_command(target))
}

fn command_available(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| executable_file_exists(&dir.join(command)))
}

fn executable_file_exists(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn installed_integration_statuses() -> Vec<IntegrationStatus> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            Some(integration_status_at(target, path.ok()?, expected_version))
        })
        .collect()
}

pub(crate) fn integration_recommendations() -> Vec<IntegrationRecommendation> {
    integration_specs()
        .into_iter()
        .filter_map(|(target, path, expected_version)| {
            let path = path.ok()?;
            let status = integration_status_at(target, path.clone(), expected_version);
            Some(IntegrationRecommendation {
                target,
                label: integration_target_label(target),
                command: integration_target_command(target),
                available: integration_target_available(target)
                    || status.state != IntegrationStatusKind::NotInstalled,
                path,
                state: status.state,
            })
        })
        .collect()
}

fn outdated_installed_integrations() -> Vec<IntegrationStatus> {
    installed_integration_statuses()
        .into_iter()
        .filter(|status| status.state == IntegrationStatusKind::Outdated)
        .collect()
}

fn integration_specs() -> [(
    crate::api::schema::IntegrationTarget,
    io::Result<PathBuf>,
    u32,
); 9] {
    [
        (
            crate::api::schema::IntegrationTarget::Pi,
            pi_extension_dir().map(|dir| dir.join(PI_EXTENSION_INSTALL_NAME)),
            PI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Omp,
            omp_extension_dir().map(|dir| dir.join(OMP_EXTENSION_INSTALL_NAME)),
            OMP_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Claude,
            claude_dir().map(|dir| dir.join("hooks").join(CLAUDE_HOOK_INSTALL_NAME)),
            CLAUDE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Codex,
            codex_dir().map(|dir| dir.join(CODEX_HOOK_INSTALL_NAME)),
            CODEX_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Copilot,
            copilot_dir().map(|dir| dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME)),
            COPILOT_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Kimi,
            kimi_dir().map(|dir| dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME)),
            KIMI_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Opencode,
            opencode_dir().map(|dir| dir.join("plugins").join(OPENCODE_PLUGIN_INSTALL_NAME)),
            OPENCODE_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Hermes,
            hermes_plugin_dir().map(|dir| dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME)),
            HERMES_INTEGRATION_VERSION,
        ),
        (
            crate::api::schema::IntegrationTarget::Qodercli,
            qodercli_dir().map(|dir| dir.join("hooks").join(QODERCLI_HOOK_INSTALL_NAME)),
            QODERCLI_INTEGRATION_VERSION,
        ),
    ]
}

pub(crate) fn integration_update_instructions(
    targets: &[crate::api::schema::IntegrationTarget],
) -> String {
    let commands: Vec<String> = targets
        .iter()
        .map(|target| {
            format!(
                "`herdr integration install {}`",
                integration_target_label(*target)
            )
        })
        .collect();

    match commands.as_slice() {
        [] => String::new(),
        [command] => format!("run {command}"),
        [rest @ .., last] => format!("run {} and {last}", rest.join(", ")),
    }
}

pub(crate) fn print_outdated_update_notice() -> bool {
    let outdated = outdated_installed_integrations();
    if outdated.is_empty() {
        return false;
    }

    let targets = outdated
        .iter()
        .map(|integration| integration.target)
        .collect::<Vec<_>>();
    eprintln!(
        "installed herdr integrations need updating; {}.",
        integration_update_instructions(&targets).replace('`', "")
    );
    true
}

fn integration_status_at(
    target: crate::api::schema::IntegrationTarget,
    path: PathBuf,
    expected_version: u32,
) -> IntegrationStatus {
    if !path.is_file() {
        return IntegrationStatus {
            target,
            path,
            state: IntegrationStatusKind::NotInstalled,
            installed_version: None,
            expected_version,
        };
    }

    let installed_version = fs::read_to_string(&path)
        .ok()
        .and_then(|content| parse_integration_version(&content));
    let state = if installed_version.is_some_and(|version| version >= expected_version) {
        IntegrationStatusKind::Current
    } else {
        IntegrationStatusKind::Outdated
    };

    IntegrationStatus {
        target,
        path,
        state,
        installed_version,
        expected_version,
    }
}

fn parse_integration_version(content: &str) -> Option<u32> {
    content.lines().find_map(|line| {
        let marker_line = line
            .trim()
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim();
        marker_line
            .strip_prefix(INTEGRATION_VERSION_MARKER)?
            .trim()
            .parse()
            .ok()
    })
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

pub(crate) fn install_omp() -> io::Result<OmpInstallPaths> {
    let dir = omp_extension_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "omp extension directory not found at {}. install omp and create the extensions directory first",
            dir.display()
        )));
    }

    let removed_legacy_pi_extension = remove_legacy_pi_extension_from_omp_dir(&dir)?;
    let extension_path = dir.join(OMP_EXTENSION_INSTALL_NAME);
    fs::write(&extension_path, OMP_EXTENSION_ASSET)?;
    Ok(OmpInstallPaths {
        extension_path,
        removed_legacy_pi_extension,
    })
}

fn remove_legacy_pi_extension_from_omp_dir(dir: &Path) -> io::Result<bool> {
    let legacy_path = dir.join(PI_EXTENSION_INSTALL_NAME);
    if !legacy_path.is_file() {
        return Ok(false);
    }

    let content = fs::read_to_string(&legacy_path)?;
    if content.contains("HERDR_INTEGRATION_ID=pi") {
        fs::remove_file(legacy_path)?;
        return Ok(true);
    }

    Ok(false)
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
    remove_command_hook(
        hooks,
        "PostToolUse",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(
        hooks,
        "PostToolUseFailure",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(
        hooks,
        "SubagentStop",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(
        hooks,
        "PermissionRequest",
        &format!("bash {quoted_hook_path} blocked"),
    )?;
    remove_command_hook(
        hooks,
        "SessionStart",
        &format!("bash {quoted_hook_path} idle"),
    )?;
    remove_command_hook(
        hooks,
        "UserPromptSubmit",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(
        hooks,
        "PreToolUse",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(hooks, "Stop", &format!("bash {quoted_hook_path} idle"))?;
    remove_command_hook(
        hooks,
        "SessionEnd",
        &format!("bash {quoted_hook_path} release"),
    )?;
    ensure_command_hook(
        hooks,
        "SessionStart",
        format!("bash {quoted_hook_path} session"),
        10,
        Some("*"),
    )?;
    ensure_command_hook(
        hooks,
        "UserPromptSubmit",
        format!("bash {quoted_hook_path} prompt"),
        10,
        None,
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
    remove_command_hook(
        hooks,
        "PermissionRequest",
        &format!("bash {quoted_hook_path} blocked"),
    )?;
    remove_command_hook(
        hooks,
        "SessionStart",
        &format!("bash {quoted_hook_path} idle"),
    )?;
    remove_command_hook(
        hooks,
        "UserPromptSubmit",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(
        hooks,
        "PreToolUse",
        &format!("bash {quoted_hook_path} working"),
    )?;
    remove_command_hook(hooks, "Stop", &format!("bash {quoted_hook_path} idle"))?;
    ensure_command_hook(
        hooks,
        "SessionStart",
        format!("bash {quoted_hook_path} session"),
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

pub(crate) fn install_kimi() -> io::Result<KimiInstallPaths> {
    let dir = kimi_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "kimi code config directory not found at {}. install kimi code first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(KIMI_HOOK_INSTALL_NAME);
    fs::write(&hook_path, KIMI_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    let config_path = dir.join("config.toml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = build_kimi_config_with_hooks(&existing_config, &hook_path);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }

    Ok(KimiInstallPaths {
        hook_path,
        config_path,
    })
}

pub(crate) fn install_copilot() -> io::Result<CopilotInstallPaths> {
    let dir = copilot_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "copilot config directory not found at {}. install github copilot cli first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(COPILOT_HOOK_INSTALL_NAME);
    fs::write(&hook_path, COPILOT_HOOK_ASSET)?;
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
        "copilot settings",
        "copilot settings hooks",
    )?;
    let command = format!(
        "bash {}",
        shell_single_quote(&hook_path.display().to_string())
    );
    ensure_direct_command_hook(hooks, "SessionStart", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "UserPromptSubmit", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "PreToolUse", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "PostToolUse", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "PostToolUseFailure", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "Stop", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "agentStop", command.clone(), 10, None)?;
    ensure_direct_command_hook(hooks, "SessionEnd", command.clone(), 10, None)?;
    ensure_direct_command_hook(
        hooks,
        "notification",
        command,
        10,
        Some("permission_prompt|elicitation_dialog|agent_idle"),
    )?;

    fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    Ok(CopilotInstallPaths {
        hook_path,
        settings_path,
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

pub(crate) fn install_hermes() -> io::Result<HermesInstallPaths> {
    let dir = hermes_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "hermes config directory not found at {}. install hermes agent first",
            dir.display()
        )));
    }

    let plugin_dir = hermes_plugin_dir()?;
    fs::create_dir_all(&plugin_dir)?;
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_MANIFEST_INSTALL_NAME),
        HERMES_PLUGIN_MANIFEST_ASSET,
    )?;
    fs::write(
        plugin_dir.join(HERMES_PLUGIN_INIT_INSTALL_NAME),
        HERMES_PLUGIN_INIT_ASSET,
    )?;

    let config_path = dir.join("config.yaml");
    let existing_config = if config_path.is_file() {
        fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    let new_config = ensure_hermes_plugin_enabled(&existing_config);
    if new_config != existing_config {
        fs::write(&config_path, new_config)?;
    }

    Ok(HermesInstallPaths {
        plugin_dir,
        config_path,
    })
}

pub(crate) fn uninstall_pi() -> io::Result<PiUninstallResult> {
    let extension_path = pi_extension_dir()?.join(PI_EXTENSION_INSTALL_NAME);
    let removed_extension = remove_file_if_exists(&extension_path)?;

    Ok(PiUninstallResult {
        extension_path,
        removed_extension,
    })
}

pub(crate) fn uninstall_omp() -> io::Result<OmpUninstallResult> {
    let extension_path = omp_extension_dir()?.join(OMP_EXTENSION_INSTALL_NAME);
    let removed_extension = remove_file_if_exists(&extension_path)?;

    Ok(OmpUninstallResult {
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
                "SessionStart",
                &format!("bash {quoted_hook_path} idle"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "SessionStart",
                &format!("bash {quoted_hook_path} session"),
            )?;
            updated_settings |= remove_command_hook(
                hooks,
                "UserPromptSubmit",
                &format!("bash {quoted_hook_path} prompt"),
            )?;
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
                "SessionStart",
                &format!("bash {quoted_hook_path} session"),
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
            updated_hooks |= remove_command_hook(
                hooks,
                "PermissionRequest",
                &format!("bash {quoted_hook_path} blocked"),
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

pub(crate) fn uninstall_kimi() -> io::Result<KimiUninstallResult> {
    let kimi_dir = kimi_dir()?;
    let hook_path = kimi_dir.join("hooks").join(KIMI_HOOK_INSTALL_NAME);
    let config_path = kimi_dir.join("config.toml");
    let mut updated_config = false;

    if config_path.is_file() {
        let existing_config = fs::read_to_string(&config_path)?;
        let new_config = remove_kimi_config_block(&existing_config);
        if new_config != existing_config {
            fs::write(&config_path, new_config)?;
            updated_config = true;
        }
    }

    let removed_hook_file = remove_file_if_exists(&hook_path)?;

    Ok(KimiUninstallResult {
        hook_path,
        config_path,
        removed_hook_file,
        updated_config,
    })
}

pub(crate) fn uninstall_copilot() -> io::Result<CopilotUninstallResult> {
    let copilot_dir = copilot_dir()?;
    let hook_path = copilot_dir.join("hooks").join(COPILOT_HOOK_INSTALL_NAME);
    let settings_path = copilot_dir.join("settings.json");
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
            "copilot settings",
            "copilot settings hooks",
        )? {
            let command = format!(
                "bash {}",
                shell_single_quote(&hook_path.display().to_string())
            );
            updated_settings |= remove_direct_command_hook(hooks, "SessionStart", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "UserPromptSubmit", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "PreToolUse", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "PostToolUse", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "PostToolUseFailure", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "Stop", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "agentStop", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "SessionEnd", &command)?;
            updated_settings |= remove_direct_command_hook(hooks, "notification", &command)?;
        }

        if updated_settings {
            fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        }
    }

    let removed_hook_file = remove_file_if_exists(&hook_path)?;

    Ok(CopilotUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
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

pub(crate) fn uninstall_hermes() -> io::Result<HermesUninstallResult> {
    let dir = hermes_dir()?;
    let plugin_dir = hermes_plugin_dir()?;
    let config_path = dir.join("config.yaml");

    let removed_plugin_dir = remove_dir_all_if_exists(&plugin_dir)?;
    let mut updated_config = false;
    if config_path.is_file() {
        let existing_config = fs::read_to_string(&config_path)?;
        let new_config = remove_hermes_plugin_enabled(&existing_config);
        if new_config != existing_config {
            fs::write(&config_path, new_config)?;
            updated_config = true;
        }
    }

    Ok(HermesUninstallResult {
        plugin_dir,
        config_path,
        removed_plugin_dir,
        updated_config,
    })
}

pub(crate) fn install_qodercli() -> io::Result<QodercliInstallPaths> {
    let dir = qodercli_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "qodercli config directory not found at {}. install qodercli first",
            dir.display()
        )));
    }

    let hooks_dir = dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(QODERCLI_HOOK_INSTALL_NAME);
    fs::write(&hook_path, QODERCLI_HOOK_ASSET)?;
    make_executable(&hook_path)?;

    // Register the hook in ~/.qoder/settings.json. The schema mirrors claude
    // settings.json (per https://docs.qoder.com/zh/cli/hooks): a top-level
    // `hooks` object keyed by event name, each entry holding a matcher + a
    // list of `{type: "command", command, timeout?}` invocations. The hook
    // script reads the event payload from stdin via `hook_event_name` so the
    // installation never depends on a `QODER_HOOK_EVENT` environment
    // variable.
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
        "qodercli settings",
        "qodercli settings hooks",
    )?;
    let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());

    // SubagentStop is intentionally *not* mapped to working: the hook script
    // returns early on it (mirroring assets/claude/herdr-agent-state.sh) so
    // that recap/away-summary frames cannot revive an idle pane.
    ensure_command_hook(
        hooks,
        "SessionStart",
        format!("bash {quoted_hook_path} idle"),
        10,
        Some("*"),
    )?;
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

    Ok(QodercliInstallPaths {
        hook_path,
        settings_path,
    })
}

pub(crate) fn uninstall_qodercli() -> io::Result<QodercliUninstallResult> {
    let hook_path = qodercli_dir()?
        .join("hooks")
        .join(QODERCLI_HOOK_INSTALL_NAME);
    let settings_path = qodercli_dir()?.join("settings.json");
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
            "qodercli settings",
            "qodercli settings hooks",
        )? {
            let quoted_hook_path = shell_single_quote(&hook_path.display().to_string());
            updated_settings |= remove_command_hook(
                hooks,
                "SessionStart",
                &format!("bash {quoted_hook_path} idle"),
            )?;
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

    Ok(QodercliUninstallResult {
        hook_path,
        settings_path,
        removed_hook_file,
        updated_settings,
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

// Claude and Codex use nested hook groups:
//   { "matcher": "...", "hooks": [{ "type": "command", ... }] }
// Copilot and Qoder CLI use the flatter settings shape:
//   { "type": "command", "matcher": "...", "command": "...", ... }
// Keep the helpers separate so install/uninstall preserves unrelated hooks in
// each agent's native format instead of normalizing user configuration.
fn ensure_direct_command_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    command: String,
    timeout_sec: u64,
    matcher: Option<&str>,
) -> io::Result<()> {
    let entries = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other(format!("hook entries for {event} must be an array")))?;

    if let Some(entry) = entries.iter_mut().find(|entry| {
        entry.get("type").and_then(Value::as_str) == Some("command")
            && entry.get("command").and_then(Value::as_str) == Some(command.as_str())
    }) {
        let Some(entry_object) = entry.as_object_mut() else {
            return Ok(());
        };
        entry_object.insert("timeoutSec".to_string(), Value::Number(timeout_sec.into()));
        match matcher {
            Some(matcher) => {
                entry_object.insert("matcher".to_string(), Value::String(matcher.to_string()));
            }
            None => {
                entry_object.remove("matcher");
            }
        }
        return Ok(());
    }

    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("command".to_string()));
    if let Some(matcher) = matcher {
        entry.insert("matcher".to_string(), Value::String(matcher.to_string()));
    }
    entry.insert("command".to_string(), Value::String(command));
    entry.insert("timeoutSec".to_string(), Value::Number(timeout_sec.into()));
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

fn remove_direct_command_hook(
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

    let before = entries.len();
    entries.retain(|entry| {
        !(entry.get("type").and_then(Value::as_str) == Some("command")
            && entry.get("command").and_then(Value::as_str) == Some(command))
    });
    let removed = entries.len() != before;
    if entries.is_empty() {
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

fn remove_dir_all_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn ensure_hermes_plugin_enabled(content: &str) -> String {
    update_hermes_enabled_plugin(content, true)
}

fn remove_hermes_plugin_enabled(content: &str) -> String {
    update_hermes_enabled_plugin(content, false)
}

fn update_hermes_enabled_plugin(content: &str, enabled: bool) -> String {
    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let Some(plugins_index) = top_level_yaml_key_index(&lines, "plugins") else {
        if !enabled {
            return content.to_string();
        }
        let mut result = content.trim_end_matches('\n').to_string();
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("plugins:\n  enabled:\n    - herdr-agent-state\n");
        return result;
    };

    let plugins_end =
        next_top_level_yaml_key_index(&lines, plugins_index + 1).unwrap_or(lines.len());
    let enabled_index = lines[plugins_index + 1..plugins_end]
        .iter()
        .position(|line| yaml_key_at_indent(line, 2) == Some("enabled"))
        .map(|offset| plugins_index + 1 + offset);

    if let Some(enabled_index) = enabled_index {
        let line = lines[enabled_index].trim();
        if line == "enabled: []" || line == "enabled: [] # herdr" {
            if enabled {
                lines[enabled_index] = "  enabled:".to_string();
                lines.insert(enabled_index + 1, "    - herdr-agent-state".to_string());
            }
            return join_yaml_lines(lines, trailing_newline);
        }

        let list_start = enabled_index + 1;
        let list_end = lines[list_start..plugins_end]
            .iter()
            .position(|line| {
                yaml_indent(line).is_some_and(|indent| indent <= 2) && yaml_key_name(line).is_some()
            })
            .map(|offset| list_start + offset)
            .unwrap_or(plugins_end);
        let existing_item_index = lines[list_start..list_end]
            .iter()
            .position(|line| yaml_list_item_value(line) == Some(HERMES_PLUGIN_INSTALL_NAME))
            .map(|offset| list_start + offset);

        match (enabled, existing_item_index) {
            (true, Some(_)) | (false, None) => return content.to_string(),
            (true, None) => lines.insert(list_start, "    - herdr-agent-state".to_string()),
            (false, Some(index)) => {
                lines.remove(index);
            }
        }
        return join_yaml_lines(lines, trailing_newline);
    }

    if enabled {
        lines.insert(plugins_index + 1, "  enabled:".to_string());
        lines.insert(plugins_index + 2, "    - herdr-agent-state".to_string());
        return join_yaml_lines(lines, trailing_newline);
    }

    content.to_string()
}

fn top_level_yaml_key_index(lines: &[String], key: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| yaml_key_at_indent(line, 0) == Some(key))
}

fn next_top_level_yaml_key_index(lines: &[String], start: usize) -> Option<usize> {
    lines[start..]
        .iter()
        .position(|line| yaml_indent(line) == Some(0) && yaml_key_name(line).is_some())
        .map(|offset| start + offset)
}

fn yaml_key_at_indent(line: &str, indent: usize) -> Option<&str> {
    if yaml_indent(line)? != indent {
        return None;
    }
    yaml_key_name(line)
}

fn yaml_key_name(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
        return None;
    }
    let (key, _) = trimmed.split_once(':')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

fn yaml_indent(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    Some(line.len() - trimmed.len())
}

fn yaml_list_item_value(line: &str) -> Option<&str> {
    line.trim().strip_prefix("- ").map(str::trim)
}

fn join_yaml_lines(lines: Vec<String>, trailing_newline: bool) -> String {
    let mut result = lines.join("\n");
    if trailing_newline || result.is_empty() {
        result.push('\n');
    }
    result
}

fn build_codex_config_with_hooks(content: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let trailing_newline = content.ends_with('\n');
    let mut in_top_level_features = false;
    let mut features_header_index = None;
    let mut hooks_index = None;
    let mut deprecated_hooks_indexes = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if let Some(header) = toml_table_header(line) {
            in_top_level_features = header == "[features]";
            if in_top_level_features && features_header_index.is_none() {
                features_header_index = Some(index);
            }
            continue;
        }

        if !in_top_level_features {
            continue;
        }

        if is_toml_key(line, "codex_hooks") {
            deprecated_hooks_indexes.push(index);
        } else if is_toml_key(line, "hooks") {
            hooks_index = Some(index);
        }
    }

    if let Some(index) = hooks_index {
        lines[index] = "hooks = true".to_string();
    }

    for index in deprecated_hooks_indexes.into_iter().rev() {
        lines.remove(index);
    }

    if hooks_index.is_none() {
        if let Some(index) = features_header_index {
            lines.insert(index + 1, "hooks = true".to_string());
            return join_toml_lines(lines, trailing_newline);
        }

        let mut result = content.trim_end_matches('\n').to_string();
        if !result.is_empty() {
            result.push('\n');
            result.push('\n');
        }
        result.push_str("[features]\nhooks = true\n");
        return result;
    }

    join_toml_lines(lines, trailing_newline)
}

fn build_kimi_config_with_hooks(content: &str, hook_path: &Path) -> String {
    let mut result = remove_kimi_config_block(content)
        .trim_end_matches('\n')
        .to_string();
    if !result.is_empty() {
        result.push('\n');
        result.push('\n');
    }

    result.push_str(KIMI_CONFIG_BLOCK_BEGIN);
    result.push('\n');
    for (event, action) in KIMI_HOOK_EVENTS {
        result.push_str(&kimi_hook_table(event, hook_path, action));
    }
    result.push_str(KIMI_CONFIG_BLOCK_END);
    result.push('\n');
    result
}

fn kimi_hook_table(event: &str, hook_path: &Path, action: &str) -> String {
    let command = format!(
        "bash {} {action}",
        shell_single_quote(&hook_path.display().to_string())
    );
    format!(
        "[[hooks]]\nevent = {}\ncommand = {}\ntimeout = 10\n\n",
        toml_basic_string(event),
        toml_basic_string(&command)
    )
}

fn remove_kimi_config_block(content: &str) -> String {
    let trailing_newline = content.ends_with('\n');
    let mut lines = Vec::new();
    let mut in_block = false;
    let mut removed_block = false;

    for line in content.lines() {
        if line.trim() == KIMI_CONFIG_BLOCK_BEGIN {
            in_block = true;
            removed_block = true;
            continue;
        }
        if in_block {
            if line.trim() == KIMI_CONFIG_BLOCK_END {
                in_block = false;
            }
            continue;
        }
        lines.push(line.to_string());
    }

    if !removed_block {
        return content.to_string();
    }

    let mut result = join_toml_lines(lines, trailing_newline);
    while result.ends_with("\n\n") {
        result.pop();
    }
    if result == "\n" {
        String::new()
    } else {
        result
    }
}

fn toml_basic_string(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 2);
    result.push('"');
    for ch in value.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{08}' => result.push_str("\\b"),
            '\t' => result.push_str("\\t"),
            '\n' => result.push_str("\\n"),
            '\u{0c}' => result.push_str("\\f"),
            '\r' => result.push_str("\\r"),
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => {
                result.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => result.push(ch),
        }
    }
    result.push('"');
    result
}

fn join_toml_lines(lines: Vec<String>, trailing_newline: bool) -> String {
    let mut result = lines.join("\n");
    if trailing_newline || result.is_empty() {
        result.push('\n');
    }
    result
}

fn toml_table_header(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || !trimmed.starts_with('[') {
        return None;
    }

    let header_end = if trimmed.starts_with("[[") {
        trimmed.find("]]").map(|index| index + 2)?
    } else {
        trimmed.find(']').map(|index| index + 1)?
    };
    let header = &trimmed[..header_end];
    let rest = trimmed[header_end..].trim_start();
    if !rest.is_empty() && !rest.starts_with('#') {
        return None;
    }

    Some(header)
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
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".pi", "agent"])?
            .join("extensions"),
    )
}

fn omp_extension_dir() -> io::Result<PathBuf> {
    Ok(
        config_dir_from_env_or_home(PI_CODING_AGENT_DIR_ENV_VAR, &[".omp", "agent"])?
            .join("extensions"),
    )
}

fn claude_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CLAUDE_CONFIG_DIR_ENV_VAR, &[".claude"])
}

fn codex_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(CODEX_HOME_ENV_VAR, &[".codex"])
}

fn kimi_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(KIMI_CODE_HOME_ENV_VAR, &[".kimi-code"])
}

fn copilot_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(COPILOT_HOME_ENV_VAR, &[".copilot"])
}

fn config_dir_from_env_or_home(
    env_var: &str,
    home_relative_segments: &[&str],
) -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(env_var).filter(|value| !value.is_empty()) {
        return expand_tilde_path(PathBuf::from(value));
    }

    let mut path = home_dir()?;
    for segment in home_relative_segments {
        path.push(segment);
    }
    Ok(path)
}

fn expand_tilde_path(path: PathBuf) -> io::Result<PathBuf> {
    let Some(raw) = path.to_str() else {
        return Ok(path);
    };

    if raw == "~" {
        return home_dir();
    }

    if let Some(rest) = raw
        .strip_prefix("~/")
        .or_else(|| raw.strip_prefix("~\\"))
        .or_else(|| raw.strip_prefix('~'))
    {
        return Ok(home_dir()?.join(rest));
    }

    Ok(path)
}

fn opencode_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".config/opencode"))
}

fn hermes_dir() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".hermes"))
}

fn hermes_plugin_dir() -> io::Result<PathBuf> {
    Ok(hermes_dir()?
        .join("plugins")
        .join(HERMES_PLUGIN_INSTALL_NAME))
}

fn qodercli_dir() -> io::Result<PathBuf> {
    config_dir_from_env_or_home(QODERCLI_CONFIG_DIR_ENV_VAR, &[".qoder"])
}

fn home_dir() -> io::Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::other("HOME is not set; cannot locate home directory"))
}

#[cfg(test)]
pub(crate) fn integration_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_integration_path_env() {
        std::env::remove_var(PI_CODING_AGENT_DIR_ENV_VAR);
        std::env::remove_var(CLAUDE_CONFIG_DIR_ENV_VAR);
        std::env::remove_var(CODEX_HOME_ENV_VAR);
        std::env::remove_var(COPILOT_HOME_ENV_VAR);
        std::env::remove_var(KIMI_CODE_HOME_ENV_VAR);
        std::env::remove_var(QODERCLI_CONFIG_DIR_ENV_VAR);
    }

    fn kimi_hook_command(hook_path: &Path, action: &str) -> String {
        format!(
            "bash {} {action}",
            shell_single_quote(&hook_path.display().to_string())
        )
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
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with(" prompt")
        );
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
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with(" prompt")
        );
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
        fs::write(
            claude_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"PostToolUse":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep-post","timeout":10}}]}}],"PostToolUseFailure":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep-failure","timeout":10}}]}}],"SubagentStop":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep-subagent","timeout":10}}]}}],"SessionEnd":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' release","timeout":10}},{{"type":"command","command":"echo keep-session-end","timeout":10}}]}}]}}}}"#,
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
            ),
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
        assert_eq!(
            settings["hooks"]["UserPromptSubmit"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with(" prompt")
        );
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
        assert_eq!(claude.expected_version, 6);
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
        assert_eq!(claude.expected_version, 6);
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
        fs::write(
            claude_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}],"UserPromptSubmit":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"PermissionRequest":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' blocked","timeout":10}}]}}],"PostToolUse":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"PostToolUseFailure":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"SubagentStop":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"Stop":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}],"SessionEnd":[{{"matcher":"*","hooks":[{{"type":"command","command":"bash '{}' release","timeout":10}}]}}]}}}}"#,
                hook_path.display(),
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
        assert_eq!(codex.expected_version, 5);
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
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
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
        fs::write(
            codex_dir.join("hooks.json"),
            format!(
                r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}],"UserPromptSubmit":[{{"hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}},{{"type":"command","command":"echo keep","timeout":10}}]}}],"PreToolUse":[{{"hooks":[{{"type":"command","command":"bash '{}' working","timeout":10}}]}}],"PermissionRequest":[{{"hooks":[{{"type":"command","command":"bash '{}' blocked","timeout":10}}]}}],"Stop":[{{"hooks":[{{"type":"command","command":"bash '{}' idle","timeout":10}}]}}]}}}}"#,
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
                hook_path.display(),
            ),
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
            serde_json::from_str(&fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
                .unwrap();
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
        assert_eq!(hooks.len(), 11);
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
        fs::write(
            copilot_dir.join("settings.json"),
            r#"{"theme":"dark","hooks":{"PreToolUse":[{"type":"command","command":"echo keep","timeoutSec":10}]}}"#,
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
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["PreToolUse"][0]["command"], "echo keep");
        assert!(settings["hooks"]["PreToolUse"][1]["command"]
            .as_str()
            .unwrap()
            .contains(COPILOT_HOOK_INSTALL_NAME));
        assert!(settings["hooks"]["PostToolUse"][0].get("matcher").is_none());
        assert!(settings["hooks"]["PostToolUseFailure"][0]
            .get("matcher")
            .is_none());
        assert_eq!(
            settings["hooks"]["notification"][0]["matcher"],
            "permission_prompt|elicitation_dialog|agent_idle"
        );
        assert!(settings["hooks"]["Stop"][0]["command"]
            .as_str()
            .unwrap()
            .contains(COPILOT_HOOK_INSTALL_NAME));
        assert!(settings["hooks"]["agentStop"][0]["command"]
            .as_str()
            .unwrap()
            .contains(COPILOT_HOOK_INSTALL_NAME));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn copilot_v1_integration_status_is_current() {
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
        assert_eq!(copilot.expected_version, 1);
        assert_eq!(copilot.state, IntegrationStatusKind::Current);

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
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
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
            settings["hooks"]["notification"].as_array().unwrap().len(),
            1
        );

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
        fs::write(
            copilot_dir.join("settings.json"),
            format!(
                r#"{{"hooks":{{"PreToolUse":[{{"type":"command","command":"{}","timeoutSec":10}},{{"type":"command","command":"echo keep","timeoutSec":10}}],"PostToolUse":[{{"type":"command","command":"{}","timeoutSec":10}}],"notification":[{{"type":"command","matcher":"permission_prompt|elicitation_dialog|agent_idle","command":"{}","timeoutSec":10}}]}}}}"#,
                command,
                command,
                command,
            ),
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
        assert!(PI_EXTENSION_ASSET.contains("agent_session_path: currentAgentSessionPath"));
        assert!(PI_EXTENSION_ASSET.contains("agent_session_id: currentAgentSessionId"));
        assert!(PI_EXTENSION_ASSET.contains("publishState(true)"));
        assert!(CLAUDE_HOOK_ASSET.contains("agent_session_id"));
        assert!(CLAUDE_HOOK_ASSET.contains("pane.report_agent_session"));
        assert!(!CLAUDE_HOOK_ASSET.contains("\"state\": action"));
        assert!(!CLAUDE_HOOK_ASSET.contains("pane.release_agent"));
        assert!(CODEX_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE"));
        assert!(CODEX_HOOK_ASSET.contains("agent_session_id"));
        assert!(CODEX_HOOK_ASSET.contains("pane.report_agent_session"));
        assert!(!CODEX_HOOK_ASSET.contains("\"state\": action"));
        assert!(!CODEX_HOOK_ASSET.contains("pane.release_agent"));
        assert!(KIMI_HOOK_ASSET.contains("source = \"herdr:kimi\""));
        assert!(KIMI_HOOK_ASSET.contains("pane.report_agent"));
        assert!(KIMI_HOOK_ASSET.contains("pane.release_agent"));
        assert!(!KIMI_HOOK_ASSET.contains("agent_session_id"));
        assert!(COPILOT_HOOK_ASSET.contains("agent_session_id"));
        assert!(COPILOT_HOOK_ASSET.contains("notification_type"));
        assert!(COPILOT_HOOK_ASSET.contains("ask_user"));
        assert!(COPILOT_HOOK_ASSET.contains("exit_plan_mode"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("properties?.sessionID"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("agent_session_id: sessionID"));
        assert!(OPENCODE_PLUGIN_ASSET.contains("pane.report_agent_session"));
        assert!(!OPENCODE_PLUGIN_ASSET.contains("reportState"));
        assert!(!OPENCODE_PLUGIN_ASSET.contains("pane.release_agent"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("session_id = _session_id(kwargs)"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("agent_session_id"));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("pane.report_agent\","));
        assert!(HERMES_PLUGIN_INIT_ASSET.contains("pane.release_agent"));
        // Qoder hook reads the event from the stdin JSON payload (per
        // https://docs.qoder.com/zh/cli/hooks). Make sure the bundled script
        // never reaches for a QODER_HOOK_EVENT environment variable.
        assert!(QODERCLI_HOOK_ASSET.contains("HERDR_HOOK_INPUT_FILE"));
        assert!(QODERCLI_HOOK_ASSET.contains("hook_event_name"));
        assert!(QODERCLI_HOOK_ASSET.contains("agent_session_id"));
        assert!(!QODERCLI_HOOK_ASSET.contains("QODER_HOOK_EVENT"));
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
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
        ] {
            assert!(
                hooks.contains_key(event),
                "expected hooks.{event} to be registered"
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
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
        ] {
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
        settings["hooks"]["UserPromptSubmit"]
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
        let remaining = hooks
            .get("UserPromptSubmit")
            .and_then(Value::as_array)
            .unwrap();
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
}
