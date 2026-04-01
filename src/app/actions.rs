//! Pure state mutations on AppState.
//! These don't need channels, async, or PTY runtime.

use tracing::{info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::{find_in_direction, NavDirection, PaneId};
use crate::pane::EffectiveStateChange;

use super::state::{AppState, Mode, ToastKind, ToastNotification};

fn notification_sound_for_state_change(
    is_active_ws: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle if prev_state != AgentState::Idle && !is_active_ws => {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

fn notification_toast_for_state_change(
    is_active_ws: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<ToastKind> {
    if is_active_ws || new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle if prev_state != AgentState::Idle => Some(ToastKind::Finished),
        _ => None,
    }
}

fn agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "pi",
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Gemini => "gemini",
        Agent::Cursor => "cursor",
        Agent::Cline => "cline",
        Agent::OpenCode => "opencode",
        Agent::GithubCopilot => "copilot",
        Agent::Kimi => "kimi",
        Agent::Droid => "droid",
        Agent::Amp => "amp",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneStateUpdate {
    pub pane_id: PaneId,
    pub ws_idx: usize,
    pub previous_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub agent: Option<Agent>,
    pub state: AgentState,
}

// ---------------------------------------------------------------------------
// Workspace operations
// ---------------------------------------------------------------------------

impl AppState {
    pub fn switch_workspace(&mut self, idx: usize) {
        if idx < self.workspaces.len() {
            self.active = Some(idx);
            self.selected = idx;
            for pane in self.workspaces[idx].panes.values_mut() {
                pane.seen = true;
            }
        }
    }

    pub fn close_selected_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        let name = self.workspaces[self.selected].display_name();
        info!(workspace = %name, "workspace closed");
        self.workspaces.remove(self.selected);
        if self.workspaces.is_empty() {
            self.active = None;
            self.selected = 0;
        } else {
            if self.selected >= self.workspaces.len() {
                self.selected = self.workspaces.len() - 1;
            }
            self.active = Some(self.selected);
        }
    }
}

// ---------------------------------------------------------------------------
// Pane operations
// ---------------------------------------------------------------------------

impl AppState {
    pub fn navigate_pane(&mut self, direction: NavDirection) {
        let panes = &self.view.pane_infos;
        if let Some(focused) = panes.iter().find(|p| p.is_focused) {
            if let Some(target) = find_in_direction(focused, direction, panes) {
                if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                    ws.layout.focus_pane(target);
                }
            }
        }
    }

    pub fn resize_pane(&mut self, direction: NavDirection) {
        if let Some(first) = self.view.pane_infos.first() {
            let area = self
                .view
                .pane_infos
                .iter()
                .fold(first.rect, |acc, p| acc.union(p.rect));
            if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
                ws.layout.resize_focused(direction, 0.05, area);
            }
        }
    }

    pub fn cycle_pane(&mut self, reverse: bool) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if reverse {
                ws.layout.focus_prev();
            } else {
                ws.layout.focus_next();
            }
        }
    }

    pub fn toggle_fullscreen(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            if ws.layout.pane_count() > 1 {
                ws.zoomed = !ws.zoomed;
            }
        }
    }

    pub fn close_pane(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get_mut(i)) {
            ws.close_focused();
        }
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

impl AppState {
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn copy_selection(&mut self) {
        let sel = match self.selection.as_mut() {
            Some(s) => {
                if !s.finish() {
                    self.selection = None;
                    return;
                }
                s
            }
            None => return,
        };

        let ws = match self.active.and_then(|i| self.workspaces.get(i)) {
            Some(ws) => ws,
            None => return,
        };

        let rt = match ws.runtimes.get(&sel.pane_id) {
            Some(r) => r,
            None => return,
        };

        if let Ok(parser) = rt.parser.read() {
            let text = crate::selection::extract_text(parser.screen(), sel);
            if !text.is_empty() {
                crate::selection::write_osc52(&text);
                info!(len = text.len(), "copied selection to clipboard");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl AppState {
    pub fn handle_app_event(&mut self, event: AppEvent) -> Vec<PaneStateUpdate> {
        match event {
            AppEvent::PaneDied { pane_id } => {
                self.handle_pane_died(pane_id);
                Vec::new()
            }
            AppEvent::UpdateReady { version } => {
                self.update_available = Some(version.clone());
                self.update_dismissed = true;
                self.toast = Some(ToastNotification {
                    kind: ToastKind::UpdateInstalled,
                    title: format!("updated to v{version}"),
                    context: "restart to use it".to_string(),
                });
                Vec::new()
            }
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
            } => self
                .update_pane_state(pane_id, |pane| pane.set_detected_state(agent, state))
                .into_iter()
                .collect(),
            AppEvent::HookStateReported {
                pane_id,
                source,
                agent,
                state,
                message,
            } => self
                .update_pane_state(pane_id, |pane| {
                    pane.set_hook_authority(source, agent, state, message)
                })
                .into_iter()
                .collect(),
            AppEvent::HookAuthorityCleared { pane_id, source } => self
                .update_pane_state(pane_id, |pane| pane.clear_hook_authority(source.as_deref()))
                .into_iter()
                .collect(),
            AppEvent::HookAgentReleased {
                pane_id,
                source,
                agent,
            } => self
                .update_pane_state(pane_id, |pane| pane.release_agent(&source, agent))
                .into_iter()
                .collect(),
        }
    }

    fn update_pane_state<F>(&mut self, pane_id: PaneId, update: F) -> Option<PaneStateUpdate>
    where
        F: FnOnce(&mut crate::pane::PaneState) -> Option<EffectiveStateChange>,
    {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.panes.contains_key(&pane_id))?;
        let workspace_name = self.workspaces[ws_idx].display_name();
        let change = {
            let pane = self.workspaces[ws_idx].panes.get_mut(&pane_id)?;
            update(pane)?
        };
        self.apply_pane_state_change(ws_idx, pane_id, &workspace_name, change);
        Some(PaneStateUpdate {
            pane_id,
            ws_idx,
            previous_agent: change.previous_agent,
            previous_state: change.previous_state,
            agent: change.agent,
            state: change.state,
        })
    }

    fn apply_pane_state_change(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        workspace_name: &str,
        change: EffectiveStateChange,
    ) {
        let is_active_ws = self.active == Some(ws_idx);
        let Some(pane) = self.workspaces[ws_idx].panes.get_mut(&pane_id) else {
            return;
        };

        if change.state == AgentState::Idle
            && change.previous_state != AgentState::Idle
            && !is_active_ws
        {
            pane.seen = false;
        }

        if self.sound.allows(change.agent) {
            if let Some(sound) = notification_sound_for_state_change(
                is_active_ws,
                change.previous_state,
                change.state,
            ) {
                crate::sound::play(sound);
            }
        }

        if self.toast_config.enabled {
            if let (Some(agent), Some(kind)) = (
                change.agent,
                notification_toast_for_state_change(
                    is_active_ws,
                    change.previous_state,
                    change.state,
                ),
            ) {
                let event_text = match kind {
                    ToastKind::NeedsAttention => "needs attention",
                    ToastKind::Finished => "finished",
                    ToastKind::UpdateInstalled => "updated",
                };
                self.toast = Some(ToastNotification {
                    kind,
                    title: format!("{} {}", agent_label(agent), event_text),
                    context: format!("{} · {}", workspace_name, ws_idx + 1),
                });
            }
        }
    }

    fn handle_pane_died(&mut self, pane_id: PaneId) {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.panes.contains_key(&pane_id));

        let Some(ws_idx) = ws_idx else {
            warn!(pane = pane_id.raw(), "PaneDied for unknown pane");
            return;
        };

        let ws = &mut self.workspaces[ws_idx];

        if ws.layout.pane_count() <= 1 {
            self.workspaces.remove(ws_idx);
            if self.workspaces.is_empty() {
                self.active = None;
                self.selected = 0;
                if self.mode == Mode::Terminal {
                    self.mode = Mode::Navigate;
                }
            } else {
                if let Some(active) = self.active {
                    if active >= self.workspaces.len() {
                        self.active = Some(self.workspaces.len() - 1);
                    }
                }
                if self.selected >= self.workspaces.len() {
                    self.selected = self.workspaces.len() - 1;
                }
            }
        } else {
            ws.remove_pane(pane_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;
    use ratatui::layout::Direction;

    fn app_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        for name in names {
            let ws = Workspace::test_new(name);
            state.workspaces.push(ws);
        }
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.mode = Mode::Terminal;
        }
        state
    }

    #[test]
    fn switch_workspace_updates_active_and_selected() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.switch_workspace(2);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn switch_workspace_marks_panes_seen() {
        let mut state = app_with_workspaces(&["a", "b"]);
        // Mark a pane in workspace 1 as unseen
        let id = *state.workspaces[1].panes.keys().next().unwrap();
        state.workspaces[1].panes.get_mut(&id).unwrap().seen = false;

        state.switch_workspace(1);
        assert!(state.workspaces[1].panes.get(&id).unwrap().seen);
    }

    #[test]
    fn switch_workspace_out_of_bounds_is_noop() {
        let mut state = app_with_workspaces(&["a"]);
        state.switch_workspace(5);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn close_workspace_adjusts_indices() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 2);
        assert_eq!(state.selected, 1);
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].custom_name.as_deref(), Some("c"));
    }

    #[test]
    fn close_last_workspace_clears_active() {
        let mut state = app_with_workspaces(&["only"]);
        state.selected = 0;
        state.close_selected_workspace();

        assert!(state.workspaces.is_empty());
        assert_eq!(state.active, None);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn close_workspace_at_end_adjusts_selected() {
        let mut state = app_with_workspaces(&["a", "b"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.active, Some(0));
    }

    #[test]
    fn pane_died_last_pane_removes_workspace() {
        let mut state = app_with_workspaces(&["a", "b"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].custom_name.as_deref(), Some("b"));
    }

    #[test]
    fn pane_died_last_workspace_enters_navigate() {
        let mut state = app_with_workspaces(&["only"]);
        state.mode = Mode::Terminal;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert!(state.workspaces.is_empty());
        assert_eq!(state.mode, Mode::Navigate);
    }

    #[test]
    fn pane_died_multi_pane_keeps_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);

        state.handle_pane_died(second_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn pane_died_unknown_pane_is_noop() {
        let mut state = app_with_workspaces(&["test"]);
        let fake_id = PaneId::from_raw(9999);

        state.handle_pane_died(fake_id);

        assert_eq!(state.workspaces.len(), 1);
    }

    #[test]
    fn state_changed_updates_pane() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
        });

        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert_eq!(pane.state, AgentState::Working);
        assert_eq!(pane.detected_agent, Some(Agent::Pi));
    }

    #[test]
    fn state_changed_idle_in_background_marks_unseen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        // First set it to Working
        state.workspaces[1]
            .panes
            .get_mut(&bg_pane_id)
            .unwrap()
            .state = AgentState::Working;

        // Now transition to Idle while in background
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(!pane.seen);
    }

    #[test]
    fn hook_authority_ignores_fallback_state_changes() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:pi".into(),
            agent: Agent::Pi,
            state: AgentState::Working,
            message: None,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });

        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert_eq!(pane.fallback_state, AgentState::Idle);
        assert_eq!(pane.state, AgentState::Working);
        assert!(pane.hook_authority.is_some());
    }

    #[test]
    fn hook_authority_clears_when_agent_disappears() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:pi".into(),
            agent: Agent::Pi,
            state: AgentState::Working,
            message: None,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
        });

        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert!(pane.hook_authority.is_none());
        assert_eq!(pane.detected_agent, None);
        assert_eq!(pane.state, AgentState::Unknown);
    }

    #[test]
    fn hook_agent_release_clears_identity_immediately() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:pi".into(),
            agent: Agent::Pi,
            state: AgentState::Working,
            message: None,
        });
        state.handle_app_event(AppEvent::HookAgentReleased {
            pane_id,
            source: "herdr:pi".into(),
            agent: Agent::Pi,
        });

        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert!(pane.hook_authority.is_none());
        assert_eq!(pane.detected_agent, None);
        assert_eq!(pane.fallback_state, AgentState::Unknown);
        assert_eq!(pane.state, AgentState::Unknown);
    }

    #[test]
    fn waiting_sound_plays_even_in_active_workspace() {
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Blocked),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn done_sound_only_plays_in_background() {
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Working, AgentState::Idle),
            Some(crate::sound::Sound::Done)
        );
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Idle),
            None
        );
    }

    #[test]
    fn background_waiting_sets_attention_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.enabled = true;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn background_idle_sets_finished_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.enabled = true;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        state.workspaces[1]
            .panes
            .get_mut(&bg_pane_id)
            .unwrap()
            .state = AgentState::Working;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Droid),
            state: AgentState::Idle,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::Finished);
        assert_eq!(toast.title, "droid finished");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn active_workspace_does_not_set_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.enabled = true;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn toggle_fullscreen_works() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        assert!(!state.workspaces[0].zoomed);
        state.toggle_fullscreen();
        assert!(state.workspaces[0].zoomed);
        state.toggle_fullscreen();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn toggle_fullscreen_single_pane_noop() {
        let mut state = app_with_workspaces(&["test"]);
        state.toggle_fullscreen();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn close_pane_removes_from_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);
        assert_eq!(state.workspaces[0].panes.len(), 2);

        state.close_pane();
        assert_eq!(state.workspaces[0].panes.len(), 1);
    }
}
