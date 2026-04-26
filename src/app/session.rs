use std::time::Instant;

use super::{App, Mode, SESSION_SAVE_DEBOUNCE};
use crate::app::state::ToastKind;
use crate::persist::{SessionId, SessionLoadError, SessionName};

impl App {
    pub(super) fn schedule_session_save(&mut self) {
        if !self.no_session {
            self.session_save_deadline = Some(Instant::now() + SESSION_SAVE_DEBOUNCE);
        }
    }

    pub(crate) fn sync_session_save_schedule(&mut self) {
        if self.state.session_dirty {
            self.state.session_dirty = false;
            self.schedule_session_save();
        }
    }

    pub(crate) fn save_session_now(&mut self) -> bool {
        if self.no_session {
            self.session_save_deadline = None;
            return false;
        }

        let previous_toast = self.state.toast.clone();
        let active_id = self.state.current_session_id();
        let changed = if self.state.workspaces.is_empty() && active_id.is_default() {
            if let Err(err) = crate::persist::clear_session(&active_id) {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "failed to clear session".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
                true
            } else {
                self.session_save_deadline = None;
                false
            }
        } else {
            let snap = if self.state.workspaces.is_empty() {
                self.empty_session_snapshot()
            } else {
                crate::persist::capture(
                    &self.state.workspaces,
                    self.state.active,
                    self.state.selected,
                    self.state.agent_panel_scope,
                    self.state.sidebar_width,
                    self.state.sidebar_section_split,
                )
            };
            if let Err(err) = crate::persist::save_session(&active_id, &snap) {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "failed to save session".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
                true
            } else {
                self.session_save_deadline = None;
                false
            }
        };

        changed
    }

    fn empty_session_snapshot(&self) -> crate::persist::SessionSnapshot {
        crate::persist::capture(
            &[],
            None,
            0,
            self.state.agent_panel_scope,
            self.state.sidebar_width,
            self.state.sidebar_section_split,
        )
    }

    pub(crate) fn open_session_picker(&mut self) {
        if !self.state.session_picker_enabled {
            return;
        }
        let previous_toast = self.state.toast.clone();
        let names = match crate::persist::list_session_names() {
            Ok(names) => names,
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "failed to list sessions".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
                Vec::new()
            }
        };

        let active_id = self.state.current_session_id();
        let mut entries = vec![crate::app::state::SessionEntry {
            id: SessionId::Default,
            label: "default".to_string(),
            active: matches!(active_id, SessionId::Default),
            deletable: false,
        }];
        for name in names {
            let is_active = self
                .state
                .active_session
                .as_ref()
                .is_some_and(|a| a == &name);
            entries.push(crate::app::state::SessionEntry {
                id: SessionId::Named(name.clone()),
                label: name.to_string(),
                active: is_active,
                deletable: !is_active,
            });
        }

        self.state.session_picker =
            crate::app::state::SessionPickerState::reset(entries, Some(&active_id));
        self.state.mode = Mode::SessionPicker;
    }

    pub(crate) fn ensure_session_picker_populated(&mut self) {
        if self.state.mode == Mode::SessionPicker && self.state.session_picker.sessions.is_empty() {
            self.open_session_picker();
        }
    }

    pub(crate) fn switch_session(&mut self, target: SessionId) {
        let current = self.state.current_session_id();
        if target == current {
            return;
        }

        let previous_toast = self.state.toast.clone();

        // 1. Save current session.
        let snap = crate::persist::capture(
            &self.state.workspaces,
            self.state.active,
            self.state.selected,
            self.state.agent_panel_scope,
            self.state.sidebar_width,
            self.state.sidebar_section_split,
        );
        if let Err(err) = crate::persist::save_session(&current, &snap) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "failed to save current session".to_string(),
                context: err.to_string(),
            });
            self.sync_toast_deadline(previous_toast);
            return;
        }

        // 2. Load target session.
        let target_snap = match crate::persist::load_session(&target) {
            Ok(Some(snap)) => snap,
            Ok(None) => {
                // Missing default → empty session.
                if target.is_default() {
                    crate::persist::SessionSnapshot {
                        version: crate::persist::SNAPSHOT_VERSION,
                        workspaces: vec![],
                        active: None,
                        selected: 0,
                        agent_panel_scope: crate::app::state::AgentPanelScope::CurrentWorkspace,
                        sidebar_width: Some(self.state.default_sidebar_width),
                        sidebar_section_split: Some(0.5),
                    }
                } else {
                    self.state.toast = Some(crate::app::state::ToastNotification {
                        kind: ToastKind::NeedsAttention,
                        title: "session not found".to_string(),
                        context: format!("{target}"),
                    });
                    self.sync_toast_deadline(previous_toast);
                    return;
                }
            }
            Err(SessionLoadError::NotFound) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "session not found".to_string(),
                    context: format!("{target}"),
                });
                self.sync_toast_deadline(previous_toast);
                return;
            }
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "failed to load session".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
                return;
            }
        };

        // 3. Restore into temporary workspaces.
        let (rows, cols) = self.state.estimate_pane_size();
        let temp_workspaces = crate::persist::restore(
            &target_snap,
            rows.max(4),
            cols.max(10),
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        );

        if !target_snap.workspaces.is_empty() && temp_workspaces.is_empty() {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "session restore failed".to_string(),
                context: "all panes failed to spawn".to_string(),
            });
            self.sync_toast_deadline(previous_toast);
            return;
        }

        // 4. Take old workspaces (triggers Tab::drop → PaneRuntime::shutdown).
        let _old_workspaces = std::mem::take(&mut self.state.workspaces);

        // 5. Replace state.
        self.state.workspaces = temp_workspaces;
        self.state.active = target_snap
            .active
            .filter(|&i| i < self.state.workspaces.len());
        self.state.selected = target_snap
            .selected
            .min(self.state.workspaces.len().saturating_sub(1));
        self.state.agent_panel_scope = target_snap.agent_panel_scope;
        self.state.sidebar_width = target_snap
            .sidebar_width
            .unwrap_or(self.state.default_sidebar_width);
        self.state.sidebar_section_split = target_snap.sidebar_section_split.unwrap_or(0.5);
        self.state.active_session = match target {
            SessionId::Named(name) => Some(name),
            SessionId::Default => None,
        };

        // 6. Clear stale bookkeeping.
        self.overlay_panes.clear();
        self.last_focus = self.state.active.and_then(|idx| {
            self.state
                .workspaces
                .get(idx)
                .and_then(|ws| ws.focused_pane_id().map(|pane_id| (idx, pane_id)))
        });
        self.state.selection = None;
        self.state.drag = None;
        self.state.workspace_press = None;
        self.state.tab_press = None;
        self.state.context_menu = None;
        self.state.workspace_scroll = 0;
        self.state.agent_panel_scroll = 0;
        self.state.tab_scroll = 0;
        self.state.tab_scroll_follow_active = true;
        self.state.session_dirty = false;
        self.session_save_deadline = None;

        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };

        for ws in &mut self.state.workspaces {
            ws.refresh_git_ahead_behind();
        }
    }

    pub(crate) fn create_session(&mut self, name: SessionName) {
        let previous_toast = self.state.toast.clone();
        match crate::persist::list_session_names() {
            Ok(names) => {
                if names.iter().any(|n| n == &name) {
                    self.state.toast = Some(crate::app::state::ToastNotification {
                        kind: ToastKind::NeedsAttention,
                        title: "session already exists".to_string(),
                        context: format!("'{name}' already exists"),
                    });
                    self.sync_toast_deadline(previous_toast);
                    return;
                }
            }
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: ToastKind::NeedsAttention,
                    title: "failed to list sessions".to_string(),
                    context: err.to_string(),
                });
                self.sync_toast_deadline(previous_toast);
                return;
            }
        }

        let snap = self.empty_session_snapshot();
        let target = SessionId::Named(name.clone());
        if let Err(err) = crate::persist::save_session(&target, &snap) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "failed to create session".to_string(),
                context: err.to_string(),
            });
            self.sync_toast_deadline(previous_toast.clone());
            return;
        }

        self.switch_session(target);
        if self.state.active_session.as_ref() == Some(&name) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::Finished,
                title: "session created".to_string(),
                context: format!("'{name}'"),
            });
            self.sync_toast_deadline(previous_toast);
        }
    }

    pub(crate) fn delete_session(&mut self, name: SessionName) {
        let previous_toast = self.state.toast.clone();
        if self.state.active_session.as_ref() == Some(&name) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "cannot delete active session".to_string(),
                context: format!("'{name}' is currently active"),
            });
            self.sync_toast_deadline(previous_toast);
            return;
        }

        if let Err(err) = crate::persist::delete_session(&name) {
            self.state.toast = Some(crate::app::state::ToastNotification {
                kind: ToastKind::NeedsAttention,
                title: "failed to delete session".to_string(),
                context: err.to_string(),
            });
            self.sync_toast_deadline(previous_toast);
            return;
        }

        if self.state.mode == Mode::SessionPicker {
            self.open_session_picker();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn empty_session_snapshot_does_not_clone_current_workspaces() {
        let mut app = test_app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("current")];
        app.state.active = Some(0);
        app.state.selected = 0;

        let snapshot = app.empty_session_snapshot();

        assert!(snapshot.workspaces.is_empty());
        assert_eq!(snapshot.active, None);
        assert_eq!(snapshot.selected, 0);
    }
}
