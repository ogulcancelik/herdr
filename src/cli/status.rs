use serde::Serialize;

use crate::api;
use crate::api::client::{ApiClient, ApiClientError};

pub(super) fn run_status_command(args: &[String]) -> std::io::Result<i32> {
    let Some((scope, json)) = parse_status_args(args) else {
        return Ok(2);
    };

    match scope {
        StatusScope::Full => print_full_status(json),
        StatusScope::Server => print_server_status(json),
        StatusScope::Client => {
            print_client_status(json)?;
            Ok(0)
        }
        StatusScope::Help => {
            print_status_help();
            Ok(0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusScope {
    Full,
    Server,
    Client,
    Help,
}

fn parse_status_args(args: &[String]) -> Option<(StatusScope, bool)> {
    match args.first().map(|arg| arg.as_str()) {
        None => Some((StatusScope::Full, false)),
        Some("--json") if args.len() == 1 => Some((StatusScope::Full, true)),
        Some("server") => {
            parse_status_scope_args(args, StatusScope::Server, "herdr status server [--json]")
        }
        Some("client") => {
            parse_status_scope_args(args, StatusScope::Client, "herdr status client [--json]")
        }
        Some("help" | "--help" | "-h") => {
            if args.len() > 1 {
                print_status_help();
                return None;
            }
            Some((StatusScope::Help, false))
        }
        Some(_) => {
            print_status_help();
            None
        }
    }
}

fn parse_status_scope_args(
    args: &[String],
    scope: StatusScope,
    usage: &str,
) -> Option<(StatusScope, bool)> {
    match args.get(1).map(|arg| arg.as_str()) {
        None => Some((scope, false)),
        Some("--json") if args.len() == 2 => Some((scope, true)),
        _ => {
            eprintln!("usage: {usage}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerRuntimeStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        capabilities: Option<crate::api::schema::ServerCapabilities>,
    },
    NotRunning,
}

fn print_full_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;

    if json {
        print_json(&FullStatusJson {
            client: client_status_json(),
            server: server_status_json(&server),
            update: update_status_json(&server),
        })?;
        return Ok(0);
    }

    println!("summary: {}", status_summary(&server));
    println!();
    println!("client:");
    println!("  version: {}", crate::build_info::version());
    println!(
        "  channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("  protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!(
        "  session: {}",
        option_label(crate::session::active_name().as_deref())
    );
    println!("  binary: {}", current_exe_label());
    println!();
    println!("server:");
    print_server_status_body(&server, "  ");
    println!();
    println!("update:");
    println!("  restart_needed: {}", restart_needed_label(&server));
    println!();
    println!("next: {}", status_next_action(&server));

    Ok(0)
}

fn print_server_status(json: bool) -> std::io::Result<i32> {
    let server = read_server_runtime_status()?;
    if json {
        print_json(&server_status_json(&server))?;
        return Ok(0);
    }
    println!("summary: {}", status_summary(&server));
    print_server_status_body(&server, "");
    println!("next: {}", status_next_action(&server));
    Ok(0)
}

fn print_client_status(json: bool) -> std::io::Result<()> {
    if json {
        print_json(&client_status_json())?;
        return Ok(());
    }

    println!(
        "summary: client {} on {}",
        crate::build_info::version(),
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("version: {}", crate::build_info::version());
    println!(
        "channel: {}",
        crate::config::Config::load().config.update.channel.as_str()
    );
    println!("protocol: {}", crate::protocol::PROTOCOL_VERSION);
    println!(
        "session: {}",
        option_label(crate::session::active_name().as_deref())
    );
    println!("binary: {}", current_exe_label());
    Ok(())
}

fn print_server_status_body(server: &ServerRuntimeStatus, indent: &str) {
    match server {
        ServerRuntimeStatus::Running {
            version, protocol, ..
        } => {
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
    match ApiClient::local().status() {
        Ok(status) => Ok(ServerRuntimeStatus::Running {
            version: status.version,
            protocol: status.protocol,
            capabilities: status.capabilities,
        }),
        Err(ApiClientError::Io(err)) if server_not_running_error(&err) => {
            Ok(ServerRuntimeStatus::NotRunning)
        }
        Err(err) => Err(api_client_error_to_io(err)),
    }
}

fn server_not_running_error(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn api_client_error_to_io(err: ApiClientError) -> std::io::Error {
    match err {
        ApiClientError::Io(err) => err,
        err => std::io::Error::other(err),
    }
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
        Some(protocol) if protocol == crate::protocol::PROTOCOL_VERSION => "yes",
        Some(_) => "no",
        None => "unknown",
    }
}

fn restart_needed_label(server: &ServerRuntimeStatus) -> &'static str {
    match server {
        ServerRuntimeStatus::Running { version, .. } => match version.as_deref() {
            Some(version) if version == crate::build_info::version() => "no",
            Some(_) => "yes",
            None => "unknown",
        },
        ServerRuntimeStatus::NotRunning => "no",
    }
}

fn status_summary(server: &ServerRuntimeStatus) -> &'static str {
    match server {
        ServerRuntimeStatus::Running {
            version, protocol, ..
        } => {
            let current_version = crate::build_info::version();
            let version_matches = version.as_deref() == Some(current_version.as_str());
            let protocol_matches = *protocol == Some(crate::protocol::PROTOCOL_VERSION);
            if version_matches && protocol_matches {
                "server running; client and server match"
            } else if protocol_mismatch(*protocol) {
                "server running; protocol mismatch"
            } else if version_mismatch(version.as_deref()) {
                "server running; restart recommended"
            } else {
                "server running; compatibility unknown"
            }
        }
        ServerRuntimeStatus::NotRunning => "server not running",
    }
}

fn status_next_action(server: &ServerRuntimeStatus) -> &'static str {
    match server {
        ServerRuntimeStatus::Running { protocol, .. } if protocol_mismatch(*protocol) => {
            "run `herdr server stop`, then launch `herdr` again"
        }
        ServerRuntimeStatus::Running { version, .. } if version_mismatch(version.as_deref()) => {
            "restart Herdr when convenient to use the installed client version"
        }
        ServerRuntimeStatus::Running { .. } => "no action needed",
        ServerRuntimeStatus::NotRunning => "run `herdr` to launch the persistent session",
    }
}

fn protocol_mismatch(protocol: Option<u32>) -> bool {
    protocol.is_some_and(|value| value != crate::protocol::PROTOCOL_VERSION)
}

fn version_mismatch(version: Option<&str>) -> bool {
    let current_version = crate::build_info::version();
    version.is_some_and(|value| value != current_version.as_str())
}

#[derive(Serialize)]
struct FullStatusJson {
    client: ClientStatusJson,
    server: ServerStatusJson,
    update: UpdateStatusJson,
}

#[derive(Serialize)]
struct ClientStatusJson {
    version: String,
    channel: &'static str,
    protocol: u32,
    binary: String,
    session: Option<String>,
}

#[derive(Serialize)]
struct ServerStatusJson {
    status: &'static str,
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<ServerCapabilitiesJson>,
    compatible: Option<bool>,
    socket: String,
    session: Option<String>,
    restart_needed: Option<bool>,
}

#[derive(Serialize)]
struct ServerCapabilitiesJson {
    live_handoff: bool,
    detached_server_daemon: bool,
}

#[derive(Serialize)]
struct UpdateStatusJson {
    restart_needed: Option<bool>,
}

fn client_status_json() -> ClientStatusJson {
    ClientStatusJson {
        version: crate::build_info::version(),
        channel: crate::config::Config::load().config.update.channel.as_str(),
        protocol: crate::protocol::PROTOCOL_VERSION,
        binary: current_exe_label(),
        session: crate::session::active_name(),
    }
}

fn server_status_json(server: &ServerRuntimeStatus) -> ServerStatusJson {
    match server {
        ServerRuntimeStatus::Running {
            version,
            protocol,
            capabilities,
        } => ServerStatusJson {
            status: "running",
            running: true,
            version: version.clone(),
            protocol: *protocol,
            capabilities: capabilities
                .as_ref()
                .map(|capabilities| ServerCapabilitiesJson {
                    live_handoff: capabilities.live_handoff,
                    detached_server_daemon: capabilities.detached_server_daemon,
                }),
            compatible: protocol.map(|value| value == crate::protocol::PROTOCOL_VERSION),
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: restart_needed_bool(server),
        },
        ServerRuntimeStatus::NotRunning => ServerStatusJson {
            status: "not_running",
            running: false,
            version: None,
            protocol: None,
            capabilities: None,
            compatible: None,
            socket: api::socket_path().display().to_string(),
            session: crate::session::active_name(),
            restart_needed: Some(false),
        },
    }
}

fn update_status_json(server: &ServerRuntimeStatus) -> UpdateStatusJson {
    UpdateStatusJson {
        restart_needed: restart_needed_bool(server),
    }
}

fn restart_needed_bool(server: &ServerRuntimeStatus) -> Option<bool> {
    match server {
        ServerRuntimeStatus::Running { version, .. } => match version.as_deref() {
            Some(version) if version == crate::build_info::version() => Some(false),
            Some(_) => Some(true),
            None => None,
        },
        ServerRuntimeStatus::NotRunning => Some(false),
    }
}

fn print_json(value: &impl Serialize) -> std::io::Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

fn current_exe_label() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"))
}

fn print_status_help() {
    eprintln!("herdr status commands:");
    eprintln!("  herdr status [--json]         show local client and running server status");
    eprintln!("  herdr status server [--json]  show running server status");
    eprintln!("  herdr status client [--json]  show local client binary status");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_server(version: Option<String>, protocol: Option<u32>) -> ServerRuntimeStatus {
        ServerRuntimeStatus::Running {
            version,
            protocol,
            capabilities: None,
        }
    }

    #[test]
    fn status_summary_reports_matching_running_server() {
        let server = running_server(
            Some(crate::build_info::version()),
            Some(crate::protocol::PROTOCOL_VERSION),
        );

        assert_eq!(
            status_summary(&server),
            "server running; client and server match"
        );
        assert_eq!(status_next_action(&server), "no action needed");
    }

    #[test]
    fn status_summary_reports_not_running_server() {
        let server = ServerRuntimeStatus::NotRunning;

        assert_eq!(status_summary(&server), "server not running");
        assert_eq!(
            status_next_action(&server),
            "run `herdr` to launch the persistent session"
        );
    }

    #[test]
    fn status_summary_prioritizes_protocol_mismatch() {
        let server = running_server(Some(crate::build_info::version()), Some(0));

        assert_eq!(status_summary(&server), "server running; protocol mismatch");
        assert_eq!(
            status_next_action(&server),
            "run `herdr server stop`, then launch `herdr` again"
        );
    }
}
