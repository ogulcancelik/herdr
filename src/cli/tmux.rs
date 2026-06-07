//! `herdr tmux` subcommand — manage tmux control mode monitoring.

use crate::tmux_control::TmuxControlConfig;

pub fn run_tmux_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("status") => run_status(),
        Some("help" | "--help" | "-h") => {
            print_help();
            Ok(0)
        }
        _ => {
            print_help();
            Ok(2)
        }
    }
}

fn run_status() -> std::io::Result<i32> {
    let config = crate::config::Config::load().config;
    let tmux_config = &config.tmux_control;

    if !tmux_config.enabled {
        println!("tmux control mode: disabled");
        println!("  Enable in config: [tmux_control] enabled = true");
        return Ok(0);
    }

    println!("tmux control mode: enabled");

    if tmux_config.target_sessions.is_empty() {
        println!("  sessions: all");
    } else {
        println!("  sessions: {}", tmux_config.target_sessions.join(", "));
    }

    match &tmux_config.socket_path {
        Some(path) => println!("  socket: {path}"),
        None => println!("  socket: default"),
    }

    // Check if tmux is available.
    match which_tmux() {
        Some(path) => println!("  tmux: {path}"),
        None => {
            println!("  tmux: NOT FOUND");
            println!("  Install tmux to use control mode monitoring.");
        }
    }

    // Check if a tmux server is running.
    if let Some(tmux_path) = which_tmux() {
        if is_tmux_server_running(&tmux_path) {
            println!("  server: running");
        } else {
            println!("  server: not running");
        }
    }

    Ok(0)
}

fn print_help() {
    println!("herdr tmux — tmux control mode monitoring");
    println!();
    println!("Usage: herdr tmux <command>");
    println!();
    println!("Commands:");
    println!("  status    Show tmux control mode status and configuration");
    println!("  help      Show this help");
    println!();
    println!("Tmux control mode receives push-based pane output and lifecycle");
    println!("events from tmux sessions. Enable in config.toml:");
    println!();
    println!("  [tmux_control]");
    println!("  enabled = true");
}

fn which_tmux() -> Option<String> {
    for path in [
        "/usr/bin/tmux",
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
    ] {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    std::env::var("PATH").ok().and_then(|path_var| {
        path_var
            .split(':')
            .map(|dir| format!("{dir}/tmux"))
            .find(|p| std::path::Path::new(p).exists())
    })
}

fn is_tmux_server_running(tmux_path: &str) -> bool {
    std::process::Command::new(tmux_path)
        .arg("list-sessions")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}
