use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneRuntime, PaneState};
use crate::workspace::Workspace;

use super::{DirectionSnapshot, LayoutSnapshot, SessionSnapshot, TabSnapshot, WorkspaceSnapshot};

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Vec<Workspace> {
    snapshot
        .workspaces
        .iter()
        .filter_map(|ws_snap| {
            restore_workspace(
                ws_snap,
                rows,
                cols,
                scrollback_limit_bytes,
                events.clone(),
                render_notify.clone(),
                render_dirty.clone(),
            )
        })
        .collect()
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<Workspace> {
    let mut tabs = Vec::new();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_pane_number = 1;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let tab = restore_tab(
            tab_snap,
            idx + 1,
            rows,
            cols,
            scrollback_limit_bytes,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        )?;
        for pane_id in tab.layout.pane_ids() {
            public_pane_numbers.insert(pane_id, next_public_pane_number);
            next_public_pane_number += 1;
        }
        tabs.push(tab);
    }

    if tabs.is_empty() {
        return None;
    }

    Some(Workspace {
        id: snap
            .id
            .clone()
            .unwrap_or_else(crate::workspace::generate_workspace_id),
        custom_name: snap.custom_name.clone(),
        identity_cwd: snap.identity_cwd.clone(),
        cached_git_ahead_behind: None,
        public_pane_numbers,
        next_public_pane_number,
        active_tab: snap.active_tab.min(tabs.len().saturating_sub(1)),
        tabs,
    })
}

fn restore_tab(
    snap: &TabSnapshot,
    number: usize,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<crate::workspace::Tab> {
    let (node, id_map) = restore_node_remapped(&snap.layout);
    let reverse_id_map: HashMap<PaneId, u32> = id_map
        .iter()
        .map(|(&old_id, &new_id)| (new_id, old_id))
        .collect();
    let pane_ids = collect_pane_ids(&node);

    let mut panes = HashMap::new();
    let mut pane_cwds = HashMap::new();
    let mut runtimes = HashMap::new();
    for id in &pane_ids {
        let saved_cwd = reverse_id_map
            .get(id)
            .and_then(|old_id| snap.panes.get(old_id))
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let cwd = if saved_cwd.exists() {
            saved_cwd
        } else {
            warn!(
                cwd = %saved_cwd.display(),
                "saved pane cwd does not exist, falling back to HOME"
            );
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/"));
            if home.exists() {
                home
            } else {
                PathBuf::from("/")
            }
        };

        match PaneRuntime::spawn(
            *id,
            rows,
            cols,
            cwd.clone(),
            scrollback_limit_bytes,
            crate::terminal_theme::TerminalTheme::default(),
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        ) {
            Ok(runtime) => {
                panes.insert(*id, PaneState::new());
                pane_cwds.insert(*id, cwd.clone());
                runtimes.insert(*id, runtime);
            }
            Err(e) => {
                error!(
                    tab = ?snap.custom_name,
                    pane_id = id.raw(),
                    err = %e,
                    "failed to restore pane, skipping"
                );
            }
        }
    }

    if panes.is_empty() {
        warn!(
            tab = ?snap.custom_name,
            "no panes could be restored for tab, dropping it"
        );
        return None;
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(node) = prune_restored_node(node, &surviving) else {
        warn!(
            tab = ?snap.custom_name,
            "restored tab lost all panes after pruning missing layout nodes"
        );
        return None;
    };
    let pane_ids = collect_pane_ids(&node);
    let focus = resolve_restored_pane(snap.focused, &id_map, &surviving, &pane_ids)?;
    let root_pane = resolve_restored_pane(snap.root_pane, &id_map, &surviving, &pane_ids)?;
    let layout = TileLayout::from_saved(node, focus);

    Some(crate::workspace::Tab {
        custom_name: snap.custom_name.clone(),
        number,
        root_pane,
        layout,
        panes,
        pane_cwds,
        runtimes,
        zoomed: snap.zoomed,
        events,
        render_notify,
        render_dirty,
    })
}

pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_old_id: Option<u32>,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_old_id
        .and_then(|old_id| id_map.get(&old_id).copied())
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_node_round_trip() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(0))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(1))),
                second: Box::new(Node::Pane(PaneId::from_raw(2))),
            }),
        };

        let snap = super::super::snapshot::capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snap);

        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };
        let surviving = std::collections::HashSet::from([keep]);

        let pruned = prune_restored_node(node, &surviving).expect("remaining pane should survive");

        assert!(matches!(pruned, Node::Pane(id) if id == keep));
    }

    #[test]
    fn resolve_restored_pane_prefers_surviving_saved_id_and_falls_back_to_first_remaining() {
        let first = PaneId::from_raw(21);
        let second = PaneId::from_raw(22);
        let id_map = HashMap::from([(0_u32, first), (1_u32, second)]);
        let surviving = std::collections::HashSet::from([first]);
        let pane_ids = vec![first];

        assert_eq!(
            resolve_restored_pane(Some(0), &id_map, &surviving, &pane_ids),
            Some(first)
        );
        assert_eq!(
            resolve_restored_pane(Some(1), &id_map, &surviving, &pane_ids),
            Some(first)
        );
    }
}
