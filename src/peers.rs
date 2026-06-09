//! Federated peer servers: poll each configured `[[peers]]` entry over SSH
//! for its `peers.summary`, cache the results for the sidebar's project-
//! folded remote rows, and provide the attach target for switch-on-select.
//!
//! Peers never share PTYs or frames — only this lightweight summary gossip.

use std::time::Instant;

use crate::api::schema::PeerWorkspaceSummary;
use crate::config::PeerConfig;

/// Seconds between summary poll rounds.
pub const PEER_POLL_INTERVAL_SECS: u64 = 15;
/// First poll fires shortly after startup so the sidebar populates fast.
pub const PEER_POLL_INITIAL_DELAY_SECS: u64 = 3;
/// A peer whose last successful poll is older than this renders as stale.
pub const PEER_STALE_AFTER_SECS: u64 = 60;

/// Cached state of one configured peer, updated by the poll loop.
#[derive(Debug, Clone)]
pub struct PeerSummaryState {
    /// Peer name from config (sidebar host badge).
    pub peer: String,
    /// SSH destination used for polling and switch-on-select attach.
    pub ssh_target: String,
    /// Hostname the peer reported about itself (display fallback: peer name).
    pub host: Option<String>,
    pub workspaces: Vec<PeerWorkspaceSummary>,
    pub last_ok: Option<Instant>,
    /// Last poll error, cleared on success.
    pub error: Option<String>,
}

impl PeerSummaryState {
    pub fn new(config: &PeerConfig) -> Self {
        Self {
            peer: config.name.clone(),
            ssh_target: config.ssh_target().to_string(),
            host: None,
            workspaces: Vec::new(),
            last_ok: None,
            error: None,
        }
    }

    pub fn is_stale(&self) -> bool {
        match self.last_ok {
            Some(at) => at.elapsed().as_secs() > PEER_STALE_AFTER_SECS,
            None => true,
        }
    }
}

/// Result of one poll of one peer, sent back as an AppEvent.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerSummaryFetch {
    pub peer: String,
    pub result: Result<(String, Vec<PeerWorkspaceSummary>), String>,
}

/// Fetch a peer's summary over SSH (blocking; run off the UI thread).
pub fn fetch_peer_summary(peer: &PeerConfig) -> PeerSummaryFetch {
    let result = run_summary_command(peer).and_then(|stdout| parse_summary_response(&stdout));
    PeerSummaryFetch {
        peer: peer.name.clone(),
        result,
    }
}

fn run_summary_command(peer: &PeerConfig) -> Result<String, String> {
    let output = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=2",
            peer.ssh_target(),
            &peer.summary_command,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("ssh spawn failed: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            output.status.to_string()
        } else {
            // Keep the tail: ssh banners/motd come first, the error last.
            stderr.lines().next_back().unwrap_or(stderr).to_string()
        };
        return Err(detail);
    }
    String::from_utf8(output.stdout).map_err(|_| "non-utf8 summary output".to_string())
}

/// Parse the CLI's response envelope: `{"id":..,"result":{"host":..,"workspaces":[..]}}`.
fn parse_summary_response(stdout: &str) -> Result<(String, Vec<PeerWorkspaceSummary>), String> {
    // Login shells can print banners before the JSON; find the envelope line.
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .ok_or_else(|| "no JSON in summary output".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|err| format!("summary parse error: {err}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!("peer error: {error}"));
    }
    let result = value
        .get("result")
        .ok_or_else(|| "summary response has no result".to_string())?;
    let host = result
        .get("host")
        .and_then(|host| host.as_str())
        .unwrap_or_default()
        .to_string();
    let workspaces: Vec<PeerWorkspaceSummary> = result
        .get("workspaces")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| format!("summary workspaces parse error: {err}"))?
        .unwrap_or_default();
    Ok((host, workspaces))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;

    #[test]
    fn parse_summary_response_reads_envelope() {
        let stdout = r#"
Last login: whatever banner
{"id":"cli:peers:summary","result":{"host":"anvil","workspaces":[{"workspace":"herdr","project_key":"github.com/gerchowl/herdr","project_label":"herdr","branch":"fix/pty","is_linked_worktree":true,"agent":"cc","status":"blocked","status_age_secs":840}]}}
"#;
        let (host, workspaces) = parse_summary_response(stdout).unwrap();
        assert_eq!(host, "anvil");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].workspace, "herdr");
        assert_eq!(workspaces[0].status, AgentStatus::Blocked);
        assert_eq!(workspaces[0].status_age_secs, Some(840));
        assert!(workspaces[0].is_linked_worktree);
    }

    #[test]
    fn parse_summary_response_surfaces_peer_errors() {
        let err = parse_summary_response(r#"{"id":"x","error":{"code":"nope"}}"#).unwrap_err();
        assert!(err.contains("peer error"));
        assert!(parse_summary_response("no json here").is_err());
    }
}
