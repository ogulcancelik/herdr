use std::collections::HashMap;
use std::fmt;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::schema::{
    InstalledPluginInfo, Method, PluginActionInvokeParams, PluginActionListParams,
    PluginInvocationContext, PluginLinkParams, PluginListParams, PluginLogListParams,
    PluginPaneCloseParams, PluginPaneFocusParams, PluginPaneOpenParams, PluginPanePlacement,
    PluginPlatform, PluginSetEnabledParams, PluginSourceInfo, PluginSourceKind, PluginUnlinkParams,
    Request, ResponseResult, SplitDirection, SuccessResponse,
};

const PLUGIN_BUILD_OUTPUT_MAX_BYTES: usize = 64 * 1024;

pub(super) fn run_plugin_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_plugin_help();
        return Ok(2);
    };

    match subcommand {
        "install" => plugin_install(&args[1..]),
        "uninstall" => plugin_uninstall(&args[1..]),
        "link" => plugin_link(&args[1..]),
        "list" => plugin_list(&args[1..]),
        "config-dir" => plugin_config_dir_command(&args[1..]),
        "unlink" => plugin_unlink(&args[1..]),
        "enable" => plugin_set_enabled(&args[1..], true),
        "disable" => plugin_set_enabled(&args[1..], false),
        "action" => run_plugin_action_command(&args[1..]),
        "log" | "logs" => plugin_log_list(&args[1..]),
        "pane" => run_plugin_pane_command(&args[1..]),
        "help" | "--help" | "-h" => {
            print_plugin_help();
            Ok(0)
        }
        _ => {
            print_plugin_help();
            Ok(2)
        }
    }
}

fn plugin_link(args: &[String]) -> std::io::Result<i32> {
    let Some(path) = args.first() else {
        eprintln!("usage: herdr plugin link <path> [--disabled]");
        return Ok(2);
    };
    let path = normalize_plugin_path_arg(path)?;
    let mut enabled = true;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--disabled" => {
                enabled = false;
                index += 1;
            }
            "--enabled" => {
                enabled = true;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    print_plugin_response(Method::PluginLink(PluginLinkParams {
        path,
        enabled,
        source: None,
    }))
}

fn plugin_config_dir_command(args: &[String]) -> std::io::Result<i32> {
    let Some(plugin_id) = args.first() else {
        eprintln!("usage: herdr plugin config-dir <plugin_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr plugin config-dir <plugin_id>");
        return Ok(2);
    }
    let path = crate::plugin_paths::plugin_config_dir(plugin_id);
    crate::plugin_paths::ensure_plugin_user_dirs(plugin_id)?;
    println!("{}", path.display());
    Ok(0)
}

fn plugin_list(args: &[String]) -> std::io::Result<i32> {
    let mut plugin_id = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--plugin" => {
                let Some(value) = required_value(args, &mut index, "--plugin") else {
                    return Ok(2);
                };
                plugin_id = Some(value);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let params = PluginListParams { plugin_id };
    let response = match super::send_request(&Request {
        id: "cli:plugin".into(),
        method: Method::PluginList(params.clone()),
    }) {
        Ok(response) => response,
        Err(err) if is_connection_error(&err) => offline_plugin_list_response(&params)?,
        Err(err) => return Err(err),
    };
    if json {
        return super::print_response(&response);
    }
    print_plugin_list_human(&response)
}

fn plugin_unlink(args: &[String]) -> std::io::Result<i32> {
    let Some(plugin_id) = args.first() else {
        eprintln!("usage: herdr plugin unlink <plugin_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr plugin unlink <plugin_id>");
        return Ok(2);
    }
    print_plugin_response(Method::PluginUnlink(PluginUnlinkParams {
        plugin_id: plugin_id.clone(),
    }))
}

fn plugin_install(args: &[String]) -> std::io::Result<i32> {
    let Some(source_arg) = args.first() else {
        eprintln!("usage: herdr plugin install <owner>/<repo>[/subdir...] [--ref REF] [--yes]");
        return Ok(2);
    };
    let source = match GithubPluginSource::parse(source_arg) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("{err}");
            return Ok(2);
        }
    };
    let mut requested_ref = None;
    let mut yes = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--ref" => {
                let Some(value) = required_value(args, &mut index, "--ref") else {
                    return Ok(2);
                };
                requested_ref = Some(value);
            }
            "--yes" | "-y" => {
                yes = true;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    if !yes && !io::stdin().is_terminal() {
        eprintln!("remote plugin install requires --yes when stdin is not interactive");
        return Ok(2);
    }

    let temp_root = create_plugin_temp_dir("install")?;
    let checkout = temp_root.join("checkout");
    let install_result = (|| {
        git_checkout(&source, requested_ref.as_deref(), &checkout)?;
        let resolved_commit = git_output(&checkout, ["rev-parse", "HEAD"])?;
        let manifest_root = source.manifest_root(&checkout);
        let preview_plugin = load_cli_plugin_manifest(&manifest_root, true)?;
        let existing = installed_plugin_info(&preview_plugin.plugin_id)?;
        ensure_replacement_allowed(&preview_plugin, existing.as_ref())?;

        let mut source_info =
            source.to_source_info(requested_ref, resolved_commit, None, current_unix_ms());
        print_install_preview(&preview_plugin, &source_info, existing.as_ref());
        if !yes && !confirm("Install this plugin?")? {
            eprintln!("plugin install cancelled");
            return Ok(0);
        }
        if let Err(err) = run_plugin_build_commands(&preview_plugin, &manifest_root) {
            eprintln!("{err}");
            return Ok(1);
        }
        let post_build_plugin = load_cli_plugin_manifest(&manifest_root, true)?;
        ensure_manifest_unchanged_after_build(&preview_plugin, &post_build_plugin)?;

        let final_checkout = managed_checkout_path(&preview_plugin.plugin_id);
        let backup_checkout = temp_root.join("previous-checkout");
        let mut backup_moved = false;
        if final_checkout.exists() {
            std::fs::rename(&final_checkout, &backup_checkout)
                .map_err(|err| plugin_checkout_lifecycle_error("replace", &final_checkout, err))?;
            backup_moved = true;
        }
        let install_attempt = (|| {
            if let Some(parent) = final_checkout.parent() {
                std::fs::create_dir_all(parent).map_err(InstallFailure::Rollback)?;
            }
            std::fs::rename(&checkout, &final_checkout)
                .map_err(|err| plugin_checkout_lifecycle_error("install", &final_checkout, err))
                .map_err(InstallFailure::Rollback)?;

            source_info.managed_path = Some(final_checkout.display().to_string());
            let final_manifest_root = source.manifest_root(&final_checkout);
            let mut plugin = load_cli_plugin_manifest(&final_manifest_root, true)
                .map_err(InstallFailure::Rollback)?;
            plugin.source = source_info.clone();
            register_installed_plugin(plugin.clone(), source_info.clone())?;
            Ok::<InstalledPluginInfo, InstallFailure>(plugin)
        })();
        let plugin = match install_attempt {
            Ok(plugin) => plugin,
            Err(InstallFailure::Rollback(err)) => {
                let _ = std::fs::remove_dir_all(&final_checkout);
                if backup_moved && backup_checkout.exists() {
                    let _ = std::fs::rename(&backup_checkout, &final_checkout);
                }
                return Err(err);
            }
            Err(InstallFailure::KeepCheckout(err)) => return Err(err),
        };
        println!("Installed {} from {}.", plugin.plugin_id, source.display());
        println!(
            "Config: {}",
            crate::plugin_paths::plugin_config_dir(&plugin.plugin_id).display()
        );
        Ok(0)
    })();
    let _ = std::fs::remove_dir_all(&temp_root);
    install_result
}

fn plugin_uninstall(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr plugin uninstall <plugin_id|owner/repo[/subdir...]>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr plugin uninstall <plugin_id|owner/repo[/subdir...]>");
        return Ok(2);
    }

    let (plugin_id, existing) = match GithubPluginSource::parse(target) {
        Ok(source) => {
            let existing = installed_plugin_by_github_source(&source)?;
            let Some(existing) = existing else {
                eprintln!("plugin not installed: {}", source.display());
                return Ok(1);
            };
            (existing.plugin_id.clone(), Some(existing))
        }
        Err(_) => {
            let existing = match live_installed_plugin_info(target) {
                Ok(plugin) => plugin,
                Err(err) if is_connection_error(&err) => registry_plugin_info(target),
                Err(err) => return Err(err),
            };
            (target.clone(), existing)
        }
    };

    match super::send_request(&Request {
        id: "cli:plugin".into(),
        method: Method::PluginUnlink(PluginUnlinkParams {
            plugin_id: plugin_id.clone(),
        }),
    }) {
        Ok(response) => {
            if response.get("error").is_some() {
                return super::print_response(&response);
            }
            if response["result"]["removed"].as_bool() == Some(false) {
                eprintln!("plugin not installed: {target}");
                return Ok(1);
            }
        }
        Err(err) if is_connection_error(&err) => {
            let mut plugins = crate::persist::plugin_registry::load();
            let before = plugins.len();
            plugins.retain(|plugin| plugin.plugin_id != plugin_id);
            if before == plugins.len() {
                eprintln!("plugin not installed: {target}");
                return Ok(1);
            }
            crate::persist::plugin_registry::save(&plugins)?;
        }
        Err(err) => return Err(err),
    }

    if let Some(plugin) = existing.as_ref() {
        remove_managed_plugin_files(plugin)?;
    }
    println!("Uninstalled {plugin_id}.");
    Ok(0)
}

fn plugin_set_enabled(args: &[String], enabled: bool) -> std::io::Result<i32> {
    let Some(plugin_id) = args.first() else {
        eprintln!(
            "usage: herdr plugin {} <plugin_id>",
            if enabled { "enable" } else { "disable" }
        );
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!(
            "usage: herdr plugin {} <plugin_id>",
            if enabled { "enable" } else { "disable" }
        );
        return Ok(2);
    }
    let params = PluginSetEnabledParams {
        plugin_id: plugin_id.clone(),
    };
    if enabled {
        print_plugin_response(Method::PluginEnable(params))
    } else {
        print_plugin_response(Method::PluginDisable(params))
    }
}

fn plugin_log_list(args: &[String]) -> std::io::Result<i32> {
    let mut plugin_id = None;
    let mut limit = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "list" if index == 0 => index += 1,
            "--plugin" => {
                let Some(value) = required_value(args, &mut index, "--plugin") else {
                    return Ok(2);
                };
                plugin_id = Some(value);
            }
            "--limit" => {
                let Some(raw) = required_value(args, &mut index, "--limit") else {
                    return Ok(2);
                };
                let Ok(parsed) = raw.parse::<usize>() else {
                    eprintln!("invalid --limit value: {raw}");
                    return Ok(2);
                };
                limit = Some(parsed);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    print_plugin_response(Method::PluginLogList(PluginLogListParams {
        plugin_id,
        limit,
    }))
}

fn run_plugin_action_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_plugin_action_help();
        return Ok(2);
    };

    match subcommand {
        "list" => plugin_action_list(&args[1..]),
        "invoke" => plugin_action_invoke(&args[1..]),
        "help" | "--help" | "-h" => {
            print_plugin_action_help();
            Ok(0)
        }
        _ => {
            print_plugin_action_help();
            Ok(2)
        }
    }
}

fn plugin_action_list(args: &[String]) -> std::io::Result<i32> {
    let mut plugin_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin" => {
                let Some(value) = required_value(args, &mut index, "--plugin") else {
                    return Ok(2);
                };
                plugin_id = Some(value);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_plugin_response(Method::PluginActionList(PluginActionListParams {
        plugin_id,
    }))
}

fn plugin_action_invoke(args: &[String]) -> std::io::Result<i32> {
    let Some(action_id) = args.first() else {
        eprintln!("usage: herdr plugin action invoke <action_id> [--plugin ID]");
        return Ok(2);
    };
    let mut plugin_id = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin" => {
                let Some(value) = required_value(args, &mut index, "--plugin") else {
                    return Ok(2);
                };
                plugin_id = Some(value);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_plugin_response(Method::PluginActionInvoke(PluginActionInvokeParams {
        action_id: action_id.clone(),
        plugin_id,
        context: Some(PluginInvocationContext {
            workspace_id: None,
            workspace_label: None,
            workspace_cwd: None,
            worktree: None,
            tab_id: None,
            tab_label: None,
            focused_pane_id: None,
            focused_pane_cwd: None,
            focused_pane_agent: None,
            focused_pane_status: None,
            selected_text: None,
            invocation_source: Some("cli".into()),
            correlation_id: None,
            clicked_url: None,
            link_handler_id: None,
        }),
    }))
}

fn run_plugin_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_plugin_pane_help();
        return Ok(2);
    };

    match subcommand {
        "open" => plugin_pane_open(&args[1..]),
        "focus" => plugin_pane_focus(&args[1..]),
        "close" => plugin_pane_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_plugin_pane_help();
            Ok(0)
        }
        _ => {
            print_plugin_pane_help();
            Ok(2)
        }
    }
}

fn plugin_pane_open(args: &[String]) -> std::io::Result<i32> {
    let mut plugin_id = None;
    let mut entrypoint = None;
    let mut placement = None;
    let mut workspace_id = None;
    let mut target_pane_id = None;
    let mut direction = None;
    let mut cwd = None;
    let mut focus = true;
    let mut env = HashMap::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin" => {
                let Some(value) = required_value(args, &mut index, "--plugin") else {
                    return Ok(2);
                };
                plugin_id = Some(value);
            }
            "--entrypoint" => {
                let Some(value) = required_value(args, &mut index, "--entrypoint") else {
                    return Ok(2);
                };
                entrypoint = Some(value);
            }
            "--placement" => {
                let Some(value) = required_value(args, &mut index, "--placement") else {
                    return Ok(2);
                };
                let Some(parsed) = parse_pane_placement(&value) else {
                    return Ok(2);
                };
                placement = Some(parsed);
            }
            "--workspace" => {
                let Some(value) = required_value(args, &mut index, "--workspace") else {
                    return Ok(2);
                };
                workspace_id = Some(value);
            }
            "--target-pane" => {
                let Some(value) = required_value(args, &mut index, "--target-pane") else {
                    return Ok(2);
                };
                target_pane_id = Some(value);
            }
            "--direction" => {
                let Some(value) = required_value(args, &mut index, "--direction") else {
                    return Ok(2);
                };
                let Some(parsed) = parse_split_direction(&value) else {
                    return Ok(2);
                };
                direction = Some(parsed);
            }
            "--cwd" => {
                let Some(value) = required_value(args, &mut index, "--cwd") else {
                    return Ok(2);
                };
                cwd = Some(value);
            }
            "--env" => {
                let Some(value) = required_value(args, &mut index, "--env") else {
                    return Ok(2);
                };
                let (key, value) = match super::parse_env_assignment(&value) {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                env.insert(key, value);
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(plugin_id) = plugin_id else {
        eprintln!("missing required --plugin");
        return Ok(2);
    };
    let Some(entrypoint) = entrypoint else {
        eprintln!("missing required --entrypoint");
        return Ok(2);
    };

    print_plugin_response(Method::PluginPaneOpen(PluginPaneOpenParams {
        plugin_id,
        entrypoint,
        placement,
        workspace_id,
        target_pane_id,
        direction,
        cwd,
        focus,
        env,
    }))
}

fn plugin_pane_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(pane_id) = args.first() else {
        eprintln!("usage: herdr plugin pane focus <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr plugin pane focus <pane_id>");
        return Ok(2);
    }
    print_plugin_response(Method::PluginPaneFocus(PluginPaneFocusParams {
        pane_id: super::normalize_pane_id(pane_id),
    }))
}

fn plugin_pane_close(args: &[String]) -> std::io::Result<i32> {
    let Some(pane_id) = args.first() else {
        eprintln!("usage: herdr plugin pane close <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr plugin pane close <pane_id>");
        return Ok(2);
    }
    print_plugin_response(Method::PluginPaneClose(PluginPaneCloseParams {
        pane_id: super::normalize_pane_id(pane_id),
    }))
}

fn required_value(args: &[String], index: &mut usize, flag: &str) -> Option<String> {
    let Some(value) = args.get(*index + 1) else {
        eprintln!("missing value for {flag}");
        return None;
    };
    *index += 2;
    Some(value.clone())
}

fn parse_pane_placement(value: &str) -> Option<PluginPanePlacement> {
    match value {
        "overlay" => Some(PluginPanePlacement::Overlay),
        "split" => Some(PluginPanePlacement::Split),
        "tab" => Some(PluginPanePlacement::Tab),
        "zoomed" | "fullscreen" => Some(PluginPanePlacement::Zoomed),
        _ => {
            eprintln!("invalid pane placement: {value}");
            None
        }
    }
}

fn parse_split_direction(value: &str) -> Option<SplitDirection> {
    match value {
        "right" => Some(SplitDirection::Right),
        "down" => Some(SplitDirection::Down),
        _ => {
            eprintln!("invalid split direction: {value}");
            None
        }
    }
}

fn normalize_plugin_path_arg(value: &str) -> std::io::Result<String> {
    let path = crate::worktree::expand_tilde_path(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.display().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GithubPluginSource {
    owner: String,
    repo: String,
    subdir: Option<String>,
}

impl GithubPluginSource {
    fn parse(value: &str) -> Result<Self, String> {
        if value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("git@")
            || value.contains(':')
        {
            return Err("plugin install v1 accepts only owner/repo[/subdir] shorthand".into());
        }
        let parts = value.split('/').collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err("usage: herdr plugin install <owner>/<repo>[/subdir...]".into());
        }
        let owner = parts[0];
        let repo = parts[1];
        validate_github_segment("owner", owner)?;
        validate_github_segment("repo", repo)?;
        let subdir_parts = &parts[2..];
        for part in subdir_parts {
            validate_subdir_segment(part)?;
        }
        let subdir = if subdir_parts.is_empty() {
            None
        } else {
            Some(subdir_parts.join("/"))
        };
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            subdir,
        })
    }

    fn remote_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.repo)
    }

    fn display(&self) -> String {
        match &self.subdir {
            Some(subdir) => format!("{}/{}/{}", self.owner, self.repo, subdir),
            None => format!("{}/{}", self.owner, self.repo),
        }
    }

    fn manifest_root(&self, checkout: &Path) -> PathBuf {
        match &self.subdir {
            Some(subdir) => checkout.join(subdir),
            None => checkout.to_path_buf(),
        }
    }

    fn to_source_info(
        &self,
        requested_ref: Option<String>,
        resolved_commit: String,
        managed_path: Option<String>,
        installed_unix_ms: u64,
    ) -> PluginSourceInfo {
        PluginSourceInfo {
            kind: PluginSourceKind::Github,
            owner: Some(self.owner.clone()),
            repo: Some(self.repo.clone()),
            subdir: self.subdir.clone(),
            requested_ref,
            resolved_commit: Some(resolved_commit),
            managed_path,
            installed_unix_ms: Some(installed_unix_ms),
        }
    }
}

fn ensure_replacement_allowed(
    plugin: &InstalledPluginInfo,
    existing: Option<&InstalledPluginInfo>,
) -> std::io::Result<()> {
    let Some(existing) = existing else {
        return Ok(());
    };
    if existing.source.kind != PluginSourceKind::Github {
        return Err(std::io::Error::other(format!(
            "plugin {} is already linked from a local path; uninstall/unlink it before installing from GitHub",
            plugin.plugin_id
        )));
    }
    Ok(())
}

fn validate_github_segment(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("GitHub {label} must not be empty"));
    }
    if value == "." || value == ".." {
        return Err(format!("GitHub {label} is invalid: {value}"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(format!(
            "GitHub {label} contains invalid characters: {value}"
        ));
    }
    Ok(())
}

fn validate_subdir_segment(value: &str) -> Result<(), String> {
    if value.is_empty() || value == "." || value == ".." {
        return Err(format!("invalid plugin subdir segment: {value}"));
    }
    if value.contains('\\') || value.contains('\0') {
        return Err(format!("invalid plugin subdir segment: {value}"));
    }
    Ok(())
}

fn git_checkout(
    source: &GithubPluginSource,
    requested_ref: Option<&str>,
    checkout: &Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(checkout)?;
    run_git(Some(checkout), ["init"])?;
    run_git(
        Some(checkout),
        ["remote", "add", "origin", &source.remote_url()],
    )?;
    match requested_ref {
        Some(reference) => {
            run_git(
                Some(checkout),
                ["fetch", "--depth", "1", "origin", reference],
            )?;
        }
        None => {
            run_git(Some(checkout), ["fetch", "--depth", "1", "origin", "HEAD"])?;
        }
    }
    run_git(Some(checkout), ["checkout", "--detach", "FETCH_HEAD"])?;
    Ok(())
}

fn run_git<const N: usize>(cwd: Option<&Path>, args: [&str; N]) -> std::io::Result<()> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.stdin(Stdio::null());
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(command_failure_message(
        "git", &output,
    )))
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> std::io::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(std::io::Error::other(command_failure_message(
        "git", &output,
    )))
}

fn command_failure_message(program: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("{program} failed with status {}", output.status)
    } else {
        format!("{program} failed with status {}: {stderr}", output.status)
    }
}

fn load_cli_plugin_manifest(path: &Path, enabled: bool) -> std::io::Result<InstalledPluginInfo> {
    crate::app::load_plugin_manifest(&path.display().to_string(), enabled)
        .map_err(|(_, message)| std::io::Error::other(message))
}

fn register_installed_plugin(
    plugin: InstalledPluginInfo,
    source: PluginSourceInfo,
) -> Result<(), InstallFailure> {
    let request = Request {
        id: "cli:plugin".into(),
        method: Method::PluginLink(PluginLinkParams {
            path: plugin.manifest_path.clone(),
            enabled: plugin.enabled,
            source: Some(source.clone()),
        }),
    };
    match super::send_request(&request) {
        Ok(response) => {
            if response.get("error").is_some() {
                return Err(InstallFailure::Rollback(std::io::Error::other(
                    serde_json::to_string(&response).unwrap(),
                )));
            }
            if let Err(err) =
                verify_plugin_link_source_response(response, &plugin.plugin_id, &source)
            {
                let unlink = super::send_request(&Request {
                    id: "cli:plugin".into(),
                    method: Method::PluginUnlink(PluginUnlinkParams {
                        plugin_id: plugin.plugin_id.clone(),
                    }),
                });
                match unlink {
                    Ok(response) if response.get("error").is_none() => {
                        return Err(InstallFailure::Rollback(err));
                    }
                    Ok(response) => {
                        return Err(InstallFailure::KeepCheckout(std::io::Error::other(
                            format!(
                                "{err}; failed to undo incompatible plugin registration: {}",
                                serde_json::to_string(&response).unwrap()
                            ),
                        )));
                    }
                    Err(unlink_err) => {
                        return Err(InstallFailure::KeepCheckout(std::io::Error::other(
                            format!(
                                "{err}; failed to undo incompatible plugin registration: {unlink_err}"
                            ),
                        )));
                    }
                }
            }
            Ok(())
        }
        Err(err) if is_connection_error(&err) => {
            let mut plugins = crate::persist::plugin_registry::load();
            plugins.retain(|entry| entry.plugin_id != plugin.plugin_id);
            crate::plugin_paths::ensure_plugin_user_dirs(&plugin.plugin_id)
                .map_err(InstallFailure::Rollback)?;
            plugins.push(plugin);
            crate::persist::plugin_registry::save(&plugins).map_err(InstallFailure::Rollback)
        }
        Err(err) => Err(InstallFailure::Rollback(err)),
    }
}

#[derive(Debug)]
enum InstallFailure {
    Rollback(std::io::Error),
    KeepCheckout(std::io::Error),
}

fn verify_plugin_link_source_response(
    response: serde_json::Value,
    plugin_id: &str,
    expected: &PluginSourceInfo,
) -> std::io::Result<()> {
    let parsed: SuccessResponse =
        serde_json::from_value(response).map_err(std::io::Error::other)?;
    let ResponseResult::PluginLinked { plugin } = parsed.result else {
        return Err(std::io::Error::other("expected plugin_linked response"));
    };
    if plugin.plugin_id != plugin_id
        || plugin.source.kind != PluginSourceKind::Github
        || plugin.source.owner != expected.owner
        || plugin.source.repo != expected.repo
        || plugin.source.subdir != expected.subdir
        || plugin.source.requested_ref != expected.requested_ref
        || plugin.source.resolved_commit != expected.resolved_commit
        || plugin.source.managed_path != expected.managed_path
    {
        return Err(std::io::Error::other(
            "running Herdr server did not persist GitHub plugin source metadata",
        ));
    }
    Ok(())
}

fn installed_plugin_info(plugin_id: &str) -> std::io::Result<Option<InstalledPluginInfo>> {
    match live_installed_plugin_info(plugin_id) {
        Ok(plugin) => Ok(plugin),
        Err(err) if is_connection_error(&err) => Ok(registry_plugin_info(plugin_id)),
        Err(err) => Err(err),
    }
}

fn live_installed_plugin_info(plugin_id: &str) -> std::io::Result<Option<InstalledPluginInfo>> {
    let response = super::send_request(&Request {
        id: "cli:plugin".into(),
        method: Method::PluginList(PluginListParams {
            plugin_id: Some(plugin_id.to_string()),
        }),
    })?;
    if response.get("error").is_some() {
        return Err(std::io::Error::other(
            serde_json::to_string(&response).unwrap(),
        ));
    }
    plugin_info_from_list_response(response)
}

fn installed_plugin_by_github_source(
    source: &GithubPluginSource,
) -> std::io::Result<Option<InstalledPluginInfo>> {
    let plugins = match live_installed_plugins() {
        Ok(plugins) => plugins,
        Err(err) if is_connection_error(&err) => registry_plugins(),
        Err(err) => return Err(err),
    };
    Ok(plugin_by_github_source(plugins, source))
}

fn live_installed_plugins() -> std::io::Result<Vec<InstalledPluginInfo>> {
    let response = super::send_request(&Request {
        id: "cli:plugin".into(),
        method: Method::PluginList(PluginListParams { plugin_id: None }),
    })?;
    if response.get("error").is_some() {
        return Err(std::io::Error::other(
            serde_json::to_string(&response).unwrap(),
        ));
    }
    plugin_list_from_response(response)
}

fn registry_plugin_info(plugin_id: &str) -> Option<InstalledPluginInfo> {
    registry_plugins()
        .into_iter()
        .find(|plugin| plugin.plugin_id == plugin_id)
}

fn registry_plugins() -> Vec<InstalledPluginInfo> {
    crate::persist::plugin_registry::load()
}

fn plugin_info_from_list_response(
    response: serde_json::Value,
) -> std::io::Result<Option<InstalledPluginInfo>> {
    let mut plugins = plugin_list_from_response(response)?;
    Ok(plugins.pop())
}

fn plugin_list_from_response(
    response: serde_json::Value,
) -> std::io::Result<Vec<InstalledPluginInfo>> {
    let parsed: SuccessResponse =
        serde_json::from_value(response).map_err(std::io::Error::other)?;
    let ResponseResult::PluginList { mut plugins } = parsed.result else {
        return Err(std::io::Error::other("expected plugin_list response"));
    };
    plugins.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
    Ok(plugins)
}

fn plugin_by_github_source(
    plugins: impl IntoIterator<Item = InstalledPluginInfo>,
    source: &GithubPluginSource,
) -> Option<InstalledPluginInfo> {
    plugins
        .into_iter()
        .find(|plugin| plugin_matches_github_source(plugin, source))
}

fn plugin_matches_github_source(plugin: &InstalledPluginInfo, source: &GithubPluginSource) -> bool {
    plugin.source.kind == PluginSourceKind::Github
        && plugin.source.owner.as_deref() == Some(source.owner.as_str())
        && plugin.source.repo.as_deref() == Some(source.repo.as_str())
        && plugin.source.subdir.as_deref() == source.subdir.as_deref()
}

fn offline_plugin_list_response(params: &PluginListParams) -> std::io::Result<serde_json::Value> {
    let entries = crate::persist::plugin_registry::load();
    let mut plugins =
        crate::persist::plugin_registry::reload_manifests(entries, |path, enabled| {
            crate::app::load_plugin_manifest(path, enabled).map_err(|(_, msg)| msg)
        })
        .into_iter()
        .filter(|plugin| {
            params
                .plugin_id
                .as_deref()
                .is_none_or(|plugin_id| plugin.plugin_id == plugin_id)
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
    serde_json::to_value(SuccessResponse {
        id: "cli:plugin".into(),
        result: ResponseResult::PluginList { plugins },
    })
    .map_err(std::io::Error::other)
}

fn print_plugin_list_human(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        return super::print_response(response);
    }
    let parsed: SuccessResponse =
        serde_json::from_value(response.clone()).map_err(std::io::Error::other)?;
    let ResponseResult::PluginList { plugins } = parsed.result else {
        return super::print_response(response);
    };
    if plugins.is_empty() {
        println!("No plugins installed.");
        return Ok(0);
    }
    println!(
        "{} plugin{} installed:",
        plugins.len(),
        if plugins.len() == 1 { "" } else { "s" }
    );
    for plugin in plugins {
        let enabled = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let warning = if plugin.warnings.is_empty() {
            String::new()
        } else {
            format!("; {} warning(s)", plugin.warnings.len())
        };
        println!(
            "- {} ({}) {} [{}{}]",
            plugin.plugin_id,
            plugin.name,
            enabled,
            source_display(&plugin),
            warning
        );
        println!(
            "  config: {}",
            crate::plugin_paths::plugin_config_dir(&plugin.plugin_id).display()
        );
        for warning in plugin.warnings {
            println!("  warning: {warning}");
        }
    }
    Ok(0)
}

fn source_display(plugin: &InstalledPluginInfo) -> String {
    match plugin.source.kind {
        PluginSourceKind::Github => {
            let owner = plugin.source.owner.as_deref().unwrap_or("unknown");
            let repo = plugin.source.repo.as_deref().unwrap_or("unknown");
            let subdir = plugin
                .source
                .subdir
                .as_deref()
                .map(|subdir| format!("/{subdir}"))
                .unwrap_or_default();
            let reference = plugin
                .source
                .requested_ref
                .as_deref()
                .or(plugin.source.resolved_commit.as_deref())
                .unwrap_or("unknown");
            format!("github:{owner}/{repo}{subdir}@{reference}")
        }
        PluginSourceKind::Local => format!("local:{}", plugin.plugin_root),
    }
}

fn print_install_preview(
    plugin: &InstalledPluginInfo,
    source: &PluginSourceInfo,
    existing: Option<&InstalledPluginInfo>,
) {
    eprintln!("Plugin install preview:");
    eprintln!("  id: {}", plugin.plugin_id);
    eprintln!("  name: {}", plugin.name);
    eprintln!("  version: {}", plugin.version);
    if let (Some(owner), Some(repo)) = (&source.owner, &source.repo) {
        let subdir = source
            .subdir
            .as_deref()
            .map(|subdir| format!("/{subdir}"))
            .unwrap_or_default();
        eprintln!("  source: {owner}/{repo}{subdir}");
    }
    if let Some(reference) = &source.requested_ref {
        eprintln!("  ref: {reference}");
    }
    if let Some(commit) = &source.resolved_commit {
        eprintln!("  commit: {commit}");
    }
    eprintln!("  actions: {}", plugin.actions.len());
    eprintln!("  events: {}", plugin.events.len());
    eprintln!("  panes: {}", plugin.panes.len());
    eprintln!("  link handlers: {}", plugin.link_handlers.len());
    eprintln!("  build commands: {}", plugin.build.len());
    for build in &plugin.build {
        let support = if build_platform_supported(&build.platforms, &plugin.platforms) {
            String::new()
        } else {
            format!(
                " (skipped on {})",
                plugin_platform_name(current_plugin_platform())
            )
        };
        eprintln!("    build{}: {}", support, build.command.join(" "));
    }
    for action in &plugin.actions {
        eprintln!("    action {}: {}", action.id, action.command.join(" "));
    }
    for event in &plugin.events {
        eprintln!("    event {}: {}", event.on, event.command.join(" "));
    }
    for pane in &plugin.panes {
        eprintln!("    pane {}: {}", pane.id, pane.command.join(" "));
    }
    for warning in &plugin.warnings {
        eprintln!("  warning: {warning}");
    }
    if let Some(existing) = existing {
        eprintln!(
            "  replaces: {} from {}",
            existing.plugin_id,
            source_display(existing)
        );
    }
}

fn run_plugin_build_commands(
    plugin: &InstalledPluginInfo,
    manifest_root: &Path,
) -> Result<(), Box<PluginBuildFailure>> {
    let total = plugin.build.len();
    for (index, build) in plugin.build.iter().enumerate() {
        if !build_platform_supported(&build.platforms, &plugin.platforms) {
            continue;
        }
        run_plugin_build_command(
            &plugin.plugin_id,
            index + 1,
            total,
            manifest_root,
            &build.command,
        )?;
    }
    Ok(())
}

fn ensure_manifest_unchanged_after_build(
    before: &InstalledPluginInfo,
    after: &InstalledPluginInfo,
) -> io::Result<()> {
    if before == after {
        return Ok(());
    }
    Err(io::Error::other(
        "plugin build changed herdr-plugin.toml after install preview; aborting install",
    ))
}

fn run_plugin_build_command(
    plugin_id: &str,
    build_index: usize,
    build_total: usize,
    cwd: &Path,
    command: &[String],
) -> Result<(), Box<PluginBuildFailure>> {
    let context = plugin_build_context(plugin_id, build_index, build_total, cwd, command);
    let Some(program) = command.first() else {
        return Err(Box::new(PluginBuildFailure {
            context,
            kind: PluginBuildFailureKind::Start {
                error: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "build command must not be empty",
                ),
            },
        }));
    };
    let args = command.iter().skip(1).cloned().collect::<Vec<_>>();
    let mut child = crate::plugin_command::command_for_argv(program, &args);
    child
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_herdr_runtime_env(&mut child);

    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(err) => {
            return Err(Box::new(PluginBuildFailure {
                context,
                kind: PluginBuildFailureKind::Start { error: err },
            }));
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = stdout.map(|stdout| {
        std::thread::spawn(move || read_tail_capped_output(stdout, PLUGIN_BUILD_OUTPUT_MAX_BYTES))
    });
    let stderr_reader = stderr.map(|stderr| {
        std::thread::spawn(move || read_tail_capped_output(stderr, PLUGIN_BUILD_OUTPUT_MAX_BYTES))
    });
    let status = child.wait().map_err(|error| {
        Box::new(PluginBuildFailure {
            context: context.clone(),
            kind: PluginBuildFailureKind::Wait { error },
        })
    })?;
    let stdout = stdout_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(Box::new(PluginBuildFailure {
        context,
        kind: PluginBuildFailureKind::Exit {
            status,
            stdout,
            stderr,
        },
    }))
}

fn plugin_build_context(
    plugin_id: &str,
    build_index: usize,
    build_total: usize,
    cwd: &Path,
    command: &[String],
) -> PluginBuildContext {
    PluginBuildContext {
        plugin_id: plugin_id.to_string(),
        build_index,
        build_total,
        cwd: cwd.display().to_string(),
        command: command.to_vec(),
    }
}

#[derive(Debug, Default)]
struct CappedOutput {
    text: String,
    truncated: bool,
}

#[derive(Debug, Clone)]
struct PluginBuildContext {
    plugin_id: String,
    build_index: usize,
    build_total: usize,
    cwd: String,
    command: Vec<String>,
}

#[derive(Debug)]
struct PluginBuildFailure {
    context: PluginBuildContext,
    kind: PluginBuildFailureKind,
}

#[derive(Debug)]
enum PluginBuildFailureKind {
    Start {
        error: io::Error,
    },
    Wait {
        error: io::Error,
    },
    Exit {
        status: ExitStatus,
        stdout: CappedOutput,
        stderr: CappedOutput,
    },
}

impl fmt::Display for PluginBuildFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "error: plugin build failed")?;
        write_build_context(f, &self.context)?;
        match &self.kind {
            PluginBuildFailureKind::Start { error } => {
                writeln!(f, "  error: failed to start: {error}")?;
            }
            PluginBuildFailureKind::Wait { error } => {
                writeln!(f, "  error: failed to wait for command: {error}")?;
            }
            PluginBuildFailureKind::Exit {
                status,
                stdout,
                stderr,
            } => {
                writeln!(f, "  status: {status}")?;
                write_output_section(f, "stderr", stderr)?;
                write_output_section(f, "stdout", stdout)?;
            }
        }
        writeln!(f)?;
        write!(f, "Plugin was not installed.")
    }
}

fn write_build_context(f: &mut fmt::Formatter<'_>, context: &PluginBuildContext) -> fmt::Result {
    writeln!(f, "  plugin: {}", context.plugin_id)?;
    writeln!(
        f,
        "  build: {}/{}",
        context.build_index, context.build_total
    )?;
    writeln!(f, "  cwd: {}", context.cwd)?;
    writeln!(f, "  command: {}", context.command.join(" "))
}

fn write_output_section(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    output: &CappedOutput,
) -> fmt::Result {
    let text = output.text.trim_end();
    if text.is_empty() {
        return Ok(());
    }
    writeln!(f)?;
    if output.truncated {
        writeln!(
            f,
            "{label}: showing last {PLUGIN_BUILD_OUTPUT_MAX_BYTES} bytes; earlier output omitted"
        )?;
    } else {
        writeln!(f, "{label}:")?;
    }
    writeln!(f, "{text}")
}

fn read_tail_capped_output(mut reader: impl Read, cap: usize) -> CappedOutput {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > cap {
                    let excess = out.len() - cap;
                    out.drain(0..excess);
                    truncated = true;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    CappedOutput {
        text: String::from_utf8_lossy(&out).to_string(),
        truncated,
    }
}

fn scrub_herdr_runtime_env(command: &mut Command) {
    for key in [
        crate::api::SOCKET_PATH_ENV_VAR,
        crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
        crate::session::SESSION_ENV_VAR,
        "HERDR_BIN_PATH",
        "HERDR_ENV",
        "HERDR_WORKSPACE_ID",
        "HERDR_TAB_ID",
        "HERDR_PANE_ID",
    ] {
        command.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("HERDR_PLUGIN_") {
            command.env_remove(key);
        }
    }
}

fn build_platform_supported(
    platforms: &Option<Vec<PluginPlatform>>,
    plugin_platforms: &Option<Vec<PluginPlatform>>,
) -> bool {
    platforms
        .as_ref()
        .or(plugin_platforms.as_ref())
        .is_none_or(|platforms| platforms.contains(&current_plugin_platform()))
}

fn current_plugin_platform() -> PluginPlatform {
    if cfg!(target_os = "linux") {
        PluginPlatform::Linux
    } else if cfg!(target_os = "macos") {
        PluginPlatform::Macos
    } else {
        PluginPlatform::Windows
    }
}

fn plugin_platform_name(platform: PluginPlatform) -> &'static str {
    match platform {
        PluginPlatform::Linux => "linux",
        PluginPlatform::Macos => "macos",
        PluginPlatform::Windows => "windows",
    }
}

fn confirm(prompt: &str) -> std::io::Result<bool> {
    eprint!("{prompt} [y/N] ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

fn create_plugin_temp_dir(label: &str) -> std::io::Result<PathBuf> {
    let path = managed_plugins_dir().join(format!(
        ".tmp-{label}-{}-{}",
        std::process::id(),
        current_unix_ms()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn managed_plugins_dir() -> PathBuf {
    crate::session::data_dir().join("plugins")
}

fn managed_checkout_path(plugin_id: &str) -> PathBuf {
    managed_plugins_dir()
        .join("github")
        .join(crate::api::schema::plugin_managed_path_component(plugin_id))
}

fn remove_managed_plugin_files(plugin: &InstalledPluginInfo) -> std::io::Result<()> {
    if plugin.source.kind != PluginSourceKind::Github {
        return Ok(());
    }
    let Some(path) = plugin.source.managed_path.as_deref() else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        return Ok(());
    }
    if !is_expected_managed_path(plugin, &path) {
        return Err(std::io::Error::other(format!(
            "refusing to delete unmanaged plugin path: {}",
            path.display()
        )));
    }
    std::fs::remove_dir_all(&path)
        .map_err(|err| plugin_checkout_lifecycle_error("remove", &path, err))
}

fn plugin_checkout_lifecycle_error(operation: &str, path: &Path, err: io::Error) -> io::Error {
    if cfg!(windows) && err.kind() == io::ErrorKind::PermissionDenied {
        return io::Error::new(
            err.kind(),
            format!(
                "failed to {operation} managed plugin checkout at {}; close any Herdr plugin panes or plugin commands using that checkout, then retry: {err}",
                path.display()
            ),
        );
    }
    err
}

fn is_expected_managed_path(plugin: &InstalledPluginInfo, path: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    let expected = managed_checkout_path(&plugin.plugin_id);
    let Ok(expected) = expected.canonicalize() else {
        return false;
    };
    path == expected
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn is_connection_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

fn print_plugin_response(method: Method) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:plugin".into(),
        method,
    })?)
}

fn print_plugin_help() {
    eprintln!("herdr plugin commands:");
    eprintln!("  herdr plugin install <owner>/<repo>[/subdir...] [--ref REF] [--yes]");
    eprintln!("  herdr plugin uninstall <plugin_id|owner/repo[/subdir...]>");
    eprintln!("  herdr plugin link <path> [--disabled]");
    eprintln!("  herdr plugin list [--plugin ID] [--json]");
    eprintln!("  herdr plugin config-dir <plugin_id>");
    eprintln!("  herdr plugin unlink <plugin_id>");
    eprintln!("  herdr plugin enable <plugin_id>");
    eprintln!("  herdr plugin disable <plugin_id>");
    eprintln!("  herdr plugin action <list|invoke>");
    eprintln!("  herdr plugin log list [--plugin ID] [--limit N]");
    eprintln!("  herdr plugin pane <open|focus|close>");
}

fn print_plugin_action_help() {
    eprintln!("herdr plugin action commands:");
    eprintln!("  herdr plugin action list [--plugin ID]");
    eprintln!("  herdr plugin action invoke <action_id> [--plugin ID]");
}

fn print_plugin_pane_help() {
    eprintln!("herdr plugin pane commands:");
    eprintln!("  herdr plugin pane open --plugin ID --entrypoint ID [--placement overlay|split|tab|zoomed] [--workspace ID] [--target-pane PANE] [--direction right|down] [--cwd PATH] [--env KEY=VALUE] [--focus|--no-focus]");
    eprintln!("  herdr plugin pane focus <pane_id>");
    eprintln!("  herdr plugin pane close <pane_id>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_plugin_id(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        format!("test.{label}.{}.{nanos}", std::process::id())
    }

    fn github_plugin(
        id: &str,
        owner: &str,
        repo: &str,
        subdir: Option<&str>,
    ) -> InstalledPluginInfo {
        InstalledPluginInfo {
            plugin_id: id.to_string(),
            name: "Test Plugin".to_string(),
            version: "0.1.0".to_string(),
            min_herdr_version: crate::build_info::BASE_VERSION.to_string(),
            description: None,
            manifest_path: format!("/tmp/{id}/herdr-plugin.toml"),
            plugin_root: format!("/tmp/{id}"),
            enabled: true,
            platforms: None,
            build: vec![],
            actions: vec![],
            events: vec![],
            panes: vec![],
            link_handlers: vec![],
            source: PluginSourceInfo {
                kind: PluginSourceKind::Github,
                owner: Some(owner.to_string()),
                repo: Some(repo.to_string()),
                subdir: subdir.map(str::to_string),
                requested_ref: None,
                resolved_commit: Some("abc123".to_string()),
                managed_path: Some(format!("/tmp/herdr/plugins/{id}")),
                installed_unix_ms: Some(42),
            },
            warnings: vec![],
        }
    }

    #[test]
    fn github_plugin_source_parses_root_repo() {
        let source = GithubPluginSource::parse("ogulcancelik/herdr-plugin-examples").unwrap();
        assert_eq!(source.owner, "ogulcancelik");
        assert_eq!(source.repo, "herdr-plugin-examples");
        assert_eq!(source.subdir, None);
        assert_eq!(
            source.remote_url(),
            "https://github.com/ogulcancelik/herdr-plugin-examples.git"
        );
    }

    #[test]
    fn github_plugin_source_parses_subdir() {
        let source =
            GithubPluginSource::parse("ogulcancelik/herdr-plugin-examples/worktree-bootstrap")
                .unwrap();
        assert_eq!(source.owner, "ogulcancelik");
        assert_eq!(source.repo, "herdr-plugin-examples");
        assert_eq!(source.subdir.as_deref(), Some("worktree-bootstrap"));
    }

    #[test]
    fn github_plugin_source_rejects_non_shorthand_sources() {
        for source in [
            "https://github.com/ogulcancelik/herdr-plugin-examples",
            "git@github.com:ogulcancelik/herdr-plugin-examples.git",
            "ogulcancelik",
            "ogulcancelik/herdr-plugin-examples/../bad",
            "ogulcancelik/herdr-plugin-examples//bad",
        ] {
            assert!(
                GithubPluginSource::parse(source).is_err(),
                "{source} should be rejected"
            );
        }
    }

    #[test]
    fn github_source_lookup_matches_installed_plugin_source() {
        let source =
            GithubPluginSource::parse("ogulcancelik/herdr-plugin-examples/agent-telegram-notify")
                .unwrap();
        let plugins = vec![
            github_plugin(
                "examples.github-link-preview",
                "ogulcancelik",
                "herdr-plugin-examples",
                Some("github-link-preview"),
            ),
            github_plugin(
                "examples.agent-telegram-notify",
                "ogulcancelik",
                "herdr-plugin-examples",
                Some("agent-telegram-notify"),
            ),
        ];

        let plugin = plugin_by_github_source(plugins, &source).unwrap();
        assert_eq!(plugin.plugin_id, "examples.agent-telegram-notify");
    }

    #[test]
    fn github_source_lookup_requires_exact_subdir() {
        let source = GithubPluginSource::parse("ogulcancelik/herdr-plugin-examples").unwrap();
        let plugins = vec![github_plugin(
            "examples.agent-telegram-notify",
            "ogulcancelik",
            "herdr-plugin-examples",
            Some("agent-telegram-notify"),
        )];

        assert!(plugin_by_github_source(plugins, &source).is_none());
    }

    #[test]
    fn github_source_lookup_ignores_local_plugins() {
        let source = GithubPluginSource::parse("ogulcancelik/herdr-plugin-examples").unwrap();
        let mut plugin = github_plugin(
            "examples.local",
            "ogulcancelik",
            "herdr-plugin-examples",
            None,
        );
        plugin.source = PluginSourceInfo::default();

        assert!(plugin_by_github_source([plugin], &source).is_none());
    }

    #[test]
    fn cli_user_dir_creation_seeds_legacy_config_before_printing_config_dir() {
        let plugin_id = unique_plugin_id("legacy-config");
        let config_dir = crate::plugin_paths::plugin_config_dir(&plugin_id);
        let state_dir = crate::plugin_paths::plugin_state_dir(&plugin_id);
        let legacy_dir = crate::config::config_dir().join("plugins").join(&plugin_id);
        let _ = std::fs::remove_dir_all(&config_dir);
        let _ = std::fs::remove_dir_all(&state_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join(".env"), "TOKEN=legacy\n").unwrap();

        assert_eq!(
            plugin_config_dir_command(std::slice::from_ref(&plugin_id)).unwrap(),
            0
        );

        assert_eq!(
            std::fs::read_to_string(config_dir.join(".env")).unwrap(),
            "TOKEN=legacy\n"
        );

        let _ = std::fs::remove_dir_all(config_dir);
        let _ = std::fs::remove_dir_all(state_dir);
        let _ = std::fs::remove_dir_all(legacy_dir);
    }
}
