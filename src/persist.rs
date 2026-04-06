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
const SNAPSHOT_VERSION: u32 = 3;

/// Serializable snapshot of the entire herdr session.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version — used to detect incompatible changes.
    #[serde(default)]
    pub version: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub active: Option<usize>,
    pub selected: usize,
    #[serde(default)]
    pub agent_panel_scope: crate::app::state::AgentPanelScope,
    #[serde(default)]
    pub sidebar_width: Option<u16>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub identity_cwd: PathBuf,
    pub tabs: Vec<TabSnapshot>,
    #[serde(default)]
    pub active_tab: usize,
}

#[derive(Deserialize)]
struct LegacyWorkspaceSnapshot {
    #[serde(default)]
    custom_name: Option<String>,
    layout: LayoutSnapshot,
    panes: HashMap<u32, PaneSnapshot>,
    zoomed: bool,
    #[serde(default)]
    focused: Option<u32>,
    #[serde(default)]
    root_pane: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct TabSnapshot {
    #[serde(default)]
    pub custom_name: Option<String>,
    pub layout: LayoutSnapshot,
    pub panes: HashMap<u32, PaneSnapshot>,
    pub zoomed: bool,
    #[serde(default)]
    pub focused: Option<u32>,
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

impl From<LegacyWorkspaceSnapshot> for WorkspaceSnapshot {
    fn from(snap: LegacyWorkspaceSnapshot) -> Self {
        let identity_cwd = legacy_identity_cwd(&snap);
        let tab = TabSnapshot {
            custom_name: None,
            layout: snap.layout,
            panes: snap.panes,
            zoomed: snap.zoomed,
            focused: snap.focused,
            root_pane: snap.root_pane,
        };

        Self {
            id: None,
            custom_name: snap.custom_name,
            identity_cwd,
            tabs: vec![tab],
            active_tab: 0,
        }
    }
}

#[derive(Deserialize)]
struct RawSessionSnapshot {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    workspaces: Vec<serde_json::Value>,
    #[serde(default)]
    active: Option<usize>,
    #[serde(default)]
    selected: usize,
    #[serde(default)]
    agent_panel_scope: crate::app::state::AgentPanelScope,
    #[serde(default)]
    sidebar_width: Option<u16>,
}

fn migrate_snapshot(raw: RawSessionSnapshot) -> Result<SessionSnapshot, String> {
    Ok(SessionSnapshot {
        version: raw.version,
        workspaces: raw
            .workspaces
            .into_iter()
            .map(migrate_workspace)
            .collect::<Result<Vec<_>, _>>()?,
        active: raw.active,
        selected: raw.selected,
        agent_panel_scope: raw.agent_panel_scope,
        sidebar_width: raw.sidebar_width,
    })
}

fn migrate_workspace(raw: serde_json::Value) -> Result<WorkspaceSnapshot, String> {
    if raw.get("identity_cwd").is_some() {
        return serde_json::from_value(raw).map_err(|e| e.to_string());
    }

    if raw.get("layout").is_some() {
        let legacy =
            serde_json::from_value::<LegacyWorkspaceSnapshot>(raw).map_err(|e| e.to_string())?;
        return Ok(legacy.into());
    }

    Err("workspace snapshot is neither current nor legacy format".to_string())
}

fn legacy_identity_cwd(snap: &LegacyWorkspaceSnapshot) -> PathBuf {
    let root_pane = snap
        .root_pane
        .or_else(|| first_pane_id_in_layout(&snap.layout));

    root_pane
        .and_then(|pane_id| snap.panes.get(&pane_id))
        .map(|pane| pane.cwd.clone())
        .or_else(|| {
            first_pane_id_in_layout(&snap.layout)
                .and_then(|pane_id| snap.panes.get(&pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .or_else(|| {
            snap.panes
                .keys()
                .min()
                .and_then(|pane_id| snap.panes.get(pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()))
}

fn first_pane_id_in_layout(layout: &LayoutSnapshot) -> Option<u32> {
    match layout {
        LayoutSnapshot::Pane(id) => Some(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            first_pane_id_in_layout(first).or_else(|| first_pane_id_in_layout(second))
        }
    }
}

// --- Capture ---

/// Capture the current app state into a serializable snapshot.
pub fn capture(
    workspaces: &[Workspace],
    active: Option<usize>,
    selected: usize,
    agent_panel_scope: crate::app::state::AgentPanelScope,
    sidebar_width: u16,
) -> SessionSnapshot {
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        workspaces: workspaces.iter().map(capture_workspace).collect(),
        active,
        selected,
        agent_panel_scope,
        sidebar_width: Some(sidebar_width),
    }
}

fn capture_workspace(ws: &Workspace) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        id: Some(ws.id.clone()),
        custom_name: ws.custom_name.clone(),
        identity_cwd: ws
            .resolved_identity_cwd()
            .unwrap_or_else(|| ws.identity_cwd.clone()),
        tabs: ws.tabs.iter().map(capture_tab).collect(),
        active_tab: ws.active_tab,
    }
}

fn capture_tab(tab: &crate::workspace::Tab) -> TabSnapshot {
    let mut panes = HashMap::new();
    for id in tab.panes.keys() {
        let cwd = tab
            .cwd_for_pane(*id)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        panes.insert(id.raw(), PaneSnapshot { cwd });
    }
    TabSnapshot {
        custom_name: tab.custom_name.clone(),
        layout: capture_node(tab.layout.root()),
        panes,
        zoomed: tab.zoomed,
        focused: Some(tab.layout.focused().raw()),
        root_pane: Some(tab.root_pane.raw()),
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
    let pane_ids = collect_pane_ids(&node);
    let focus = snap
        .focused
        .and_then(|old_raw| id_map.get(&old_raw).copied())
        .or_else(|| pane_ids.first().copied())
        .unwrap_or(PaneId::from_raw(0));
    let layout = TileLayout::from_saved(node, focus);

    let mut panes = HashMap::new();
    let mut pane_cwds = HashMap::new();
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
                error!(tab = ?snap.custom_name, err = %e, "failed to restore pane");
                return None;
            }
        }
    }

    let root_pane = snap
        .root_pane
        .and_then(|old_raw| id_map.get(&old_raw).copied())
        .or_else(|| pane_ids.first().copied())
        .unwrap_or(PaneId::from_raw(0));

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
    crate::config::config_dir().join("session.json")
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

fn parse_snapshot(content: &str) -> Result<SessionSnapshot, String> {
    let raw = serde_json::from_str::<RawSessionSnapshot>(content).map_err(|e| e.to_string())?;
    if raw.version > SNAPSHOT_VERSION {
        return Err(format!(
            "snapshot version {} is newer than supported {}",
            raw.version, SNAPSHOT_VERSION
        ));
    }
    migrate_snapshot(raw)
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
    match parse_snapshot(&content) {
        Ok(snap) => Some(snap),
        Err(e) => {
            if let Ok(raw) = serde_json::from_str::<RawSessionSnapshot>(&content) {
                if raw.version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = raw.version,
                        supported = SNAPSHOT_VERSION,
                        "session file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(err = %e, "failed to parse session file, ignoring");
            None
        }
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn session_fixture(name: &str) -> &'static str {
        match name {
            "current-herdr" => include_str!("../tests/fixtures/session/current-herdr-session.json"),
            "current-herdr-dev" => {
                include_str!("../tests/fixtures/session/current-herdr-dev-session.json")
            }
            "legacy-pre-tabs-v2" => {
                include_str!("../tests/fixtures/session/legacy-pre-tabs-v2.json")
            }
            other => panic!("unknown session fixture: {other}"),
        }
    }

    #[test]
    fn round_trip_empty_session() {
        let snap = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![],
            active: None,
            selected: 0,
            agent_panel_scope: crate::app::state::AgentPanelScope::CurrentWorkspace,
            sidebar_width: Some(26),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
        assert_eq!(restored.sidebar_width, Some(26));
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
                id: Some("wproj".to_string()),
                custom_name: Some("pi-mono".to_string()),
                identity_cwd: PathBuf::from("/home/can/Projects/herdr"),
                tabs: vec![TabSnapshot {
                    custom_name: Some("api".to_string()),
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
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            agent_panel_scope: crate::app::state::AgentPanelScope::CurrentWorkspace,
            sidebar_width: Some(26),
            version: SNAPSHOT_VERSION,
        };

        let json = serde_json::to_string_pretty(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(restored.workspaces[0].id.as_deref(), Some("wproj"));
        assert_eq!(
            restored.workspaces[0].custom_name.as_deref(),
            Some("pi-mono")
        );
        assert_eq!(restored.workspaces[0].tabs.len(), 1);
        assert_eq!(restored.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/home/can/Projects/herdr")
        );
        assert_eq!(
            restored.agent_panel_scope,
            crate::app::state::AgentPanelScope::CurrentWorkspace
        );
        assert_eq!(restored.sidebar_width, Some(26));
    }

    #[test]
    fn current_session_fixture_parses() {
        let snap = parse_snapshot(session_fixture("current-herdr")).unwrap();

        assert_eq!(snap.version, 3);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(snap.active, Some(0));
        assert_eq!(snap.selected, 0);
        assert_eq!(
            snap.agent_panel_scope,
            crate::app::state::AgentPanelScope::CurrentWorkspace
        );
        assert_eq!(snap.sidebar_width, None);
        assert_eq!(snap.workspaces[0].tabs.len(), 2);
        assert_eq!(
            snap.workspaces[1].identity_cwd,
            PathBuf::from("/home/test/projects/project-b")
        );
    }

    #[test]
    fn current_dev_session_fixture_parses_additive_fields() {
        let snap = parse_snapshot(session_fixture("current-herdr-dev")).unwrap();

        assert_eq!(snap.version, 3);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(
            snap.agent_panel_scope,
            crate::app::state::AgentPanelScope::CurrentWorkspace
        );
        assert_eq!(snap.workspaces[0].active_tab, 1);
        assert_eq!(snap.workspaces[1].tabs[0].panes.len(), 2);
    }

    #[test]
    fn old_snapshot_defaults_agent_panel_scope() {
        let json = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "workspaces": [],
            "active": null,
            "selected": 0
        })
        .to_string();

        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(
            restored.agent_panel_scope,
            crate::app::state::AgentPanelScope::CurrentWorkspace
        );
        assert_eq!(restored.sidebar_width, None);
    }

    #[test]
    fn legacy_workspace_snapshot_migrates_to_single_tab() {
        let snap = parse_snapshot(session_fixture("legacy-pre-tabs-v2")).unwrap();
        let ws = &snap.workspaces[0];

        assert_eq!(snap.version, 2);
        assert_eq!(snap.workspaces.len(), 1);
        assert_eq!(ws.custom_name.as_deref(), Some("legacy"));
        assert_eq!(ws.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.active_tab, 0);
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].focused, Some(1));
        assert_eq!(ws.tabs[0].root_pane, Some(0));
        assert_eq!(ws.tabs[0].panes[&0].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.tabs[0].panes[&1].cwd, PathBuf::from("/tmp/herdr"));
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
        let snap = parse_snapshot(json).unwrap();
        assert_eq!(snap.version, 0);
    }

    #[test]
    fn future_version_is_rejected() {
        let json = r#"{"version":999,"workspaces":[],"active":null,"selected":0}"#;
        assert!(parse_snapshot(json).is_err());
    }

    #[test]
    fn active_tab_default_is_zero() {
        let json = r#"{"custom_name":"test","identity_cwd":"/tmp","tabs":[]}"#;
        let ws: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(ws.active_tab, 0);
    }
}
