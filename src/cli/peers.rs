use crate::api::schema::{EmptyParams, Method, Request};

pub(super) fn run_peers_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_peers_help();
        return Ok(2);
    };

    match subcommand {
        "summary" => peers_summary(&args[1..]),
        "help" | "--help" | "-h" => {
            print_peers_help();
            Ok(0)
        }
        _ => {
            print_peers_help();
            Ok(2)
        }
    }
}

/// This server's federated summary (workspaces + agent statuses). Peer
/// servers run this over SSH to fold our workspaces into their sidebars.
fn peers_summary(args: &[String]) -> std::io::Result<i32> {
    for arg in args {
        match arg.as_str() {
            // Output is always JSON; the flag is accepted for symmetry with
            // the other read-only subcommands.
            "--json" => {}
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:peers:summary".into(),
        method: Method::PeersSummary(EmptyParams {}),
    })?)
}

fn print_peers_help() {
    eprintln!("usage: herdr peers summary [--json]");
}
