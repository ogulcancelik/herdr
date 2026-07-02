use std::io;

use super::registry::{integration_target_label, integration_target_supported};
use super::targets::{
    install_claude, install_codex, install_copilot, install_cursor, install_devin, install_droid,
    install_hermes, install_kilo, install_kimi, install_mastracode, install_omp, install_opencode,
    install_pi, install_qodercli, uninstall_claude, uninstall_codex, uninstall_copilot,
    uninstall_cursor, uninstall_devin, uninstall_droid, uninstall_hermes, uninstall_kilo,
    uninstall_kimi, uninstall_mastracode, uninstall_omp, uninstall_opencode, uninstall_pi,
    uninstall_qodercli,
};
use super::version::{agent_version_requirement, enforce_agent_version};
use super::{KIMI_MIN_VERSION, PI_EXTENSION_INSTALL_NAME};

pub(crate) fn install_target(
    target: crate::api::schema::IntegrationTarget,
) -> io::Result<Vec<String>> {
    let result = install_target_inner(target);
    let outcome = if result.is_ok() { "ok" } else { "error" };
    crate::logging::integration_action("install", integration_target_label(target), outcome);
    result
}

fn install_target_inner(target: crate::api::schema::IntegrationTarget) -> io::Result<Vec<String>> {
    if !integration_target_supported(target) {
        return Err(io::Error::other(format!(
            "{} integration is not supported on Windows",
            integration_target_label(target)
        )));
    }

    let version_warning = match agent_version_requirement(target) {
        Some(requirement) => enforce_agent_version(&requirement)?,
        None => None,
    };

    let mut messages = match target {
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
        crate::api::schema::IntegrationTarget::Devin => {
            let installed = install_devin()?;
            vec![
                format!(
                    "installed devin integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured devin settings at {}",
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
        crate::api::schema::IntegrationTarget::Droid => {
            let installed = install_droid()?;
            let mut messages = vec![
                format!(
                    "installed droid integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured droid hooks at {}",
                    installed.settings_path.display()
                ),
            ];
            if installed.updated_legacy_hooks {
                messages.push(format!(
                    "removed legacy herdr droid hook entries from {}",
                    installed.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Opencode => {
            let installed = install_opencode()?;
            vec![format!(
                "installed opencode integration plugin to {}",
                installed.plugin_path.display()
            )]
        }
        crate::api::schema::IntegrationTarget::Kilo => {
            let installed = install_kilo()?;
            vec![format!(
                "installed kilo integration plugin to {}",
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
        crate::api::schema::IntegrationTarget::Cursor => {
            let installed = install_cursor()?;
            vec![
                format!(
                    "installed cursor integration hook to {}",
                    installed.hook_path.display()
                ),
                format!("updated cursor hooks at {}", installed.hooks_path.display()),
            ]
        }
        crate::api::schema::IntegrationTarget::Mastracode => {
            let installed = install_mastracode()?;
            vec![
                format!(
                    "installed mastracode integration hook to {}",
                    installed.hook_path.display()
                ),
                format!(
                    "ensured mastracode hooks at {}",
                    installed.hooks_path.display()
                ),
            ]
        }
    };

    if let Some(warning) = version_warning {
        messages.push(warning);
    }

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
        crate::api::schema::IntegrationTarget::Devin => {
            let result = uninstall_devin()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed devin hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no devin hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed herdr devin hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr devin hook entries found in {}",
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
        crate::api::schema::IntegrationTarget::Droid => {
            let result = uninstall_droid()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed droid hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no droid hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed legacy herdr droid hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no legacy herdr droid hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            if result.updated_settings {
                messages.push(format!(
                    "removed herdr droid hook entries from {}",
                    result.settings_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr droid hook entries found in {}",
                    result.settings_path.display()
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
        crate::api::schema::IntegrationTarget::Kilo => {
            let result = uninstall_kilo()?;
            if result.removed_plugin {
                vec![format!(
                    "removed kilo integration plugin at {}",
                    result.plugin_path.display()
                )]
            } else {
                vec![format!(
                    "no kilo integration plugin found at {}",
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
        crate::api::schema::IntegrationTarget::Cursor => {
            let result = uninstall_cursor()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed cursor hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no cursor hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed herdr cursor hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr cursor hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
        crate::api::schema::IntegrationTarget::Mastracode => {
            let result = uninstall_mastracode()?;
            let mut messages = Vec::new();
            if result.removed_hook_file {
                messages.push(format!(
                    "removed mastracode hook at {}",
                    result.hook_path.display()
                ));
            } else {
                messages.push(format!(
                    "no mastracode hook found at {}",
                    result.hook_path.display()
                ));
            }
            if result.updated_hooks {
                messages.push(format!(
                    "removed herdr mastracode hook entries from {}",
                    result.hooks_path.display()
                ));
            } else {
                messages.push(format!(
                    "no herdr mastracode hook entries found in {}",
                    result.hooks_path.display()
                ));
            }
            messages
        }
    };

    crate::logging::integration_action("uninstall", integration_target_label(target), "ok");
    Ok(messages)
}
