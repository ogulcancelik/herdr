//! Ephemeral per-workspace floating pane (zellij-style "float").
//!
//! A float is a throwaway shell rendered as a centered overlay above the
//! active workspace's panes. It deliberately lives OUTSIDE the
//! workspace/tab/pane tree (`AppState::floats`, keyed by workspace id) so
//! that snapshot capture, pane walkers, ancestry resolution, and the public
//! pane API never see it. Toggling hides the overlay without killing the
//! shell; the float is removed when its process exits and is never persisted
//! across restarts.

use super::state::AppState;
use super::App;
use crate::layout::PaneId;
use crate::terminal::TerminalId;

/// Runtime state for one workspace's floating pane. The terminal metadata
/// lives in `AppState::terminals` and the PTY runtime in the app-level
/// `TerminalRuntimeRegistry`, exactly like layout panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatPane {
    pub pane_id: PaneId,
    pub terminal_id: TerminalId,
    pub visible: bool,
}

impl AppState {
    fn active_workspace_id(&self) -> Option<&str> {
        self.active
            .and_then(|idx| self.workspaces.get(idx))
            .map(|ws| ws.id.as_str())
    }

    pub(crate) fn float_for_active_workspace(&self) -> Option<&FloatPane> {
        self.floats.get(self.active_workspace_id()?)
    }

    pub(crate) fn visible_float_for_active_workspace(&self) -> Option<&FloatPane> {
        self.float_for_active_workspace()
            .filter(|float| float.visible)
    }

    /// Register a freshly spawned float for a workspace.
    ///
    /// Purges any restored pane-id alias the new pane id would shadow: a
    /// stale alias key equal to the float's pane id could mis-route agent
    /// reports from inside the float onto a live layout pane. Never marks
    /// the session dirty — floats are ephemeral by definition.
    pub(crate) fn register_float(&mut self, workspace_id: String, float: FloatPane) {
        self.remove_alias_shadowed_by_new_pane(float.pane_id);
        // The toggle path never spawns over an existing float, but if a
        // displaced entry ever exists, reap its terminal instead of leaking
        // the PTY silently.
        if let Some(displaced) = self.floats.insert(workspace_id, float) {
            if self.terminals.remove(&displaced.terminal_id).is_some()
                && !self
                    .terminal_runtime_shutdowns
                    .contains(&displaced.terminal_id)
            {
                self.terminal_runtime_shutdowns.push(displaced.terminal_id);
            }
        }
    }

    /// Flip the active workspace's float visibility (hide-not-kill).
    /// Returns false when the workspace has no float yet — the caller is
    /// expected to spawn one.
    pub(crate) fn toggle_active_float_visibility(&mut self) -> bool {
        let Some(ws_id) = self.active_workspace_id().map(str::to_string) else {
            return false;
        };
        match self.floats.get_mut(&ws_id) {
            Some(float) => {
                float.visible = !float.visible;
                true
            }
            None => false,
        }
    }

    /// Hide the active workspace's float (the shell keeps running).
    pub(crate) fn hide_active_float(&mut self) {
        let Some(ws_id) = self.active_workspace_id().map(str::to_string) else {
            return;
        };
        if let Some(float) = self.floats.get_mut(&ws_id) {
            float.visible = false;
        }
    }

    /// Remove the float owning `pane_id` after its process exited: drop the
    /// float entry and its terminal metadata, purge aliases targeting it,
    /// and queue its runtime for shutdown. Returns true when a float was
    /// removed.
    pub(crate) fn remove_float_for_pane(&mut self, pane_id: PaneId) -> bool {
        let Some(ws_id) = self
            .floats
            .iter()
            .find_map(|(ws_id, float)| (float.pane_id == pane_id).then(|| ws_id.clone()))
        else {
            return false;
        };
        let float = self
            .floats
            .remove(&ws_id)
            .expect("float key resolved above");
        self.terminals.remove(&float.terminal_id);
        self.pane_id_aliases.retain(|_, target| *target != pane_id);
        if !self.terminal_runtime_shutdowns.contains(&float.terminal_id) {
            self.terminal_runtime_shutdowns.push(float.terminal_id);
        }
        true
    }
}

impl App {
    /// Toggle the active workspace's floating pane: hide it when visible,
    /// re-show it when hidden, spawn it when the workspace has none yet.
    pub(crate) fn toggle_float_pane(&mut self) {
        if self.state.toggle_active_float_visibility() {
            self.notify_float_render();
            return;
        }
        let previous_toast = self.state.toast.clone();
        if let Err(err) = self.spawn_float_pane() {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: crate::app::state::ToastKind::NeedsAttention,
                title: "float pane failed".to_string(),
                context: err.to_string(),
                target: None,
            });
            self.sync_toast_deadline(previous_toast);
        }
        self.notify_float_render();
    }

    /// Spawn the float shell synchronously (no deferred-request state: the
    /// headless server shares this App-level path, so both event loops get
    /// identical behavior). The float never joins a tab layout and never
    /// marks the session dirty.
    fn spawn_float_pane(&mut self) -> std::io::Result<()> {
        let ws_idx = self
            .state
            .active
            .ok_or_else(|| std::io::Error::other("no active workspace"))?;
        let ws = self
            .state
            .workspaces
            .get(ws_idx)
            .ok_or_else(|| std::io::Error::other("active workspace disappeared"))?;
        let ws_id = ws.id.clone();
        let cwd = ws
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            .unwrap_or_else(|| ws.identity_cwd.clone());

        let (rows, cols) = float_spawn_size(&self.state);
        let pane_id = PaneId::alloc();
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd.clone(),
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        )?;

        let terminal_id = TerminalId::alloc();
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        self.state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id.clone(), cwd),
        );
        self.state.register_float(
            ws_id,
            FloatPane {
                pane_id,
                terminal_id,
                visible: true,
            },
        );
        Ok(())
    }

    fn notify_float_render(&self) {
        self.render_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        self.render_notify.notify_one();
    }
}

/// PTY dimensions for a freshly spawned float: the overlay's inner rect when
/// the view geometry is known, otherwise the same estimate panes use.
fn float_spawn_size(state: &AppState) -> (u16, u16) {
    if let Some(inner) = crate::ui::float_overlay_inner_rect(state.view.terminal_area) {
        return (inner.height, inner.width);
    }
    let (rows, cols) = state.estimate_pane_size();
    (rows.max(4), cols.max(10))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn state_with_workspace() -> (AppState, String) {
        let mut state = AppState::test_new();
        let ws = Workspace::test_new("main");
        let ws_id = ws.id.clone();
        state.workspaces.push(ws);
        state.active = Some(0);
        state.ensure_test_terminals();
        (state, ws_id)
    }

    fn synthetic_float(raw_pane_id: u32) -> FloatPane {
        FloatPane {
            pane_id: PaneId::from_raw(raw_pane_id),
            terminal_id: TerminalId::alloc(),
            visible: true,
        }
    }

    #[test]
    fn toggle_state_machine_spawn_hide_show_exit() {
        let (mut state, ws_id) = state_with_workspace();

        // No float yet: toggle reports "nothing to toggle" (spawn path).
        assert!(!state.toggle_active_float_visibility());

        // Spawn (state-level registration).
        let float = synthetic_float(909_090);
        let pane_id = float.pane_id;
        let terminal_id = float.terminal_id.clone();
        state.terminals.insert(
            terminal_id.clone(),
            crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into()),
        );
        state.register_float(ws_id.clone(), float);
        assert!(state.visible_float_for_active_workspace().is_some());

        // Toggle -> hidden but still present (hide-not-kill).
        assert!(state.toggle_active_float_visibility());
        assert!(state.visible_float_for_active_workspace().is_none());
        assert!(state.floats.contains_key(&ws_id));
        assert!(state.terminals.contains_key(&terminal_id));

        // Toggle -> visible again (same float).
        assert!(state.toggle_active_float_visibility());
        assert_eq!(
            state
                .visible_float_for_active_workspace()
                .map(|f| f.pane_id),
            Some(pane_id)
        );

        // Process exit -> removed entirely, runtime queued for shutdown.
        assert!(state.remove_float_for_pane(pane_id));
        assert!(state.floats.is_empty());
        assert!(!state.terminals.contains_key(&terminal_id));
        assert!(state.terminal_runtime_shutdowns.contains(&terminal_id));

        // Exit for an unknown pane is a no-op.
        assert!(!state.remove_float_for_pane(pane_id));
    }

    #[test]
    fn float_spawn_and_hide_never_mark_session_dirty() {
        let (mut state, ws_id) = state_with_workspace();
        state.session_dirty = false;

        state.register_float(ws_id, synthetic_float(909_091));
        assert!(state.toggle_active_float_visibility());
        assert!(state.toggle_active_float_visibility());
        state.hide_active_float();

        assert!(!state.session_dirty);
    }

    #[test]
    fn register_float_purges_alias_shadowed_by_new_pane_id() {
        let (mut state, ws_id) = state_with_workspace();
        let layout_pane = state.workspaces[0].tabs[0].root_pane;
        let float = synthetic_float(424_242);

        // A restored alias whose KEY collides with the float's new pane id
        // would mis-route reports from inside the float onto a layout pane.
        state
            .pane_id_aliases
            .insert(float.pane_id.raw(), layout_pane);

        state.register_float(ws_id, float);

        assert!(!state.pane_id_aliases.contains_key(&424_242));
    }

    #[test]
    fn float_removal_purges_aliases_targeting_the_float() {
        let (mut state, ws_id) = state_with_workspace();
        let float = synthetic_float(515_151);
        let float_pane = float.pane_id;
        state.register_float(ws_id, float);
        state.pane_id_aliases.insert(999_999, float_pane);

        assert!(state.remove_float_for_pane(float_pane));

        assert!(!state.pane_id_aliases.values().any(|id| *id == float_pane));
    }

    #[test]
    fn floats_are_excluded_from_session_snapshots() {
        let (mut state, ws_id) = state_with_workspace();
        let float = synthetic_float(909_990);
        let float_terminal = float.terminal_id.clone();
        state.terminals.insert(
            float_terminal.clone(),
            crate::terminal::TerminalState::new(float_terminal.clone(), "/tmp".into()),
        );
        state.register_float(ws_id, float);

        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let snapshot = crate::persist::capture(
            &state.workspaces,
            &state.terminals,
            &runtimes,
            state.active,
            state.selected,
            state.panel_scopes(),
            state.sidebar_width,
            state.sidebar_section_split,
            state.collapsed_space_keys.clone(),
            std::collections::HashMap::new(),
        );
        let serialized = serde_json::to_string(&snapshot).expect("snapshot serializes");
        assert!(
            !serialized.contains("909990"),
            "float pane id leaked into the session snapshot: {serialized}"
        );
        assert!(
            !serialized.contains(&float_terminal.to_string()),
            "float terminal id leaked into the session snapshot: {serialized}"
        );

        let history = crate::persist::capture_history(&state.workspaces, &runtimes);
        let serialized_history =
            serde_json::to_string(&history).expect("history snapshot serializes");
        assert!(
            !serialized_history.contains("909990"),
            "float pane id leaked into the history snapshot: {serialized_history}"
        );
    }

    #[test]
    fn floats_are_invisible_to_workspace_pane_walkers() {
        let (mut state, ws_id) = state_with_workspace();
        let float = synthetic_float(606_060);
        let float_pane = float.pane_id;
        state.register_float(ws_id, float);

        for ws in &state.workspaces {
            assert!(
                ws.find_tab_index_for_pane(float_pane).is_none(),
                "float pane must not be discoverable through workspace tabs"
            );
        }
    }
}
