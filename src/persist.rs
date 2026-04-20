//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.

use std::path::PathBuf;

use tracing::{error, info, warn};

mod restore;
mod snapshot;

pub use self::restore::restore;
pub use self::snapshot::{
    capture, DirectionSnapshot, LayoutSnapshot, SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
use self::snapshot::{parse_snapshot, snapshot_file_version, SNAPSHOT_VERSION};
#[cfg(test)]
use self::{
    restore::{
        collect_pane_ids, prune_restored_node, resolve_restored_pane, restore_node_remapped,
    },
    snapshot::{capture_node, PaneSnapshot},
};

// --- File I/O ---

// --- File I/O ---

fn session_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

fn save_to_path(path: &std::path::Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(snapshot)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn clear_path(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub fn save(snapshot: &SessionSnapshot) {
    let path = session_path();
    if let Err(err) = save_to_path(&path, snapshot) {
        error!(err = %err, path = %path.display(), "failed to save session");
        return;
    }
    info!(workspaces = snapshot.workspaces.len(), "session saved");
}

pub fn clear() {
    let path = session_path();
    if let Err(err) = clear_path(&path) {
        error!(err = %err, path = %path.display(), "failed to clear session");
        return;
    }
    info!(path = %path.display(), "session cleared");
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
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
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
    use std::collections::HashMap;

    use ratatui::layout::{Direction, Rect};

    use crate::app::{state::AgentPanelScope, AppState, Mode};
    use crate::layout::{NavDirection, Node, PaneId};
    use crate::workspace::Workspace;

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

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-session-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("session.json")
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
            sidebar_section_split: Some(0.5),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear-existing");
        save_to_path(
            &path,
            &SessionSnapshot {
                version: SNAPSHOT_VERSION,
                workspaces: vec![],
                active: None,
                selected: 0,
                agent_panel_scope: AgentPanelScope::CurrentWorkspace,
                sidebar_width: Some(26),
                sidebar_section_split: Some(0.5),
            },
        )
        .unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("clear-missing");

        clear_path(&path).unwrap();

        assert!(!path.exists());
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
            sidebar_section_split: Some(0.5),
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
        assert_eq!(restored.sidebar_section_split, Some(0.5));
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
        assert_eq!(snap.sidebar_section_split, None);
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
        assert_eq!(snap.sidebar_section_split, Some(0.4));
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
        assert_eq!(restored.sidebar_section_split, None);
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

    fn state_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.selected = 0;
            state.mode = Mode::Terminal;
        }
        state
    }

    fn capture_from_state(state: &AppState) -> SessionSnapshot {
        capture(
            &state.workspaces,
            state.active,
            state.selected,
            state.agent_panel_scope,
            state.sidebar_width,
            state.sidebar_section_split,
        )
    }

    fn root_split_ratio(tab: &TabSnapshot) -> Option<f32> {
        match &tab.layout {
            LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
            LayoutSnapshot::Pane(_) => None,
        }
    }

    #[test]
    fn capture_contract_tracks_workspace_order_active_and_selected() {
        let mut state = state_with_workspaces(&["a", "b", "c"]);
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let snapshot = capture_from_state(&state);
        let ids: Vec<_> = state.workspaces.iter().map(|ws| ws.id.clone()).collect();
        let captured_ids: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.id.clone().unwrap())
            .collect();
        assert_eq!(captured_ids, ids);
        assert_eq!(snapshot.active, state.active);
        assert_eq!(snapshot.selected, state.selected);
    }

    #[test]
    fn capture_contract_tracks_workspace_and_tab_names_and_active_tab() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].set_custom_name("renamed-workspace".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.workspaces[0].switch_tab(second_tab);
        state.workspaces[0].tabs[0].set_custom_name("main".into());

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.custom_name.as_deref(), Some("renamed-workspace"));
        assert_eq!(workspace.active_tab, second_tab);
        assert_eq!(workspace.tabs[0].custom_name.as_deref(), Some("main"));
        assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("logs"));
    }

    #[test]
    fn capture_contract_tracks_workspace_closure() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].custom_name.as_deref(), Some("one"));
        assert_eq!(snapshot.active, Some(0));
        assert_eq!(snapshot.selected, 0);
    }

    #[test]
    fn capture_contract_tracks_sidebar_state() {
        let mut state = state_with_workspaces(&["one"]);
        state.sidebar_width = 31;
        state.sidebar_section_split = 0.4;
        state.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.sidebar_width, Some(31));
        assert_eq!(snapshot.sidebar_section_split, Some(0.4));
        assert_eq!(snapshot.agent_panel_scope, AgentPanelScope::AllWorkspaces);
    }

    #[test]
    fn capture_contract_tracks_layout_focus_zoom_and_root_pane() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0].layout.focus_pane(second);
        state.toggle_fullscreen();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert!(matches!(tab.layout, LayoutSnapshot::Split { .. }));
        assert_eq!(tab.focused, Some(second.raw()));
        assert_eq!(tab.root_pane, Some(root.raw()));
        assert!(tab.zoomed);
        assert_eq!(tab.panes.len(), 2);
    }

    #[test]
    fn capture_contract_tracks_focus_navigation() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 106, 20));

        state.navigate_pane(NavDirection::Right);

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces[0].tabs[0].focused, Some(second.raw()));
        assert_ne!(snapshot.workspaces[0].tabs[0].focused, Some(root.raw()));
    }

    #[test]
    fn capture_contract_tracks_resize_ratio_changes() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut state, Rect::new(0, 0, 106, 20));
        let before = capture_from_state(&state);

        state.resize_pane(NavDirection::Right);

        let after = capture_from_state(&state);
        let before_ratio = root_split_ratio(&before.workspaces[0].tabs[0]).unwrap();
        let after_ratio = root_split_ratio(&after.workspaces[0].tabs[0]).unwrap();
        assert_ne!(before_ratio, after_ratio);
    }

    #[test]
    fn capture_contract_tracks_tab_closure() {
        let mut state = state_with_workspaces(&["one"]);
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.switch_tab(second_tab);

        state.close_tab();

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.active_tab, 0);
        assert!(workspace.tabs[0].custom_name.is_none());
    }

    #[test]
    fn capture_contract_tracks_pane_closure() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        state.close_pane();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert_eq!(tab.panes.len(), 1);
        assert!(matches!(tab.layout, LayoutSnapshot::Pane(_)));
        assert!(!tab.zoomed);
    }

    #[test]
    fn capture_contract_tracks_workspace_identity_and_pane_cwds() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.workspaces[0].tabs[0]
            .pane_cwds
            .insert(root, PathBuf::from("/tmp/pion"));
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0]
            .pane_cwds
            .insert(second, PathBuf::from("/tmp/herdr"));

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        let tab = &workspace.tabs[0];
        assert_eq!(workspace.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&root.raw()].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&second.raw()].cwd, PathBuf::from("/tmp/herdr"));
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

    #[test]
    fn restore_falls_back_to_home_when_cwd_missing() {
        // Pane spawn failure during session restore falls back
        // to HOME instead of dropping the entire workspace/tab.
        // Create a snapshot with a pane pointing to a non-existent directory.
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                cwd: PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test"),
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                cwd: std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/tmp")),
            },
        );

        let snap = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceSnapshot {
                id: Some("test-ws".to_string()),
                custom_name: Some("fallback test".to_string()),
                identity_cwd: PathBuf::from("/tmp"),
                tabs: vec![TabSnapshot {
                    custom_name: None,
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
            sidebar_section_split: Some(0.5),
        };

        // The snapshot should parse correctly.
        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert_eq!(restored.workspaces.len(), 1);

        // The pane with the missing cwd should still have its saved cwd
        // in the snapshot (the fallback happens during restore, not parse).
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test")
        );
    }
}
