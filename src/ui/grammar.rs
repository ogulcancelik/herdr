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

/// The agents-panel single-row location string (#62), matching the spaces
/// grammar: `<server> <proj> <target>` (e.g. `mba22 herdr keyboard-shorcuts`).
/// Under width pressure the location truncates right-to-left: the branch/target
/// shrinks first (middle-truncated), then the project, while the server
/// qualifier stays whole so "where" is always answered. Returns the rendered
/// location that fits `max_width` columns; the leading `<icon> <agent> ` is the
/// caller's responsibility and is excluded from `max_width`.
pub(crate) fn agent_location_label(
    server: &str,
    project: Option<&str>,
    target: &str,
    max_width: usize,
) -> String {
    if max_width == 0 {
        return String::new();
    }
    // Assemble server → proj → target, dropping the project segment entirely
    // before sacrificing the server qualifier.
    let mut segments: Vec<&str> = Vec::with_capacity(3);
    segments.push(server);
    if let Some(project) = project.filter(|p| !p.is_empty()) {
        segments.push(project);
    }
    segments.push(target);

    let joined = segments.join(" ");
    if joined.chars().count() <= max_width {
        return joined;
    }

    // Over budget: shrink target first (it carries the least-stable identity),
    // then drop the project, keeping the server whole.
    let sep = 1; // single space between segments
    let server_len = server.chars().count();
    let has_project = segments.len() == 3;

    if has_project {
        let project = segments[1];
        let project_len = project.chars().count();
        // Width left for the target after server + proj + two separators.
        let fixed = server_len + sep + project_len + sep;
        if fixed < max_width {
            let target_budget = max_width - fixed;
            return format!(
                "{server} {project} {}",
                crate::terminal::middle_truncate_chars(target, target_budget)
            );
        }
        // Even a 1-col target won't fit alongside the project: drop the project.
    }

    let fixed = server_len + sep;
    if fixed < max_width {
        let target_budget = max_width - fixed;
        return format!(
            "{server} {}",
            crate::terminal::middle_truncate_chars(target, target_budget)
        );
    }
    // Server alone overflows: middle-truncate the whole thing.
    crate::terminal::middle_truncate_chars(&joined, max_width)
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

    #[test]
    fn agent_location_joins_server_proj_target_when_it_fits() {
        assert_eq!(
            agent_location_label("mba22", Some("herdr"), "keyboard-shorcuts", 80),
            "mba22 herdr keyboard-shorcuts"
        );
    }

    #[test]
    fn agent_location_omits_absent_project() {
        assert_eq!(agent_location_label("sage", None, "main", 80), "sage main");
    }

    #[test]
    fn agent_location_truncates_target_before_project() {
        // Server + project stay whole; the target shrinks (middle-truncated).
        let out = agent_location_label("mba22", Some("herdr"), "keyboard-shorcuts", 20);
        assert!(out.starts_with("mba22 herdr "), "got {out:?}");
        assert!(out.chars().count() <= 20, "got {out:?}");
        assert!(out.contains('…'), "got {out:?}");
    }

    #[test]
    fn agent_location_drops_project_under_hard_pressure() {
        // Too tight for any project segment: drop it, keep server + target.
        let out = agent_location_label("mba22", Some("herdr"), "main", 9);
        assert!(out.starts_with("mba22 "), "got {out:?}");
        assert!(!out.contains("herdr"), "got {out:?}");
        assert!(out.chars().count() <= 9, "got {out:?}");
    }
}
