use crate::api::schema::{EmptyParams, HostAttachParams, HostDetachParams, Method, Request};

pub(super) fn run_host_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_host_help();
        return Ok(2);
    };

    match subcommand {
        "attach" => host_attach(&args[1..]),
        "list" => host_list(&args[1..]),
        "detach" => host_detach(&args[1..]),
        "help" | "--help" | "-h" => {
            print_host_help();
            Ok(0)
        }
        _ => {
            print_host_help();
            Ok(2)
        }
    }
}

fn host_attach(args: &[String]) -> std::io::Result<i32> {
    let Some(host) = args.first() else {
        eprintln!("usage: herdr host attach <host>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr host attach <host>");
        return Ok(2);
    }

    let response = super::send_request(&Request {
        id: "cli:host:attach".into(),
        method: Method::HostAttach(HostAttachParams { host: host.clone() }),
    })?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    println!("{}", serde_json::to_string(&response).unwrap());
    // attach returns immediately in the `connecting` state; the link only
    // reaches `connected` (or degrades to `offline`) asynchronously, so an
    // exit-0 here is not a confirmed connection. Nudge the user to check.
    // The hint goes to stderr so it never corrupts the JSON on stdout.
    eprintln!("attaching to {host}; run `herdr host list` to check status");
    Ok(0)
}

fn host_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr host list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:host:list".into(),
        method: Method::HostList(EmptyParams::default()),
    })?)
}

fn host_detach(args: &[String]) -> std::io::Result<i32> {
    let Some(host) = args.first() else {
        eprintln!("usage: herdr host detach <host>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr host detach <host>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:host:detach".into(),
        method: Method::HostDetach(HostDetachParams { host: host.clone() }),
    })?)
}

fn print_host_help() {
    eprintln!("herdr host commands:");
    eprintln!("  herdr host attach <host>");
    eprintln!("  herdr host list");
    eprintln!("  herdr host detach <host>");
    eprintln!("  <host> is an ssh alias/target identifying the remote herdr server");
}
