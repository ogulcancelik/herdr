use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::medallion::{ring_medallion, MedallionStyle};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::state_signal::{
    counts_lead_width, join_states, leading_count_spans, medallion_rings, packed_rects,
    pr_state_glyph, tally_states, StateClass, StateJoin, StateTally,
};
use super::status::{agent_icon, state_dot, state_label, state_label_color};
use crate::app::state::{AgentPanelScope, Palette, PanelScope};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;
/// Rows pinned to the very bottom of the expanded sidebar: a hairline
/// divider over the standalone `menu` row (#41 — the global-menu entry
/// lives last, below the agents section).
pub(crate) const SIDEBAR_MENU_BAND_ROWS: u16 = 2;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub custom_status: Option<String>,
    pub state_labels: std::collections::HashMap<String, String>,
    /// Session-promoted header fields (chips), non-expired, insertion order.
    pub header_fields: Vec<(String, String)>,
    pub live_activity: Option<String>,
}

/// Agent-panel width budget for a promoted field value (header > agent
/// panel > nav list).
const AGENT_PANEL_HEADER_FIELD_VALUE_COLS: usize = 24;

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

/// The expanded sidebar's content rect: everything left of the vertical
/// divider column and its breathing-room gap.
fn expanded_sidebar_content(area: Rect, pane_gap: u16) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(1 + pane_gap),
        area.height,
    )
}

pub(crate) fn expanded_sidebar_sections(
    area: Rect,
    split_ratio: f32,
    pane_gap: u16,
) -> (Rect, Rect) {
    let content = expanded_sidebar_content(area, pane_gap);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    // The pinned menu band at the very bottom is carved out first; the
    // spaces/agents sections split whatever sits above it.
    let sections_h = content.height.saturating_sub(SIDEBAR_MENU_BAND_ROWS);
    let (ws_h, detail_h) = sidebar_section_heights(sections_h, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32, pane_gap: u16) -> Rect {
    let content = expanded_sidebar_content(area, pane_gap);
    let sections_h = content.height.saturating_sub(SIDEBAR_MENU_BAND_ROWS);
    if content.width == 0 || sections_h < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(sections_h, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

/// The standalone `menu` row pinned to the expanded sidebar's last row
/// (#41): servers ─ spaces ─ agents ─ … ─ menu.
pub(crate) fn sidebar_menu_row_rect(area: Rect, pane_gap: u16) -> Rect {
    let content = expanded_sidebar_content(area, pane_gap);
    if content.width == 0 || content.height < SIDEBAR_MENU_BAND_ROWS {
        return Rect::default();
    }
    Rect::new(content.x, content.y + content.height - 1, content.width, 1)
}

/// The hairline divider directly above the pinned `menu` row — the same
/// visual language as the other section boundaries.
pub(crate) fn sidebar_menu_divider_rect(area: Rect, pane_gap: u16) -> Rect {
    let row = sidebar_menu_row_rect(area, pane_gap);
    if row == Rect::default() {
        return Rect::default();
    }
    Rect::new(row.x, row.y - 1, row.width, 1)
}

fn agent_panel_current_workspace_idx(app: &AppState) -> Option<usize> {
    if matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameWorkspace
            | Mode::RenamePane
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
            | Mode::GlobalMenu
            | Mode::KeybindHelp
            | Mode::ProductAnnouncement
    ) {
        Some(app.selected)
    } else {
        app.active
    }
}

fn agent_panel_toggle_label(scope: AgentPanelScope) -> &'static str {
    match scope {
        AgentPanelScope::CurrentWorkspace => "current",
        AgentPanelScope::AllWorkspaces => "all",
    }
}

fn panel_scope_toggle_label(scope: PanelScope) -> &'static str {
    match scope {
        PanelScope::Current => "current",
        PanelScope::All => "all",
    }
}

/// Right-aligned all/current toggle inside a one-row section header — the
/// servers and spaces counterpart of [`agent_panel_toggle_rect`].
pub(crate) fn panel_scope_toggle_rect(header: Rect, scope: PanelScope) -> Rect {
    if header.width == 0 || header.height == 0 {
        return Rect::default();
    }

    let width = panel_scope_toggle_label(scope).chars().count() as u16;
    Rect::new(
        header.x + header.width.saturating_sub(width),
        header.y,
        width.min(header.width),
        1,
    )
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, scope: AgentPanelScope) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_toggle_label(scope);
    let width = label.chars().count() as u16;
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    match app.agent_panel_scope {
        AgentPanelScope::CurrentWorkspace => {
            let Some(ws_idx) = agent_panel_current_workspace_idx(app) else {
                return Vec::new();
            };
            let Some(ws) = app.workspaces.get(ws_idx) else {
                return Vec::new();
            };
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(|detail| AgentPanelEntry {
                    ws_idx,
                    tab_idx: detail.tab_idx,
                    pane_id: detail.pane_id,
                    primary_label: detail.label,
                    primary_tab_label: None,
                    agent_label: None,
                    state: detail.state,
                    seen: detail.seen,
                    custom_status: detail.custom_status,
                    state_labels: detail.state_labels,
                    header_fields: detail.header_fields,
                    live_activity: detail.live_activity,
                })
                .collect()
        }
        AgentPanelScope::AllWorkspaces => app
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                let multi_tab = ws.tabs.len() > 1;
                let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
                ws.pane_details(&app.terminals)
                    .into_iter()
                    .map(move |detail| AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: multi_tab.then_some(detail.tab_label),
                        agent_label: Some(detail.agent_label),
                        state: detail.state,
                        seen: detail.seen,
                        custom_status: detail.custom_status,
                        state_labels: detail.state_labels,
                        header_fields: detail.header_fields,
                        live_activity: detail.live_activity,
                    })
            })
            .collect(),
    }
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn truncate_text(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

fn format_agent_panel_primary_label(entry: &AgentPanelEntry, max_width: usize) -> String {
    let Some(tab_label) = entry.primary_tab_label.as_deref() else {
        return truncate_text(&entry.primary_label, max_width);
    };

    let separator = " · ";
    let separator_width = separator.chars().count();
    if max_width <= separator_width + 2 {
        return truncate_text(
            &format!("{}{}{}", entry.primary_label, separator, tab_label),
            max_width,
        );
    }

    let available = max_width.saturating_sub(separator_width);
    let min_tab = 4.min(available.saturating_sub(1)).max(1);
    let preferred_workspace = ((available * 2) / 3).max(1);
    let mut workspace_budget = preferred_workspace
        .min(available.saturating_sub(min_tab))
        .max(1);
    let mut tab_budget = available.saturating_sub(workspace_budget);

    let workspace_len = entry.primary_label.chars().count();
    let tab_len = tab_label.chars().count();

    if workspace_len < workspace_budget {
        let spare = workspace_budget - workspace_len;
        workspace_budget = workspace_len;
        tab_budget = (tab_budget + spare).min(available.saturating_sub(workspace_budget));
    }
    if tab_len < tab_budget {
        let spare = tab_budget - tab_len;
        tab_budget = tab_len;
        workspace_budget = (workspace_budget + spare).min(available.saturating_sub(tab_budget));
    }

    format!(
        "{}{}{}",
        truncate_text(&entry.primary_label, workspace_budget),
        separator,
        truncate_text(tab_label, tab_budget)
    )
}

fn workspace_row_height(ws: &crate::workspace::Workspace) -> u16 {
    if ws.branch().is_some() {
        2
    } else {
        1
    }
}

/// Member workspaces of one project section (#33), by section key.
fn section_member_indices(app: &AppState, key: &str) -> Vec<usize> {
    app.project_section_keys()
        .into_iter()
        .enumerate()
        .filter(|(_, section)| section.as_deref() == Some(key))
        .map(|(ws_idx, _)| ws_idx)
        .collect()
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    section_member_indices(app, key)
        .into_iter()
        .filter_map(|ws_idx| app.workspaces.get(ws_idx))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| StateClass::of(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

/// The join (severity-sorted top-3 state multiset) of one workspace's panes.
fn workspace_join(app: &AppState, ws: &crate::workspace::Workspace) -> StateJoin {
    join_states(
        ws.pane_states(&app.terminals)
            .map(|(state, seen)| StateClass::of(state, seen)),
    )
}

/// The join across every member workspace of a project section.
fn space_join(app: &AppState, key: &str) -> StateJoin {
    join_states(
        section_member_indices(app, key)
            .into_iter()
            .filter_map(|ws_idx| app.workspaces.get(ws_idx))
            .flat_map(|ws| ws.pane_states(&app.terminals))
            .map(|(state, seen)| StateClass::of(state, seen)),
    )
}

/// The full state tally across every local workspace — the self server-row
/// leading counts (#42: `0 2 1 herdr`).
fn local_server_tally(app: &AppState) -> StateTally {
    tally_states(
        app.workspaces
            .iter()
            .flat_map(|ws| ws.pane_states(&app.terminals))
            .map(|(state, seen)| StateClass::of(state, seen)),
    )
}

/// The tally across a peer's workspace summaries (one status per workspace —
/// the granularity the peer protocol carries).
fn peer_tally(peer: &crate::peers::PeerSummaryState) -> StateTally {
    tally_states(
        peer.workspaces
            .iter()
            .map(|ws| StateClass::of_remote(ws.status)),
    )
}

/// Pane counts per state bucket across a project section's members, in
/// attention-priority order: blocked, done (unseen idle), working, idle.
fn space_state_counts(app: &AppState, key: &str) -> Vec<(AgentState, bool, usize)> {
    let mut blocked = 0usize;
    let mut done = 0usize;
    let mut working = 0usize;
    let mut idle = 0usize;
    for ws in section_member_indices(app, key)
        .into_iter()
        .filter_map(|ws_idx| app.workspaces.get(ws_idx))
    {
        for (state, seen) in ws.pane_states(&app.terminals) {
            match (state, seen) {
                (AgentState::Blocked, _) => blocked += 1,
                (AgentState::Idle, false) => done += 1,
                (AgentState::Working, _) => working += 1,
                (AgentState::Idle, true) => idle += 1,
                (AgentState::Unknown, _) => {}
            }
        }
    }
    [
        (AgentState::Blocked, true, blocked),
        (AgentState::Idle, false, done),
        (AgentState::Working, true, working),
        (AgentState::Idle, true, idle),
    ]
    .into_iter()
    .filter(|(_, _, count)| *count > 0)
    .collect()
}

/// The group affordances (key + collapsed flag) a workspace row carries —
/// only the section's PRIMARY row (the first non-linked member, i.e. the
/// main checkout) of a multi-member project section gets them (#33).
pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let key = app.project_section_key(ws_idx)?;
    let members = section_member_indices(app, &key);
    let primary = members
        .iter()
        .copied()
        .find(|idx| !app.workspaces[*idx].is_linked_checkout())?;
    (primary == ws_idx && members.len() >= 2)
        .then(|| (key.clone(), app.collapsed_space_keys.contains(&key)))
}

pub(super) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    /// A workspace on a federated peer server, folded into the project group
    /// it shares with local checkouts (indented) or trailing the list as its
    /// own project (unindented). Selecting one switches servers. Fed from
    /// config-peer summaries AND carried fleet-snapshot entries (#46).
    Remote {
        peer: crate::app::state::RemotePeerRef,
        ws_idx: usize,
        indented: bool,
    },
}

fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(
            WorkspaceListEntry::Workspace { indented: true, .. }
                | WorkspaceListEntry::Remote { indented: true, .. }
        )
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(
        area,
        app.sidebar_section_split,
        app.sidebar_pane_gap,
        servers_section_height(app),
    );
    let body = workspace_list_body_rect(ws_area, false, app.sidebar_new_entry_visible());
    if body.height == 0 {
        return requested;
    }

    let entry_count = workspace_list_entries(app).len();
    if entry_count == 0 {
        0
    } else {
        requested.min(entry_count.saturating_sub(1))
    }
}

/// The spaces list, sectioned git-first by project (#33):
///
/// - Git workspaces group into project sections ([`AppState::project_section_keys`]):
///   the main checkout is the section's primary row, every other member
///   (linked worktrees AND plain same-repo checkouts) indents under it.
///   Sections appear in workspace storage order of their first member.
/// - Workspaces whose git identity probe hasn't finished yet ("pending")
///   hold their storage position as plain rows — they never flash into
///   `misc` only to jump into a project section a sweep later.
/// - Resolved non-git workspaces collect at the tail as the positional
///   `misc` section (after remote-only project groups). No synthetic header
///   row: like the project sections themselves, the section is its rows.
pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    let section_keys = app.project_section_keys();
    let mut members_by_key = std::collections::HashMap::<&str, Vec<usize>>::new();
    for (ws_idx, key) in section_keys.iter().enumerate() {
        if let Some(key) = key.as_deref() {
            members_by_key.entry(key).or_default().push(ws_idx);
        }
    }
    let grouped_keys = app.collapsible_space_keys();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| section_keys.get(idx).cloned().flatten());

    let mut emitted_groups = std::collections::HashSet::<&str>::new();
    let mut entries = Vec::new();
    let mut misc = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(key) = section_keys[ws_idx].as_deref() else {
            if ws.git_identity_pending() {
                // Pending probe: hold position among the git sections.
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                });
            } else {
                misc.push(ws_idx);
            }
            continue;
        };
        if !grouped_keys.contains(key) {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        }

        if !emitted_groups.insert(key) {
            continue;
        }

        let Some(members) = members_by_key.get(key) else {
            continue;
        };
        let Some(parent_idx) = members
            .iter()
            .copied()
            .find(|idx| !app.workspaces[*idx].is_linked_checkout())
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = app.collapsed_space_keys.contains(key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(key))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    if matches!(app.spaces_panel_scope, PanelScope::Current) {
        retain_focused_space_group(
            app,
            &section_keys,
            &mut entries,
            visible_group_idx,
            active_group.as_deref(),
            &grouped_keys,
        );
    }
    // The per-server filter (#46) has the final say, in this single source
    // of the rendered list so hit-areas, scroll, and selection clamp stay
    // consistent. `Local` keeps the local entries and folds no remote rows.
    // `Peer` replaces the list with that one peer's rows. Without a filter
    // every federated peer folds in as usual.
    let peer_filtered = match app.server_filter.as_ref() {
        Some(crate::app::state::ServerFilter::Local) => false,
        Some(crate::app::state::ServerFilter::Peer { ssh_target }) => {
            entries = single_peer_entries(app, ssh_target);
            true
        }
        None => {
            fold_remote_entries(app, &mut entries);
            false
        }
    };
    // The trailing `misc` section: resolved non-git workspaces, after every
    // git project (local AND remote-only) — git projects first, misc last.
    // A peer filter replaces the whole list; scope current pins the list to
    // the focused row, so misc only renders when it IS the focused row.
    if !peer_filtered {
        for ws_idx in misc {
            if matches!(app.spaces_panel_scope, PanelScope::Current)
                && visible_group_idx != Some(ws_idx)
            {
                continue;
            }
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
        }
    }
    entries
}

/// Server filter `only <peer>`: the spaces list becomes that peer's remote
/// rows alone, grouped by project — leader row unindented (carries the
/// project label), members indented — exactly like remote-only trailing
/// groups. An unresolvable target (peer dropped from config, snapshot
/// replaced) renders an empty list rather than silently un-filtering; the
/// scope toggle or the context menu clears the filter.
fn single_peer_entries(app: &AppState, ssh_target: &str) -> Vec<WorkspaceListEntry> {
    let Some((peer_ref, peer)) = app
        .remote_peers()
        .into_iter()
        .find(|(_, peer)| peer.ssh_target == ssh_target)
    else {
        return Vec::new();
    };

    let mut by_project: Vec<(&str, Vec<usize>)> = Vec::new();
    for (ws_idx, ws) in peer.workspaces.iter().enumerate() {
        let Some(project_key) = ws.project_key.as_deref() else {
            continue;
        };
        match by_project.iter_mut().find(|(key, _)| *key == project_key) {
            Some((_, rows)) => rows.push(ws_idx),
            None => by_project.push((project_key, vec![ws_idx])),
        }
    }

    let mut entries = Vec::new();
    for (_, rows) in by_project {
        for (offset, ws_idx) in rows.into_iter().enumerate() {
            entries.push(WorkspaceListEntry::Remote {
                peer: peer_ref,
                ws_idx,
                indented: offset > 0,
            });
        }
    }
    entries
}

/// Spaces scope `current`: keep only the focused workspace's space-group
/// block — or just the focused workspace itself when it is not part of a
/// collapsible group. Filtering happens here, in the single source of the
/// rendered list, so hit-areas, scroll, and keyboard selection all stay
/// consistent with what is on screen. With no focused workspace the full
/// list stays (nothing to pin to).
fn retain_focused_space_group(
    app: &AppState,
    section_keys: &[Option<String>],
    entries: &mut Vec<WorkspaceListEntry>,
    focused_idx: Option<usize>,
    active_group: Option<&str>,
    grouped_keys: &std::collections::HashSet<String>,
) {
    let Some(focused_idx) = focused_idx.filter(|idx| *idx < app.workspaces.len()) else {
        return;
    };
    let focused_key = active_group.filter(|key| grouped_keys.contains(*key));
    entries.retain(|entry| match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => match focused_key {
            Some(key) => section_keys
                .get(*ws_idx)
                .is_some_and(|section| section.as_deref() == Some(key)),
            None => *ws_idx == focused_idx,
        },
        // Remote rows are folded in after this filter runs.
        WorkspaceListEntry::Remote { .. } => false,
    });
}

/// Fold federated peer workspaces into the spaces list: rows whose
/// project_key matches a local checkout splice in (indented) after that
/// project's block; remote-only projects trail the list grouped together.
/// Remote rows of a collapsed local group stay hidden with it. Peers come
/// from [`AppState::remote_peers`]: config-peer summaries first, then any
/// carried fleet-snapshot entries a config peer does not shadow (#46).
fn fold_remote_entries(app: &AppState, entries: &mut Vec<WorkspaceListEntry>) {
    use crate::app::state::RemotePeerRef;
    // project_key -> remote rows, in (peer order, summary order).
    let mut remotes_by_project =
        std::collections::HashMap::<&str, Vec<(RemotePeerRef, usize)>>::new();
    let mut project_order = Vec::<&str>::new();
    for (peer_ref, peer) in app.remote_peers() {
        for (ws_idx, ws) in peer.workspaces.iter().enumerate() {
            let Some(project_key) = ws.project_key.as_deref() else {
                continue;
            };
            let rows = remotes_by_project.entry(project_key).or_insert_with(|| {
                project_order.push(project_key);
                Vec::new()
            });
            rows.push((peer_ref, ws_idx));
        }
    }
    if remotes_by_project.is_empty() {
        return;
    }

    // Last entry index of each local project's block, and whether that block
    // is a collapsed project group (remote rows hide with it).
    let section_keys = app.project_section_keys();
    let mut block_end = std::collections::HashMap::<&str, usize>::new();
    let mut collapsed_projects = std::collections::HashSet::<&str>::new();
    for (entry_idx, entry) in entries.iter().enumerate() {
        let WorkspaceListEntry::Workspace { ws_idx, .. } = entry else {
            continue;
        };
        let Some(ws) = app.workspaces.get(*ws_idx) else {
            continue;
        };
        let Some(project_key) = ws.project_key() else {
            continue;
        };
        block_end.insert(project_key, entry_idx);
        if section_keys
            .get(*ws_idx)
            .and_then(|section| section.as_deref())
            .is_some_and(|section| app.collapsed_space_keys.contains(section))
        {
            collapsed_projects.insert(project_key);
        }
    }

    // Splice matched projects back-to-front so earlier indices stay valid.
    let mut matched = project_order
        .iter()
        .filter_map(|project_key| block_end.get(project_key).map(|end| (*end, *project_key)))
        .collect::<Vec<_>>();
    matched.sort_by_key(|(end, _)| std::cmp::Reverse(*end));
    for (end, project_key) in matched {
        if collapsed_projects.contains(project_key) {
            continue;
        }
        let rows = &remotes_by_project[project_key];
        for (offset, (peer_ref, ws_idx)) in rows.iter().enumerate() {
            entries.insert(
                end + 1 + offset,
                WorkspaceListEntry::Remote {
                    peer: *peer_ref,
                    ws_idx: *ws_idx,
                    indented: true,
                },
            );
        }
    }

    // Spaces scope current pins the list to the focused project: remote-only
    // projects (no local block to splice into) stay hidden with the rest.
    if matches!(app.spaces_panel_scope, PanelScope::Current) {
        return;
    }

    // Remote-only projects trail the list; the first row of each project is
    // unindented and labels the project, the rest indent under it.
    for project_key in project_order {
        if block_end.contains_key(project_key) {
            continue;
        }
        for (offset, (peer_ref, ws_idx)) in remotes_by_project[project_key].iter().enumerate() {
            entries.push(WorkspaceListEntry::Remote {
                peer: *peer_ref,
                ws_idx: *ws_idx,
                indented: offset > 0,
            });
        }
    }
}

/// Status dot for remote rows: the same shape language as the local
/// `state_dot` (`●` live signal, `○` settled idle, `·` none), colored by
/// the shared severity mapping.
fn remote_status_dot(
    status: crate::api::schema::AgentStatus,
    p: &crate::app::state::Palette,
) -> (&'static str, Style) {
    use crate::api::schema::AgentStatus;
    let glyph = match status {
        AgentStatus::Blocked | AgentStatus::Working | AgentStatus::Done => "●",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "·",
    };
    (
        glyph,
        Style::default().fg(StateClass::of_remote(status).color(p)),
    )
}

/// Display name of the active server filter for the spaces header, if one
/// is set: the local host name, or the filtered peer's host (ssh target
/// when the peer no longer resolves — the filter still narrows the list).
fn server_filter_label(app: &AppState) -> Option<String> {
    match app.server_filter.as_ref()? {
        crate::app::state::ServerFilter::Local => Some(crate::app::short_host_name()),
        crate::app::state::ServerFilter::Peer { ssh_target } => Some(
            app.remote_peers()
                .into_iter()
                .find(|(_, peer)| peer.ssh_target == *ssh_target)
                .map(|(_, peer)| peer.host.clone().unwrap_or_else(|| peer.peer.clone()))
                .unwrap_or_else(|| ssh_target.clone()),
        ),
    }
}

/// Display label for a remote row: `host:branch` (matched/indented rows) or
/// `project · host:branch` for rows that lead a remote-only project group.
pub(crate) fn remote_entry_label(
    app: &AppState,
    peer_ref: crate::app::state::RemotePeerRef,
    ws_idx: usize,
    indented: bool,
) -> String {
    let Some(peer) = app.remote_peer(peer_ref) else {
        return String::new();
    };
    let Some(ws) = peer.workspaces.get(ws_idx) else {
        return String::new();
    };
    let host = peer.host.as_deref().unwrap_or(peer.peer.as_str());
    let target = ws.branch.as_deref().unwrap_or(ws.workspace.as_str());
    if indented {
        format!("{host}:{target}")
    } else {
        let project = ws.project_label.as_deref().unwrap_or(ws.workspace.as_str());
        format!("{project} · {host}:{target}")
    }
}

/// Max peer rows shown in the `servers` section before it stops growing
/// (extra peers still poll; the section just caps its height).
const SERVERS_SECTION_MAX_ROWS: u16 = 8;

/// Every server row (self and peers) renders as two lines: identity on the
/// first, compact health on the second.
const SERVER_ROW_LINES: u16 = 2;

/// The band's two-line rows in render order: the home/origin row pinned
/// first when the attached client carried a fleet snapshot, then the local
/// server (`None` — it never gets a switch hit-area), then the carried
/// snapshot rows, then the server's own configured peers. A locally
/// attached client has no snapshot: just self + config peers, as before.
fn server_band_slots(app: &AppState) -> Vec<Option<crate::app::state::PeerSwitchRequest>> {
    use crate::app::state::PeerSwitchRequest;
    let mut slots = Vec::new();
    if let Some(snapshot) = app.fleet_snapshot.as_ref() {
        slots.push(Some(PeerSwitchRequest::Home));
        slots.push(None);
        slots.extend(
            (0..snapshot.peers.len())
                .map(|entry_idx| Some(PeerSwitchRequest::SnapshotPeer { entry_idx })),
        );
    } else {
        slots.push(None);
    }
    slots.extend((0..app.peer_summaries.len()).map(|peer_idx| {
        Some(PeerSwitchRequest::ConfigPeer {
            peer_idx,
            ws_idx: 0,
        })
    }));
    slots
}

/// The band rows actually rendered, honoring the servers scope toggle:
/// `all` keeps every slot, `current` only the local server — plus the home
/// row when a fleet snapshot origin exists, because the way home must never
/// hide.
fn visible_server_band_slots(app: &AppState) -> Vec<Option<crate::app::state::PeerSwitchRequest>> {
    let slots = server_band_slots(app);
    match app.servers_panel_scope {
        PanelScope::All => slots,
        PanelScope::Current => slots
            .into_iter()
            .filter(|slot| {
                matches!(
                    slot,
                    None | Some(crate::app::state::PeerSwitchRequest::Home)
                )
            })
            .collect(),
    }
}

/// Height the `servers` section wants: 0 with nothing but the self row,
/// else a header row plus two lines per visible server row (capped) plus
/// the trailing hairline divider that separates `servers` from `spaces`.
pub(crate) fn servers_section_height(app: &AppState) -> u16 {
    if server_band_slots(app).len() <= 1 {
        return 0;
    }
    let rows = (visible_server_band_slots(app).len() as u16).min(SERVERS_SECTION_MAX_ROWS);
    1 + rows * SERVER_ROW_LINES + 1
}

/// The band minus its trailing divider row: the header plus the server rows.
fn server_band_rows_area(area: Rect) -> Rect {
    Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1))
}

/// Split the spaces-section rect into the `servers` band (top) and the
/// workspace-list area (below). The band never takes more than half the
/// section, so the workspace list always keeps room.
pub(crate) fn carve_servers_band(ws_area: Rect, servers_height: u16) -> (Rect, Rect) {
    if servers_height == 0 || ws_area.height == 0 {
        return (Rect::default(), ws_area);
    }
    let band_h = servers_height.min(ws_area.height / 2);
    if band_h == 0 {
        return (Rect::default(), ws_area);
    }
    let servers_area = Rect::new(ws_area.x, ws_area.y, ws_area.width, band_h);
    let list_area = Rect::new(
        ws_area.x,
        ws_area.y + band_h,
        ws_area.width,
        ws_area.height - band_h,
    );
    (servers_area, list_area)
}

pub(crate) fn workspace_list_rect(
    area: Rect,
    split_ratio: f32,
    pane_gap: u16,
    servers_height: u16,
) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio, pane_gap);
    carve_servers_band(ws_area, servers_height).1
}

/// The spaces list's row area: below the header rows, above the `new`
/// footer row when one is reserved (`has_footer` — hidden in workspace
/// tab-mode, where the slot returns to the list).
pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool, has_footer: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let body_bottom = (area.y + area.height).saturating_sub(u16::from(has_footer));
    let body_height = body_bottom.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false, app.sidebar_new_entry_visible());
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let needed = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(ws)
                };
                let gap = if *indented && next_entry_is_indented_workspace(&entries, entry_idx) {
                    0
                } else {
                    app.sidebar_row_gap
                };
                row_height.saturating_add(gap)
            }
            WorkspaceListEntry::Remote { indented, .. } => {
                let gap = if *indented && next_entry_is_indented_workspace(&entries, entry_idx) {
                    0
                } else {
                    app.sidebar_row_gap
                };
                1u16.saturating_add(gap)
            }
        };
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        visible += 1;
    }
    visible
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let entries = workspace_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.workspace_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true, app.sidebar_new_entry_visible());
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn agent_panel_visible_count(area: Rect, row_gap: u16) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height < 2 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    while used_rows.saturating_add(2) <= body.height {
        used_rows = used_rows.saturating_add(2);
        visible += 1;
        if used_rows < body.height {
            used_rows = used_rows.saturating_add(row_gap);
        }
    }
    visible
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let viewport_rows = agent_panel_visible_count(area, app.sidebar_row_gap);
    let total_rows = agent_panel_entries(app).len();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.agent_panel_scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (
    Vec<crate::app::state::WorkspaceCardArea>,
    Vec<crate::app::state::RemoteCardArea>,
) {
    let ws_area = workspace_list_rect(
        area,
        app.sidebar_section_split,
        app.sidebar_pane_gap,
        servers_section_height(app),
    );
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(
        ws_area,
        should_show_scrollbar(metrics),
        app.sidebar_new_entry_visible(),
    );
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let mut remote_cards = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(ws)
                };
                let gap = if *indented && next_entry_is_indented_workspace(&entries, entry_idx) {
                    0
                } else {
                    app.sidebar_row_gap
                };
                if row_y.saturating_add(row_height).saturating_add(gap) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                row_y = row_y.saturating_add(row_height + gap);
            }
            WorkspaceListEntry::Remote {
                peer,
                ws_idx,
                indented,
            } => {
                let gap = if *indented && next_entry_is_indented_workspace(&entries, entry_idx) {
                    0
                } else {
                    app.sidebar_row_gap
                };
                if row_y.saturating_add(1).saturating_add(gap) > body_bottom {
                    break;
                }
                remote_cards.push(crate::app::state::RemoteCardArea {
                    peer: *peer,
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, 1),
                    indented: *indented,
                });
                row_y = row_y.saturating_add(1 + gap);
            }
        }
    }

    (cards, remote_cards)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect, pane_gap: u16) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(1 + pane_gap),
        area.height,
    );
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.divider_color())
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area, app.sidebar_pane_gap);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.divider_color()));
        }
    }

    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };
    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        if let Some(ws_idx) = detail_ws_idx {
            if let Some(ws) = app.workspaces.get(ws_idx) {
                for (detail_idx, detail) in ws.pane_details(&app.terminals).iter().enumerate() {
                    let y = detail_content_area.y + detail_idx as u16;
                    if y >= detail_content_area.y + detail_content_area.height {
                        break;
                    }
                    let pane_num = ws
                        .public_pane_number(detail.pane_id)
                        .unwrap_or(detail_idx + 1);
                    let pane_style = Style::default().fg(p.overlay0);
                    let (icon, icon_style) =
                        agent_icon(detail.state, detail.seen, app.spinner_tick, p);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("{pane_num}"), pane_style),
                            Span::styled(" ", pane_style),
                            Span::styled(icon, icon_style),
                        ])),
                        Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
                    );
                }
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
    has_footer: bool,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = (area.y + area.height).saturating_sub(u16::from(has_footer));

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.divider_color())
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) =
        expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);

    let (servers_area, list_area) = carve_servers_band(ws_area, servers_section_height(app));
    if servers_area != Rect::default() {
        render_servers_section(app, frame, servers_area, is_navigating);
    }
    render_workspace_list(app, terminal_runtimes, frame, list_area, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, detail_area);
    render_menu_row(app, frame, area);
    render_sidebar_toggle(app, frame, area, false, p);
}

/// Rect of the two-line server row at `slot` (0 = the local server, then
/// one slot per peer) inside the band, or `None` when it does not fully fit.
fn server_slot_rect(servers_area: Rect, slot: u16) -> Option<Rect> {
    let y = servers_area
        .y
        .checked_add(1 + slot.checked_mul(SERVER_ROW_LINES)?)?;
    (y + SERVER_ROW_LINES <= servers_area.y + servers_area.height)
        .then(|| Rect::new(servers_area.x, y, servers_area.width, SERVER_ROW_LINES))
}

/// Compute hit areas for the `servers` section: the header rect (hosts the
/// all/current scope toggle) and one two-line rect per visible switchable
/// row (home, snapshot, config peer — see [`server_band_slots`] for the
/// order). The local server's slot deliberately gets NO card — clicking
/// yourself must never request a server switch.
pub(crate) fn compute_server_section_areas(
    app: &AppState,
    area: Rect,
) -> (Rect, Vec<crate::app::state::ServerCardArea>) {
    let (servers_area, _) = carve_servers_band(
        workspace_list_rect(area, app.sidebar_section_split, app.sidebar_pane_gap, 0),
        servers_section_height(app),
    );
    if servers_area == Rect::default() || servers_area.height == 0 {
        return (Rect::default(), Vec::new());
    }
    let header_rect = Rect::new(servers_area.x, servers_area.y, servers_area.width, 1);
    let rows_area = server_band_rows_area(servers_area);
    let mut cards = Vec::new();
    for (slot, target) in visible_server_band_slots(app).into_iter().enumerate() {
        // The self row (None) gets no card.
        let Some(target) = target else {
            continue;
        };
        let Some(rect) = server_slot_rect(rows_area, slot as u16) else {
            break;
        };
        cards.push(crate::app::state::ServerCardArea { target, rect });
    }
    (header_rect, cards)
}

/// The servers-band row under (col, row): `Some(None)` is the local self
/// row, `Some(Some(target))` a switchable row. Right-click uses this for
/// the per-server spaces filter (#46) — unlike the left-click cards, the
/// self row matters here, so it cannot reuse `compute_server_section_areas`
/// (which deliberately gives self no hit-area).
pub(crate) fn server_band_slot_at(
    app: &AppState,
    area: Rect,
    col: u16,
    row: u16,
) -> Option<Option<crate::app::state::PeerSwitchRequest>> {
    let (servers_area, _) = carve_servers_band(
        workspace_list_rect(area, app.sidebar_section_split, app.sidebar_pane_gap, 0),
        servers_section_height(app),
    );
    if servers_area == Rect::default() || servers_area.height == 0 {
        return None;
    }
    let rows_area = server_band_rows_area(servers_area);
    for (slot, target) in visible_server_band_slots(app).into_iter().enumerate() {
        let Some(rect) = server_slot_rect(rows_area, slot as u16) else {
            break;
        };
        if col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
        {
            return Some(target);
        }
    }
    None
}

fn render_servers_section(app: &AppState, frame: &mut Frame, area: Rect, is_navigating: bool) {
    let p = &app.palette;
    let down = app
        .peer_summaries
        .iter()
        .filter(|peer| peer.reachability() == crate::peers::PeerReachability::Down)
        .count();
    let header = if down > 0 {
        format!(" servers ({down} down)")
    } else {
        " servers".to_string()
    };
    let header_rect = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            header,
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        header_rect,
    );
    let toggle_rect = panel_scope_toggle_rect(header_rect, app.servers_panel_scope);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                panel_scope_toggle_label(app.servers_panel_scope),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }
    let _ = is_navigating;

    // Hairline divider on the band's last row: the same visual language as
    // the spaces↔agents divider below.
    if area.height > 1 {
        let divider_y = area.y + area.height - 1;
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().fg(p.divider_color()),
            )),
            Rect::new(area.x, divider_y, area.width, 1),
        );
    }

    let rows_area = server_band_rows_area(area);
    // Two phases: build every visible row first so the count columns share a
    // band-GLOBAL digit width (one server hitting 10 widens every row), then
    // paint.
    let mut prepared = Vec::new();
    for (slot, target) in visible_server_band_slots(app).into_iter().enumerate() {
        let Some(rect) = server_slot_rect(rows_area, slot as u16) else {
            break;
        };
        // The currently-attached machine reads like the active workspace row —
        // the standard highlight fill, one visual language for "current"
        // across workspaces, agents, and servers.
        let is_current = target.is_none();
        let (rows, tally, ghosted) = match target {
            // The local server: no hit-area, anchors the band.
            None => self_server_rows(app),
            Some(crate::app::state::PeerSwitchRequest::Home) => {
                let Some(snapshot) = app.fleet_snapshot.as_ref() else {
                    continue;
                };
                (home_server_rows(snapshot, p), None, false)
            }
            Some(crate::app::state::PeerSwitchRequest::SnapshotPeer { entry_idx }) => {
                let Some(peer) = app
                    .fleet_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.peers.get(entry_idx))
                else {
                    continue;
                };
                snapshot_server_rows(peer, p)
            }
            Some(crate::app::state::PeerSwitchRequest::ConfigPeer { peer_idx, .. }) => {
                let Some(peer) = app.peer_summaries.get(peer_idx) else {
                    continue;
                };
                peer_server_rows(peer, p)
            }
        };
        prepared.push((rect, is_current, rows, tally, ghosted));
    }
    let digit_width = prepared
        .iter()
        .filter_map(|(_, _, _, tally, _)| tally.as_ref())
        .map(StateTally::digit_width)
        .max()
        .unwrap_or(1);
    for (rect, is_current, rows, tally, ghosted) in prepared {
        if is_current {
            let buf = frame.buffer_mut();
            let fill = Style::default().bg(p.surface_dim);
            for y in rect.y..rect.y.saturating_add(2).min(area.y + area.height) {
                for x in rect.x..rect.x.saturating_add(rect.width) {
                    buf[(x, y)].set_style(fill);
                }
            }
        }
        // The medallion's base_bg must be the row's ACTUAL fill so it
        // composes with the current-row highlight.
        let base_bg = if is_current {
            p.surface_dim
        } else {
            ratatui::style::Color::Reset
        };
        let rows = match tally {
            Some(tally) => match configured_medallion_style(app) {
                None => with_leading_counts(rows, &tally, digit_width, ghosted, p),
                Some(style) => with_leading_medallion(rows, &tally.join(), base_bg, style, p),
            },
            None => rows,
        };
        render_server_rows(frame, rect, rows);
    }
}

/// The configured medallion raster: `[ui] medallion_style` ("sextant"
/// default, "quadrant" for fonts without sextant coverage).
fn configured_medallion_style(app: &AppState) -> Option<MedallionStyle> {
    match app.server_state_mark {
        crate::config::ServerStateMarkConfig::Counts => None,
        crate::config::ServerStateMarkConfig::MedallionSextant => Some(MedallionStyle::Sextant),
        crate::config::ServerStateMarkConfig::MedallionQuadrant => Some(MedallionStyle::Quadrant),
    }
}

/// Prepend the ring medallion (the band's leading state mark, #42) to a
/// two-line server row: rings = the row's join (outer→inner), then a gap
/// column so the name/health columns line up with the non-medallion rows'
/// three-column lead-in.
fn with_leading_medallion(
    [title, health]: [Line<'static>; 2],
    join: &StateJoin,
    base_bg: ratatui::style::Color,
    style: MedallionStyle,
    p: &Palette,
) -> [Line<'static>; 2] {
    let rings = medallion_rings(join, p);
    let [top, bottom] = ring_medallion(&rings, base_bg, style);
    let lead = |mut mark: Vec<Span<'static>>, rest: Line<'static>| {
        mark.push(Span::styled(" ", Style::default()));
        mark.extend(rest.spans);
        Line::from(mark)
    };
    [lead(top, title), lead(bottom, health)]
}

/// One band row as built by the slot builders: the two lines, the state
/// tally feeding the leading counts (None = no counts: home row), and
/// whether the row is a ghost (unreachable — counts grey out with it).
type ServerRowBuild = ([Line<'static>; 2], Option<StateTally>, bool);

/// Prepend the leading count columns (#42 final form: `0 2 1 herdr`) to a
/// two-line server row: title gets the counts, the health line indents by
/// the same width so both stay aligned across the band.
fn with_leading_counts(
    [title, health]: [Line<'static>; 2],
    tally: &StateTally,
    digit_width: usize,
    ghosted: bool,
    p: &Palette,
) -> [Line<'static>; 2] {
    let mut lead = leading_count_spans(tally, digit_width, ghosted, p);
    lead.extend(title.spans);
    let mut indent = vec![Span::styled(
        " ".repeat(counts_lead_width(digit_width)),
        Style::default(),
    )];
    indent.extend(health.spans);
    [Line::from(lead), Line::from(indent)]
}

/// Paint a two-line server row into its slot rect, clamping each line.
fn render_server_rows(frame: &mut Frame, rect: Rect, [title, health]: [Line<'static>; 2]) {
    frame.render_widget(
        Paragraph::new(clamp_line(title, rect.width)),
        Rect::new(rect.x, rect.y, rect.width, 1),
    );
    frame.render_widget(
        Paragraph::new(clamp_line(health, rect.width)),
        Rect::new(rect.x, rect.y + 1, rect.width, 1),
    );
}

/// Indentation that lines the health glyphs up under the server name for
/// rows WITHOUT a leading medallion (home, unreachable peers) — the same
/// three columns the medallion + gap occupy.
const SERVER_HEALTH_INDENT: &str = "    ";

/// The local server's row: the leading state medallion (the join of every
/// local agent state, #42), then `mba22 ✦` plus battery and net throughput
/// on the title line (#41 — peers don't carry those), over the shared
/// fixed-width metric line, all fed from the same local `SystemStats`
/// sample the HUD shows.
fn self_server_rows(app: &AppState) -> ServerRowBuild {
    use super::status::{battery_icon, battery_style, format_net_io, push_band_metric};
    let p = &app.palette;
    let mut title = vec![
        Span::styled(
            crate::app::short_host_name(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ✦", Style::default().fg(p.accent)),
    ];

    let mut health: Vec<Span<'static>> = Vec::new();
    if let Some(stats) = app.system_stats.as_ref() {
        if let Some(percent) = stats.battery_percent {
            push_band_metric(
                &mut title,
                battery_icon(percent, stats.battery_charging),
                format!("{percent}%"),
                battery_style(percent, p),
                p,
            );
        }
        if let (Some(rx), Some(tx)) = (stats.net_rx_per_sec, stats.net_tx_per_sec) {
            push_band_metric(
                &mut title,
                "\u{f06f3}", // nf-md-network
                format_net_io(rx, tx),
                Style::default().fg(p.teal),
                p,
            );
        }
        health = server_health_spans(
            stats.cpu_percent,
            stats.mem_used,
            stats.mem_total,
            stats.disk_free,
            stats.gpu_percent.map(f32::from),
            p,
        );
    }
    (
        [Line::from(title), Line::from(health)],
        Some(local_server_tally(app)),
        false,
    )
}

/// The pinned origin row of a carried fleet snapshot: `← mba22 home` over a
/// dim snapshot-age line. Selecting it re-attaches the client locally.
fn home_server_rows(
    snapshot: &crate::peers::FleetSnapshotState,
    p: &crate::app::state::Palette,
) -> [Line<'static>; 2] {
    let title = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled("←", Style::default().fg(p.accent)),
        Span::styled(" ", Style::default()),
        Span::styled(
            snapshot.origin.clone(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" home", Style::default().fg(p.accent)),
    ]);
    let health = Line::from(vec![
        Span::styled(SERVER_HEALTH_INDENT, Style::default()),
        Span::styled(
            format!(
                "snapshot {} old",
                format_age(snapshot.received_at.elapsed().as_secs())
            ),
            Style::default().fg(p.overlay0),
        ),
    ]);
    [title, health]
}

/// A carried fleet-snapshot row: the regular peer row plus an explicit
/// staleness-age chip. These rows are render-only — the server never polls
/// them, so their age only grows until the next leap refreshes the snapshot.
fn snapshot_server_rows(
    peer: &crate::peers::PeerSummaryState,
    p: &crate::app::state::Palette,
) -> ServerRowBuild {
    let ([mut title, health], tally, ghosted) = peer_server_rows(peer, p);
    // The down (ghost) form already carries the broken-link icon + age.
    if peer.reachability() != crate::peers::PeerReachability::Down {
        if let Some(at) = peer.last_ok {
            title.spans.push(Span::styled(
                format!("  {} old", format_age(at.elapsed().as_secs())),
                Style::default().fg(p.overlay0),
            ));
        }
    }
    ([title, health], tally, ghosted)
}

/// One peer's two `servers` lines: the leading state medallion (the join of
/// the peer's workspace statuses, #42) before `anvil  34ms` over the band's
/// fixed-width metric line — or the compact `unreachable {age}` form with a
/// muted hollow dot when the peer is down (shape = reachability).
fn peer_server_rows(
    peer: &crate::peers::PeerSummaryState,
    p: &crate::app::state::Palette,
) -> ServerRowBuild {
    use crate::peers::PeerReachability;
    let reach = peer.reachability();
    let host = peer.host.clone().unwrap_or_else(|| peer.peer.clone());

    if reach == PeerReachability::Down {
        // Unreachable = the GHOST of the normal row (#42 refinement): the
        // hollow no-conn dot, then the name and the LAST-KNOWN stats — all
        // greyed out and italic, the title struck through, the age where
        // latency would sit. The stale data stays visible; its styling says
        // "as of {age}".
        // nf-md-link_off in the latency slot says no-conn; the age says how
        // stale the ghosted stats are.
        let age = match peer.last_ok {
            Some(at) => format!("  \u{f0337} {}", format_age(at.elapsed().as_secs())),
            None => "  \u{f0337}".to_string(),
        };
        let title = Line::from(vec![
            Span::styled(
                "○ ",
                Style::default()
                    .fg(p.overlay0)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                host,
                Style::default()
                    .fg(p.overlay0)
                    .add_modifier(Modifier::ITALIC | Modifier::CROSSED_OUT),
            ),
            Span::styled(
                age,
                Style::default()
                    .fg(p.overlay0)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]);
        let mut health: Vec<Span<'static>> = Vec::new();
        if let Some(system) = peer.system.as_ref() {
            health = server_health_spans(
                system.cpu_percent.map(f32::from),
                system.mem_used,
                system.mem_total,
                system.disk_free,
                None,
                p,
            );
            for span in &mut health {
                span.style = Style::default()
                    .fg(p.overlay0)
                    .add_modifier(Modifier::ITALIC);
            }
        }
        return ([title, Line::from(health)], Some(peer_tally(peer)), true);
    }

    let mut title = vec![Span::styled(host, Style::default().fg(p.subtext0))];
    if let Some(ms) = peer.latency_ms {
        let color = if ms > crate::peers::PEER_SLOW_LATENCY_MS {
            p.yellow
        } else {
            p.overlay0
        };
        title.push(Span::styled(
            format!("  \u{f04c5} {ms}ms"), // nf-md-speedometer
            Style::default().fg(color),
        ));
    }

    let mut health: Vec<Span<'static>> = Vec::new();
    if let Some(system) = peer.system.as_ref() {
        // Peer summaries don't carry gpu (or net/battery) — see
        // `PeerSystemSummary`; the shared formatter simply omits them.
        health = server_health_spans(
            system.cpu_percent.map(f32::from),
            system.mem_used,
            system.mem_total,
            system.disk_free,
            None,
            p,
        );
    }
    (
        [Line::from(title), Line::from(health)],
        Some(peer_tally(peer)),
        false,
    )
}

/// A server row's metric line in the band's fixed-width glyph language:
/// ` 42% 13G/16G 213G  37%` (cpu, mem, disk, gpu) — space-separated, no
/// `·` (the dots cost width for nothing at this density). CPU/GPU render
/// right-aligned width-3 and mem used pads to the width of total so the
/// columns hold still across refreshes. One formatter for the self,
/// snapshot, and config-peer rows alike (#41).
fn server_health_spans(
    cpu_percent: Option<f32>,
    mem_used: Option<u64>,
    mem_total: Option<u64>,
    disk_free: Option<u64>,
    gpu_percent: Option<f32>,
    p: &crate::app::state::Palette,
) -> Vec<Span<'static>> {
    use super::status::{
        format_mem_ratio, format_percent3, mem_percent, push_band_metric, utilization_style,
    };
    let mut spans = Vec::new();
    if let Some(cpu) = cpu_percent {
        push_band_metric(
            &mut spans,
            "\u{f0ee0}", // nf-md-cpu_64_bit
            format_percent3(cpu),
            utilization_style(cpu, p),
            p,
        );
    }
    if let (Some(used), Some(total)) = (mem_used, mem_total) {
        push_band_metric(
            &mut spans,
            "\u{f035b}", // nf-md-memory
            format_mem_ratio(used, total),
            utilization_style(mem_percent(used, total), p),
            p,
        );
    }
    if let Some(free) = disk_free {
        push_band_metric(
            &mut spans,
            "\u{f02ca}", // nf-md-harddisk
            crate::system_stats::human_bytes(free),
            Style::default().fg(p.text),
            p,
        );
    }
    if let Some(gpu) = gpu_percent {
        push_band_metric(
            &mut spans,
            "\u{f08ae}", // nf-md-expansion_card
            format_percent3(gpu),
            utilization_style(gpu, p),
            p,
        );
    }
    spans
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Truncate a single-line span list to `width` columns with an ellipsis.
fn clamp_line(line: Line<'_>, width: u16) -> Line<'_> {
    let total: usize = line
        .spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum();
    if total <= width as usize {
        return line;
    }
    let mut budget = (width as usize).saturating_sub(1);
    let mut out = Vec::new();
    for span in line.spans {
        if budget == 0 {
            break;
        }
        let len = span.content.chars().count();
        if len <= budget {
            budget -= len;
            out.push(span);
        } else {
            let truncated: String = span.content.chars().take(budget).collect();
            out.push(Span::styled(format!("{truncated}…"), span.style));
            budget = 0;
        }
    }
    Line::from(out)
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let has_footer = app.sidebar_new_entry_visible();
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(
            &app.view.workspace_card_areas,
            area,
            *insert_idx,
            has_footer,
        ),
        _ => None,
    };

    let list_bottom = (area.y + area.height).saturating_sub(u16::from(has_footer));
    if area.height > 0 {
        let header_rect = Rect::new(area.x, area.y, area.width, 1);
        // An active server filter (#46) announces itself in the header so a
        // narrowed list never reads like the full fleet.
        let header_label = match server_filter_label(app) {
            Some(server) => format!(" spaces · only {server}"),
            None => " spaces".to_string(),
        };
        frame.render_widget(
            Paragraph::new(clamp_line(
                Line::from(vec![Span::styled(
                    header_label,
                    Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
                )]),
                area.width,
            )),
            header_rect,
        );
        let toggle_rect = panel_scope_toggle_rect(header_rect, app.spaces_panel_scope);
        if toggle_rect != Rect::default() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    panel_scope_toggle_label(app.spaces_panel_scope),
                    Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Right),
                toggle_rect,
            );
        }
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    // #33 two-level highlight: while any member of a project is focused,
    // the section's primary row carries the same always-on surface_dim
    // currency fill as the active row (and the current-server row) — one
    // "where am I" idiom from server to session to workspace.
    let active_section_primary = app.active_section_primary();

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let is_session_primary = !card.indented && !is_active && active_section_primary == Some(i);
        let highlighted = selected || is_active || is_dragged || is_session_primary;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let mut line1 = Vec::new();
        let mut show_workspace_icon = true;
        let mut collapsed_group_key: Option<String> = None;
        let mut group_key: Option<String> = None;
        if card.indented {
            line1.push(Span::styled("   ", Style::default()));
        } else if let Some((key, collapsed)) = workspace_parent_group_state(app, i) {
            let icon = if collapsed { "▸" } else { "▾" };
            let (state_icon, state_style) = if collapsed {
                let (state, seen) = space_aggregate_state(app, &key);
                state_dot(state, seen, p)
            } else {
                (icon, Style::default().fg(p.accent))
            };
            line1.push(Span::styled(icon, Style::default().fg(p.accent)));
            if collapsed {
                line1.push(Span::styled(" ", Style::default()));
                line1.push(Span::styled(state_icon, state_style));
                show_workspace_icon = false;
                collapsed_group_key = Some(key.clone());
            }
            group_key = Some(key);
            line1.push(Span::styled(" ", Style::default()));
        } else {
            line1.push(Span::styled(" ", Style::default()));
        }
        if show_workspace_icon {
            line1.push(Span::styled(icon, icon_style));
            line1.push(Span::styled(" ", Style::default()));
        }
        if card.indented {
            let display_label = grouped_child_display_label(
                &label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            );
            line1.push(Span::styled(display_label, name_style));
        } else {
            line1.push(Span::styled(label, name_style));
        }
        // The row's state language as packed rects (#42): the group join on
        // space header rows — even when expanded, the primary row always
        // carries its group's aggregate — the workspace's own join elsewhere.
        // Hollow ▯ = no live agents.
        let join = match group_key.as_deref() {
            Some(key) => space_join(app, key),
            None => workspace_join(app, ws),
        };
        line1.push(Span::styled(" ", Style::default()));
        line1.extend(packed_rects(&join, p));
        // Collapsed groups additionally summarize their hidden members with
        // exact per-state counts: plain colored digits in the terminal font.
        if let Some(key) = collapsed_group_key.as_deref() {
            for (state, seen, count) in space_state_counts(app, key) {
                line1.push(Span::styled(" ", Style::default()));
                line1.push(Span::styled(
                    count.to_string(),
                    Style::default().fg(state_label_color(state, seen, p)),
                ));
            }
        }
        // Single-line rows carry the PR glyph inline (their branch IS the
        // label); two-line rows render it on the branch line below.
        if row_height == 1 {
            if let Some(pr) = ws.pr_state() {
                let (glyph, color) = pr_state_glyph(pr.state, p);
                line1.push(Span::styled(
                    format!(" #{} {glyph}", pr.number),
                    Style::default().fg(color),
                ));
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(card.rect.x, row_y, card.rect.width, 1),
        );

        if row_height > 1 && row_y + 1 < list_bottom {
            if let Some(branch) = ws.branch() {
                // Ahead/behind arrows render only when non-zero: silence
                // means in sync with upstream (#42, deliberate — a synced
                // chip on every row would be noise at sidebar density).
                let upstream_label = ws.git_ahead_behind().and_then(|(ahead, behind)| {
                    let mut parts = Vec::new();
                    if ahead > 0 {
                        parts.push((format!("↑{}", ahead), p.green));
                    }
                    if behind > 0 {
                        parts.push((format!("↓{}", behind), p.red));
                    }
                    (!parts.is_empty()).then_some(parts)
                });
                // Compact PR state after the ahead/behind arrows (#42): the
                // same `#N ⊙/◐/✓/✗` language as the pane-header HUD.
                let pr_label = ws.pr_state().map(|pr| {
                    let (glyph, color) = pr_state_glyph(pr.state, p);
                    (format!("#{} {glyph}", pr.number), color)
                });
                let reserved = upstream_label
                    .as_ref()
                    .map(|parts| {
                        parts.iter().map(|(label, _)| label.len()).sum::<usize>() + parts.len()
                    })
                    .unwrap_or(0)
                    + pr_label
                        .as_ref()
                        .map(|(label, _)| label.chars().count() + 1)
                        .unwrap_or(0);
                let max_branch_len = (card.rect.width as usize).saturating_sub(5 + reserved);
                let branch_display = if branch.len() > max_branch_len {
                    format!("{}…", &branch[..max_branch_len.saturating_sub(1)])
                } else {
                    branch
                };
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                let branch_indent = if card.indented { "     " } else { "   " };
                let mut spans = vec![
                    Span::styled(branch_indent, Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ];
                if let Some(parts) = upstream_label {
                    spans.push(Span::styled(" ", Style::default()));
                    for (idx, (label, color)) in parts.into_iter().enumerate() {
                        if idx > 0 {
                            spans.push(Span::styled(" ", Style::default()));
                        }
                        spans.push(Span::styled(label, Style::default().fg(color)));
                    }
                }
                if let Some((label, color)) = pr_label {
                    spans.push(Span::styled(" ", Style::default()));
                    spans.push(Span::styled(label, Style::default().fg(color)));
                }
                frame.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect::new(card.rect.x, row_y + 1, card.rect.width, 1),
                );
            }
        }
    }

    for card in &app.view.remote_card_areas {
        let Some(peer) = app.remote_peer(card.peer) else {
            continue;
        };
        let Some(remote_ws) = peer.workspaces.get(card.ws_idx) else {
            continue;
        };
        if card.rect.y >= list_bottom {
            continue;
        }
        let stale = peer.is_stale() || peer.error.is_some();
        let (icon, icon_style) = remote_status_dot(remote_ws.status, p);
        let label = remote_entry_label(app, card.peer, card.ws_idx, card.indented);
        let max_label =
            (card.rect.width as usize).saturating_sub(if card.indented { 5 } else { 3 });
        let label = if label.len() > max_label {
            format!("{}…", &label[..max_label.saturating_sub(1)])
        } else {
            label
        };
        let mut label_style = Style::default().fg(p.subtext0);
        let mut final_icon_style = icon_style;
        if stale {
            label_style = label_style.add_modifier(Modifier::DIM);
            final_icon_style = final_icon_style.add_modifier(Modifier::DIM);
        }
        let indent = if card.indented { "   " } else { " " };
        let line = Line::from(vec![
            Span::styled(indent, Style::default()),
            Span::styled(icon, final_icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(label, label_style),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(card.rect.x, card.rect.y, card.rect.width, 1),
        );
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && has_footer && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );
    }
}

/// The pinned bottom band (#41): a hairline divider over the standalone
/// `menu` row — the sidebar's last row, below the agents section. Gated on
/// the mouse UI like the old footer entry; the click target is the whole
/// row ([`AppState::global_launcher_rect`]).
fn render_menu_row(app: &AppState, frame: &mut Frame, area: Rect) {
    if !app.mouse_capture {
        return;
    }
    let p = &app.palette;
    let divider = sidebar_menu_divider_rect(area, app.sidebar_pane_gap);
    if divider != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(divider.width as usize),
                Style::default().fg(p.divider_color()),
            )),
            divider,
        );
    }
    let row = sidebar_menu_row_rect(area, app.sidebar_pane_gap);
    if row == Rect::default() {
        return;
    }
    let mut spans = vec![Span::styled(" ", Style::default())];
    if app.global_menu_attention_badge_visible() {
        spans.push(Span::styled(
            "● ",
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled("menu", Style::default().fg(p.overlay0)));
    frame.render_widget(Paragraph::new(Line::from(spans)), row);
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(
            &sep_line,
            Style::default().fg(p.divider_color()),
        )),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_scope);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_toggle_label(app.agent_panel_scope),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for detail in details.iter().skip(app.agent_panel_scroll) {
        if row_y.saturating_add(1) >= body_bottom {
            break;
        }

        // Check if this agent entry corresponds to the active session
        let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);

        let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
        let label_color = state_label_color(detail.state, detail.seen, p);
        let label = detail
            .state_labels
            .get(agent_panel_status_key(detail.state, detail.seen))
            .map(String::as_str)
            .unwrap_or_else(|| state_label(detail.state, detail.seen));

        let row_style = if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };

        let name_style = if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        // Status colors render at full strength regardless of selection —
        // blocked/working/done are attention signals, not decoration. Only a
        // settled idle (seen, nothing to report) stays toned down; the row
        // highlight bar already marks the selected entry.
        let settled_idle =
            matches!(detail.state, AgentState::Idle | AgentState::Unknown) && detail.seen;
        let status_style = if settled_idle && !is_active {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(label_color)
        };
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

        let primary_label =
            format_agent_panel_primary_label(detail, body.width.saturating_sub(3) as usize);
        let name_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(primary_label, name_style),
        ]);
        frame.render_widget(
            Paragraph::new(name_line).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        // '<agent> · <activity-or-status>': the short agent code leads so the
        // (potentially long) live activity text gets the remaining width.
        let mut status_spans = vec![Span::styled("   ", Style::default())];
        if let Some(agent_label) = &detail.agent_label {
            status_spans.push(Span::styled(
                app.agent_alias(agent_label).to_string(),
                agent_style,
            ));
            status_spans.push(Span::styled(" · ", agent_style));
        }
        match &detail.live_activity {
            Some(activity) => {
                status_spans.push(Span::styled(format!("{activity}…"), status_style));
            }
            None => status_spans.push(Span::styled(label, status_style)),
        }
        if let Some(custom_status) = &detail.custom_status {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(custom_status.clone(), agent_style));
        }
        // Promoted header fields, compact: muted key, readable value. The
        // row is clipped at the panel edge; values get the agent-panel
        // budget (header > agent panel > nav list).
        for (key, value) in &detail.header_fields {
            status_spans.push(Span::styled(" · ", agent_style));
            status_spans.push(Span::styled(format!("{key} "), agent_style));
            status_spans.push(Span::styled(
                crate::terminal::middle_truncate_chars(value, AGENT_PANEL_HEADER_FIELD_VALUE_COLS),
                Style::default().fg(p.subtext0),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(status_spans)).style(row_style),
            Rect::new(body.x, row_y, body.width, 1),
        );
        row_y += 1;

        if row_y < body_bottom {
            row_y = row_y.saturating_add(app.sidebar_row_gap);
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};
    use ratatui::{backend::TestBackend, Terminal};

    fn remote_summary(
        workspace: &str,
        project_key: Option<&str>,
        project_label: Option<&str>,
        branch: Option<&str>,
    ) -> crate::api::schema::PeerWorkspaceSummary {
        crate::api::schema::PeerWorkspaceSummary {
            id: format!("ws_{workspace}"),
            workspace: workspace.into(),
            project_key: project_key.map(str::to_string),
            project_label: project_label.map(str::to_string),
            branch: branch.map(str::to_string),
            is_linked_worktree: branch.is_some(),
            agent: Some("cc".into()),
            status: crate::api::schema::AgentStatus::Working,
            status_age_secs: Some(10),
            activity: None,
        }
    }

    fn peer_with_workspaces(
        name: &str,
        workspaces: Vec<crate::api::schema::PeerWorkspaceSummary>,
    ) -> crate::peers::PeerSummaryState {
        let mut peer = crate::peers::PeerSummaryState::new(&crate::config::PeerConfig {
            name: name.into(),
            ..Default::default()
        });
        peer.workspaces = workspaces;
        peer.last_ok = Some(std::time::Instant::now());
        peer
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn servers_section_height_tracks_peers_and_scope() {
        let mut app = crate::app::state::AppState::test_new();
        assert_eq!(servers_section_height(&app), 0);
        app.peer_summaries = vec![
            peer_with_workspaces("anvil", vec![]),
            peer_with_workspaces("sage", vec![]),
        ];
        // Header + two lines each for the self row and both peers + the
        // trailing divider before the spaces list.
        assert_eq!(servers_section_height(&app), 1 + 3 * SERVER_ROW_LINES + 1);
        // Scope current: only the self row renders, the band stays visible
        // (header keeps the toggle reachable).
        app.servers_panel_scope = PanelScope::Current;
        assert_eq!(servers_section_height(&app), 1 + SERVER_ROW_LINES + 1);
    }

    #[test]
    fn compute_server_section_areas_lays_out_self_slot_then_two_line_peer_rows() {
        let mut app = crate::app::state::AppState::test_new();
        app.peer_summaries = vec![
            peer_with_workspaces("anvil", vec![]),
            peer_with_workspaces("sage", vec![]),
        ];
        // Tall enough that the half-section cap fits the full band: header +
        // three two-line rows + the trailing divider (8 rows).
        let area = Rect::new(0, 0, 30, 34);
        let (header, cards) = compute_server_section_areas(&app, area);
        assert_ne!(header, Rect::default());
        assert_eq!(cards.len(), 2);
        // Slot 0 (the two lines under the header) belongs to the local
        // server and has NO hit-area, so clicking it can never request a
        // SwitchServer; the first peer card starts below it.
        assert_eq!(
            cards[0].target,
            crate::app::state::PeerSwitchRequest::ConfigPeer {
                peer_idx: 0,
                ws_idx: 0,
            }
        );
        assert_eq!(cards[0].rect.y, header.y + 1 + SERVER_ROW_LINES);
        assert!(cards
            .iter()
            .all(|card| card.rect.y > header.y + SERVER_ROW_LINES));
        // Each peer row spans two lines and stacks below the previous one.
        assert_eq!(cards[0].rect.height, SERVER_ROW_LINES);
        assert_eq!(
            cards[1].target,
            crate::app::state::PeerSwitchRequest::ConfigPeer {
                peer_idx: 1,
                ws_idx: 0,
            }
        );
        assert_eq!(cards[1].rect.y, cards[0].rect.y + SERVER_ROW_LINES);
        assert_eq!(cards[1].rect.height, SERVER_ROW_LINES);

        // Scope current without a carried snapshot: only the self row stays,
        // which has no hit-area — the header (with its toggle) remains.
        app.servers_panel_scope = PanelScope::Current;
        let (header, cards) = compute_server_section_areas(&app, area);
        assert_ne!(header, Rect::default());
        assert!(cards.is_empty());
    }

    fn carried_snapshot(origin: &str, peers: Vec<&str>) -> crate::peers::FleetSnapshotState {
        crate::peers::FleetSnapshotState {
            origin: origin.to_string(),
            peers: peers
                .into_iter()
                .map(|name| peer_with_workspaces(name, vec![]))
                .collect(),
            received_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn server_band_orders_home_then_self_then_snapshot_then_config_peers() {
        use crate::app::state::PeerSwitchRequest;
        let mut app = crate::app::state::AppState::test_new();
        app.fleet_snapshot = Some(carried_snapshot("mba22", vec!["anvil", "ksb"]));
        app.peer_summaries = vec![peer_with_workspaces("ownpeer", vec![])];

        assert_eq!(
            server_band_slots(&app),
            vec![
                Some(PeerSwitchRequest::Home),
                None, // self — no switch hit-area
                Some(PeerSwitchRequest::SnapshotPeer { entry_idx: 0 }),
                Some(PeerSwitchRequest::SnapshotPeer { entry_idx: 1 }),
                Some(PeerSwitchRequest::ConfigPeer {
                    peer_idx: 0,
                    ws_idx: 0,
                }),
            ]
        );

        // Header + five two-line rows + the trailing divider.
        assert_eq!(servers_section_height(&app), 1 + 5 * SERVER_ROW_LINES + 1);

        // The hit-areas skip the self slot: home sits directly under the
        // header, the first snapshot row two lines below the self row.
        let (header, cards) = compute_server_section_areas(&app, Rect::new(0, 0, 30, 80));
        assert_eq!(cards.len(), 4);
        assert_eq!(cards[0].target, PeerSwitchRequest::Home);
        assert_eq!(cards[0].rect.y, header.y + 1);
        assert_eq!(
            cards[1].target,
            PeerSwitchRequest::SnapshotPeer { entry_idx: 0 }
        );
        assert_eq!(cards[1].rect.y, header.y + 1 + 2 * SERVER_ROW_LINES);
    }

    #[test]
    fn no_home_row_without_carried_origin() {
        let mut app = crate::app::state::AppState::test_new();
        // Locally-attached client: no snapshot, no peers — band hidden.
        assert!(server_band_slots(&app).iter().all(Option::is_none));
        assert_eq!(servers_section_height(&app), 0);

        // Config peers alone keep the pre-federation order: self first.
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![])];
        let slots = server_band_slots(&app);
        assert_eq!(slots[0], None);
        assert!(!slots.contains(&Some(crate::app::state::PeerSwitchRequest::Home)));
    }

    #[test]
    fn snapshot_only_spoke_shows_band_with_home_row() {
        let mut app = crate::app::state::AppState::test_new();
        // The typical spoke: zero config peers, but a carried snapshot.
        app.fleet_snapshot = Some(carried_snapshot("mba22", vec!["anvil"]));
        // Header + home + self + one snapshot row + the trailing divider.
        assert_eq!(servers_section_height(&app), 1 + 3 * SERVER_ROW_LINES + 1);
    }

    #[test]
    fn servers_current_scope_keeps_home_row_when_snapshot_present() {
        use crate::app::state::PeerSwitchRequest;
        let mut app = crate::app::state::AppState::test_new();
        app.fleet_snapshot = Some(carried_snapshot("mba22", vec!["anvil", "ksb"]));
        app.peer_summaries = vec![peer_with_workspaces("ownpeer", vec![])];
        app.servers_panel_scope = PanelScope::Current;

        // The way home must never hide: scope current keeps home + self.
        assert_eq!(
            visible_server_band_slots(&app),
            vec![Some(PeerSwitchRequest::Home), None]
        );
        assert_eq!(servers_section_height(&app), 1 + 2 * SERVER_ROW_LINES + 1);

        // Home stays clickable directly under the header; snapshot/config
        // peers lose their hit-areas with their rows.
        let (header, cards) = compute_server_section_areas(&app, Rect::new(0, 0, 30, 80));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].target, PeerSwitchRequest::Home);
        assert_eq!(cards[0].rect.y, header.y + 1);
    }

    #[test]
    fn servers_header_scope_toggle_sits_right_aligned_in_header_row() {
        let header = Rect::new(2, 5, 24, 1);
        let toggle = panel_scope_toggle_rect(header, PanelScope::All);
        assert_eq!(toggle, Rect::new(2 + 24 - 3, 5, 3, 1));
        let toggle = panel_scope_toggle_rect(header, PanelScope::Current);
        assert_eq!(toggle, Rect::new(2 + 24 - 7, 5, 7, 1));
        assert_eq!(
            panel_scope_toggle_rect(Rect::default(), PanelScope::All),
            Rect::default()
        );
    }

    #[test]
    fn servers_band_renders_scope_label_and_divider_above_spaces() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Terminal;
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![])];

        let area = Rect::new(0, 0, 30, 40);
        let mut terminal =
            Terminal::new(TestBackend::new(30, 40)).expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(&app, &runtimes, frame, area))
            .expect("sidebar should render");

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (servers_area, list_area) = carve_servers_band(ws_area, servers_section_height(&app));
        let buffer = terminal.backend().buffer();

        // Header row: " servers" with the right-aligned scope label.
        let header_text: String = (servers_area.x..servers_area.x + servers_area.width)
            .map(|x| buffer[(x, servers_area.y)].symbol().to_string())
            .collect();
        assert!(header_text.starts_with(" servers"), "{header_text:?}");
        assert!(header_text.trim_end().ends_with("all"), "{header_text:?}");

        // The band's last row is a hairline divider — the same visual
        // language as the spaces↔agents divider.
        let divider_y = servers_area.y + servers_area.height - 1;
        for x in servers_area.x..servers_area.x + servers_area.width {
            assert_eq!(buffer[(x, divider_y)].symbol(), "─", "col {x}");
        }

        // The spaces header starts directly below the divider.
        let spaces_text: String = (list_area.x..list_area.x + list_area.width)
            .map(|x| buffer[(x, list_area.y)].symbol().to_string())
            .collect();
        assert!(spaces_text.starts_with(" spaces"), "{spaces_text:?}");
    }

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, rect: Rect, y: u16) -> String {
        (rect.x..rect.x + rect.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn menu_renders_pinned_to_sidebar_bottom_with_divider_above() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 30, 40);
        // The `new` footer renders through the state's hit-area rect.
        app.view.sidebar_rect = area;
        let mut terminal =
            Terminal::new(TestBackend::new(30, 40)).expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(&app, &runtimes, frame, area))
            .expect("sidebar should render");
        let buffer = terminal.backend().buffer();

        // The menu is a standalone row pinned to the sidebar's last row…
        let row = sidebar_menu_row_rect(area, app.sidebar_pane_gap);
        assert_eq!(row.y, area.y + area.height - 1);
        let row_text = buffer_row_text(buffer, row, row.y);
        assert!(row_text.trim_start().starts_with("menu"), "{row_text:?}");

        // …separated above by the hairline divider idiom.
        let divider = sidebar_menu_divider_rect(area, app.sidebar_pane_gap);
        for x in divider.x..divider.x + divider.width {
            assert_eq!(buffer[(x, divider.y)].symbol(), "─", "col {x}");
        }

        // The spaces footer hosts only `new` now — no mid-field menu.
        let ws_rect = workspace_list_rect(area, app.sidebar_section_split, app.sidebar_pane_gap, 0);
        let footer_text = buffer_row_text(buffer, ws_rect, ws_rect.y + ws_rect.height - 1);
        assert!(footer_text.contains("new"), "{footer_text:?}");
        assert!(!footer_text.contains("menu"), "{footer_text:?}");
    }

    #[test]
    fn workspace_tab_mode_hides_the_new_entry_but_keeps_the_menu() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Terminal;
        app.tab_mode = crate::config::TabModeConfig::Workspace;

        let area = Rect::new(0, 0, 30, 40);
        let mut terminal =
            Terminal::new(TestBackend::new(30, 40)).expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(&app, &runtimes, frame, area))
            .expect("sidebar should render");
        let buffer = terminal.backend().buffer();

        // No `new` anywhere in the sidebar; the pinned menu row remains.
        let all_text: String = (area.y..area.y + area.height)
            .map(|y| buffer_row_text(buffer, area, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all_text.contains("new"), "{all_text}");
        let row = sidebar_menu_row_rect(area, app.sidebar_pane_gap);
        let row_text = buffer_row_text(buffer, row, row.y);
        assert!(row_text.trim_start().starts_with("menu"), "{row_text:?}");

        // The spaces list reclaims the footer row.
        let ws_rect = workspace_list_rect(area, app.sidebar_section_split, app.sidebar_pane_gap, 0);
        let body_tabs = workspace_list_body_rect(ws_rect, false, true);
        let body_workspace = workspace_list_body_rect(ws_rect, false, false);
        assert_eq!(body_workspace.height, body_tabs.height + 1);
        assert_eq!(
            body_workspace.y + body_workspace.height,
            ws_rect.y + ws_rect.height
        );
    }

    #[test]
    fn home_server_rows_mark_origin_and_snapshot_age() {
        let app = crate::app::state::AppState::test_new();
        let snapshot = carried_snapshot("mba22", vec![]);
        let [title, health] = home_server_rows(&snapshot, &app.palette);
        let title = line_text(&title);
        let health = line_text(&health);
        assert!(title.contains('←'), "{title}");
        assert!(title.contains("mba22"), "{title}");
        assert!(title.contains("home"), "{title}");
        assert!(health.contains("snapshot"), "{health}");
        assert!(health.contains("old"), "{health}");
    }

    #[test]
    fn snapshot_server_rows_show_staleness_age() {
        let app = crate::app::state::AppState::test_new();
        let mut peer = peer_with_workspaces("anvil", vec![]);
        peer.last_ok = Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        let ([title, _], join, _ghosted) = snapshot_server_rows(&peer, &app.palette);
        let title = line_text(&title);
        assert!(title.contains("30s old"), "{title}");
        // Reachable snapshot rows carry a join for the leading medallion.
        assert!(join.is_some());
    }

    #[test]
    fn self_server_rows_show_local_identity_and_glyph_health() {
        const G: u64 = 1024 * 1024 * 1024;
        let mut app = crate::app::state::AppState::test_new();
        app.system_stats = Some(crate::system_stats::SystemStats {
            cpu_percent: Some(42.0),
            mem_used: Some(13 * G),
            mem_total: Some(16 * G),
            disk_free: Some(213 * G),
            battery_percent: Some(85),
            battery_charging: Some(false),
            net_rx_per_sec: Some(1500),
            net_tx_per_sec: Some(512),
            gpu_percent: Some(8),
            ..Default::default()
        });
        let ([title, health], join, _ghosted) = self_server_rows(&app);
        // The self row always joins the local agent states (none here).
        assert_eq!(join, Some(tally_states([])));
        let title = line_text(&title);
        let health = line_text(&health);
        assert!(title.contains(&crate::app::short_host_name()), "{title}");
        assert!(title.contains('\u{2726}'), "{title}"); // ✦ current marker
                                                        // Battery and net live on the title line (#41): quintile glyph +
                                                        // charge, then the net glyph with ▼rx ▲tx.
        assert!(title.contains("\u{f0079} 85%"), "{title}");
        assert!(
            title.contains("\u{f06f3} \u{25bc}1.5K \u{25b2}512B"),
            "{title}"
        );
        // The metric line: cpu/mem/disk/gpu, space-separated (no `·`),
        // cpu/gpu right-aligned width-3.
        assert!(health.contains("\u{f0ee0}  42%"), "{health}");
        assert!(health.contains("\u{f035b} 13G/16G"), "{health}");
        assert!(health.contains("\u{f02ca} 213G"), "{health}");
        assert!(health.contains("\u{f08ae}   8%"), "{health}");
        assert!(!health.contains('\u{b7}'), "{health}");
        assert!(
            health.contains("42% \u{f035b} 13G/16G \u{f02ca} 213G \u{f08ae}"),
            "{health}"
        );
    }

    #[test]
    fn self_server_rows_omit_battery_net_and_gpu_when_unsampled() {
        let mut app = crate::app::state::AppState::test_new();
        app.system_stats = Some(crate::system_stats::SystemStats {
            cpu_percent: Some(42.0),
            ..Default::default()
        });
        let ([title, health], _, _ghosted) = self_server_rows(&app);
        let title = line_text(&title);
        let health = line_text(&health);
        assert!(!title.contains("\u{f06f3}"), "{title}");
        assert!(!title.contains('%'), "{title}");
        assert!(!health.contains("\u{f08ae}"), "{health}");
    }

    #[test]
    fn peer_server_rows_split_identity_and_glyph_health() {
        let p = crate::app::state::AppState::test_new().palette;
        let mut peer = peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        );
        peer.host = Some("anvil".into());
        peer.latency_ms = Some(34);
        peer.system = Some(crate::api::schema::PeerSystemSummary {
            cpu_percent: Some(71),
            mem_used: Some(48 * 1024 * 1024 * 1024),
            mem_total: Some(64 * 1024 * 1024 * 1024),
            disk_free: None,
        });
        let ([title, health], join, _ghosted) = peer_server_rows(&peer, &p);
        let title = line_text(&title);
        let health = line_text(&health);
        assert!(title.contains("anvil"), "{title}");
        assert!(title.contains("34ms"), "{title}");
        // The second line speaks the band's fixed-width glyph language
        // (cpu right-aligned width-3, no `·` separators); the agent rollup
        // moved into the leading medallion's join — no dingbat counts.
        assert!(health.contains("\u{f0ee0}  71%"), "{health}");
        assert!(health.contains("\u{f035b} 48G/64G"), "{health}");
        assert!(!health.contains('\u{b7}'), "{health}");
        assert!(!health.contains('\u{2776}'), "{health}");
        assert_eq!(join, Some(tally_states([StateClass::Working])));
        assert!(!health.contains("anvil"), "{health}");
    }

    #[test]
    fn peer_server_rows_keep_metric_columns_stable_at_full_utilization() {
        const G: u64 = 1024 * 1024 * 1024;
        let p = crate::app::state::AppState::test_new().palette;
        let mut peer = peer_with_workspaces("anvil", vec![]);
        peer.host = Some("anvil".into());
        peer.system = Some(crate::api::schema::PeerSystemSummary {
            cpu_percent: Some(100),
            mem_used: Some(92 * G),
            mem_total: Some(512 * G),
            disk_free: None,
        });
        let ([_, health], _, _ghosted) = peer_server_rows(&peer, &p);
        let health = line_text(&health);
        // 100% fills the width-3 column exactly; mem used pads to the
        // width of total so the slash column never jitters.
        assert!(health.contains("\u{f0ee0} 100%"), "{health}");
        assert!(health.contains("\u{f035b}  92G/512G"), "{health}");
    }

    #[test]
    fn peer_server_rows_ghost_unreachable_peers() {
        use ratatui::style::Modifier;
        let p = crate::app::state::AppState::test_new().palette;
        let mut peer = peer_with_workspaces("ksb", vec![]);
        peer.last_ok = None;
        peer.error = Some("connect timed out".into());
        peer.system = Some(crate::api::schema::PeerSystemSummary {
            cpu_percent: Some(42),
            mem_used: Some(8 * 1024 * 1024 * 1024),
            mem_total: Some(16 * 1024 * 1024 * 1024),
            disk_free: Some(100 * 1024 * 1024 * 1024),
        });
        let ([title, health], tally, _ghosted) = peer_server_rows(&peer, &p);
        // Ghost of the normal row (#42): hollow no-conn dot, struck name,
        // broken-link glyph in the latency slot, everything muted + italic —
        // including the LAST-KNOWN stats, which stay visible.
        let text = line_text(&title);
        assert!(text.contains("ksb"), "{text:?}");
        assert!(text.contains('\u{f0337}'), "broken-link icon: {text:?}");
        for span in title.spans.iter().filter(|s| !s.content.trim().is_empty()) {
            assert_eq!(span.style.fg, Some(p.overlay0), "muted: {:?}", span.content);
            assert!(
                span.style.add_modifier.contains(Modifier::ITALIC),
                "italic: {:?}",
                span.content
            );
        }
        let name = title
            .spans
            .iter()
            .find(|span| span.content.contains("ksb"))
            .expect("name span");
        assert!(name.style.add_modifier.contains(Modifier::CROSSED_OUT));
        let health_text = line_text(&health);
        assert!(
            health_text.contains("42%"),
            "last-known stats: {health_text:?}"
        );
        for span in health.spans.iter().filter(|s| !s.content.trim().is_empty()) {
            assert_eq!(span.style.fg, Some(p.overlay0));
            assert!(span.style.add_modifier.contains(Modifier::ITALIC));
        }
        // The ghosted tally still feeds the (greyed) count columns.
        assert!(tally.is_some());

        // A peer that was reachable once shows the outage age by the icon.
        peer.last_ok = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(300));
        let ([title, _], _, _ghosted) = peer_server_rows(&peer, &p);
        assert!(line_text(&title).contains("5m"), "{:?}", line_text(&title));
    }

    fn workspace_with_project_key(name: &str, project_key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: format!("/repo/{name}/.git"),
            checkout_key: format!("/repo/{name}"),
            label: name.into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
            project_key: project_key.into(),
        });
        ws
    }

    #[test]
    fn remote_rows_fold_under_matching_local_project() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            workspace_with_project_key("herdr", "github.com/gerchowl/herdr"),
            workspace_with_project_key("other", "github.com/gerchowl/other"),
        ];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        )];

        let entries = workspace_list_entries(&app);
        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Remote {
                    peer: crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                    ws_idx: 0,
                    indented: true
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
        assert_eq!(
            remote_entry_label(
                &app,
                crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                0,
                true
            ),
            "anvil:fix/pty"
        );
    }

    #[test]
    fn remote_only_projects_trail_the_list_with_project_leader() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![workspace_with_project_key(
            "herdr",
            "github.com/gerchowl/herdr",
        )];
        app.peer_summaries = vec![peer_with_workspaces(
            "sage",
            vec![
                remote_summary(
                    "dotfiles",
                    Some("github.com/gerchowl/dotfiles"),
                    Some("dotfiles"),
                    None,
                ),
                remote_summary(
                    "dotfiles-wt",
                    Some("github.com/gerchowl/dotfiles"),
                    Some("dotfiles"),
                    Some("vm-dev"),
                ),
            ],
        )];

        let entries = workspace_list_entries(&app);
        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Remote {
                    peer: crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Remote {
                    peer: crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                    ws_idx: 1,
                    indented: true
                },
            ]
        );
        // The leader row carries the project label.
        let config_peer = crate::app::state::RemotePeerRef::Config { peer_idx: 0 };
        assert_eq!(
            remote_entry_label(&app, config_peer, 0, false),
            "dotfiles · sage:dotfiles"
        );
        assert_eq!(
            remote_entry_label(&app, config_peer, 1, true),
            "sage:vm-dev"
        );
    }

    #[test]
    fn collapsed_local_group_hides_matched_remote_rows() {
        let mut app = crate::app::state::AppState::test_new();
        let space = |linked: bool| crate::workspace::WorktreeSpaceMembership {
            key: "family-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: if linked {
                "/repo/herdr-wt".into()
            } else {
                "/repo/herdr".into()
            },
            is_linked_worktree: linked,
        };
        let mut parent = workspace_with_project_key("herdr", "github.com/gerchowl/herdr");
        parent.worktree_space = Some(space(false));
        let mut child = workspace_with_project_key("herdr-wt", "github.com/gerchowl/herdr");
        child.worktree_space = Some(space(true));
        app.workspaces = vec![parent, child];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        )];

        let expanded = workspace_list_entries(&app);
        assert!(expanded
            .iter()
            .any(|entry| matches!(entry, WorkspaceListEntry::Remote { .. })));

        app.collapsed_space_keys.insert("family-key".into());
        let collapsed = workspace_list_entries(&app);
        assert!(!collapsed
            .iter()
            .any(|entry| matches!(entry, WorkspaceListEntry::Remote { .. })));
    }

    fn snapshot_with_peers(
        origin: &str,
        peers: Vec<crate::peers::PeerSummaryState>,
    ) -> crate::peers::FleetSnapshotState {
        crate::peers::FleetSnapshotState {
            origin: origin.to_string(),
            peers,
            received_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn snapshot_peer_workspaces_fold_into_spaces_list_only_while_snapshot_present() {
        use crate::app::state::RemotePeerRef;
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![workspace_with_project_key(
            "herdr",
            "github.com/gerchowl/herdr",
        )];
        // The spoke case: zero config peers, a carried snapshot whose peer
        // has a row matching the local project plus a remote-only project.
        app.fleet_snapshot = Some(snapshot_with_peers(
            "mba22",
            vec![peer_with_workspaces(
                "anvil",
                vec![
                    remote_summary(
                        "herdr",
                        Some("github.com/gerchowl/herdr"),
                        Some("herdr"),
                        Some("fix/pty"),
                    ),
                    remote_summary(
                        "dotfiles",
                        Some("github.com/gerchowl/dotfiles"),
                        Some("dotfiles"),
                        None,
                    ),
                ],
            )],
        ));

        let entries = workspace_list_entries(&app);
        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Remote {
                    peer: RemotePeerRef::Snapshot { entry_idx: 0 },
                    ws_idx: 0,
                    indented: true
                },
                WorkspaceListEntry::Remote {
                    peer: RemotePeerRef::Snapshot { entry_idx: 0 },
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
        // Snapshot rows label like config-peer rows: carried host + branch.
        assert_eq!(
            remote_entry_label(&app, RemotePeerRef::Snapshot { entry_idx: 0 }, 0, true),
            "anvil:fix/pty"
        );

        // No snapshot, no config peers: nothing remote folds in.
        app.fleet_snapshot = None;
        assert!(!workspace_list_entries(&app)
            .iter()
            .any(|entry| matches!(entry, WorkspaceListEntry::Remote { .. })));
    }

    #[test]
    fn config_peer_shadows_matching_snapshot_entry() {
        use crate::app::state::RemotePeerRef;
        let mut app = crate::app::state::AppState::test_new();
        let herdr_row = || {
            remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )
        };
        // Defensive hub-meets-snapshot case: "anvil" is both a live-polled
        // config peer and a carried snapshot entry (same ssh target). The
        // polled entry wins; the snapshot-only "sage" still folds in after.
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![herdr_row()])];
        app.fleet_snapshot = Some(snapshot_with_peers(
            "mba22",
            vec![
                peer_with_workspaces("anvil", vec![herdr_row()]),
                peer_with_workspaces(
                    "sage",
                    vec![remote_summary(
                        "dotfiles",
                        Some("github.com/gerchowl/dotfiles"),
                        Some("dotfiles"),
                        None,
                    )],
                ),
            ],
        ));

        let remote_peers: Vec<RemotePeerRef> = workspace_list_entries(&app)
            .into_iter()
            .filter_map(|entry| match entry {
                WorkspaceListEntry::Remote { peer, .. } => Some(peer),
                WorkspaceListEntry::Workspace { .. } => None,
            })
            .collect();
        assert_eq!(
            remote_peers,
            vec![
                RemotePeerRef::Config { peer_idx: 0 },
                RemotePeerRef::Snapshot { entry_idx: 1 },
            ],
            "the duplicated anvil renders once, from the polled config entry"
        );
    }

    #[test]
    fn server_filter_local_hides_every_remote_row() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![workspace_with_project_key(
            "herdr",
            "github.com/gerchowl/herdr",
        )];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        )];
        app.fleet_snapshot = Some(snapshot_with_peers(
            "mba22",
            vec![peer_with_workspaces(
                "sage",
                vec![remote_summary(
                    "dotfiles",
                    Some("github.com/gerchowl/dotfiles"),
                    Some("dotfiles"),
                    None,
                )],
            )],
        ));

        app.server_filter = Some(crate::app::state::ServerFilter::Local);
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false
            }],
            "local filter keeps local workspaces and folds no remote rows"
        );
    }

    #[test]
    fn server_filter_peer_shows_only_that_peers_rows_grouped_by_project() {
        use crate::app::state::RemotePeerRef;
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![workspace_with_project_key(
            "herdr",
            "github.com/gerchowl/herdr",
        )];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        )];
        // Interleaved projects (a, b, a) regroup under their leaders.
        app.fleet_snapshot = Some(snapshot_with_peers(
            "mba22",
            vec![peer_with_workspaces(
                "sage",
                vec![
                    remote_summary(
                        "dotfiles",
                        Some("github.com/gerchowl/dotfiles"),
                        Some("dotfiles"),
                        None,
                    ),
                    remote_summary(
                        "herdr",
                        Some("github.com/gerchowl/herdr"),
                        Some("herdr"),
                        Some("main"),
                    ),
                    remote_summary(
                        "dotfiles-wt",
                        Some("github.com/gerchowl/dotfiles"),
                        Some("dotfiles"),
                        Some("vm-dev"),
                    ),
                ],
            )],
        ));

        app.server_filter = Some(crate::app::state::ServerFilter::Peer {
            ssh_target: "sage".into(),
        });
        let sage = RemotePeerRef::Snapshot { entry_idx: 0 };
        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Remote {
                    peer: sage,
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Remote {
                    peer: sage,
                    ws_idx: 2,
                    indented: true
                },
                WorkspaceListEntry::Remote {
                    peer: sage,
                    ws_idx: 1,
                    indented: false
                },
            ],
            "only sage's rows, projects regrouped with unindented leaders"
        );

        // A filter whose server no longer resolves narrows to nothing
        // rather than silently un-filtering.
        app.server_filter = Some(crate::app::state::ServerFilter::Peer {
            ssh_target: "gone".into(),
        });
        assert!(workspace_list_entries(&app).is_empty());
    }

    #[test]
    fn server_filter_clamps_scroll_and_hit_areas_to_filtered_entries() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            workspace_with_project_key("herdr", "github.com/gerchowl/herdr"),
            workspace_with_project_key("other", "github.com/gerchowl/other"),
        ];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![remote_summary(
                "herdr",
                Some("github.com/gerchowl/herdr"),
                Some("herdr"),
                Some("fix/pty"),
            )],
        )];
        let area = Rect::new(0, 0, 30, 40);

        // Peer filter: one remote entry — scroll clamps to it, and the hit
        // areas (same single source) carry only that remote card.
        app.server_filter = Some(crate::app::state::ServerFilter::Peer {
            ssh_target: "anvil".into(),
        });
        assert_eq!(workspace_list_entries(&app).len(), 1);
        assert_eq!(normalized_workspace_scroll(&app, area, 99), 0);
        let (cards, remote_cards) = compute_workspace_list_areas(&app, area);
        assert!(cards.is_empty());
        assert_eq!(remote_cards.len(), 1);

        // Local filter: both local cards, no remote ones.
        app.server_filter = Some(crate::app::state::ServerFilter::Local);
        assert_eq!(normalized_workspace_scroll(&app, area, 99), 1);
        let (cards, remote_cards) = compute_workspace_list_areas(&app, area);
        assert_eq!(cards.len(), 2);
        assert!(remote_cards.is_empty());
    }

    #[test]
    fn server_band_slot_at_resolves_self_and_peer_rows() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![])];
        let area = Rect::new(0, 0, 30, 40);

        let (header, cards) = compute_server_section_areas(&app, area);
        // The self row spans the two lines under the header.
        assert_eq!(
            server_band_slot_at(&app, area, header.x + 2, header.y + 1),
            Some(None)
        );
        // The peer row resolves to its switch target.
        assert_eq!(
            server_band_slot_at(&app, area, cards[0].rect.x + 2, cards[0].rect.y),
            Some(Some(crate::app::state::PeerSwitchRequest::ConfigPeer {
                peer_idx: 0,
                ws_idx: 0,
            }))
        );
        // The header row itself is no server row.
        assert_eq!(server_band_slot_at(&app, area, header.x, header.y), None);
    }

    #[test]
    fn spaces_header_announces_active_server_filter() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Terminal;
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![])];
        app.server_filter = Some(crate::app::state::ServerFilter::Peer {
            ssh_target: "anvil".into(),
        });

        let area = Rect::new(0, 0, 30, 40);
        let mut terminal =
            Terminal::new(TestBackend::new(30, 40)).expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(&app, &runtimes, frame, area))
            .expect("sidebar should render");

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (_, list_area) = carve_servers_band(ws_area, servers_section_height(&app));
        let buffer = terminal.backend().buffer();
        let header_text: String = (list_area.x..list_area.x + list_area.width)
            .map(|x| buffer[(x, list_area.y)].symbol().to_string())
            .collect();
        assert!(
            header_text.starts_with(" spaces · only anvil"),
            "{header_text:?}"
        );
    }

    #[test]
    fn space_state_counts_buckets_panes_by_attention_state() {
        use crate::detect::AgentState;
        use crate::workspace::WorktreeSpaceMembership;

        let space = |idx: usize, linked: bool| WorktreeSpaceMembership {
            key: "grp".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/ws-{idx}").into(),
            is_linked_worktree: linked,
        };

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.push(Workspace::test_new("parent"));
        app.workspaces.push(Workspace::test_new("child"));
        app.workspaces[0].worktree_space = Some(space(0, false));
        app.workspaces[1].worktree_space = Some(space(1, true));
        app.ensure_test_terminals();

        // parent pane is Blocked; child pane is Idle+unseen (a "done" light).
        let parent_tid = {
            let tab = &app.workspaces[0].tabs[0];
            tab.panes
                .values()
                .next()
                .unwrap()
                .attached_terminal_id
                .clone()
        };
        let (child_tid, child_pane) = {
            let tab = &app.workspaces[1].tabs[0];
            let pane = tab.panes.keys().next().copied().unwrap();
            (app.workspaces[1].terminal_id(pane).unwrap().clone(), pane)
        };
        app.terminals.get_mut(&parent_tid).unwrap().state = AgentState::Blocked;
        app.terminals.get_mut(&child_tid).unwrap().state = AgentState::Idle;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&child_pane)
            .unwrap()
            .seen = false;

        let counts = space_state_counts(&app, "grp");

        assert!(counts.contains(&(AgentState::Blocked, true, 1)));
        assert!(counts.contains(&(AgentState::Idle, false, 1)));
        // No Working/Idle-seen panes → those buckets are filtered out.
        assert_eq!(counts.len(), 2);
    }

    /// Two local workspaces in the worktree space "grp": an unlinked parent
    /// and a linked child, with live (Unknown-state) test terminals.
    fn space_group_app() -> crate::app::state::AppState {
        use crate::workspace::WorktreeSpaceMembership;
        let space = |idx: usize, linked: bool| WorktreeSpaceMembership {
            key: "grp".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/ws-{idx}").into(),
            is_linked_worktree: linked,
        };
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.push(Workspace::test_new("parent"));
        app.workspaces.push(Workspace::test_new("child"));
        app.workspaces[0].worktree_space = Some(space(0, false));
        app.workspaces[1].worktree_space = Some(space(1, true));
        app.ensure_test_terminals();
        app.mode = crate::app::Mode::Terminal;
        app.active = Some(0);
        app
    }

    fn set_pane_state(
        app: &mut crate::app::state::AppState,
        ws_idx: usize,
        state: AgentState,
        seen: bool,
    ) {
        let pane = app.workspaces[ws_idx].tabs[0]
            .panes
            .keys()
            .next()
            .copied()
            .unwrap();
        let tid = app.workspaces[ws_idx].terminal_id(pane).unwrap().clone();
        app.terminals.get_mut(&tid).unwrap().state = state;
        app.workspaces[ws_idx].tabs[0]
            .panes
            .get_mut(&pane)
            .unwrap()
            .seen = seen;
    }

    /// Render the full sidebar with the view's card areas populated the way
    /// the live render path does, returning the buffer for cell asserts.
    fn render_sidebar_to_buffer(
        app: &mut crate::app::state::AppState,
        area: Rect,
    ) -> ratatui::buffer::Buffer {
        let (cards, remotes) = compute_workspace_list_areas(app, area);
        app.view.workspace_card_areas = cards;
        app.view.remote_card_areas = remotes;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(app, &runtimes, frame, area))
            .expect("sidebar should render");
        terminal.backend().buffer().clone()
    }

    fn row_glyph_positions(
        buffer: &ratatui::buffer::Buffer,
        rect: Rect,
        y: u16,
        glyph: &str,
    ) -> Vec<u16> {
        (rect.x..rect.x + rect.width)
            .filter(|x| buffer[(*x, y)].symbol() == glyph)
            .collect()
    }

    /// The dingbat circled digits (U+2776..=U+277F) retired with #42: no
    /// render path may emit them anymore.
    fn assert_no_circled_digit_dingbats(buffer: &ratatui::buffer::Buffer) {
        let area = *buffer.area();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                for ch in buffer[(x, y)].symbol().chars() {
                    assert!(
                        !(0x2776..=0x277f).contains(&(ch as u32)),
                        "dingbat {ch:?} rendered at ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn servers_band_rows_lead_with_counts() {
        use crate::api::schema::AgentStatus;
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("one");
        let _second = ws.test_split(ratatui::layout::Direction::Horizontal);
        app.workspaces = vec![ws];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = crate::app::Mode::Terminal;
        // Local tally: one blocked + one working pane -> `1 1 0`.
        let panes: Vec<_> = app.workspaces[0].tabs[0].panes.keys().copied().collect();
        for (pane, state) in panes
            .into_iter()
            .zip([AgentState::Blocked, AgentState::Working])
        {
            let tid = app.workspaces[0].terminal_id(pane).unwrap().clone();
            app.terminals.get_mut(&tid).unwrap().state = state;
        }
        // Peer tally: blocked + working workspace statuses -> `1 1 0`.
        let mut blocked = remote_summary("a", None, None, None);
        blocked.status = AgentStatus::Blocked;
        let mut working = remote_summary("b", None, None, None);
        working.status = AgentStatus::Working;
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![blocked, working])];

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (servers_area, _) = carve_servers_band(ws_area, servers_section_height(&app));
        let rows_area = server_band_rows_area(servers_area);
        let self_rect = server_slot_rect(rows_area, 0).expect("self slot");
        let peer_rect = server_slot_rect(rows_area, 1).expect("peer slot");

        // Default mark = leading counts `1 1 0 <name>`: fixed r/y/g columns,
        // zeros muted, single-digit width band-wide.
        for rect in [self_rect, peer_rect] {
            assert_eq!(buffer[(rect.x, rect.y)].symbol(), "1");
            assert_eq!(buffer[(rect.x, rect.y)].style().fg, Some(p.red));
            assert_eq!(buffer[(rect.x + 2, rect.y)].symbol(), "1");
            assert_eq!(buffer[(rect.x + 2, rect.y)].style().fg, Some(p.yellow));
            assert_eq!(buffer[(rect.x + 4, rect.y)].symbol(), "0");
            assert_eq!(
                buffer[(rect.x + 4, rect.y)].style().fg,
                Some(p.overlay0),
                "zero column is muted"
            );
        }
        // The host name follows the counts lead.
        let title: String = buffer_row_text(&buffer, self_rect, self_rect.y)
            .chars()
            .skip(6)
            .collect();
        assert!(
            title.starts_with(&crate::app::short_host_name()),
            "{title:?}"
        );

        // The rollup chips are gone: no circled-digit dingbats anywhere.
        assert_no_circled_digit_dingbats(&buffer);
    }

    #[test]
    fn band_counts_share_a_global_digit_width() {
        use crate::api::schema::AgentStatus;
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = crate::app::Mode::Terminal;
        // One peer with 10 working workspaces: every row pads to two digits.
        let workspaces: Vec<_> = (0..10)
            .map(|i| {
                let mut ws = remote_summary(&format!("w{i}"), None, None, None);
                ws.status = AgentStatus::Working;
                ws
            })
            .collect();
        app.peer_summaries = vec![peer_with_workspaces("anvil", workspaces)];

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (servers_area, _) = carve_servers_band(ws_area, servers_section_height(&app));
        let rows_area = server_band_rows_area(servers_area);
        let self_rect = server_slot_rect(rows_area, 0).expect("self slot");
        let peer_rect = server_slot_rect(rows_area, 1).expect("peer slot");

        // Peer: ` 0 10  0` — the working column hits two digits.
        let peer_title = buffer_row_text(&buffer, peer_rect, peer_rect.y);
        assert!(peer_title.starts_with(" 0 10  0 "), "{peer_title:?}");
        assert_eq!(
            buffer[(peer_rect.x + 3, peer_rect.y)].style().fg,
            Some(p.yellow)
        );
        // Self (no agents): every column widens to match — ` 0  0  0 <host>`.
        let self_title = buffer_row_text(&buffer, self_rect, self_rect.y);
        assert!(self_title.starts_with(" 0  0  0 "), "{self_title:?}");
    }
    #[test]
    fn medallion_mark_config_switches_the_band_to_the_medallion() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.mode = crate::app::Mode::Terminal;
        app.peer_summaries = vec![peer_with_workspaces("anvil", vec![])];
        app.server_state_mark = crate::config::ServerStateMarkConfig::MedallionQuadrant;

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (servers_area, _) = carve_servers_band(ws_area, servers_section_height(&app));
        let self_rect = server_slot_rect(server_band_rows_area(servers_area), 0).expect("self");
        // Quadrant fallback with a single (muted) ring: a solid rectangle.
        assert_eq!(buffer[(self_rect.x, self_rect.y)].symbol(), "\u{2588}");
        // No live agents: the medallion still marks presence in muted color.
        assert_eq!(
            buffer[(self_rect.x, self_rect.y)].style().fg,
            Some(app.palette.overlay0)
        );
    }

    #[test]
    fn collapsed_group_renders_packed_rects_and_plain_digit_counts() {
        let mut app = space_group_app();
        set_pane_state(&mut app, 0, AgentState::Blocked, true);
        set_pane_state(&mut app, 1, AgentState::Working, true);
        app.collapsed_space_keys.insert("grp".into());

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        let card = app.view.workspace_card_areas[0];
        let row = buffer_row_text(&buffer, card.rect, card.rect.y);
        // Group join r·y as packed rects, then exact counts as PLAIN digits
        // in the terminal font — `▮▮ 1 1`, not `❶❶`.
        assert!(row.contains("\u{25ae}\u{25ae} 1 1"), "{row:?}");
        let rects = row_glyph_positions(&buffer, card.rect, card.rect.y, "\u{25ae}");
        assert_eq!(buffer[(rects[0], card.rect.y)].style().fg, Some(p.red));
        assert_eq!(buffer[(rects[1], card.rect.y)].style().fg, Some(p.yellow));
        let digits = row_glyph_positions(&buffer, card.rect, card.rect.y, "1");
        assert_eq!(digits.len(), 2, "{row:?}");
        assert_eq!(buffer[(digits[0], card.rect.y)].style().fg, Some(p.red));
        assert_eq!(buffer[(digits[1], card.rect.y)].style().fg, Some(p.yellow));
        assert_no_circled_digit_dingbats(&buffer);
    }

    #[test]
    fn expanded_primary_row_still_carries_the_group_join_rects() {
        let mut app = space_group_app();
        set_pane_state(&mut app, 0, AgentState::Idle, true);
        set_pane_state(&mut app, 1, AgentState::Working, true);

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        // The expanded parent row aggregates its group: y·g rects even
        // though the member rows are visible ("main row has no traffic
        // light" fix).
        let parent = app.view.workspace_card_areas[0];
        let rects = row_glyph_positions(&buffer, parent.rect, parent.rect.y, "\u{25ae}");
        assert_eq!(rects.len(), 2, "parent row carries the group join");
        assert_eq!(buffer[(rects[0], parent.rect.y)].style().fg, Some(p.yellow));
        assert_eq!(buffer[(rects[1], parent.rect.y)].style().fg, Some(p.green));

        // The member row renders only its OWN join: a single working rect.
        let child = app.view.workspace_card_areas[1];
        let rects = row_glyph_positions(&buffer, child.rect, child.rect.y, "\u{25ae}");
        assert_eq!(rects.len(), 1, "member row carries its own join");
        assert_eq!(buffer[(rects[0], child.rect.y)].style().fg, Some(p.yellow));
    }

    /// #33 — two-level highlight: with a member workspace focused, BOTH the
    /// member's row (the standard active fill) and the section's primary
    /// row (the always-on session-currency marker) carry surface_dim; bold
    /// text stays on the active row alone.
    #[test]
    fn two_level_highlight_marks_active_member_and_its_primary() {
        let mut app = space_group_app();
        app.active = Some(1);

        let area = Rect::new(0, 0, 30, 40);
        let surface_dim = app.palette.surface_dim;
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let cards = app.view.workspace_card_areas.clone();
        assert_eq!((cards[0].ws_idx, cards[1].ws_idx), (0, 1));

        // Both levels carry the currency fill…
        assert_eq!(
            buffer[(cards[0].rect.x, cards[0].rect.y)].style().bg,
            Some(surface_dim),
            "primary row marks session currency"
        );
        assert_eq!(
            buffer[(cards[1].rect.x, cards[1].rect.y)].style().bg,
            Some(surface_dim),
            "active member row carries the standard fill"
        );

        // …but the focus emphasis (bold name) stays on the active row.
        let member_name_cell = &buffer[(cards[1].rect.x + 5, cards[1].rect.y)];
        assert!(member_name_cell
            .style()
            .add_modifier
            .contains(Modifier::BOLD));
        let primary_name_cell = &buffer[(cards[0].rect.x + 4, cards[0].rect.y)];
        assert!(!primary_name_cell
            .style()
            .add_modifier
            .contains(Modifier::BOLD));

        // With the primary itself active there is exactly one marked row.
        app.active = Some(0);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let cards = app.view.workspace_card_areas.clone();
        assert_eq!(
            buffer[(cards[0].rect.x, cards[0].rect.y)].style().bg,
            Some(surface_dim)
        );
        assert_ne!(
            buffer[(cards[1].rect.x, cards[1].rect.y)].style().bg,
            Some(surface_dim),
            "inactive member rows carry no fill"
        );
    }

    /// #33 — the primary row IS the section's selectable row (no synthetic
    /// header) and carries the join of the WHOLE section, including plain
    /// same-repo members merged in by the restructure; members indent
    /// under it with their own joins.
    #[test]
    fn primary_row_carries_section_join_across_plain_members() {
        let mut app = space_group_app();
        let mut plain = Workspace::test_new("scratch");
        plain.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: "grp".into(),
            checkout_key: "/repo/scratch".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/scratch"),
            is_linked_worktree: false,
            project_key: "dir:herdr".into(),
        });
        app.workspaces.push(plain);
        app.ensure_test_terminals();
        set_pane_state(&mut app, 0, AgentState::Idle, true);
        set_pane_state(&mut app, 1, AgentState::Working, true);
        set_pane_state(&mut app, 2, AgentState::Blocked, true);

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        // Three rows: the primary (main checkout) unindented, the linked
        // worktree AND the plain same-repo workspace indented under it.
        let cards = app.view.workspace_card_areas.clone();
        assert_eq!(cards.len(), 3);
        assert_eq!((cards[0].ws_idx, cards[0].indented), (0, false));
        assert_eq!((cards[1].ws_idx, cards[1].indented), (1, true));
        assert_eq!((cards[2].ws_idx, cards[2].indented), (2, true));

        // The primary row joins ALL members: r·y·g packed rects.
        let rects = row_glyph_positions(&buffer, cards[0].rect, cards[0].rect.y, "\u{25ae}");
        assert_eq!(rects.len(), 3, "primary row carries the section join");
        assert_eq!(buffer[(rects[0], cards[0].rect.y)].style().fg, Some(p.red));
        assert_eq!(
            buffer[(rects[1], cards[0].rect.y)].style().fg,
            Some(p.yellow)
        );
        assert_eq!(
            buffer[(rects[2], cards[0].rect.y)].style().fg,
            Some(p.green)
        );

        // The plain member row carries only its own (blocked) join.
        let rects = row_glyph_positions(&buffer, cards[2].rect, cards[2].rect.y, "\u{25ae}");
        assert_eq!(rects.len(), 1);
        assert_eq!(buffer[(rects[0], cards[2].rect.y)].style().fg, Some(p.red));

        // The group affordances live on the primary alone.
        assert_eq!(
            workspace_parent_group_state(&app, 0).map(|(key, _)| key),
            Some("grp".to_string())
        );
        assert_eq!(workspace_parent_group_state(&app, 2), None);
    }

    #[test]
    fn workspace_row_with_no_live_agents_renders_a_hollow_rect() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("solo")];
        app.ensure_test_terminals(); // Unknown state — no live agent signal
        app.active = Some(0);
        app.mode = crate::app::Mode::Terminal;

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);

        let card = app.view.workspace_card_areas[0];
        let hollow = row_glyph_positions(&buffer, card.rect, card.rect.y, "\u{25af}");
        assert_eq!(hollow.len(), 1, "no live agents → one hollow rect");
        assert_eq!(
            buffer[(hollow[0], card.rect.y)].style().fg,
            Some(app.palette.overlay0)
        );
        assert!(row_glyph_positions(&buffer, card.rect, card.rect.y, "\u{25ae}").is_empty());
    }

    #[test]
    fn workspace_rows_render_cached_pr_state_glyphs() {
        let mut app = space_group_app();
        // Two-line parent row: branch line carries `#12 ⊙` after the branch.
        app.workspaces[0].cached_git_branch = Some("feat-x".into());
        app.workspaces[0].pr_state = Some(crate::worktree::PrStateInfo {
            state: crate::worktree::PrState::Open,
            number: 12,
        });
        // One-line member row: the glyph rides the label line.
        app.workspaces[1].cached_git_branch = Some("worktree/feat-y".into());
        app.workspaces[1].pr_state = Some(crate::worktree::PrStateInfo {
            state: crate::worktree::PrState::Draft,
            number: 7,
        });

        let area = Rect::new(0, 0, 30, 40);
        let buffer = render_sidebar_to_buffer(&mut app, area);
        let p = &app.palette;

        let parent = app.view.workspace_card_areas[0];
        assert_eq!(parent.rect.height, 2);
        let branch_line = buffer_row_text(&buffer, parent.rect, parent.rect.y + 1);
        assert!(
            branch_line.contains("feat-x #12 \u{2299}"),
            "{branch_line:?}"
        );
        let glyph = row_glyph_positions(&buffer, parent.rect, parent.rect.y + 1, "\u{2299}");
        assert_eq!(
            buffer[(glyph[0], parent.rect.y + 1)].style().fg,
            Some(p.accent)
        );

        let child = app.view.workspace_card_areas[1];
        assert_eq!(child.rect.height, 1);
        let row = buffer_row_text(&buffer, child.rect, child.rect.y);
        assert!(row.contains("#7 \u{25d0}"), "{row:?}");
        let glyph = row_glyph_positions(&buffer, child.rect, child.rect.y, "\u{25d0}");
        assert_eq!(
            buffer[(glyph[0], child.rect.y)].style().fg,
            Some(p.overlay0)
        );
    }

    /// Consolidation proof for the agents panel + leading circles: every
    /// state rendering sources its color from the one severity mapping.
    #[test]
    fn agent_icons_and_state_dots_source_colors_from_the_state_mapping() {
        let p = crate::app::state::AppState::test_new().palette;
        for (state, seen, class) in [
            (AgentState::Blocked, true, StateClass::Blocked),
            (AgentState::Working, true, StateClass::Working),
            (AgentState::Idle, false, StateClass::Done),
            (AgentState::Idle, true, StateClass::Idle),
            (AgentState::Unknown, true, StateClass::None),
        ] {
            let expected = Some(class.color(&p));
            assert_eq!(agent_icon(state, seen, 0, &p).1.fg, expected, "{class:?}");
            assert_eq!(state_dot(state, seen, &p).1.fg, expected, "{class:?}");
            assert_eq!(
                Some(state_label_color(state, seen, &p)),
                expected,
                "{class:?}"
            );
        }
        // The blocked/working/done anchors stay pinned to the palette.
        assert_eq!(StateClass::Blocked.color(&p), p.red);
        assert_eq!(StateClass::Working.color(&p), p.yellow);
        assert_eq!(StateClass::Done.color(&p), p.teal);
        assert_eq!(StateClass::Idle.color(&p), p.green);
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn all_workspaces_primary_label_truncates_workspace_and_tab() {
        let entry = AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            primary_label: "agent-browser".into(),
            primary_tab_label: Some("test-escalation".into()),
            agent_label: Some("claude".into()),
            state: AgentState::Idle,
            seen: true,
            custom_status: None,
            header_fields: Vec::new(),
            live_activity: None,
            state_labels: std::collections::HashMap::new(),
        };

        let label = format_agent_panel_primary_label(&entry, 18);

        assert_eq!(label, "agent-bro… · test…");
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        // The bottom two rows stay reserved for the pinned menu band; the
        // sections split the three rows above it.
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9, 0);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 2));
        assert_eq!(detail_area, Rect::new(0, 2, 19, 1));
    }

    #[test]
    fn sidebar_sections_end_above_the_pinned_menu_band() {
        let area = Rect::new(0, 0, 26, 20);
        let (ws_area, detail_area) = expanded_sidebar_sections(area, 0.5, 0);

        // servers/spaces + agents fill everything above the 2-row band.
        assert_eq!(
            ws_area.height + detail_area.height,
            area.height - SIDEBAR_MENU_BAND_ROWS
        );

        // The menu row is the very last sidebar row, its divider directly
        // above, both directly below the agents section.
        let row = sidebar_menu_row_rect(area, 0);
        let divider = sidebar_menu_divider_rect(area, 0);
        assert_eq!(row, Rect::new(0, 19, 25, 1));
        assert_eq!(divider, Rect::new(0, 18, 25, 1));
        assert_eq!(detail_area.y + detail_area.height, divider.y);

        // Degenerate heights reserve nothing.
        assert_eq!(
            sidebar_menu_row_rect(Rect::new(0, 0, 26, 1), 0),
            Rect::default()
        );
        assert_eq!(
            sidebar_menu_divider_rect(Rect::new(0, 0, 26, 1), 0),
            Rect::default()
        );
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5, 0);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
            project_key: format!("dir:{key}"),
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn sidebar_pane_gap_shrinks_content_symmetrically() {
        let area = Rect::new(0, 0, 26, 20);

        // gap 0: content is everything except the divider column (legacy behavior).
        let (ws_area, detail_area) = expanded_sidebar_sections(area, 0.5, 0);
        assert_eq!(ws_area.width, 25);
        assert_eq!(detail_area.width, 25);

        // gap 2: content also leaves two blank columns before the divider.
        let (ws_area, detail_area) = expanded_sidebar_sections(area, 0.5, 2);
        assert_eq!(ws_area.width, 23);
        assert_eq!(detail_area.width, 23);

        let divider = sidebar_section_divider_rect(area, 0.5, 2);
        assert_eq!(divider.width, 23);

        let (ws_area, _, detail_area) = collapsed_sidebar_sections(Rect::new(0, 0, 6, 20), 2);
        assert_eq!(ws_area.width, 3);
        assert_eq!(detail_area.width, 3);
    }

    #[test]
    fn sidebar_row_gap_zero_packs_workspace_cards_adjacent() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            crate::workspace::Workspace::test_new("one"),
            crate::workspace::Workspace::test_new("two"),
        ];
        let area = Rect::new(0, 0, 30, 20);

        let (cards, _) = compute_workspace_list_areas(&app, area);
        assert_eq!(
            cards[1].rect.y,
            cards[0].rect.y + cards[0].rect.height + 1,
            "default gap is one blank row"
        );

        app.sidebar_row_gap = 0;
        let (cards, _) = compute_workspace_list_areas(&app, area);
        assert_eq!(
            cards[1].rect.y,
            cards[0].rect.y + cards[0].rect.height,
            "gap 0 packs cards adjacent"
        );
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_offset_can_start_inside_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 14));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn spaces_current_scope_renders_only_focused_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.mode = Mode::Terminal;
        app.active = Some(1);
        app.spaces_panel_scope = PanelScope::Current;

        // Focused grouped workspace: the whole group block renders — parent
        // plus members — and nothing else.
        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        // Focused ungrouped workspace: just that workspace.
        app.active = Some(2);
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 2,
                indented: false,
            }]
        );

        // Scope all: the full list, unchanged.
        app.spaces_panel_scope = PanelScope::All;
        assert_eq!(workspace_list_entries(&app).len(), 3);
    }

    #[test]
    fn spaces_current_scope_stays_orthogonal_to_group_collapse() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.mode = Mode::Terminal;
        app.active = Some(1);
        app.spaces_panel_scope = PanelScope::Current;
        app.collapsed_space_keys.insert("repo-key".into());

        // Collapse still folds members within the rendered group: parent +
        // the focused child only.
        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = Some(0);
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn spaces_current_scope_keeps_focused_project_remotes_and_hides_remote_only_projects() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_project_key("herdr", "github.com/gerchowl/herdr"),
            workspace_with_project_key("other", "github.com/gerchowl/other"),
        ];
        app.peer_summaries = vec![peer_with_workspaces(
            "anvil",
            vec![
                remote_summary(
                    "herdr",
                    Some("github.com/gerchowl/herdr"),
                    Some("herdr"),
                    Some("fix/pty"),
                ),
                remote_summary(
                    "dotfiles",
                    Some("github.com/gerchowl/dotfiles"),
                    Some("dotfiles"),
                    None,
                ),
            ],
        )];
        app.mode = Mode::Terminal;
        app.active = Some(0);
        app.spaces_panel_scope = PanelScope::Current;

        // The focused project keeps its spliced remote rows; the second
        // local project and the remote-only trailing project both hide.
        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Remote {
                    peer: crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                    ws_idx: 0,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn spaces_current_scope_clamps_keyboard_selection_to_visible_entries() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 0;
        app.spaces_panel_scope = PanelScope::Current;

        // Selection moves through the visible (focused-group) entries only:
        // a large delta clamps to the last group member, never reaching the
        // hidden flat workspace.
        app.move_selected_workspace_by_visible_delta(5);
        assert_eq!(app.selected, 1);
        app.move_selected_workspace_by_visible_delta(-5);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn spaces_header_renders_scope_toggle_label() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.mode = crate::app::Mode::Terminal;
        app.spaces_panel_scope = PanelScope::Current;

        let area = Rect::new(0, 0, 30, 40);
        let mut terminal =
            Terminal::new(TestBackend::new(30, 40)).expect("test terminal should initialize");
        let runtimes = TerminalRuntimeRegistry::new();
        terminal
            .draw(|frame| render_sidebar(&app, &runtimes, frame, area))
            .expect("sidebar should render");

        let (ws_area, _) =
            expanded_sidebar_sections(area, app.sidebar_section_split, app.sidebar_pane_gap);
        let (_, list_area) = carve_servers_band(ws_area, servers_section_height(&app));
        let buffer = terminal.backend().buffer();
        let header_text: String = (list_area.x..list_area.x + list_area.width)
            .map(|x| buffer[(x, list_area.y)].symbol().to_string())
            .collect();
        assert!(header_text.starts_with(" spaces"), "{header_text:?}");
        assert!(
            header_text.trim_end().ends_with("current"),
            "{header_text:?}"
        );
    }

    /// #33 — git-first sections: plain same-repo workspaces group into one
    /// project section; the first non-linked checkout is the primary row.
    #[test]
    fn plain_same_repo_workspaces_group_into_one_project_section() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    /// #33 — a plain same-repo workspace becomes a member of the existing
    /// worktree-space section (members in storage order under the primary).
    #[test]
    fn plain_same_repo_workspace_attaches_to_its_project_section() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
            ]
        );
    }

    /// #33 — two checkouts of the same project with DIFFERENT git common
    /// dirs (separate clones / a plain clone next to a worktree family)
    /// converge into one section via the machine-independent project_key;
    /// the section's canonical key stays the membership key, so collapse
    /// state keyed on it survives metadata resolution.
    #[test]
    fn same_origin_checkouts_merge_into_one_section_keyed_by_membership() {
        let mut app = AppState::test_new();
        let mut main = workspace_with_project_key("herdr", "github.com/gerchowl/herdr");
        main.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "family-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        // A separate clone of the same origin: different common dir, no
        // membership.
        let clone = workspace_with_project_key("herdr-clone", "github.com/gerchowl/herdr");
        app.workspaces = vec![main, clone];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
        assert_eq!(
            workspace_parent_group_state(&app, 0),
            Some(("family-key".into(), false))
        );
        // Collapsing by the membership key folds the whole merged section.
        app.collapsed_space_keys.insert("family-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    /// #33 — resolved non-git workspaces collect under the trailing `misc`
    /// section: git projects first, misc last, regardless of storage order.
    #[test]
    fn resolved_non_git_workspaces_collect_in_trailing_misc_section() {
        let mut app = AppState::test_new();
        let mut notes = Workspace::test_new("notes");
        notes.cached_git_branch = None;
        notes.git_identity_resolved = true;
        app.workspaces = vec![
            notes,
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("other", "elsewhere"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
            ]
        );
    }

    /// #33 — a workspace whose git probe hasn't finished must NOT flash
    /// into misc: it holds its storage position until the identity lands.
    #[test]
    fn pending_git_identity_holds_position_not_misc() {
        let mut app = AppState::test_new();
        let mut pending = Workspace::test_new("fresh");
        pending.cached_git_branch = None;
        assert!(pending.git_identity_pending());
        app.workspaces = vec![pending, workspace_with_git_space("one", "repo-key")];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );

        // The probe resolves non-git: the row moves to the trailing misc
        // section.
        app.workspaces[0].git_identity_resolved = true;
        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
            ]
        );
    }

    /// #33 — remote-only project groups still count as git projects: they
    /// trail the local sections but render BEFORE the misc section.
    #[test]
    fn remote_only_projects_render_before_misc() {
        let mut app = AppState::test_new();
        let mut notes = Workspace::test_new("notes");
        notes.cached_git_branch = None;
        notes.git_identity_resolved = true;
        app.workspaces = vec![
            notes,
            workspace_with_project_key("herdr", "github.com/gerchowl/herdr"),
        ];
        app.peer_summaries = vec![peer_with_workspaces(
            "sage",
            vec![remote_summary(
                "dotfiles",
                Some("github.com/gerchowl/dotfiles"),
                Some("dotfiles"),
                None,
            )],
        )];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
                WorkspaceListEntry::Remote {
                    peer: crate::app::state::RemotePeerRef::Config { peer_idx: 0 },
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }
}
