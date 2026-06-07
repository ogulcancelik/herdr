//! Async listener for tmux control mode events.
//!
//! Spawns `tmux -C` as a child process, reads its stdout line by line,
//! parses events, and forwards them through a tokio channel.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::protocol::{self, TmuxEvent};

/// Configuration for the tmux control mode listener.
#[derive(Debug, Clone)]
pub struct TmuxControlConfig {
    /// Whether tmux control mode monitoring is enabled.
    pub enabled: bool,
    /// Session names to monitor. Empty = all sessions.
    pub target_sessions: Vec<String>,
    /// Tmux socket path (for `-L` flag). None = default tmux socket.
    pub socket_path: Option<String>,
}

impl Default for TmuxControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            target_sessions: Vec::new(),
            socket_path: None,
        }
    }
}

/// A running tmux control mode listener.
pub struct TmuxControlListener {
    child: Child,
    event_tx: mpsc::UnboundedSender<TmuxEvent>,
}

impl TmuxControlListener {
    /// Spawn a new tmux control mode listener.
    ///
    /// Returns the listener handle and a receiver for parsed events.
    /// The listener runs in a background tokio task.
    pub async fn spawn(
        config: TmuxControlConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<TmuxEvent>), String> {
        if !config.enabled {
            return Err("tmux control mode is disabled".to_string());
        }

        // Check if tmux is available.
        let tmux_path = which_tmux().ok_or("tmux not found in PATH")?;

        // Check if a tmux server is running.
        if !is_tmux_server_running(&tmux_path, config.socket_path.as_deref()) {
            return Err("no tmux server running".to_string());
        }

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Build tmux -C command.
        let mut args: Vec<String> = Vec::new();

        // Use specific socket if configured.
        if let Some(ref socket) = config.socket_path {
            args.push("-L".to_string());
            args.push(socket.clone());
        }

        // -C enables control mode.
        // -CC disables echo (cleaner output).
        args.push("-CC".to_string());

        info!(args = ?args, "spawning tmux control mode client");

        let mut child = Command::new(&tmux_path)
            .args(&args)
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn tmux control client: {e}"))?;

        let stdout = child.stdout.take().ok_or("failed to capture tmux stdout")?;
        let stdin = child.stdin.take().ok_or("failed to capture tmux stdin")?;

        // Subscribe to pane output for all sessions.
        // We send commands through stdin to configure the control mode session.
        let tx_clone = event_tx.clone();
        let target_sessions = config.target_sessions.clone();

        tokio::spawn(async move {
            // Small delay to let tmux initialize control mode.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Send initial commands to subscribe to events.
            let mut stdin = stdin;
            use tokio::io::AsyncWriteExt;

            // Enable pane output events.
            let _ = stdin
                .write_all(b"set-option -g pane-border-status off\n")
                .await;
            let _ = stdin.write_all(b"set-option -g status-interval 0\n").await;

            // Request initial session list.
            let _ = stdin.write_all(b"list-sessions\n").await;

            drop(stdin);
        });

        // Read loop: parse lines from stdout and forward events.
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let filter_sessions = target_sessions;

        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                match protocol::parse_line(&line) {
                    Ok(Some(event)) => {
                        // Filter by target sessions if configured.
                        if !filter_sessions.is_empty() {
                            let session_match = match &event {
                                TmuxEvent::Output { .. } => true, // Output events don't carry session
                                TmuxEvent::PaneFocusChanged { session, .. }
                                | TmuxEvent::WindowAdd { session, .. }
                                | TmuxEvent::WindowClose { session, .. }
                                | TmuxEvent::SessionChanged { session }
                                | TmuxEvent::SessionCreated { session }
                                | TmuxEvent::SessionClosed { session } => {
                                    filter_sessions.contains(session)
                                }
                                TmuxEvent::Pause { session, .. } => {
                                    filter_sessions.contains(session)
                                }
                                TmuxEvent::Begin { .. }
                                | TmuxEvent::CmdOutput { .. }
                                | TmuxEvent::End { .. }
                                | TmuxEvent::Unknown(_) => true,
                            };

                            if !session_match {
                                debug!(event = ?event, "skipping event (session filter)");
                                continue;
                            }
                        }

                        debug!(event = ?event, "tmux control mode event");
                        if event_tx.send(event).is_err() {
                            error!("event receiver dropped, stopping listener");
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(error = %e, "failed to parse tmux control mode line");
                    }
                }
            }

            info!("tmux control mode listener stopped");
        });

        let listener = Self {
            child,
            event_tx: event_tx.clone(),
        };

        Ok((listener, event_rx))
    }

    /// Send a command to the tmux control mode session.
    pub async fn send_command(&mut self, command: &str) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;

        if let Some(ref mut stdin) = self.child.stdin {
            stdin
                .write_all(format!("{command}\n").as_bytes())
                .await
                .map_err(|e| format!("failed to send command: {e}"))?;
            Ok(())
        } else {
            Err("stdin not available".to_string())
        }
    }

    /// Stop the listener by killing the tmux client process.
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}

impl Drop for TmuxControlListener {
    fn drop(&mut self) {
        // Best-effort cleanup — kill the child process.
        let _ = self.child.start_kill();
    }
}

/// Find the `tmux` binary in PATH.
fn which_tmux() -> Option<String> {
    // Try common locations first.
    for path in [
        "/usr/bin/tmux",
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
    ] {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }

    // Fall back to PATH lookup.
    std::env::var("PATH").ok().and_then(|path_var| {
        path_var
            .split(':')
            .map(|dir| format!("{dir}/tmux"))
            .find(|p| std::path::Path::new(p).exists())
    })
}

/// Check if a tmux server is running.
fn is_tmux_server_running(tmux_path: &str, socket_path: Option<&str>) -> bool {
    let mut cmd = std::process::Command::new(tmux_path);
    if let Some(socket) = socket_path {
        cmd.arg("-L").arg(socket);
    }
    cmd.arg("list-sessions");

    cmd.output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled() {
        let config = TmuxControlConfig::default();
        assert!(!config.enabled);
        assert!(config.target_sessions.is_empty());
    }
}
