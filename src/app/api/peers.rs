use crate::api::schema::{PeerWorkspaceSummary, ResponseResult};
use crate::app::App;

use super::responses::encode_success;

impl App {
    /// Serve this server's federated summary: one entry per workspace with
    /// project identity + attention-leading agent status. Peers poll this
    /// over SSH to fold our workspaces into their sidebars.
    pub(super) fn handle_peers_summary(&mut self, id: String) -> String {
        let workspaces = self
            .state
            .workspaces
            .iter()
            .map(|ws| workspace_peer_summary(ws, &self.state.terminals))
            .collect();
        encode_success(
            id,
            ResponseResult::PeersSummary {
                host: short_host_name(),
                workspaces,
            },
        )
    }
}

impl App {
    /// Resolve a requested peer switch: returns the SSH target for the
    /// client's next attach leg and a display label, and best-effort
    /// pre-focuses the chosen workspace on the peer (off-thread).
    pub(crate) fn prepare_peer_switch(
        &mut self,
        peer_idx: usize,
        ws_idx: usize,
    ) -> Option<(String, String)> {
        let peer = self.state.peer_summaries.get(peer_idx)?;
        let ssh_target = peer.ssh_target.clone();
        let label = peer.host.clone().unwrap_or_else(|| peer.peer.clone());
        if let Some(remote_ws) = peer.workspaces.get(ws_idx) {
            let label = format!("{label}:{}", remote_ws.workspace);
            // Workspace ids are server-assigned ("ws_3"); refuse anything
            // that could escape the remote shell command.
            let id = remote_ws.id.clone();
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                let target = ssh_target.clone();
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("ssh")
                        .args([
                            "-o",
                            "BatchMode=yes",
                            "-o",
                            "ConnectTimeout=5",
                            &target,
                            &format!("sh -lc 'herdr workspace focus --workspace {id}'"),
                        ])
                        .stdin(std::process::Stdio::null())
                        .output();
                });
            }
            return Some((ssh_target, label));
        }
        Some((ssh_target, label))
    }
}

pub(crate) fn short_host_name() -> String {
    sysinfo::System::host_name()
        .map(|h| h.split('.').next().unwrap_or(&h).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn workspace_peer_summary(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
) -> PeerWorkspaceSummary {
    let (state, seen) = ws.aggregate_state(terminals);
    // The attention-leading pane: highest priority, oldest transition first —
    // mirrors the local focus_attention ordering. Panes without a transition
    // timestamp sort as newest.
    let now = std::time::Instant::now();
    let leading = ws
        .pane_details(terminals)
        .into_iter()
        .filter(|detail| (detail.state, detail.seen) == (state, seen))
        .min_by_key(|detail| detail.state_changed_at.unwrap_or(now));
    let (agent, status_age_secs, activity) = leading
        .map(|detail| {
            (
                Some(crate::detect::short_agent_label(&detail.agent_label).to_string()),
                detail
                    .state_changed_at
                    .map(|changed| changed.elapsed().as_secs()),
                detail.live_activity,
            )
        })
        .unwrap_or((None, None, None));

    // The git-space cache is populated by the periodic async refresh, so a
    // freshly-created workspace may not have it yet. Derive the project
    // identity live from the checkout in that cold-start window so the peer
    // row can still fold by project.
    let derived_space = ws
        .git_space()
        .is_none()
        .then(|| ws.resolved_identity_cwd())
        .flatten()
        .and_then(|cwd| crate::workspace::git_space_metadata(&cwd));
    let project_key = ws.project_key().map(str::to_string).or_else(|| {
        derived_space
            .as_ref()
            .map(|space| space.project_key.clone())
    });
    let project_label = ws
        .git_space()
        .map(|space| space.label.clone())
        .or_else(|| derived_space.as_ref().map(|space| space.label.clone()))
        .or_else(|| ws.worktree_space().map(|space| space.label.clone()));

    PeerWorkspaceSummary {
        id: ws.id.clone(),
        workspace: ws.display_name(),
        project_key,
        project_label,
        branch: ws.branch(),
        is_linked_worktree: ws
            .git_space()
            .map(|space| space.is_linked_worktree)
            .or_else(|| ws.worktree_space().map(|space| space.is_linked_worktree))
            .unwrap_or(false),
        agent,
        status: super::super::api_helpers::pane_agent_status(state, seen),
        status_age_secs,
        activity,
    }
}
