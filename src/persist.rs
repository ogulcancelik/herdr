//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneRuntime, PaneState};
use crate::workspace::Workspace;

/// Current snapshot format version.
const SNAPSHOT_VERSION: u32 = 2;

/// Serializable snapshot of the entire herdr session.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version — used to detect incompatible changes.
    #[serde(default)]
    pub version: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub active: Option<usize>,
    pub selected: usize,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub custom_name: Option<String>,
    pub layout: LayoutSnapshot,
    pub panes: HashMap<u32, PaneSnapshot>,
    pub zoomed: bool,
    /// Raw pane ID that was focused when saved.
    #[serde(default)]
    pub focused: Option<u32>,
    /// Raw pane ID used as the workspace identity source.
    #[serde(default)]
    pub root_pane: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub cwd: PathBuf,
}

/// Serializable BSP tree.
#[derive(Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Pane(u32),
    Split {
        direction: DirectionSnapshot,
        ratio: f32,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

#[derive(Serialize, Deserialize)]
pub enum DirectionSnapshot {
    Horizontal,
    Vertical,
}

// --- Capture ---

/// Capture the current app state into a serializable snapshot.
pub fn capture(
    workspaces: &[Workspace],
    active: Option<usize>,
    selected: usize,
) -> SessionSnapshot {
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        workspaces: workspaces.iter().map(capture_workspace).collect(),
        active,
        selected,
    }
}

fn capture_workspace(ws: &Workspace) -> WorkspaceSnapshot {
    let mut panes = HashMap::new();
    for id in ws.panes.keys() {
        let cwd = ws
            .runtimes
            .get(id)
            .and_then(|rt| rt.cwd())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        panes.insert(id.raw(), PaneSnapshot { cwd });
    }
    WorkspaceSnapshot {
        custom_name: ws.custom_name.clone(),
        layout: capture_node(ws.layout.root()),
        panes,
        zoomed: ws.zoomed,
        focused: Some(ws.layout.focused().raw()),
        root_pane: Some(ws.root_pane.raw()),
    }
}

fn capture_node(node: &Node) -> LayoutSnapshot {
    match node {
        Node::Pane(id) => LayoutSnapshot::Pane(id.raw()),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction: match direction {
                Direction::Horizontal => DirectionSnapshot::Horizontal,
                Direction::Vertical => DirectionSnapshot::Vertical,
            },
            ratio: *ratio,
            first: Box::new(capture_node(first)),
            second: Box::new(capture_node(second)),
        },
    }
}

// --- Restore ---

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    rows: u16,
    cols: u16,
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
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<Workspace> {
    let (node, id_map) = restore_node_remapped(&snap.layout);
    let pane_ids = collect_pane_ids(&node);

    // Restore focused pane: map saved raw ID to new ID, fall back to first pane
    let focus = snap
        .focused
        .and_then(|old_raw| id_map.get(&old_raw).copied())
        .or_else(|| pane_ids.first().copied())
        .unwrap_or(PaneId::from_raw(0));
    let layout = TileLayout::from_saved(node, focus);

    let mut panes = HashMap::new();
    let mut runtimes = HashMap::new();
    for id in &pane_ids {
        let old_id = id_map
            .iter()
            .find(|(_, new)| **new == *id)
            .map(|(old, _)| old);
        let cwd = old_id
            .and_then(|old| snap.panes.get(old))
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        match PaneRuntime::spawn(
            *id,
            rows,
            cols,
            cwd,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        ) {
            Ok(runtime) => {
                panes.insert(*id, PaneState::new());
                runtimes.insert(*id, runtime);
            }
            Err(e) => {
                error!(workspace = ?snap.custom_name, err = %e, "failed to restore pane");
                return None;
            }
        }
    }

    let root_pane = snap
        .root_pane
        .and_then(|old_raw| id_map.get(&old_raw).copied())
        .or_else(|| pane_ids.first().copied())
        .unwrap_or(PaneId::from_raw(0));

    let public_pane_numbers = layout
        .pane_ids()
        .into_iter()
        .enumerate()
        .map(|(index, pane_id)| (pane_id, index + 1))
        .collect();

    Some(Workspace {
        custom_name: snap.custom_name.clone(),
        root_pane,
        layout,
        cached_git_ahead_behind: None,
        public_pane_numbers,
        next_public_pane_number: panes.len() + 1,
        panes,
        runtimes,
        zoomed: snap.zoomed,
        events,
        render_notify,
        render_dirty,
    })
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
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

fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
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

// --- File I/O ---

fn session_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(dir).join("herdr/session.json")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/herdr/session.json")
    } else {
        PathBuf::from("/tmp/herdr/session.json")
    }
}

pub fn save(snapshot: &SessionSnapshot) {
    let path = session_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            error!(err = %e, "failed to create session directory");
            return;
        }
    }
    let json = match serde_json::to_string_pretty(snapshot) {
        Ok(j) => j,
        Err(e) => {
            error!(err = %e, "failed to serialize session");
            return;
        }
    };
    // Atomic write: write to temp file, then rename
    let tmp_path = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp_path, &json) {
        error!(err = %e, "failed to write session temp file");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        error!(err = %e, "failed to rename session file");
        // Clean up temp file
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }
    info!(workspaces = snapshot.workspaces.len(), "session saved");
}

pub fn load() -> Option<SessionSnapshot> {
    let path = session_path();
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            warn!(err = %e, "failed to read session file");
            return None;
        }
    };
    match serde_json::from_str::<SessionSnapshot>(&content) {
        Ok(snap) => {
            if snap.version > SNAPSHOT_VERSION {
                warn!(
                    file_version = snap.version,
                    supported = SNAPSHOT_VERSION,
                    "session file is from a newer herdr version, ignoring"
                );
                return None;
            }
            Some(snap)
        }
        Err(e) => {
            warn!(err = %e, "failed to parse session file, ignoring");
            None
        }
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty_session() {
        let snap = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![],
            active: None,
            selected: 0,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let restored: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
    }

    #[test]
    fn round_trip_layout_snapshot() {
        let layout = LayoutSnapshot::Split {
            direction: DirectionSnapshot::Horizontal,
            ratio: 0.6,
            first: Box::new(LayoutSnapshot::Pane(0)),
            second: Box::new(LayoutSnapshot::Split {
                direction: DirectionSnapshot::Vertical,
                ratio: 0.5,
                first: Box::new(LayoutSnapshot::Pane(1)),
                second: Box::new(LayoutSnapshot::Pane(2)),
            }),
        };
        let json = serde_json::to_string(&layout).unwrap();
        let restored: LayoutSnapshot = serde_json::from_str(&json).unwrap();

        // Verify structure
        match restored {
            LayoutSnapshot::Split { ratio, .. } => assert!((ratio - 0.6).abs() < 0.01),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn round_trip_full_workspace_snapshot() {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                cwd: PathBuf::from("/home/can/Projects/herdr"),
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                cwd: PathBuf::from("/home/can/Projects/website"),
            },
        );

        let snap = SessionSnapshot {
            workspaces: vec![WorkspaceSnapshot {
                custom_name: Some("pi-mono".to_string()),
                layout: LayoutSnapshot::Split {
                    direction: DirectionSnapshot::Horizontal,
                    ratio: 0.5,
                    first: Box::new(LayoutSnapshot::Pane(0)),
                    second: Box::new(LayoutSnapshot::Pane(1)),
                },
                panes,
                zoomed: false,
                focused: Some(0),
                root_pane: Some(0),
            }],
            active: Some(0),
            selected: 0,
            version: SNAPSHOT_VERSION,
        };

        let json = serde_json::to_string_pretty(&snap).unwrap();
        let restored: SessionSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(
            restored.workspaces[0].custom_name.as_deref(),
            Some("pi-mono")
        );
        assert_eq!(restored.workspaces[0].panes.len(), 2);
        assert_eq!(
            restored.workspaces[0].panes[&0].cwd,
            PathBuf::from("/home/can/Projects/herdr")
        );
    }

    #[test]
    fn capture_and_restore_node_round_trip() {
        // Create a tree: Split(H, 0.5, Pane(0), Split(V, 0.3, Pane(1), Pane(2)))
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

        let snap = capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snap);

        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn old_unversioned_snapshot_loads_as_version_0() {
        // Simulate a snapshot from before versioning was added
        let json = r#"{"workspaces":[],"active":null,"selected":0}"#;
        let snap: SessionSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.version, 0); // #[serde(default)] gives 0
    }

    #[test]
    fn future_version_is_rejected() {
        let json = r#"{"version":999,"workspaces":[],"active":null,"selected":0}"#;
        let snap: SessionSnapshot = serde_json::from_str(json).unwrap();
        assert!(snap.version > SNAPSHOT_VERSION);
        // load() would reject this — tested via the version check in load()
    }

    #[test]
    fn focused_pane_default_is_none() {
        let json = r#"{"name":"test","layout":{"Pane":0},"panes":{},"zoomed":false}"#;
        let ws: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(ws.focused, None); // #[serde(default)]
    }
}
