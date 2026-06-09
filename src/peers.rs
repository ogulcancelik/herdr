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

/// A peer whose latency exceeds this renders as "slow" (yellow dot).
pub const PEER_SLOW_LATENCY_MS: u64 = 150;

/// Cached state of one configured peer, updated by the poll loop.
#[derive(Debug, Clone)]
pub struct PeerSummaryState {
    /// Peer name from config (sidebar host badge).
    pub peer: String,
    /// SSH destination used for polling and switch-on-select attach.
    pub ssh_target: String,
    /// Hostname the peer reported about itself (display fallback: peer name).
    pub host: Option<String>,
    /// herdr version the peer reported (spot un-deployed peers).
    pub version: Option<String>,
    /// Machine health snapshot from the last successful poll.
    pub system: Option<crate::api::schema::PeerSystemSummary>,
    /// Round-trip latency of the last successful summary poll.
    pub latency_ms: Option<u64>,
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
            version: None,
            system: None,
            latency_ms: None,
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

    /// Reachability for the sidebar dot: live / slow / stale-or-error.
    pub fn reachability(&self) -> PeerReachability {
        if self.is_stale() || self.error.is_some() {
            PeerReachability::Down
        } else if self.latency_ms.is_some_and(|ms| ms > PEER_SLOW_LATENCY_MS) {
            PeerReachability::Slow
        } else {
            PeerReachability::Live
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerReachability {
    Live,
    Slow,
    Down,
}

/// Parsed summary payload from one peer (everything its `peers.summary` carries).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerSummaryPayload {
    pub host: String,
    pub version: Option<String>,
    pub system: Option<crate::api::schema::PeerSystemSummary>,
    pub workspaces: Vec<PeerWorkspaceSummary>,
    /// Round-trip wall time of the summary SSH call (free latency probe).
    pub latency_ms: u64,
}

/// Result of one poll of one peer, sent back as an AppEvent.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerSummaryFetch {
    pub peer: String,
    pub result: Result<PeerSummaryPayload, String>,
}

/// Fetch a peer's summary over SSH (blocking; run off the UI thread). The
/// round-trip wall time doubles as a free latency probe — no separate ping.
pub fn fetch_peer_summary(peer: &PeerConfig) -> PeerSummaryFetch {
    let started = Instant::now();
    let result = run_summary_command(peer).and_then(|stdout| {
        let latency_ms = started.elapsed().as_millis() as u64;
        parse_summary_response(&stdout, latency_ms)
    });
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

/// Parse the CLI's response envelope:
/// `{"id":..,"result":{"host":..,"version":..,"system":..,"workspaces":[..]}}`.
fn parse_summary_response(stdout: &str, latency_ms: u64) -> Result<PeerSummaryPayload, String> {
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
    let version = result
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let system = result
        .get("system")
        .filter(|system| !system.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| format!("summary system parse error: {err}"))?;
    let workspaces: Vec<PeerWorkspaceSummary> = result
        .get("workspaces")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| format!("summary workspaces parse error: {err}"))?
        .unwrap_or_default();
    Ok(PeerSummaryPayload {
        host,
        version,
        system,
        workspaces,
        latency_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;

    #[test]
    fn parse_summary_response_reads_envelope() {
        let stdout = r#"
Last login: whatever banner
{"id":"cli:peers:summary","result":{"host":"anvil","version":"0.6.8","system":{"cpu_percent":71,"mem_used":48000000000,"mem_total":64000000000,"disk_free":200000000000},"workspaces":[{"workspace":"herdr","project_key":"github.com/gerchowl/herdr","project_label":"herdr","branch":"fix/pty","is_linked_worktree":true,"agent":"cc","status":"blocked","status_age_secs":840}]}}
"#;
        let payload = parse_summary_response(stdout, 34).unwrap();
        assert_eq!(payload.host, "anvil");
        assert_eq!(payload.version.as_deref(), Some("0.6.8"));
        assert_eq!(payload.latency_ms, 34);
        let system = payload.system.expect("system stats present");
        assert_eq!(system.cpu_percent, Some(71));
        assert_eq!(system.mem_total, Some(64000000000));
        assert_eq!(payload.workspaces.len(), 1);
        assert_eq!(payload.workspaces[0].workspace, "herdr");
        assert_eq!(payload.workspaces[0].status, AgentStatus::Blocked);
        assert_eq!(payload.workspaces[0].status_age_secs, Some(840));
        assert!(payload.workspaces[0].is_linked_worktree);
    }

    #[test]
    fn parse_summary_response_tolerates_missing_system_block() {
        let stdout = r#"{"id":"x","result":{"host":"sage","workspaces":[]}}"#;
        let payload = parse_summary_response(stdout, 5).unwrap();
        assert_eq!(payload.host, "sage");
        assert!(payload.system.is_none());
        assert!(payload.version.is_none());
        assert!(payload.workspaces.is_empty());
    }

    #[test]
    fn parse_summary_response_surfaces_peer_errors() {
        let err = parse_summary_response(r#"{"id":"x","error":{"code":"nope"}}"#, 1).unwrap_err();
        assert!(err.contains("peer error"));
        assert!(parse_summary_response("no json here", 1).is_err());
    }

    #[test]
    fn reachability_reflects_latency_and_staleness() {
        let mut peer = PeerSummaryState::new(&PeerConfig {
            name: "anvil".into(),
            ..Default::default()
        });
        assert_eq!(peer.reachability(), PeerReachability::Down); // never polled
        peer.last_ok = Some(Instant::now());
        peer.latency_ms = Some(20);
        assert_eq!(peer.reachability(), PeerReachability::Live);
        peer.latency_ms = Some(PEER_SLOW_LATENCY_MS + 1);
        assert_eq!(peer.reachability(), PeerReachability::Slow);
        peer.error = Some("timeout".into());
        assert_eq!(peer.reachability(), PeerReachability::Down);
    }
}
