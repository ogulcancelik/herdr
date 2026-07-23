use std::io::Write;

use clap::{Arg, ArgAction, ArgGroup, Command, ValueHint};

pub(super) fn command() -> Command {
    let command = Command::new("herdr")
        .about("terminal workspace manager for AI coding agents")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(help_flag())
        .arg(flag("no-session").help("Run monolithically without server/client session mode"))
        .arg(option("session", "NAME").help("Use or create a named persistent session"))
        .arg(option("remote", "TARGET").help("Attach through SSH to a remote Herdr server"))
        .arg(
            option("remote-keybindings", "MODE")
                .value_parser(["local", "server"])
                .help("Choose local or server keybindings for remote attach"),
        )
        .arg(flag("handoff").help("Opt into live handoff for update or remote attach"))
        .arg(flag("default-config").help("Print default configuration and exit"))
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::SetTrue)
                .help("Print version and exit"),
        )
        .subcommand(completion_command())
        .subcommand(update_command())
        .subcommand(status_command())
        .subcommand(config_command())
        .subcommand(channel_command())
        .subcommand(server_command())
        .subcommand(api_command())
        .subcommand(workspace_command())
        .subcommand(worktree_command())
        .subcommand(tab_command())
        .subcommand(notification_command())
        .subcommand(agent_command())
        .subcommand(pane_command())
        .subcommand(terminal_command())
        .subcommand(session_command())
        .subcommand(integration_command())
        .subcommand(plugin_command());
    configure_help(command, true)
}

fn configure_help(command: Command, root: bool) -> Command {
    let command = if root {
        command
    } else {
        command.disable_help_flag(false)
    };
    command
        .disable_help_subcommand(true)
        .mut_subcommands(|subcommand| configure_help(subcommand, false))
}

pub(super) fn print_requested_help(args: &[String]) -> std::io::Result<bool> {
    let mut stdout = std::io::stdout().lock();
    write_requested_help(args, &mut stdout)
}

fn write_requested_help(args: &[String], output: &mut impl Write) -> std::io::Result<bool> {
    let Some(help_index) = args
        .iter()
        .position(|arg| matches!(arg.as_str(), "--help" | "-h"))
    else {
        return Ok(false);
    };
    if help_index < 2 {
        return Ok(false);
    }
    if args[1..help_index].iter().any(|arg| arg == "--") {
        return Ok(false);
    }

    let mut root = command();
    root.build();
    let mut selected = &mut root;
    let mut path = vec!["herdr".to_string()];
    for segment in &args[1..help_index] {
        if selected.find_subcommand(segment).is_none() {
            break;
        }
        path.push(segment.clone());
        selected = selected
            .find_subcommand_mut(segment)
            .expect("subcommand checked immediately before mutable lookup");
    }
    if path.len() == 1 || help_index != path.len() {
        return Ok(false);
    }

    selected.set_bin_name(path.join(" "));
    selected.write_long_help(&mut *output)?;
    writeln!(output)?;
    Ok(true)
}

fn completion_command() -> Command {
    Command::new("completion")
        .visible_alias("completions")
        .about("Generate shell completion scripts")
        .arg(
            Arg::new("shell")
                .value_name("SHELL")
                .required(true)
                .value_parser(super::completion::SUPPORTED_SHELLS)
                .help("Shell to generate completions for"),
        )
}

fn update_command() -> Command {
    Command::new("update")
        .about("Download and install the latest version")
        .arg(flag("handoff").help("Try live handoff after installing"))
}

fn status_command() -> Command {
    Command::new("status")
        .about("Show local client and running server status")
        .arg(json_flag())
        .subcommand(
            Command::new("server")
                .about("Show running server status")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("client")
                .about("Show local client status")
                .arg(json_flag()),
        )
}

fn config_command() -> Command {
    Command::new("config")
        .about("Manage local configuration")
        .subcommand(Command::new("check").about("Validate config.toml and print diagnostics"))
        .subcommand(Command::new("reset-keys").about("Reset custom keybindings"))
}

fn channel_command() -> Command {
    Command::new("channel")
        .about("Manage stable and preview update channels")
        .subcommand(Command::new("show").about("Print the configured update channel"))
        .subcommand(
            Command::new("set").about("Choose the update channel").arg(
                Arg::new("channel")
                    .value_name("CHANNEL")
                    .required(true)
                    .value_parser(["stable", "preview"]),
            ),
        )
}

fn server_command() -> Command {
    Command::new("server")
        .about("Run or control the headless server")
        .subcommand(Command::new("stop").about("Stop the running server"))
        .subcommand(Command::new("reload-config").about("Reload config in the running server"))
        .subcommand(
            Command::new("agent-manifests")
                .about("Show active agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("update-agent-manifests")
                .about("Fetch and reload agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reload-agent-manifests")
                .about("Reload local agent detection manifest overrides"),
        )
}

fn api_command() -> Command {
    Command::new("api")
        .about("Inspect socket API metadata and live runtime state")
        .subcommand(Command::new("snapshot").about("Print the live session snapshot"))
        .subcommand(
            Command::new("schema")
                .about("Print or write the bundled API schema")
                .arg(json_flag())
                .arg(path_option("output", "PATH")),
        )
}

fn workspace_command() -> Command {
    Command::new("workspace")
        .about("Manage workspaces over the socket API")
        .subcommand(Command::new("list").about("List workspaces"))
        .subcommand(
            Command::new("create")
                .about("Create a workspace")
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(env_option())
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "workspace_id", "Show a workspace"))
        .subcommand(id_command("focus", "workspace_id", "Focus a workspace"))
        .subcommand(
            Command::new("rename")
                .about("Rename a workspace")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(
            Command::new("report-metadata")
                .about("Report display-only workspace metadata")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(option("source", "ID").required(true))
                .arg(repeatable_option("token", "NAME=VALUE"))
                .arg(repeatable_option("clear-token", "NAME"))
                .arg(option("seq", "N"))
                .arg(option("ttl-ms", "N")),
        )
        .subcommand(id_command("close", "workspace_id", "Close a workspace"))
}

fn worktree_command() -> Command {
    Command::new("worktree")
        .about("Manage Git worktree-backed workspaces")
        .subcommand(
            Command::new("list")
                .about("List worktree workspaces")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("create")
                .about("Create and open a Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("base", "REF"))
                .arg(path_option("path", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("open")
                .about("Open an existing Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(path_option("path", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove a worktree checkout")
                .arg(option("workspace", "ID"))
                .arg(flag("force"))
                .arg(json_flag()),
        )
}

fn tab_command() -> Command {
    Command::new("tab")
        .about("Manage tabs over the socket API")
        .subcommand(
            Command::new("list")
                .about("List tabs")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(
            Command::new("create")
                .about("Create a tab")
                .arg(option("workspace", "WORKSPACE_ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(env_option())
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "tab_id", "Show a tab"))
        .subcommand(id_command("focus", "tab_id", "Focus a tab"))
        .subcommand(
            Command::new("rename")
                .about("Rename a tab")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(id_command("close", "tab_id", "Close a tab"))
}

fn notification_command() -> Command {
    Command::new("notification")
        .about("Show Herdr notifications")
        .subcommand(
            Command::new("show")
                .about("Show a notification")
                .arg(required("title", "TITLE"))
                .arg(option("body", "TEXT"))
                .arg(option("position", "POSITION").value_parser([
                    "top-left",
                    "top-right",
                    "bottom-left",
                    "bottom-right",
                ]))
                .arg(option("sound", "SOUND").value_parser(["none", "done", "request"])),
        )
}

fn agent_command() -> Command {
    Command::new("agent")
        .about("Control and inspect agent panes")
        .subcommand(Command::new("list").about("List agents"))
        .subcommand(id_command("get", "target", "Show an agent"))
        .subcommand(
            Command::new("read")
                .about("Read agent terminal output")
                .override_usage("herdr agent read <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi")),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to an agent")
                .arg(required("target", "TARGET"))
                .arg(required("key", "KEY").num_args(1..))
                .after_help("Use esc as the canonical Escape key name; escape is also accepted."),
        )
        .subcommand(
            Command::new("prompt")
                .about("Submit a prompt to an agent")
                .override_usage("herdr agent prompt <TARGET> <TEXT> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(required("text", "TEXT"))
                .arg(
                    flag("wait")
                        .help("Wait for the first matching state observed after submission"),
                )
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .requires("wait")
                        .value_parser(["idle", "working", "blocked", "done", "unknown"])
                        .help("State to match after --wait; repeat for more than one state"),
                )
                .arg(
                    option("timeout", "MS")
                        .requires("wait")
                        .help("Fail after this many milliseconds"),
                )
                .after_help(
                    "When submission starts from a non-working state, --wait first requires an observed state change within 5000ms; otherwise it returns agent_prompt_stalled. A shorter --timeout returns timeout instead. It then matches idle, done, or blocked by default, or any exact --until state. It does not track turns: if the agent is already working, that active turn's completion may match. Without --timeout, the settled-state wait is indefinite.",
                ),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename an agent")
                .override_usage("herdr agent rename <TARGET> <NAME>|--clear")
                .arg(required("target", "TARGET"))
                .arg(Arg::new("name").value_name("NAME"))
                .arg(flag("clear"))
                .group(
                    ArgGroup::new("rename")
                        .args(["name", "clear"])
                        .required(true),
                ),
        )
        .subcommand(id_command("focus", "target", "Focus an agent"))
        .subcommand(
            Command::new("wait")
                .about("Wait until an agent reaches one of the requested states")
                .override_usage("herdr agent wait <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .value_parser(["idle", "working", "blocked", "done", "unknown"])
                        .help("State to match; repeat for more than one state"),
                )
                .arg(option("timeout", "MS").help("Fail after this many milliseconds"))
                .after_help(
                    "Without --until, matches idle, done, or blocked. Use --until unknown explicitly when needed. Without --timeout, waits indefinitely.",
                ),
        )
        .subcommand(
            Command::new("attach")
                .about("Attach directly to an agent terminal")
                .override_usage("herdr agent attach <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("start")
                .about("Start a supported interactive agent in an existing pane")
                .override_usage(
                    "herdr agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS] [-- [AGENT_ARG]...]",
                )
                .arg(required("name", "NAME"))
                .arg(
                    option("kind", "KIND")
                        .required(true)
                        .value_parser(agent_kind_values())
                        .help("Supported agent kind and canonical executable"),
                )
                .arg(
                    option("pane", "ID")
                        .required(true)
                        .help("Existing pane at an interactive shell prompt"),
                )
                .arg(
                    option("timeout", "MS")
                        .help("Wait for interactive readiness (default: 30000; max: 300000)"),
                )
                .arg(
                    Arg::new("agent_args")
                        .value_name("AGENT_ARG")
                        .num_args(0..)
                        .last(true),
                )
                .after_help(
                    "The pane must be at its interactive shell prompt. Success means the expected agent was detected in the same terminal and is ready for input.",
                ),
        )
        .subcommand(
            Command::new("explain")
                .about("Explain agent detection state")
                .arg(Arg::new("target").value_name("TARGET"))
                .arg(path_option("file", "PATH"))
                .arg(option("agent", "LABEL"))
                .arg(json_flag())
                .arg(text_json_format_option())
                .arg(
                    Arg::new("verbose")
                        .short('v')
                        .long("verbose")
                        .action(ArgAction::SetTrue),
                ),
        )
}

pub(super) fn agent_kind_values() -> Vec<&'static str> {
    crate::detect::Agent::ALL
        .into_iter()
        .map(crate::detect::agent_label)
        .collect()
}

fn pane_command() -> Command {
    Command::new("pane")
        .about("Control terminal panes")
        .subcommand(
            Command::new("list")
                .about("List panes")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(
            Command::new("current")
                .about("Show the current pane")
                .args(current_pane_args()),
        )
        .subcommand(id_command("get", "pane_id", "Show a pane"))
        .subcommand(
            Command::new("layout")
                .about("Show pane layout information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("process-info")
                .about("Show pane process information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("neighbor")
                .about("Find a pane neighbor")
                .arg(required_direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("edges")
                .about("Show pane edge information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("focus")
                .about("Focus a neighboring pane")
                .arg(required_direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("resize")
                .about("Resize a pane split")
                .arg(required_direction_option())
                .arg(option("amount", "FLOAT"))
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("zoom")
                .about("Toggle or set pane zoom")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(flag("toggle"))
                .arg(flag("on"))
                .arg(flag("off")),
        )
        .subcommand(
            Command::new("read")
                .about("Read pane terminal output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi"))
                .arg(flag("raw")),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(Arg::new("label").value_name("LABEL").num_args(1..))
                .arg(flag("clear")),
        )
        .subcommand(
            Command::new("split")
                .about("Split a pane")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(split_direction_option())
                .arg(option("ratio", "FLOAT"))
                .arg(path_option("cwd", "PATH"))
                .arg(env_option())
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(
            Command::new("swap")
                .about("Swap panes")
                .arg(direction_option())
                .args(current_pane_args())
                .arg(option("source-pane", "ID"))
                .arg(option("target-pane", "ID")),
        )
        .subcommand(
            Command::new("move")
                .about("Move a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("tab", "TAB_ID"))
                .arg(option("split", "DIRECTION").value_parser(["right", "down"]))
                .arg(option("target-pane", "ID"))
                .arg(option("ratio", "FLOAT"))
                .arg(flag("new-tab"))
                .arg(option("workspace", "ID"))
                .arg(flag("new-workspace"))
                .arg(option("label", "TEXT"))
                .arg(option("tab-label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("close", "pane_id", "Close a pane"))
        .subcommand(
            Command::new("send-text")
                .about("Send literal text to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("text", "TEXT")),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("key", "KEY").num_args(1..))
                .after_help("Use esc as the canonical Escape key name; escape is also accepted."),
        )
        .subcommand(
            Command::new("wait-output")
                .about("Wait for matching pane output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(
                    option("match", "TEXT")
                        .conflicts_with("regex")
                        .required_unless_present("regex")
                        .help("Match a literal substring"),
                )
                .arg(
                    option("regex", "PATTERN")
                        .conflicts_with("match")
                        .required_unless_present("match")
                        .help("Match a Rust regular expression"),
                )
                .arg(read_source_option(false))
                .arg(option("lines", "N").help("Restrict the searched snapshot to N lines"))
                .arg(option("timeout", "MS").help("Fail after this many milliseconds"))
                .arg(flag("raw").help("Keep ANSI escape sequences while matching"))
                .group(
                    ArgGroup::new("matcher")
                        .args(["match", "regex"])
                        .required(true),
                )
                .after_help(
                    "The selected snapshot is searched immediately, including existing output, then polled. Without --timeout, this waits indefinitely.",
                ),
        )
        .subcommand(
            Command::new("run")
                .about("Run a command in a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("command", "COMMAND").num_args(1..)),
        )
        .subcommand(report_agent_command())
        .subcommand(report_agent_session_command())
        .subcommand(release_agent_command())
        .subcommand(report_metadata_command())
}

fn report_agent_command() -> Command {
    Command::new("report-agent")
        .about("Report pane agent lifecycle state")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(pane_agent_state_option("state"))
        .arg(option("message", "TEXT"))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
}

fn report_agent_session_command() -> Command {
    Command::new("report-agent-session")
        .about("Report pane agent session identity")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
        .arg(option("session-start-source", "SOURCE"))
}

fn release_agent_command() -> Command {
    Command::new("release-agent")
        .about("Release pane agent lifecycle authority")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(option("seq", "N"))
}

fn report_metadata_command() -> Command {
    Command::new("report-metadata")
        .about("Report display-only pane metadata")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL"))
        .arg(option("applies-to-source", "ID"))
        .arg(option("title", "TEXT"))
        .arg(flag("clear-title"))
        .arg(option("display-agent", "TEXT"))
        .arg(flag("clear-display-agent"))
        .arg(option("state-label", "STATUS=TEXT"))
        .arg(flag("clear-state-labels"))
        .arg(repeatable_option("token", "NAME=VALUE"))
        .arg(repeatable_option("clear-token", "NAME"))
        .arg(option("seq", "N"))
        .arg(option("ttl-ms", "N"))
}

fn terminal_command() -> Command {
    Command::new("terminal")
        .about("Attach to or observe raw terminal streams")
        .subcommand(
            Command::new("attach")
                .about("Attach directly to a terminal stream")
                .arg(required("terminal_id", "TERMINAL_ID"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("session")
                .about("Work with terminal sessions")
                .subcommand(
                    Command::new("control")
                        .about("Control a terminal stream")
                        .arg(required("target", "TARGET"))
                        .arg(flag("takeover"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                )
                .subcommand(
                    Command::new("observe")
                        .about("Observe a terminal stream")
                        .arg(required("target", "TARGET"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                ),
        )
        .subcommand(
            Command::new("title")
                .about("Manage the outer terminal title")
                .subcommand(
                    Command::new("set")
                        .about("Set the outer terminal title")
                        .arg(required("title", "TITLE")),
                )
                .subcommand(Command::new("clear").about("Clear the outer terminal title")),
        )
}

fn session_command() -> Command {
    Command::new("session")
        .about("Manage named persistent sessions")
        .subcommand(Command::new("list").about("List sessions").arg(json_flag()))
        .subcommand(
            Command::new("attach")
                .about("Attach to a session")
                .arg(required("name", "NAME")),
        )
        .subcommand(
            Command::new("stop")
                .about("Stop a session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("delete")
                .about("Delete a stopped session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
}

fn integration_command() -> Command {
    Command::new("integration")
        .about("Manage built-in agent integrations")
        .subcommand(
            Command::new("install")
                .about("Install an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("status")
                .about("Show integration status")
                .arg(flag("outdated-only")),
        )
}

fn plugin_command() -> Command {
    Command::new("plugin")
        .about("Install and run workflow plugins")
        .subcommand(
            Command::new("install")
                .about("Install a plugin from GitHub")
                .arg(required("source", "OWNER/REPO[/SUBDIR]"))
                .arg(option("ref", "REF"))
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall a plugin")
                .arg(required("plugin", "PLUGIN")),
        )
        .subcommand(
            Command::new("link")
                .about("Link a local plugin")
                .arg(path_arg("path", "PATH"))
                .arg(flag("disabled"))
                .arg(flag("enabled")),
        )
        .subcommand(
            Command::new("unlink")
                .about("Unlink a local plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("enable")
                .about("Enable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("disable")
                .about("Disable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("list")
                .about("List installed plugins")
                .arg(option("plugin", "ID"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("config-dir")
                .about("Print a plugin config directory")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("action")
                .about("List or invoke plugin actions")
                .subcommand(
                    Command::new("list")
                        .about("List plugin actions")
                        .arg(option("plugin", "ID")),
                )
                .subcommand(
                    Command::new("invoke")
                        .about("Invoke a plugin action")
                        .arg(required("action_id", "ACTION_ID"))
                        .arg(option("plugin", "ID")),
                ),
        )
        .subcommand(
            Command::new("log")
                .about("Inspect plugin command logs")
                .visible_alias("logs")
                .subcommand(
                    Command::new("list")
                        .about("List plugin command logs")
                        .arg(option("plugin", "ID"))
                        .arg(option("limit", "N")),
                ),
        )
        .subcommand(
            Command::new("pane")
                .about("Manage plugin-owned panes")
                .subcommand(
                    Command::new("open")
                        .about("Open a plugin pane")
                        .arg(option("plugin", "ID"))
                        .arg(option("entrypoint", "ID"))
                        .arg(
                            option("placement", "PLACEMENT")
                                .value_parser(["overlay", "split", "tab", "zoomed"]),
                        )
                        .arg(option("workspace", "ID"))
                        .arg(option("target-pane", "PANE"))
                        .arg(split_direction_option())
                        .arg(path_option("cwd", "PATH"))
                        .arg(env_option())
                        .arg(flag("focus"))
                        .arg(flag("no-focus")),
                )
                .subcommand(
                    Command::new("focus")
                        .about("Focus a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                )
                .subcommand(
                    Command::new("close")
                        .about("Close a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                ),
        )
}

fn current_pane_args() -> [Arg; 2] {
    [option("pane", "ID"), flag("current")]
}

fn integration_target_arg() -> Arg {
    Arg::new("target")
        .value_name("TARGET")
        .required(true)
        .value_parser(integration_target_values())
}

fn integration_target_values() -> Vec<&'static str> {
    crate::api::schema::IntegrationTarget::ALL
        .into_iter()
        .map(crate::integration::integration_target_label)
        .collect()
}

fn id_command(name: &'static str, id: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(required(id, id))
}

fn direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["left", "right", "up", "down"])
}

fn required_direction_option() -> Arg {
    direction_option().required(true)
}

fn split_direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["right", "down"])
}

fn pane_agent_state_option(name: &'static str) -> Arg {
    option(name, "STATUS")
        .required(true)
        .value_parser(["idle", "working", "blocked", "unknown"])
}

fn read_source_option(include_detection: bool) -> Arg {
    let values = if include_detection {
        vec!["visible", "recent", "recent-unwrapped", "detection"]
    } else {
        vec!["visible", "recent", "recent-unwrapped"]
    };
    option("source", "SOURCE")
        .value_parser(values)
        .help("Terminal snapshot source (default: recent)")
}

fn text_ansi_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "ansi"])
}

fn text_json_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "json"])
}

fn json_flag() -> Arg {
    flag("json")
}

fn help_flag() -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::SetTrue)
        .help("Show help")
}

fn env_option() -> Arg {
    option("env", "KEY=VALUE")
        .action(ArgAction::Append)
        .help("Set an environment variable for the launched process")
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn option(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name(value_name)
        .action(ArgAction::Set)
}

fn repeatable_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).action(ArgAction::Append)
}

fn path_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).value_hint(ValueHint::AnyPath)
}

fn required(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name).value_name(value_name).required(true)
}

fn path_arg(name: &'static str, value_name: &'static str) -> Arg {
    required(name, value_name).value_hint(ValueHint::AnyPath)
}

#[cfg(test)]
mod tests {
    use clap::{Arg, Command};

    fn command_path<'a>(cmd: &'a Command, path: &[&str]) -> &'a Command {
        let mut current = cmd;
        for name in path {
            current = current
                .get_subcommands()
                .find(|subcommand| subcommand.get_name() == *name)
                .unwrap_or_else(|| panic!("missing command path segment {name}"));
        }
        current
    }

    fn option_values(cmd: &Command, option: &str) -> Vec<String> {
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"));
        arg.get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .map(|value| value.get_name().to_string())
            .collect()
    }

    fn has_option(cmd: &Command, option: &str) -> bool {
        cmd.get_arguments()
            .any(|arg| arg.get_long() == Some(option))
    }

    fn option_arg<'a>(cmd: &'a Command, option: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"))
    }

    fn argument<'a>(cmd: &'a Command, id: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("missing argument {id}"))
    }

    fn collect_subcommand_paths(
        cmd: &Command,
        path: &mut Vec<String>,
        paths: &mut Vec<Vec<String>>,
    ) {
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            paths.push(path.clone());
            collect_subcommand_paths(subcommand, path, paths);
            path.pop();
        }
    }

    fn assert_command_descriptions(cmd: &Command, path: &mut Vec<String>) {
        if !path.is_empty() {
            assert!(
                cmd.get_about().is_some(),
                "missing completion description for {}",
                path.join(" ")
            );
        }
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            assert_command_descriptions(subcommand, path);
            path.pop();
        }
    }

    #[test]
    fn spec_describes_all_completion_commands() {
        let cmd = super::command();
        assert_command_descriptions(&cmd, &mut Vec::new());
    }

    #[test]
    fn spec_passes_clap_invariants() {
        super::command().debug_assert();
    }

    #[test]
    fn every_spec_subcommand_renders_short_and_long_help() {
        let mut paths = Vec::new();
        collect_subcommand_paths(&super::command(), &mut Vec::new(), &mut paths);

        for path in paths {
            for flag in ["-h", "--help"] {
                let mut args = vec!["herdr".to_string()];
                args.extend(path.iter().cloned());
                args.push(flag.to_string());
                let mut output = Vec::new();
                assert!(
                    super::write_requested_help(&args, &mut output).unwrap(),
                    "help was not handled for herdr {} {flag}",
                    path.join(" ")
                );
                let output = String::from_utf8(output).unwrap();
                assert!(
                    output.contains(&format!("Usage: herdr {}", path.join(" "))),
                    "unexpected help for herdr {}: {output}",
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn spec_includes_completion_alias_and_shells() {
        let cmd = super::command();
        let completion = command_path(&cmd, &["completion"]);
        assert!(completion
            .get_all_aliases()
            .any(|alias| alias == "completions"));
        let shells = completion
            .get_arguments()
            .find(|arg| arg.get_id() == "shell")
            .unwrap()
            .get_value_parser()
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(shells.contains(&"zsh".to_string()));
        assert!(shells.contains(&"fish".to_string()));
    }

    #[test]
    fn spec_matches_all_integration_targets() {
        let cmd = super::command();
        let install = command_path(&cmd, &["integration", "install"]);
        assert_eq!(
            argument(install, "target")
                .get_value_parser()
                .possible_values()
                .unwrap()
                .map(|value| value.get_name().to_string())
                .collect::<Vec<_>>(),
            crate::api::schema::IntegrationTarget::ALL
                .map(crate::integration::integration_target_label)
                .map(str::to_string)
        );
    }

    #[test]
    fn spec_marks_runtime_required_options_as_required() {
        for (path, options) in [
            (&["workspace", "report-metadata"][..], &["source"][..]),
            (&["pane", "neighbor"][..], &["direction"][..]),
            (&["pane", "focus"][..], &["direction"][..]),
            (&["pane", "resize"][..], &["direction"][..]),
            (&["pane", "report-agent"][..], &["source", "agent"][..]),
            (
                &["pane", "report-agent-session"][..],
                &["source", "agent"][..],
            ),
            (&["pane", "release-agent"][..], &["source", "agent"][..]),
            (&["pane", "report-metadata"][..], &["source"][..]),
        ] {
            let cmd = command_path(&super::command(), path).clone();
            for option in options {
                assert!(
                    option_arg(&cmd, option).is_required_set(),
                    "herdr {} --{option} should be required",
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn agent_prompt_until_requires_wait() {
        let error = super::command()
            .try_get_matches_from([
                "herdr", "agent", "prompt", "reviewer", "hello", "--until", "idle",
            ])
            .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn agent_rename_requires_exactly_one_name_or_clear() {
        for valid in [
            &["herdr", "agent", "rename", "reviewer", "worker"][..],
            &["herdr", "agent", "rename", "reviewer", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(valid).is_ok());
        }
        for invalid in [
            &["herdr", "agent", "rename", "reviewer"][..],
            &["herdr", "agent", "rename", "reviewer", "worker", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(invalid).is_err());
        }

        let mut help = Vec::new();
        super::write_requested_help(
            &[
                "herdr".to_string(),
                "agent".to_string(),
                "rename".to_string(),
                "--help".to_string(),
            ],
            &mut help,
        )
        .unwrap();
        assert!(String::from_utf8(help)
            .unwrap()
            .contains("Usage: herdr agent rename <TARGET> <NAME>|--clear"));
    }

    #[test]
    fn spec_includes_nested_plugin_pane_open_options() {
        let cmd = super::command();
        let open = command_path(&cmd, &["plugin", "pane", "open"]);
        assert!(open
            .get_arguments()
            .any(|arg| arg.get_long() == Some("entrypoint")));
        assert!(option_values(open, "placement").contains(&"zoomed".to_string()));
    }

    #[test]
    fn spec_keeps_agent_wait_status_free() {
        let cmd = super::command();
        let wait = command_path(&cmd, &["agent", "wait"]);
        assert!(!has_option(wait, "status"));
        assert_eq!(
            option_values(wait, "until"),
            ["idle", "working", "blocked", "done", "unknown"]
        );
        assert!(has_option(wait, "timeout"));
    }

    #[test]
    fn spec_matches_refactored_agent_and_pane_commands() {
        let cmd = super::command();
        assert!(cmd
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "wait"));

        let agent = command_path(&cmd, &["agent"]);
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "send-keys"));
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait"));
        assert!(agent
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "send"));

        let pane = command_path(&cmd, &["pane"]);
        assert!(pane
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait-output"));
    }

    #[test]
    fn spec_includes_pane_read_raw_flag() {
        let cmd = super::command();
        let pane_read = command_path(&cmd, &["pane", "read"]);
        assert!(has_option(pane_read, "raw"));
    }

    #[test]
    fn spec_matches_pane_split_direction_flag() {
        let cmd = super::command();
        let pane_split = command_path(&cmd, &["pane", "split"]);
        assert!(has_option(pane_split, "direction"));
        assert!(!has_option(pane_split, "split"));
        assert_eq!(option_values(pane_split, "direction"), ["right", "down"]);
    }

    #[test]
    fn spec_models_agent_start_target_and_trailing_args() {
        let cmd = super::command();
        let agent_start = command_path(&cmd, &["agent", "start"]);
        assert!(has_option(agent_start, "kind"));
        assert_eq!(
            option_values(agent_start, "kind"),
            crate::detect::Agent::ALL
                .map(crate::detect::agent_label)
                .map(str::to_string)
        );
        assert!(has_option(agent_start, "pane"));
        for legacy in ["cwd", "workspace", "tab", "split", "focus", "env", "argv"] {
            assert!(!has_option(agent_start, legacy), "legacy option --{legacy}");
        }
        assert!(agent_start
            .get_arguments()
            .any(|arg| arg.get_id() == "agent_args"));
    }

    #[test]
    fn completion_generation_succeeds_for_every_supported_shell() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Elvish,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Zsh,
        ] {
            let mut cmd = super::command();
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut cmd, "herdr", &mut output);
            assert!(!output.is_empty(), "empty {shell:?} completion output");
        }
    }
}
