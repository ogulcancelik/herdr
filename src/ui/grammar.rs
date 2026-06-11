//! Single source of the spaces/agents label grammar (#62).
//!
//! Every concrete checkout — local or remote — renders as `<server>:<target>`
//! (`mba22:main`, `sage:keyboard-shorcuts`). The server qualifier is always
//! present, the local host included, so "where is this checkout" is always
//! answered. The space row above carries the project identity itself
//! (`owner/repo` per #27), not a checkout. Keeping the rendering here in one
//! place makes the eventual origin-gossip merge (peers.rs may add an Origin
//! remote ref) trivial: only the call sites that build the `(server, target)`
//! pair change, never the formatting.

use crate::app::AppState;
use crate::workspace::Workspace;

/// The local server name, as it appears in member rows (`mba22:main`). Shared
/// with the servers band and status line so one machine reads the same name
/// everywhere.
pub(crate) fn local_server_name() -> String {
    crate::app::short_host_name()
}

/// `<server>:<target>` — the uniform member grammar. The single formatter all
/// concrete-checkout rows (local and remote) funnel through.
pub(crate) fn member_label(server: &str, target: &str) -> String {
    format!("{server}:{target}")
}

/// The target half of a local member's label: the branch for a git checkout
/// (the branch IS the label — the two-line name+branch row collapses), else
/// the workspace display name for misc (non-git) workspaces.
pub(crate) fn local_member_target(
    app: &AppState,
    ws: &Workspace,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> String {
    if let Some(name) = &ws.custom_name {
        return name.clone();
    }
    if let Some(branch) = ws.branch() {
        return branch
            .strip_prefix("worktree/")
            .unwrap_or(&branch)
            .to_string();
    }
    ws.display_name_from(&app.terminals, terminal_runtimes)
}

/// The full `<local-server>:<target>` label for a local workspace member row.
pub(crate) fn local_member_label(
    app: &AppState,
    ws: &Workspace,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> String {
    member_label(
        &local_server_name(),
        &local_member_target(app, ws, terminal_runtimes),
    )
}

/// The project identity label for a space row: `owner/repo` derived from the
/// machine-independent project key (`github.com/owner/repo` → `owner/repo`).
/// Origin-less repos (`dir:<name>`) and bare local paths fall back to their
/// trailing segment.
pub(crate) fn project_identity_label(project_key: &str) -> String {
    if let Some(rest) = project_key.strip_prefix("dir:") {
        return rest.to_string();
    }
    // host/owner/repo -> owner/repo; host/repo -> repo; bare -> as-is.
    let mut segments: Vec<&str> = project_key.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        0 => project_key.to_string(),
        1 => segments.remove(0).to_string(),
        // Drop the host segment, keep the remaining owner/repo path.
        _ => segments[1..].join("/"),
    }
}

/// The PR glyph + number for a member row, sharing the pane-header symbol set:
/// open `⊙`, draft `◐`, merged `✓`, closed `✗`. Returns `(text, color)` where
/// `text` is `#<n> <glyph>`.
pub(crate) fn pr_glyph(
    pr: crate::worktree::PrStateInfo,
    p: &crate::app::state::Palette,
) -> (String, ratatui::style::Color) {
    let (glyph, color) = match pr.state {
        crate::worktree::PrState::Open => ("\u{2299}", p.accent),
        crate::worktree::PrState::Draft => ("\u{25d0}", p.overlay0),
        crate::worktree::PrState::Merged => ("\u{2713}", p.mauve),
        crate::worktree::PrState::Closed => ("\u{2717}", p.red),
    };
    (format!("#{} {glyph}", pr.number), color)
}

/// The target half of a remote member's label, from a peer workspace summary:
/// the branch when present, else the remote workspace name (misc).
pub(crate) fn remote_member_target(summary: &crate::api::schema::PeerWorkspaceSummary) -> String {
    summary
        .branch
        .as_deref()
        .unwrap_or(summary.workspace.as_str())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_label_joins_server_and_target() {
        assert_eq!(member_label("mba22", "main"), "mba22:main");
        assert_eq!(
            member_label("sage", "keyboard-shorcuts"),
            "sage:keyboard-shorcuts"
        );
    }

    #[test]
    fn project_identity_strips_host_to_owner_repo() {
        assert_eq!(
            project_identity_label("github.com/gerchowl/herdr"),
            "gerchowl/herdr"
        );
        // Host + single path segment -> that segment.
        assert_eq!(project_identity_label("example.com/herdr"), "herdr");
    }

    #[test]
    fn project_identity_uses_dir_fallback_name() {
        assert_eq!(project_identity_label("dir:scratch"), "scratch");
    }

    #[test]
    fn project_identity_keeps_bare_key() {
        assert_eq!(project_identity_label("herdr"), "herdr");
    }
}
