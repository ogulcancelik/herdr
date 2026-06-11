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

/// Fleet snapshot received at attach (hub-and-spoke down-gossip, issue #36):
/// the origin (home) host label plus render-only peer rows carried from the
/// server the client switched away from. These entries are NEVER polled —
/// their freshness only decays, which the existing staleness rendering shows.
#[derive(Debug, Clone)]
pub struct FleetSnapshotState {
    /// Short host name of the original origin (the client's home).
    pub origin: String,
    /// Carried peer summaries, converted into the poller's cache shape so
    /// the sidebar reuses the existing peer-row machinery.
    pub peers: Vec<PeerSummaryState>,
    /// The origin (hub) server's OWN summary (#66): its workspaces fold into
    /// the spaces list and its health populates the home row. The hub is not
    /// its own peer, so without this the hub's spaces are invisible on a
    /// spoke. Its `ssh_target` is the reserved home sentinel — origin rows
    /// switch home, never ssh.
    pub origin_summary: Option<PeerSummaryState>,
    /// When this snapshot arrived (home-row staleness display).
    pub received_at: Instant,
}

impl FleetSnapshotState {
    pub fn from_wire(snapshot: crate::protocol::FleetSnapshot) -> Self {
        Self {
            origin: snapshot.origin,
            peers: snapshot.peers.into_iter().map(peer_from_wire).collect(),
            origin_summary: snapshot.origin_summary.map(|p| peer_from_wire(*p)),
            received_at: Instant::now(),
        }
    }

    /// Re-encode for the next leap, excluding the hop target itself (it
    /// becomes the self row on the receiving end) and any entry matching the
    /// origin — the home row owns that slot, so a hub that lists itself in
    /// [[peers]] must not render twice. Ages are recomputed so time spent on
    /// this server keeps counting against freshness. Peer count is bounded:
    /// the snapshot rides an env var between attach legs, and an unbounded
    /// fleet could brush ARG_MAX and kill the leg spawn.
    pub fn to_wire(&self, exclude_ssh_target: &str) -> crate::protocol::FleetSnapshot {
        crate::protocol::FleetSnapshot {
            origin: self.origin.clone(),
            peers: self
                .peers
                .iter()
                .filter(|peer| peer.ssh_target != exclude_ssh_target && peer.peer != self.origin)
                .take(FLEET_SNAPSHOT_MAX_PEERS)
                .map(peer_to_wire)
                .collect(),
            // Pass-through: a nested leap keeps the ORIGINAL hub's own
            // summary so the way-home spaces stay visible the whole chain.
            origin_summary: self
                .origin_summary
                .as_ref()
                .map(|p| Box::new(peer_to_wire(p))),
        }
    }
}

/// Carried-snapshot peer cap (env-var transport between attach legs — see
/// `to_wire`). Far above any realistic personal fleet.
pub const FLEET_SNAPSHOT_MAX_PEERS: usize = 16;

/// Wire shape of one cached peer summary (`Instant` freshness → age in
/// seconds at capture time).
pub fn peer_to_wire(peer: &PeerSummaryState) -> crate::protocol::FleetPeer {
    crate::protocol::FleetPeer {
        name: peer.peer.clone(),
        ssh_target: peer.ssh_target.clone(),
        host: peer.host.clone(),
        version: peer.version.clone(),
        system: peer.system.clone().map(Into::into),
        latency_ms: peer.latency_ms,
        workspaces: peer.workspaces.iter().cloned().map(Into::into).collect(),
        age_secs: peer.last_ok.map(|at| at.elapsed().as_secs()),
        error: peer.error.clone(),
    }
}

/// Rehydrate a carried peer entry into the poller's cache shape. The age is
/// mapped back onto a synthetic `last_ok` instant so `is_stale`/`reachability`
/// keep working — and keep decaying — without any reverse polling.
pub fn peer_from_wire(peer: crate::protocol::FleetPeer) -> PeerSummaryState {
    PeerSummaryState {
        peer: peer.name,
        ssh_target: peer.ssh_target,
        host: peer.host,
        version: peer.version,
        system: peer.system.map(Into::into),
        latency_ms: peer.latency_ms,
        workspaces: peer.workspaces.into_iter().map(Into::into).collect(),
        last_ok: peer
            .age_secs
            .and_then(|secs| Instant::now().checked_sub(std::time::Duration::from_secs(secs))),
        error: peer.error,
    }
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
    #[test]
    fn to_wire_dedups_origin_and_caps_peer_count() {
        let mk = |name: &str| PeerSummaryState {
            peer: name.to_string(),
            ssh_target: name.to_string(),
            host: None,
            version: None,
            system: None,
            latency_ms: None,
            workspaces: Vec::new(),
            last_ok: None,
            error: None,
        };
        let mut peers: Vec<PeerSummaryState> = (0..FLEET_SNAPSHOT_MAX_PEERS + 3)
            .map(|i| mk(&format!("p{i}")))
            .collect();
        peers.push(mk("mba22")); // a hub that lists itself in [[peers]]
        let snapshot = FleetSnapshotState {
            origin: "mba22".into(),
            peers,
            origin_summary: None,
            received_at: Instant::now(),
        };

        let wire = snapshot.to_wire("p0");

        assert!(
            wire.peers.iter().all(|p| p.name != "mba22"),
            "origin owns the home row"
        );
        assert!(
            wire.peers.iter().all(|p| p.name != "p0"),
            "hop target excluded"
        );
        assert!(
            wire.peers.len() <= FLEET_SNAPSHOT_MAX_PEERS,
            "env-var transport cap"
        );
    }

    use super::*;
    use crate::api::schema::AgentStatus;

    fn summary_state(name: &str, ssh_target: &str, age_secs: Option<u64>) -> PeerSummaryState {
        PeerSummaryState {
            peer: name.to_string(),
            ssh_target: ssh_target.to_string(),
            host: Some(format!("{name}-host")),
            version: Some("0.9.0".to_string()),
            system: Some(crate::api::schema::PeerSystemSummary {
                cpu_percent: Some(42),
                mem_used: Some(13 << 30),
                mem_total: Some(16 << 30),
                disk_free: None,
            }),
            latency_ms: Some(34),
            workspaces: vec![crate::api::schema::PeerWorkspaceSummary {
                id: "ws_3".to_string(),
                workspace: "proj".to_string(),
                project_key: Some("github.com/x/proj".to_string()),
                project_label: Some("proj".to_string()),
                branch: Some("main".to_string()),
                is_linked_worktree: false,
                agent: Some("cc".to_string()),
                status: AgentStatus::Working,
                status_age_secs: Some(12),
                activity: None,
            }],
            last_ok: age_secs
                .and_then(|secs| Instant::now().checked_sub(std::time::Duration::from_secs(secs))),
            error: None,
        }
    }

    #[test]
    fn fleet_peer_wire_roundtrip_preserves_summary_and_freshness() {
        let state = summary_state("anvil", "lars@anvil", Some(5));
        let wire = peer_to_wire(&state);
        assert_eq!(wire.age_secs, Some(5));

        let back = peer_from_wire(wire);
        assert_eq!(back.peer, state.peer);
        assert_eq!(back.ssh_target, state.ssh_target);
        assert_eq!(back.host, state.host);
        assert_eq!(back.version, state.version);
        assert_eq!(back.system, state.system);
        assert_eq!(back.latency_ms, state.latency_ms);
        assert_eq!(back.workspaces, state.workspaces);
        assert_eq!(back.error, state.error);
        // The age maps back onto a synthetic last_ok so reachability keeps
        // working — a 5s-old summary is still Live...
        let age = back.last_ok.expect("freshness carried").elapsed().as_secs();
        assert!((5..8).contains(&age), "age {age} should stay ~5s");
        assert_eq!(back.reachability(), PeerReachability::Live);

        // ...while an old one decays to Down with no polling involved.
        let stale = peer_from_wire(peer_to_wire(&summary_state(
            "sage",
            "lars@sage",
            Some(PEER_STALE_AFTER_SECS + 30),
        )));
        assert_eq!(stale.reachability(), PeerReachability::Down);

        // Never-reached peers stay never-reached.
        let never = peer_from_wire(peer_to_wire(&summary_state("ksb", "lars@ksb", None)));
        assert!(never.last_ok.is_none());
    }

    #[test]
    fn fleet_snapshot_to_wire_keeps_origin_and_excludes_hop_target() {
        let snapshot = FleetSnapshotState {
            origin: "mba22".to_string(),
            peers: vec![
                summary_state("anvil", "lars@anvil", Some(3)),
                summary_state("sage", "lars@sage", Some(9)),
            ],
            origin_summary: None,
            received_at: Instant::now(),
        };

        let wire = snapshot.to_wire("lars@sage");
        // Pass-through: the ORIGINAL origin survives nested leaps.
        assert_eq!(wire.origin, "mba22");
        // The hop target becomes the self row on the receiving end.
        assert_eq!(wire.peers.len(), 1);
        assert_eq!(wire.peers[0].ssh_target, "lars@anvil");
    }

    #[test]
    fn origin_summary_survives_wire_roundtrip_and_passthrough() {
        let mut origin = summary_state("mba22", crate::protocol::HOME_SWITCH_TARGET, Some(0));
        origin.workspaces[0].workspace = "herdr".to_string();
        let snapshot = FleetSnapshotState {
            origin: "mba22".to_string(),
            peers: vec![summary_state("anvil", "lars@anvil", Some(3))],
            origin_summary: Some(origin),
            received_at: Instant::now(),
        };

        // Round-trip carries the hub's own workspaces home-targeted.
        let back = FleetSnapshotState::from_wire(snapshot.to_wire("lars@anvil"));
        let carried = back
            .origin_summary
            .clone()
            .expect("origin summary survives");
        assert_eq!(carried.ssh_target, crate::protocol::HOME_SWITCH_TARGET);
        assert_eq!(carried.workspaces[0].workspace, "herdr");
        // A nested leap (pass-through) keeps the hub's own summary too.
        let nested = FleetSnapshotState::from_wire(back.to_wire("lars@anvil"));
        assert!(nested.origin_summary.is_some());
    }

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
