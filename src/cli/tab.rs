use crate::api::schema::{
    Method, Request, TabCreateParams, TabListParams, TabMoveParams, TabRenameParams, TabTarget,
};

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
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
        "move" => tab_move(&args[1..]),
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
        id: "cli:tab:list".into(),
        method: Method::TabList(TabListParams { workspace_id }),
    })?)
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;

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

    super::print_response(&super::send_request(&Request {
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

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:get".into(),
        method: Method::TabGet(TabTarget {
            tab_id: super::normalize_tab_id(raw_tab_id),
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

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:focus".into(),
        method: Method::TabFocus(TabTarget {
            tab_id: super::normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:rename".into(),
        method: Method::TabRename(TabRenameParams {
            tab_id: super::normalize_tab_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn tab_move(args: &[String]) -> std::io::Result<i32> {
    if args.len() != 2 {
        eprintln!("usage: herdr tab move <tab_id> <position>");
        return Ok(2);
    }

    let position: usize = match args[1].parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("position must be a positive integer");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:move".into(),
        method: Method::TabMove(TabMoveParams {
            tab_id: super::normalize_tab_id(&args[0]),
            position,
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

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:close".into(),
        method: Method::TabClose(TabTarget {
            tab_id: super::normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab move <tab_id> <position>");
    eprintln!("  herdr tab close <tab_id>");
}
