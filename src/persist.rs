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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub cwd: PathBuf,
}

/// Serializable BSP tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Pane(u32),
    Split {
        direction: DirectionSnapshot,
        ratio: f32,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreStatus {
    Clean,
    Partial,
    Failed,
}

pub struct RestoreReport {
    pub workspaces: Vec<Workspace>,
    pub status: RestoreStatus,
}

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> RestoreReport {
    let mut workspaces = Vec::new();
    let mut failed = false;
    let mut repaired = false;

    for ws_snap in &snapshot.workspaces {
        match restore_workspace(
            ws_snap,
            rows,
            cols,
            scrollback_limit_bytes,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        ) {
            Some((workspace, workspace_repaired)) => {
                repaired |= workspace_repaired;
                workspaces.push(workspace);
            }
            None => failed = true,
        }
    }

    let status = summarize_restore_status(failed, repaired, workspaces.len());

    RestoreReport { workspaces, status }
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<AtomicBool>,
) -> Option<(Workspace, bool)> {
    let mut tabs = Vec::new();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_pane_number = 1;
    let mut repaired = false;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let (tab, tab_repaired) = restore_tab(
            tab_snap,
            idx + 1,
            rows,
            cols,
            scrollback_limit_bytes,
            events.clone(),
            render_notify.clone(),
            render_dirty.clone(),
        )?;
        repaired |= tab_repaired;
        for pane_id in tab.layout.pane_ids() {
            public_pane_numbers.insert(pane_id, next_public_pane_number);
            next_public_pane_number += 1;
        }
        tabs.push(tab);
    }

    if tabs.is_empty() {
        return None;
    }

    let active_tab = snap.active_tab.min(tabs.len().saturating_sub(1));
    repaired |= active_tab != snap.active_tab;

    Some((
        Workspace {
            id: snap
                .id
                .clone()
                .unwrap_or_else(crate::workspace::generate_workspace_id),
            custom_name: snap.custom_name.clone(),
            identity_cwd: snap.identity_cwd.clone(),
            cached_git_ahead_behind: None,
            public_pane_numbers,
            next_public_pane_number,
            active_tab,
            tabs,
        },
        repaired,
    ))
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
) -> Option<(crate::workspace::Tab, bool)> {
    let (node, id_map) = restore_node_remapped(&snap.layout);
    let pane_ids = collect_pane_ids(&node);
    let metadata = resolve_tab_restore_metadata(snap, &id_map, &pane_ids);
    let layout = TileLayout::from_saved(node, metadata.focus);

    let mut panes = HashMap::new();
    let mut pane_cwds = HashMap::new();
    let mut runtimes = HashMap::new();
    for id in &pane_ids {
        let cwd = metadata
            .pane_cwds
            .get(id)
            .cloned()
            .unwrap_or_else(|| current_dir_fallback());

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

    Some((
        crate::workspace::Tab {
            custom_name: snap.custom_name.clone(),
            number,
            root_pane: metadata.root_pane,
            layout,
            panes,
            pane_cwds,
            runtimes,
            zoomed: snap.zoomed,
            events,
            render_notify,
            render_dirty,
        },
        metadata.repaired,
    ))
}

#[derive(Debug)]
struct TabRestoreMetadata {
    focus: PaneId,
    root_pane: PaneId,
    pane_cwds: HashMap<PaneId, PathBuf>,
    repaired: bool,
}

fn summarize_restore_status(
    failed: bool,
    repaired: bool,
    restored_workspaces: usize,
) -> RestoreStatus {
    if failed {
        if restored_workspaces == 0 {
            RestoreStatus::Failed
        } else {
            RestoreStatus::Partial
        }
    } else if repaired {
        RestoreStatus::Partial
    } else {
        RestoreStatus::Clean
    }
}

fn current_dir_fallback() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| "/".into())
}

fn resolve_tab_restore_metadata(
    snap: &TabSnapshot,
    id_map: &HashMap<u32, PaneId>,
    pane_ids: &[PaneId],
) -> TabRestoreMetadata {
    let mut repaired = false;

    let focus = match snap
        .focused
        .and_then(|old_raw| id_map.get(&old_raw).copied())
    {
        Some(focus) => focus,
        None => {
            repaired = true;
            pane_ids.first().copied().unwrap_or(PaneId::from_raw(0))
        }
    };

    let mut pane_cwds = HashMap::new();
    for id in pane_ids {
        let old_id = id_map
            .iter()
            .find(|(_, new)| **new == *id)
            .map(|(old, _)| *old);
        let cwd = match old_id
            .and_then(|old| snap.panes.get(&old))
            .map(|pane| pane.cwd.clone())
        {
            Some(cwd) => cwd,
            None => {
                repaired = true;
                current_dir_fallback()
            }
        };
        pane_cwds.insert(*id, cwd);
    }

    let root_pane = match snap
        .root_pane
        .and_then(|old_raw| id_map.get(&old_raw).copied())
    {
        Some(root_pane) => root_pane,
        None => {
            repaired = true;
            pane_ids.first().copied().unwrap_or(PaneId::from_raw(0))
        }
    };

    TabRestoreMetadata {
        focus,
        root_pane,
        pane_cwds,
        repaired,
    }
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

#[derive(Debug)]
pub enum LoadResult {
    NoSnapshot,
    Loaded(SessionSnapshot),
    NewerSnapshotIgnored { version: u32 },
    Failed { message: String },
}

#[derive(Debug, Clone)]
struct PersistFailure {
    generation: u64,
    action: &'static str,
    error: String,
}

#[derive(Debug)]
enum PersistAction {
    Save(SessionSnapshot),
    Clear,
}

#[derive(Debug)]
struct PendingCommand {
    generation: u64,
    action: PersistAction,
}

#[derive(Debug)]
struct WorkerState {
    pending: Option<PendingCommand>,
    in_flight_generation: Option<u64>,
    next_generation: u64,
    last_completed_generation: u64,
    last_failure: Option<PersistFailure>,
    closed: bool,
}

#[derive(Debug)]
struct WorkerShared {
    state: std::sync::Mutex<WorkerState>,
    cv: std::sync::Condvar,
    path: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
}

pub struct PersistenceWorker {
    shared: Arc<WorkerShared>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

fn session_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

fn save_to_path(path: &std::path::Path, snapshot: &SessionSnapshot) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create session directory: {e}"))?;
    }

    let json = serde_json::to_string_pretty(snapshot)
        .map_err(|e| format!("failed to serialize session: {e}"))?;

    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("failed to write session temp file: {e}"))?;
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("failed to rename session file: {e}"));
    }

    info!(workspaces = snapshot.workspaces.len(), path = %path.display(), "session saved");
    Ok(())
}

fn clear_path(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            info!(path = %path.display(), "session cleared");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove session file: {e}")),
    }
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

pub fn load() -> LoadResult {
    let path = session_path();
    load_from_path(&path)
}

fn load_from_path(path: &std::path::Path) -> LoadResult {
    if !path.exists() {
        return LoadResult::NoSnapshot;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, path = %path.display(), "failed to read session file");
            return LoadResult::Failed {
                message: format!("failed to read session file: {err}"),
            };
        }
    };

    match parse_snapshot(&content) {
        Ok(snapshot) => LoadResult::Loaded(snapshot),
        Err(err) => {
            if let Ok(raw) = serde_json::from_str::<RawSessionSnapshot>(&content) {
                if raw.version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = raw.version,
                        supported = SNAPSHOT_VERSION,
                        path = %path.display(),
                        "session file is from a newer herdr version, ignoring"
                    );
                    return LoadResult::NewerSnapshotIgnored {
                        version: raw.version,
                    };
                }
            }

            warn!(err = %err, path = %path.display(), "failed to parse session file");
            LoadResult::Failed {
                message: format!("failed to parse session file: {err}"),
            }
        }
    }
}

impl PersistenceWorker {
    pub fn new(event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self::with_path(session_path(), event_tx)
    }

    fn with_path(path: PathBuf, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        let shared = Arc::new(WorkerShared {
            state: std::sync::Mutex::new(WorkerState {
                pending: None,
                in_flight_generation: None,
                next_generation: 0,
                last_completed_generation: 0,
                last_failure: None,
                closed: false,
            }),
            cv: std::sync::Condvar::new(),
            path,
            event_tx,
        });
        let worker_shared = shared.clone();
        let join_handle = std::thread::spawn(move || worker_loop(worker_shared));

        Self {
            shared,
            join_handle: Some(join_handle),
        }
    }

    pub fn enqueue_save(&self, snapshot: SessionSnapshot) -> u64 {
        self.enqueue(PersistAction::Save(snapshot))
    }

    pub fn enqueue_clear(&self) -> u64 {
        self.enqueue(PersistAction::Clear)
    }

    fn enqueue(&self, action: PersistAction) -> u64 {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("persistence state poisoned");
        state.next_generation += 1;
        let generation = state.next_generation;
        state.pending = Some(PendingCommand { generation, action });
        self.shared.cv.notify_one();
        generation
    }

    pub fn flush(&self) -> Result<(), String> {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("persistence state poisoned");
        let target_generation = state
            .pending
            .as_ref()
            .map(|command| command.generation)
            .or(state.in_flight_generation)
            .unwrap_or(state.last_completed_generation);

        while state.pending.is_some()
            || state.in_flight_generation.is_some()
            || state.last_completed_generation < target_generation
        {
            state = self
                .shared
                .cv
                .wait(state)
                .expect("persistence state poisoned");
        }

        if let Some(failure) = &state.last_failure {
            if failure.generation == target_generation {
                return Err(format!("{}: {}", failure.action, failure.error));
            }
        }

        Ok(())
    }

    pub fn shutdown(mut self) -> Result<(), String> {
        let flush_result = self.flush();
        {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("persistence state poisoned");
            state.closed = true;
            self.shared.cv.notify_one();
        }
        let join_result = if let Some(join_handle) = self.join_handle.take() {
            join_handle
                .join()
                .map_err(|_| "persistence worker panicked".to_string())
        } else {
            Ok(())
        };

        flush_result.and(join_result)
    }
}

fn worker_loop(shared: Arc<WorkerShared>) {
    loop {
        let command = {
            let mut state = shared.state.lock().expect("persistence state poisoned");
            while state.pending.is_none() && !state.closed {
                state = shared.cv.wait(state).expect("persistence state poisoned");
            }
            if state.closed && state.pending.is_none() {
                return;
            }
            let command = state.pending.take().expect("pending command missing");
            state.in_flight_generation = Some(command.generation);
            command
        };

        let result = match &command.action {
            PersistAction::Save(snapshot) => save_to_path(&shared.path, snapshot)
                .map(|_| AppEvent::SessionPersistenceSucceeded {
                    generation: command.generation,
                })
                .map_err(|error| PersistFailure {
                    generation: command.generation,
                    action: "save session",
                    error,
                }),
            PersistAction::Clear => clear_path(&shared.path)
                .map(|_| AppEvent::SessionPersistenceSucceeded {
                    generation: command.generation,
                })
                .map_err(|error| PersistFailure {
                    generation: command.generation,
                    action: "clear session",
                    error,
                }),
        };

        let mut state = shared.state.lock().expect("persistence state poisoned");
        state.in_flight_generation = None;
        state.last_completed_generation = command.generation;
        match result {
            Ok(event) => {
                state.last_failure = None;
                shared
                    .event_tx
                    .send(event)
                    .expect("persistence status receiver dropped");
            }
            Err(failure) => {
                let event = AppEvent::SessionPersistenceFailed {
                    generation: failure.generation,
                    action: failure.action,
                    error: failure.error.clone(),
                };
                state.last_failure = Some(failure);
                shared
                    .event_tx
                    .send(event)
                    .expect("persistence status receiver dropped");
            }
        }
        shared.cv.notify_all();
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

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-persist-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.json")
    }

    fn sample_snapshot(label: &str) -> SessionSnapshot {
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                id: Some(format!("w-{label}")),
                custom_name: Some(label.to_string()),
                identity_cwd: PathBuf::from("/tmp"),
                tabs: vec![TabSnapshot {
                    custom_name: Some("main".to_string()),
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        PaneSnapshot {
                            cwd: PathBuf::from("/tmp"),
                        },
                    )]),
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
        }
    }

    #[test]
    fn restore_status_is_partial_when_any_workspace_is_repaired() {
        assert_eq!(
            summarize_restore_status(false, true, 1),
            RestoreStatus::Partial
        );
        assert_eq!(
            summarize_restore_status(false, false, 1),
            RestoreStatus::Clean
        );
        assert_eq!(
            summarize_restore_status(true, false, 1),
            RestoreStatus::Partial
        );
        assert_eq!(
            summarize_restore_status(true, false, 0),
            RestoreStatus::Failed
        );
    }

    #[test]
    fn resolve_tab_restore_metadata_marks_missing_focus_root_and_cwd_as_repaired() {
        let pane_id = PaneId::from_raw(42);
        let id_map = HashMap::from([(7, pane_id)]);
        let pane_ids = vec![pane_id];
        let snap = TabSnapshot {
            custom_name: Some("broken".to_string()),
            layout: LayoutSnapshot::Pane(7),
            panes: HashMap::new(),
            zoomed: false,
            focused: Some(999),
            root_pane: Some(998),
        };

        let metadata = resolve_tab_restore_metadata(&snap, &id_map, &pane_ids);

        assert!(metadata.repaired);
        assert_eq!(metadata.focus, pane_id);
        assert_eq!(metadata.root_pane, pane_id);
        assert!(metadata.pane_cwds.contains_key(&pane_id));
    }

    #[test]
    fn active_tab_default_is_zero() {
        let json = r#"{"custom_name":"test","identity_cwd":"/tmp","tabs":[]}"#;
        let ws: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(ws.active_tab, 0);
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear");
        save_to_path(&path, &sample_snapshot("clear")).unwrap();
        clear_path(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("missing");
        clear_path(&path).unwrap();
    }

    #[test]
    fn worker_save_then_clear_leaves_no_session_file() {
        let path = temp_session_path("save-clear");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let worker = PersistenceWorker::with_path(path.clone(), event_tx);

        worker.enqueue_save(sample_snapshot("first"));
        worker.enqueue_clear();
        worker.shutdown().unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn worker_clear_then_save_leaves_newer_snapshot() {
        let path = temp_session_path("clear-save");
        save_to_path(&path, &sample_snapshot("old")).unwrap();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let worker = PersistenceWorker::with_path(path.clone(), event_tx);

        worker.enqueue_clear();
        worker.enqueue_save(sample_snapshot("new"));
        worker.shutdown().unwrap();

        let LoadResult::Loaded(snapshot) = load_from_path(&path) else {
            panic!("expected saved snapshot after clear->save");
        };
        assert_eq!(snapshot.workspaces[0].custom_name.as_deref(), Some("new"));
    }

    #[test]
    fn worker_coalesces_to_latest_pending_snapshot() {
        let path = temp_session_path("coalesce");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let worker = PersistenceWorker::with_path(path.clone(), event_tx);

        for idx in 0..20 {
            worker.enqueue_save(sample_snapshot(&format!("snap-{idx}")));
        }
        worker.shutdown().unwrap();

        let LoadResult::Loaded(snapshot) = load_from_path(&path) else {
            panic!("expected latest snapshot to persist");
        };
        assert_eq!(
            snapshot.workspaces[0].custom_name.as_deref(),
            Some("snap-19")
        );
    }
}
