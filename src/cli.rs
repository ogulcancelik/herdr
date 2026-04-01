use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Serialize;

use crate::api;
use crate::api::schema::{
    EmptyParams, Method, OutputMatch, PaneAgentState, PaneListParams, PaneReadParams,
    PaneSendKeysParams, PaneSendTextParams, PaneSplitParams, PaneTarget, PaneWaitForOutputParams,
    ReadSource, Request, SplitDirection, Subscription, WorkspaceCreateParams,
    WorkspaceRenameParams, WorkspaceTarget,
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
        "workspace" => run_workspace_command(&args[2..])?,
        "pane" => run_pane_command(&args[2..])?,
        "wait" => run_wait_command(&args[2..])?,
        "integration" => run_integration_command(&args[2..])?,
        _ => return Ok(CommandOutcome::NotCli),
    };

    Ok(CommandOutcome::Handled(exit_code))
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
        _ => {
            print_workspace_help();
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
        "agent-state" => wait_agent_state(&args[1..]),
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
        _ => {
            print_integration_help();
            Ok(2)
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
        method: Method::WorkspaceCreate(WorkspaceCreateParams { cwd, focus }),
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
        eprintln!("usage: herdr pane read <pane_id> [--source visible|recent] [--lines N]");
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
    let send_text_status = send_ok_request(Method::PaneSendText(PaneSendTextParams {
        pane_id: pane_id.clone(),
        text,
    }))?;
    if send_text_status != 0 {
        return Ok(send_text_status);
    }

    send_ok_request(Method::PaneSendKeys(PaneSendKeysParams {
        pane_id,
        keys: vec!["Enter".into()],
    }))
}

fn integration_install(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first().map(|arg| arg.as_str()) else {
        eprintln!("usage: herdr integration install <pi>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr integration install <pi>");
        return Ok(2);
    }

    match target {
        "pi" => {
            let path = crate::integration::install_pi()?;
            println!("installed pi integration to {}", path.display());
            Ok(0)
        }
        _ => {
            eprintln!("unknown integration target: {target}");
            eprintln!("currently supported: pi");
            Ok(2)
        }
    }
}

fn wait_output(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait output <pane_id> --match <text> [--source visible|recent] [--lines N] [--timeout MS] [--regex]");
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

fn wait_agent_state(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr wait agent-state <pane_id> --state <idle|working|blocked|unknown> [--timeout MS]");
        return Ok(2);
    };

    let pane_id = normalize_pane_id(raw_pane_id);
    let mut timeout_ms = None;
    let mut desired_state = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--state" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state");
                    return Ok(2);
                };
                desired_state = Some(parse_agent_state(value)?);
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

    let Some(state) = desired_state else {
        eprintln!("missing required --state");
        return Ok(2);
    };

    let request = Request {
        id: "cli:wait:agent-state".into(),
        method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
            subscriptions: vec![Subscription::PaneAgentStateChanged {
                pane_id,
                state: Some(state),
            }],
        }),
    };

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
            eprintln!("timed out waiting for agent state change");
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
        _ => Err(std::io::Error::other(format!(
            "invalid read source: {value}"
        ))),
    }
}

fn parse_agent_state(value: &str) -> std::io::Result<PaneAgentState> {
    match value {
        "idle" => Ok(PaneAgentState::Idle),
        "working" => Ok(PaneAgentState::Working),
        "blocked" => Ok(PaneAgentState::Blocked),
        "unknown" => Ok(PaneAgentState::Unknown),
        _ => Err(std::io::Error::other(format!(
            "invalid agent state: {value} (expected idle, working, blocked, or unknown)"
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

fn print_workspace_help() {
    eprintln!("herdr workspace commands:");
    eprintln!("  herdr workspace list");
    eprintln!("  herdr workspace create [--cwd PATH] [--no-focus]");
    eprintln!("  herdr workspace get <workspace_id>");
    eprintln!("  herdr workspace focus <workspace_id>");
    eprintln!("  herdr workspace rename <workspace_id> <label>");
    eprintln!("  herdr workspace close <workspace_id>");
}

fn print_pane_help() {
    eprintln!("herdr pane commands:");
    eprintln!("  herdr pane list [--workspace <workspace_id>]");
    eprintln!("  herdr pane get <pane_id>");
    eprintln!("  herdr pane read <pane_id> [--source visible|recent] [--lines N]");
    eprintln!("  herdr pane split <pane_id> --direction right|down [--cwd PATH] [--no-focus]");
    eprintln!("  herdr pane close <pane_id>");
    eprintln!("  herdr pane send-text <pane_id> <text>");
    eprintln!("  herdr pane send-keys <pane_id> <key> [key ...]");
    eprintln!("  herdr pane run <pane_id> <command>");
}

fn print_wait_help() {
    eprintln!("herdr wait commands:");
    eprintln!("  herdr wait output <pane_id> --match <text> [--source visible|recent] [--lines N] [--timeout MS] [--regex]");
    eprintln!(
        "  herdr wait agent-state <pane_id> --state <idle|working|blocked|unknown> [--timeout MS]"
    );
}

fn print_integration_help() {
    eprintln!("herdr integration commands:");
    eprintln!("  herdr integration install pi");
}

fn _print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).unwrap());
}
