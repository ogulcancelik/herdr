use crate::api::schema::{
    Method, PaneListParams, PaneReadParams, PaneRenameParams, PaneReportAgentParams,
    PaneReportMetadataParams, PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams,
    PaneSplitParams, PaneTarget, ReadFormat, ReadSource, Request,
};

pub(super) fn run_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_pane_help();
        return Ok(2);
    };

    match subcommand {
        "list" => pane_list(&args[1..]),
        "get" => pane_get(&args[1..]),
        "read" => pane_read(&args[1..]),
        "rename" => pane_rename(&args[1..]),
        "split" => pane_split(&args[1..]),
        "close" => pane_close(&args[1..]),
        "send-text" => pane_send_text(&args[1..]),
        "send-keys" => pane_send_keys(&args[1..]),
        "report-agent" => pane_report_agent(&args[1..]),
        "report-metadata" => pane_report_metadata(&args[1..]),
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
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
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

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:get".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: super::normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane rename <pane_id> <label>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: herdr pane rename <pane_id> <label>|--clear");
        return Ok(2);
    }
    let label = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:rename".into(),
        method: Method::PaneRename(PaneRenameParams {
            pane_id: super::normalize_pane_id(raw_pane_id),
            label,
        }),
    })?)
}

fn pane_read(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = super::parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(super::parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = super::parse_read_format(value)?;
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                index += 1;
            }
            "--raw" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let response = super::send_request(&Request {
        id: "cli:pane:read".into(),
        method: Method::PaneRead(PaneReadParams {
            pane_id,
            source,
            lines,
            format,
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
            "usage: herdr pane split <pane_id> --direction right|down [--cwd PATH] [--focus] [--no-focus]"
        );
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut direction = None;
    let mut cwd = None;
    let mut focus = false;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --direction");
                    return Ok(2);
                };
                direction = Some(super::parse_split_direction(value)?);
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

    let Some(direction) = direction else {
        eprintln!("missing required --direction");
        return Ok(2);
    };

    super::print_response(&super::send_request(&Request {
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

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:close".into(),
        method: Method::PaneClose(PaneTarget {
            pane_id: super::normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_send_text(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-text <pane_id> <text>");
        return Ok(2);
    }

    let pane_id = super::normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    super::send_ok_request(Method::PaneSendText(PaneSendTextParams { pane_id, text }))
}

fn pane_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane send-keys <pane_id> <key> [key ...]");
        return Ok(2);
    }

    let pane_id = super::normalize_pane_id(&args[0]);
    let keys = args[1..].to_vec();
    super::send_ok_request(Method::PaneSendKeys(PaneSendKeysParams { pane_id, keys }))
}

fn pane_run(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr pane run <pane_id> <command>");
        return Ok(2);
    }

    let pane_id = super::normalize_pane_id(&args[0]);
    let text = args[1..].join(" ");
    super::send_ok_request(Method::PaneSendInput(PaneSendInputParams {
        pane_id,
        text,
        keys: vec!["Enter".into()],
    }))
}

fn pane_report_agent(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--agent-session-id ID] [--agent-session-path PATH]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut state = None;
    let mut message = None;
    let mut custom_status = None;
    let mut seq = None;
    let mut agent_session_id = None;
    let mut agent_session_path = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--state" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state");
                    return Ok(2);
                };
                state = Some(super::parse_pane_agent_state(value)?);
                index += 2;
            }
            "--message" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --message");
                    return Ok(2);
                };
                message = Some(value.clone());
                index += 2;
            }
            "--custom-status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --custom-status");
                    return Ok(2);
                };
                custom_status = Some(value.clone());
                index += 2;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--agent-session-id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-id");
                    return Ok(2);
                };
                agent_session_id = Some(value.clone());
                index += 2;
            }
            "--agent-session-path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-path");
                    return Ok(2);
                };
                agent_session_path = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let Some(agent) = agent else {
        eprintln!("missing required --agent");
        return Ok(2);
    };
    let Some(state) = state else {
        eprintln!("missing required --state");
        return Ok(2);
    };

    super::send_ok_request(Method::PaneReportAgent(PaneReportAgentParams {
        pane_id,
        source,
        agent,
        state,
        message,
        custom_status,
        seq,
        agent_session_id,
        agent_session_path,
    }))
}

fn pane_report_metadata(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: herdr pane report-metadata <pane_id> --source ID [--agent LABEL] [--applies-to-source ID] [--title TEXT|--clear-title] [--display-agent TEXT|--clear-display-agent] [--custom-status TEXT|--clear-custom-status] [--state-label STATUS=TEXT] [--clear-state-labels] [--seq N] [--ttl-ms N]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut applies_to_source = None;
    let mut title = None;
    let mut display_agent = None;
    let mut custom_status = None;
    let mut state_labels = std::collections::HashMap::new();
    let mut clear_title = false;
    let mut clear_display_agent = false;
    let mut clear_custom_status = false;
    let mut clear_state_labels = false;
    let mut seq = None;
    let mut ttl_ms = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--applies-to-source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --applies-to-source");
                    return Ok(2);
                };
                applies_to_source = Some(value.clone());
                index += 2;
            }
            "--title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --title");
                    return Ok(2);
                };
                title = Some(value.clone());
                index += 2;
            }
            "--clear-title" => {
                clear_title = true;
                index += 1;
            }
            "--display-agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --display-agent");
                    return Ok(2);
                };
                display_agent = Some(value.clone());
                index += 2;
            }
            "--clear-display-agent" => {
                clear_display_agent = true;
                index += 1;
            }
            "--custom-status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --custom-status");
                    return Ok(2);
                };
                custom_status = Some(value.clone());
                index += 2;
            }
            "--clear-custom-status" => {
                clear_custom_status = true;
                index += 1;
            }
            "--state-label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state-label");
                    return Ok(2);
                };
                let Some((status, label)) = value.split_once('=') else {
                    eprintln!("expected --state-label STATUS=TEXT");
                    return Ok(2);
                };
                let status = status.trim().to_ascii_lowercase();
                if !matches!(
                    status.as_str(),
                    "idle" | "working" | "blocked" | "done" | "unknown"
                ) {
                    eprintln!("unknown state label: {status}");
                    return Ok(2);
                }
                state_labels.insert(status, label.to_string());
                index += 2;
            }
            "--clear-state-labels" => {
                clear_state_labels = true;
                index += 1;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--ttl-ms" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --ttl-ms");
                    return Ok(2);
                };
                ttl_ms = Some(super::parse_u64_flag("--ttl-ms", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    if applies_to_source
        .as_deref()
        .is_some_and(|source| source.trim().is_empty())
    {
        eprintln!("missing value for --applies-to-source");
        return Ok(2);
    }
    if title.is_some() && clear_title
        || display_agent.is_some() && clear_display_agent
        || custom_status.is_some() && clear_custom_status
        || !state_labels.is_empty() && clear_state_labels
    {
        eprintln!("cannot set and clear the same metadata field");
        return Ok(2);
    }
    if title.is_none()
        && display_agent.is_none()
        && custom_status.is_none()
        && state_labels.is_empty()
        && !clear_title
        && !clear_display_agent
        && !clear_custom_status
        && !clear_state_labels
    {
        eprintln!("missing metadata field to set or clear");
        return Ok(2);
    }

    super::send_ok_request(Method::PaneReportMetadata(PaneReportMetadataParams {
        pane_id,
        source,
        agent,
        applies_to_source,
        title,
        display_agent,
        custom_status,
        state_labels,
        clear_title,
        clear_display_agent,
        clear_custom_status,
        clear_state_labels,
        seq,
        ttl_ms,
    }))
}

fn print_pane_help() {
    eprintln!("herdr pane commands:");
    eprintln!("  herdr pane list [--workspace <workspace_id>]");
    eprintln!("  herdr pane get <pane_id>");
    eprintln!("  herdr pane rename <pane_id> <label>|--clear");
    eprintln!("  herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!(
        "  herdr pane split <pane_id> --direction right|down [--cwd PATH] [--focus] [--no-focus]"
    );
    eprintln!("  herdr pane close <pane_id>");
    eprintln!("  herdr pane send-text <pane_id> <text>");
    eprintln!("  herdr pane send-keys <pane_id> <key> [key ...]");
    eprintln!("  herdr pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--agent-session-id ID] [--agent-session-path PATH]");
    eprintln!("  herdr pane report-metadata <pane_id> --source ID [--agent LABEL] [--applies-to-source ID] [--title TEXT|--clear-title] [--display-agent TEXT|--clear-display-agent] [--custom-status TEXT|--clear-custom-status] [--state-label STATUS=TEXT] [--clear-state-labels] [--seq N] [--ttl-ms N]");
    eprintln!("  herdr pane run <pane_id> <command>");
}
