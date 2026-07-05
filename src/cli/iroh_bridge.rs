//! CLI handler for `herdr iroh-bridge`.

use std::io;
use std::path::PathBuf;

use crate::iroh_bridge::{self, ConnectConfig, ServeConfig};

pub(crate) fn run_iroh_bridge_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|s| s.as_str()) else {
        print_usage();
        return Ok(2);
    };

    match subcommand {
        "serve" => run_serve(&args[1..]),
        "connect" => run_connect(&args[1..]),
        "id" => run_id(),
        "key" => run_key_command(&args[1..]),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(0)
        }
        _ => {
            eprintln!("unknown iroh-bridge subcommand: {subcommand}");
            print_usage();
            Ok(2)
        }
    }
}

fn run_serve(args: &[String]) -> std::io::Result<i32> {
    let mut server_socket: Option<PathBuf> = None;
    let mut relay_urls: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --socket requires a path");
                    return Ok(2);
                }
                server_socket = Some(PathBuf::from(&args[i]));
            }
            "--relay" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --relay requires a URL");
                    return Ok(2);
                }
                relay_urls.push(args[i].clone());
            }
            other if other.starts_with("--relay=") => {
                relay_urls.push(other[8..].to_string());
            }
            other => {
                eprintln!("unknown flag: {other}");
                print_usage();
                return Ok(2);
            }
        }
        i += 1;
    }

    let server_socket =
        server_socket.unwrap_or_else(crate::server::socket_paths::client_socket_path);

    let secret_key = iroh_bridge::load_or_create_identity_key().ok();

    let config = ServeConfig {
        server_socket,
        secret_key,
        relay_urls,
    };

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| std::io::Error::other(format!("failed to create tokio runtime: {e}")))?;

    rt.block_on(iroh_bridge::run_serve(config))?;
    Ok(0)
}

fn run_connect(args: &[String]) -> std::io::Result<i32> {
    let mut remote_id: Option<String> = None;
    let mut local_socket: Option<PathBuf> = None;
    let mut relay_urls: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --socket requires a path");
                    return Ok(2);
                }
                local_socket = Some(PathBuf::from(&args[i]));
            }
            "--relay" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --relay requires a URL");
                    return Ok(2);
                }
                relay_urls.push(args[i].clone());
            }
            other if other.starts_with("--relay=") => {
                relay_urls.push(other[8..].to_string());
            }
            other if !other.starts_with('-') && remote_id.is_none() => {
                remote_id = Some(other.to_string());
            }
            other => {
                eprintln!("unknown flag: {other}");
                print_usage();
                return Ok(2);
            }
        }
        i += 1;
    }

    let remote_id_str = match remote_id {
        Some(id) => id,
        None => {
            eprintln!("error: missing <endpoint-id> argument");
            print_usage();
            return Ok(2);
        }
    };

    let remote_endpoint_id: iroh::EndpointId = remote_id_str
        .parse()
        .map_err(|e| std::io::Error::other(format!("invalid endpoint id: {e}")))?;

    let local_socket = local_socket.unwrap_or_else(|| {
        let pid = std::process::id();
        std::env::temp_dir().join(format!("herdr-iroh-bridge-{pid}.sock"))
    });

    let secret_key = iroh_bridge::load_or_create_identity_key().ok();

    let config = ConnectConfig {
        remote_endpoint_id,
        local_socket: Some(local_socket),
        secret_key,
        relay_urls,
    };

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| std::io::Error::other(format!("failed to create tokio runtime: {e}")))?;

    rt.block_on(iroh_bridge::run_connect(config))?;
    Ok(0)
}

fn run_id() -> std::io::Result<i32> {
    match iroh_bridge::load_identity_public_key() {
        Ok(Some(id)) => {
            println!("{id}");
            Ok(0)
        }
        Ok(None) => {
            // Generate one so the user gets an id immediately.
            let secret = iroh_bridge::load_or_create_identity_key()?;
            let secret_key = iroh::SecretKey::from_bytes(&secret);
            let public_key = secret_key.public();
            println!("{public_key}");
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}

fn print_usage() {
    eprintln!("usage: herdr iroh-bridge serve [--socket <path>] [--relay <url>]");
    eprintln!("       herdr iroh-bridge connect <endpoint-id> [--socket <path>] [--relay <url>]");
    eprintln!("       herdr iroh-bridge id");
    eprintln!("       herdr iroh-bridge key passwd");
}

fn run_key_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|s| s.as_str()) else {
        eprintln!("usage: herdr iroh-bridge key passwd");
        return Ok(2);
    };

    match subcommand {
        "passwd" => {
            let key_dir = iroh_bridge::identity_key_dir()?;
            crate::iroh_keyfile::change_passphrase(&key_dir, "iroh_id.key")
                .map_err(|e| io::Error::other(format!("failed to change passphrase: {e}")))?;
            Ok(0)
        }
        _ => {
            eprintln!("usage: herdr iroh-bridge key passwd");
            Ok(2)
        }
    }
}
