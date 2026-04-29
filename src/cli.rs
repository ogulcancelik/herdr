use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Serialize;

use crate::api;
use crate::api::schema::{
    AgentStatus, EmptyParams, IntegrationTarget, Method, OutputMatch, PaneListParams,
    PaneReadParams, PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams, PaneSplitParams,
    PaneTarget, PaneWaitForOutputParams, PingParams, ReadSource, Request, SplitDirection,
    Subscription, TabCreateParams, TabListParams, TabRenameParams, TabTarget,
    WorkspaceCreateParams, WorkspaceRenameParams, WorkspaceTarget,
};

pub enum CommandOutcome {
    Handled(i32),
    NotCli,
}

pub fn maybe_run(args: &[String]) -> std::io::Result<CommandOutcome> {
    let Some(command) = args.get(1).map(|arg| arg.as_str()) else {
        return Ok(CommandOutcome::NotCli);
    };

    let exit_code = match command {
        "server" => {
            let Some(exit_code) = run_server_command(&args[2..])? else {
                return Ok(CommandOutcome::NotCli);
            };
            exit_code
        }
        "status" => run_status_command(&args[2..])?,
        "workspace" => run_workspace_command(&args[2..])?,
        "tab" => run_tab_command(&args[2..])?,
        "pane" => run_pane_command(&args[2..])?,
        "wait" => run_wait_command(&args[2..])?,
        "integration" => run_integration_command(&args[2..])?,
        "session" => run_session_command(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    Ok(CommandOutcome::Handled(exit_code))
}

fn run_server_command(args: &[String]) -> std::io::Result<Option<i32>> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        return Ok(None);
    };

    match subcommand {
        "stop" => server_stop(&args[1..]).map(Some),
        "reload-config" => server_reload_config(&args[1..]).map(Some),
        "help" | "--help" | "-h" => {
            print_server_help();
            Ok(Some(0))
        }
        _ => {
            print_server_help();
            Ok(Some(2))
        }
    }
}

fn run_status_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        None => print_full_status(),
        Some("server") => {
            if args.len() > 1 {
                eprintln!("usage: herdr status server");
                return Ok(2);
            }
            print_server_status()
        }
        Some("client") => {
            if args.len() > 1 {
                eprintln!("usage: herdr status client");
                return Ok(2);
            }
            print_client_status();
            Ok(0)
        }
        Some("help" | "--help" | "-h") => {
            print_status_help();
            Ok(0)
        }
        Some(_) => {
            print_status_help();
            Ok(2)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRuntimeStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
    },
    NotRunning,
}

fn print_full_status() -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;

    println!("client:");
    println!("  version: {}", env!("CARGO_PKG_VERSION"));
    println!("  protocol: {}", crate::server::protocol::PROTOCOL_VERSION);
    println!();
    println!("server:");
    print_server_status_body(&server, "  ");
    println!();
    println!("update:");
    println!("  restart_needed: {}", restart_needed_label(&server));

    Ok(0)
}

fn print_server_status() -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;
    print_server_status_body(&server, "");
    Ok(0)
}

fn print_client_status() {
    println!("version: {}", env!("CARGO_PKG_VERSION"));
    println!("protocol: {}", crate::server::protocol::PROTOCOL_VERSION);
    println!("binary: {}", current_exe_label());
}

fn print_server_status_body(server: &ServerRuntimeStatus, indent: &str) {
    match server {
        ServerRuntimeStatus::Running { version, protocol } => {
            println!("{indent}status: running");
            println!("{indent}version: {}", option_label(version.as_deref()));
            println!("{indent}protocol: {}", protocol_label(*protocol));
            println!("{indent}compatible: {}", compatibility_label(*protocol));
            println!("{indent}socket: {}", api::socket_path().display());
        }
        ServerRuntimeStatus::NotRunning => {
            println!("{indent}status: not running");
            println!("{indent}socket: {}", api::socket_path().display());
        }
    }
}

fn read_server_runtime_status() -> std::io::Result<ServerRuntimeStatus> {
    match send_request(&Request {
        id: "cli:status:server".into(),
        method: Method::Ping(PingParams::default()),
    }) {
        Ok(response) => {
            if response.get("error").is_some() {
                return Err(std::io::Error::other(format!(
                    "server status request failed: {}",
                    response
                )));
            }

            let result = &response["result"];
            Ok(ServerRuntimeStatus::Running {
                version: result
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                protocol: result
                    .get("protocol")
                    .and_then(|value| value.as_u64())
                    .and_then(|value| u32::try_from(value).ok()),
            })
        }
        Err(err) if server_not_running_error(&err) => Ok(ServerRuntimeStatus::NotRunning),
        Err(err) => Err(err),
    }
}

fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn option_label(value: Option<&str>) -> &str {
    value.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn compatibility_label(protocol: Option<u32>) -> &'static str {
    match protocol {
        Some(protocol) if protocol == crate::server::protocol::PROTOCOL_VERSION => "yes",
        Some(_) => "no",
        None => "unknown",
    }
}

fn restart_needed_label(server: &ServerRuntimeStatus) -> &'static str {
    match server {
        ServerRuntimeStatus::Running { version, .. } => match version.as_deref() {
            Some(version) if version == env!("CARGO_PKG_VERSION") => "no",
            Some(_) => "yes",
            None => "unknown",
        },
        ServerRuntimeStatus::NotRunning => "no",
    }
}

fn current_exe_label() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"))
}

fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_workspace_help();
        return Ok(2);
    };

    match subcommand {
        "list" => workspace_list(&args[1..]),
        "create" => workspace_create(&args[1..]),
        "get" => workspace_get(&args[1..]),
        "focus" => workspace_focus(&args[1..]),
        "rename" => workspace_rename(&args[1..]),
        "close" => workspace_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_workspace_help();
            Ok(0)
        }
        _ => {
            print_workspace_help();
            Ok(2)
        }
    }
}

fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn run_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_pane_help();
        return Ok(2);
    };

    match subcommand {
        "list" => pane_list(&args[1..]),
        "get" => pane_get(&args[1..]),
        "read" => pane_read(&args[1..]),
        "split" => pane_split(&args[1..]),
        "close" => pane_close(&args[1..]),
        "send-text" => pane_send_text(&args[1..]),
        "send-keys" => pane_send_keys(&args[1..]),
        "run" => pane_run(&args[1..]),
        "help" | "--help" | "-h" => {
            print_pane_help();
            Ok(0)
        }
        _ => {
            print_pane_help();
            Ok(2)
        }
    }
}

fn run_wait_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_wait_help();
        return Ok(2);
    };

    match subcommand {
        "output" => wait_output(&args[1..]),
        "agent-status" => wait_agent_status(&args[1..]),
        "help" | "--help" | "-h" => {
            print_wait_help();
            Ok(0)
        }
        _ => {
            print_wait_help();
            Ok(2)
        }
    }
}

fn run_integration_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_integration_help();
        return Ok(2);
    };

    match subcommand {
        "install" => integration_install(&args[1..]),
        "uninstall" => integration_uninstall(&args[1..]),
        "help" | "--help" | "-h" => {
            print_integration_help();
            Ok(0)
        }
        _ => {
            print_integration_help();
            Ok(2)
        }
    }
}

fn run_session_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_session_help();
        return Ok(2);
    };

    match subcommand {
        "list" => session_list(&args[1..]),
        "attach" => session_attach_help(&args[1..]),
        "stop" => session_stop(&args[1..]),
        "delete" => session_delete(&args[1..]),
        "help" | "--help" | "-h" => {
            print_session_help();
            Ok(0)
        }
        _ => {
            print_session_help();
            Ok(2)
        }
    }
}

fn server_stop(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr server stop");
        return Ok(2);
    }

    send_ok_request(Method::ServerStop(EmptyParams::default()))
}

fn server_reload_config(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr server reload-config");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:server:reload-config".into(),
        method: Method::ServerReloadConfig(EmptyParams::default()),
    })?)
}

fn session_attach_help(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("usage: herdr session attach <name>");
        return Ok(0);
    }
    eprintln!("usage: herdr session attach <name>");
    Ok(2)
}

fn session_list(args: &[String]) -> std::io::Result<i32> {
    let json = match parse_session_json_only(args, "usage: herdr session list [--json]") {
        Ok(json) => json,
        Err(code) => return Ok(code),
    };

    let sessions = crate::session::list_sessions()?;
    if json {
        _print_json(&serde_json::json!({
            "sessions": sessions,
        }));
    } else {
        print_session_table(&sessions);
    }
    Ok(0)
}

fn session_stop(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session stop <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let target = match crate::session::parse_target_name(&name) {
        Ok(target) => target,
        Err(message) => {
            print_session_error("invalid_session_name", &message);
            return Ok(1);
        }
    };
    match crate::session::stop_session(target.as_deref()) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "stopped": true,
                    "session": session,
                }));
            } else {
                println!("stopped session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_stop_failed", &message);
            Ok(1)
        }
    }
}

fn session_delete(args: &[String]) -> std::io::Result<i32> {
    let (name, json) =
        match parse_session_name_and_json(args, "usage: herdr session delete <name> [--json]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    match crate::session::delete_session(&name) {
        Ok(session) => {
            if json {
                _print_json(&serde_json::json!({
                    "deleted": true,
                    "session": session,
                }));
            } else {
                println!("deleted session {}", session.name);
            }
            Ok(0)
        }
        Err(message) => {
            print_session_error("session_delete_failed", &message);
            Ok(1)
        }
    }
}

fn workspace_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr workspace list");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:list".into(),
        method: Method::WorkspaceList(EmptyParams::default()),
    })?)
}

fn workspace_create(args: &[String]) -> std::io::Result<i32> {
    let mut cwd = None;
    let mut focus = true;
    let mut label = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
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

    print_response(&send_request(&Request {
        id: "cli:workspace:create".into(),
        method: Method::WorkspaceCreate(WorkspaceCreateParams { cwd, focus, label }),
    })?)
}

fn workspace_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:get".into(),
        method: Method::WorkspaceGet(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:focus".into(),
        method: Method::WorkspaceFocus(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr workspace rename <workspace_id> <label>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:rename".into(),
        method: Method::WorkspaceRename(WorkspaceRenameParams {
            workspace_id: normalize_workspace_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn workspace_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace close <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace close <workspace_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:workspace:close".into(),
        method: Method::WorkspaceClose(WorkspaceTarget {
            workspace_id: normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:tab:list".into(),
        method: Method::TabList(TabListParams { workspace_id }),
    })?)
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = true;
    let mut label = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
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

    print_response(&send_request(&Request {
        id: "cli:tab:create".into(),
        method: Method::TabCreate(TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
        }),
    })?)
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:get".into(),
        method: Method::TabGet(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:focus".into(),
        method: Method::TabFocus(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:rename".into(),
        method: Method::TabRename(TabRenameParams {
            tab_id: normalize_tab_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:tab:close".into(),
        method: Method::TabClose(TabTarget {
            tab_id: normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn pane_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    print_response(&send_request(&Request {
        id: "cli:pane:list".into(),
        method: Method::PaneList(PaneListParams { workspace_id }),
    })?)
}

fn pane_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane get <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr pane get <pane_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:pane:get".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_read(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--raw" => {
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let response = send_request(&Request {
        id: "cli:pane:read".into(),
        method: Method::PaneRead(PaneReadParams {
            pane_id,
            source,
            lines,
            strip_ansi,
        }),
    })?;

    if let Some(error) = response.get("error") {
        eprintln!("{}", serde_json::to_string(error).unwrap());
        return Ok(1);
    }

    if let Some(text) = response["result"]["read"]["text"].as_str() {
        print!("{text}");
    }
    Ok(0)
}

fn pane_split(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!(
            "usage: herdr pane split <pane_id> --direction right|down [--cwd PATH] [--no-focus]"
        );
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut direction = None;
    let mut cwd = None;
    let mut focus = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --direction");
                    return Ok(2);
                };
                direction = Some(parse_split_direction(value)?);
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
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

    let Some(direction) = direction else {
        eprintln!("missing required --direction");
        return Ok(2);
    };

    print_response(&send_request(&Request {
        id: "cli:pane:split".into(),
        method: Method::PaneSplit(PaneSplitParams {
            workspace_id: None,
            target_pane_id: pane_id,
            direction,
            cwd,
            focus,
        }),
    })?)
}

fn pane_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane close <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr pane close <pane_id>");
        return Ok(2);
    }

    print_response(&send_request(&Request {
        id: "cli:pane:close".into(),
        method: Method::PaneClose(PaneTarget {
            pane_id: normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_send_text(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-text <pane_id> <text>");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    send_ok_request(Method::PaneSendText(PaneSendTextParams { pane_id, text }))
}

fn pane_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-keys <pane_id> <key> [key ...]");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let keys = args[1..].to_vec();
    send_ok_request(Method::PaneSendKeys(PaneSendKeysParams { pane_id, keys }))
}

fn pane_run(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane run <pane_id> <command>");
        return Ok(2);
    }

    let pane_id = normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    send_ok_request(Method::PaneSendInput(PaneSendInputParams {
        pane_id,
        text,
        keys: vec!["Enter".into()],
    }))
}

fn integration_install(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "install")? else {
        return Ok(2);
    };

    match crate::integration::install_target(target) {
        Ok(messages) => {
            print_integration_messages(messages);
            Ok(0)
        }
        Err(err) => {
            eprintln!("{err}");
            Ok(1)
        }
    }
}

fn integration_uninstall(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = parse_integration_target(args, "uninstall")? else {
        return Ok(2);
    };

    match crate::integration::uninstall_target(target) {
        Ok(messages) => {
            print_integration_messages(messages);
            Ok(0)
        }
        Err(err) => {
            eprintln!("{err}");
            Ok(1)
        }
    }
}

fn print_integration_messages(messages: Vec<String>) {
    for message in messages {
        println!("{message}");
    }
}

fn parse_integration_target(
    args: &[String],
    action: &str,
) -> std::io::Result<Option<IntegrationTarget>> {
    let Some(target) = args.first().map(|arg| arg.as_str()) else {
        eprintln!("usage: herdr integration {action} <pi|claude|codex|opencode>");
        return Ok(None);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr integration {action} <pi|claude|codex|opencode>");
        return Ok(None);
    }

    let parsed = match target {
        "pi" => IntegrationTarget::Pi,
        "claude" => IntegrationTarget::Claude,
        "codex" => IntegrationTarget::Codex,
        "opencode" => IntegrationTarget::Opencode,
        _ => {
            eprintln!("unknown integration target: {target}");
            eprintln!("currently supported: pi, claude, codex, opencode");
            return Ok(None);
        }
    };

    Ok(Some(parsed))
}

fn wait_output(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--regex]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut timeout_ms = None;
    let mut strip_ansi = true;
    let mut regex = false;
    let mut match_value = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--match" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --match");
                    return Ok(2);
                };
                match_value = Some(value.clone());
                index += 2;
            }
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "--regex" => {
                regex = true;
                index += 1;
            }
            "--raw" => {
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(match_value) = match_value else {
        eprintln!("missing required --match");
        return Ok(2);
    };

    let matcher = if regex {
        OutputMatch::Regex { value: match_value }
    } else {
        OutputMatch::Substring { value: match_value }
    };

    let response = send_request(&Request {
        id: "cli:wait:output".into(),
        method: Method::PaneWaitForOutput(PaneWaitForOutputParams {
            pane_id,
            source,
            lines,
            r#match: matcher,
            timeout_ms,
            strip_ansi,
        }),
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(0)
}

fn wait_agent_status(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    wait_for_agent_change(
        Request {
            id: "cli:wait:agent-status".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions: vec![Subscription::PaneAgentStatusChanged {
                    pane_id,
                    agent_status: Some(agent_status),
                }],
            }),
        },
        timeout_ms,
        "timed out waiting for agent status change",
    )
}

fn wait_for_agent_change(
    request: Request,
    timeout_ms: Option<u64>,
    timeout_message: &str,
) -> std::io::Result<i32> {
    let mut stream = UnixStream::connect(api::socket_path())?;
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    if let Some(timeout_ms) = timeout_ms {
        stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)))?;
    }

    let mut reader = BufReader::new(stream);
    let mut ack = String::new();
    reader.read_line(&mut ack)?;
    if ack.trim().is_empty() {
        eprintln!("empty subscription ack");
        return Ok(1);
    }
    let ack_value: serde_json::Value = serde_json::from_str(&ack)?;
    if ack_value.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&ack_value).unwrap());
        return Ok(1);
    }

    let mut event = String::new();
    match reader.read_line(&mut event) {
        Ok(0) => {
            eprintln!("subscription closed before event arrived");
            Ok(1)
        }
        Ok(_) => {
            let event_value: serde_json::Value = serde_json::from_str(&event)?;
            println!("{}", serde_json::to_string(&event_value).unwrap());
            Ok(0)
        }
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            eprintln!("{timeout_message}");
            Ok(1)
        }
        Err(err) => Err(err),
    }
}

fn print_response(response: &serde_json::Value) -> std::io::Result<i32> {
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(response).unwrap());
        return Ok(1);
    }

    println!("{}", serde_json::to_string(response).unwrap());
    Ok(0)
}

fn send_ok_request(method: Method) -> std::io::Result<i32> {
    let response = send_request(&Request {
        id: "cli:request".into(),
        method,
    })?;

    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }

    Ok(0)
}

fn send_request(request: &Request) -> std::io::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(api::socket_path())?;
    stream.write_all(serde_json::to_string(request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

fn normalize_workspace_id(value: &str) -> String {
    value.to_string()
}

fn normalize_tab_id(value: &str) -> String {
    value.to_string()
}

fn normalize_pane_id(value: &str) -> String {
    value.to_string()
}

fn parse_split_direction(value: &str) -> std::io::Result<SplitDirection> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err(std::io::Error::other(format!(
            "invalid split direction: {value}"
        ))),
    }
}

fn parse_read_source(value: &str) -> std::io::Result<ReadSource> {
    match value {
        "visible" => Ok(ReadSource::Visible),
        "recent" => Ok(ReadSource::Recent),
        "recent-unwrapped" | "recent_unwrapped" => Ok(ReadSource::RecentUnwrapped),
        _ => Err(std::io::Error::other(format!(
            "invalid read source: {value}"
        ))),
    }
}

fn parse_agent_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "done" => Ok(AgentStatus::Done),
        "unknown" => Ok(AgentStatus::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, done, or unknown)"
        ))),
    }
}

fn parse_u32_flag(flag: &str, value: &str) -> std::io::Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

fn parse_u64_flag(flag: &str, value: &str) -> std::io::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| std::io::Error::other(format!("invalid value for {flag}: {value}")))
}

fn parse_session_json_only(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn parse_session_name_and_json(args: &[String], usage: &str) -> Result<(String, bool), i32> {
    let mut name = None;
    let mut json = false;
    for arg in args {
        if arg == "--json" {
            json = true;
        } else if name.is_none() {
            name = Some(arg.clone());
        } else {
            eprintln!("{usage}");
            return Err(2);
        }
    }

    let Some(name) = name else {
        eprintln!("{usage}");
        return Err(2);
    };
    Ok((name, json))
}

fn print_session_table(sessions: &[crate::session::SessionInfo]) {
    println!(
        "{:<20} {:<8} {:<48} {}",
        "name", "status", "directory", "socket"
    );
    for session in sessions {
        println!(
            "{:<20} {:<8} {:<48} {}",
            session.name,
            if session.running {
                "running"
            } else {
                "stopped"
            },
            session.session_dir,
            session.socket_path
        );
    }
}

fn print_session_error(code: &str, message: &str) {
    eprintln!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "error": {
                "code": code,
                "message": message,
            }
        }))
        .unwrap()
    );
}

fn print_server_help() {
    eprintln!("herdr server commands:");
    eprintln!("  herdr server                run as headless server");
    eprintln!("  herdr server stop           stop the running server via the API socket");
    eprintln!("  herdr server reload-config  reload config.toml in the running server");
}

fn print_status_help() {
    eprintln!("herdr status commands:");
    eprintln!("  herdr status         show local client and running server status");
    eprintln!("  herdr status server  show running server status");
    eprintln!("  herdr status client  show local client binary status");
}

fn print_workspace_help() {
    eprintln!("herdr workspace commands:");
    eprintln!("  herdr workspace list");
    eprintln!("  herdr workspace create [--cwd PATH] [--label TEXT] [--no-focus]");
    eprintln!("  herdr workspace get <workspace_id>");
    eprintln!("  herdr workspace focus <workspace_id>");
    eprintln!("  herdr workspace rename <workspace_id> <label>");
    eprintln!("  herdr workspace close <workspace_id>");
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab close <tab_id>");
}

fn print_pane_help() {
    eprintln!("herdr pane commands:");
    eprintln!("  herdr pane list [--workspace <workspace_id>]");
    eprintln!("  herdr pane get <pane_id>");
    eprintln!("  herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--raw]");
    eprintln!("  herdr pane split <pane_id> --direction right|down [--cwd PATH] [--no-focus]");
    eprintln!("  herdr pane close <pane_id>");
    eprintln!("  herdr pane send-text <pane_id> <text>");
    eprintln!("  herdr pane send-keys <pane_id> <key> [key ...]");
    eprintln!("  herdr pane run <pane_id> <command>");
}

fn print_wait_help() {
    eprintln!("herdr wait commands:");
    eprintln!("  herdr wait output <pane_id> --match <text> [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--regex] [--raw]");
    eprintln!(
        "  herdr wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]"
    );
}

fn print_integration_help() {
    eprintln!("herdr integration commands:");
    eprintln!("  herdr integration install pi");
    eprintln!("  herdr integration install claude");
    eprintln!("  herdr integration install codex");
    eprintln!("  herdr integration install opencode");
    eprintln!("  herdr integration uninstall pi");
    eprintln!("  herdr integration uninstall claude");
    eprintln!("  herdr integration uninstall codex");
    eprintln!("  herdr integration uninstall opencode");
}

fn print_session_help() {
    eprintln!("herdr session commands:");
    eprintln!("  herdr session list [--json]");
    eprintln!("  herdr session attach <name>");
    eprintln!("  herdr session stop <name> [--json]");
    eprintln!("  herdr session delete <name> [--json]");
    eprintln!("  use 'default' as <name> to target the default session for stop");
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}
