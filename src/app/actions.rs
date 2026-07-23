//! Pure state mutations on AppState.
//! These don't need channels, async, or PTY runtime.

use std::time::Instant;

use tracing::{info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
#[cfg(test)]
use crate::layout::{find_in_direction, NavDirection};
use crate::selection::Selection;
use crate::terminal::{EffectiveStateChange, TerminalStateMutation};
use crate::workspace::WorkspaceGitStatus;

use super::api_helpers::pane_agent_status;
use super::state::{
    navigator_display_index_of_row, navigator_display_lines, navigator_first_row_at_or_after,
    text_matches_query, AgentNotificationDelivery, AppState, Mode, NavigatorRow,
    NavigatorStateFilter, NavigatorTarget, PaneFocusTarget, PendingAgentNotification, ToastKind,
    ToastNotification, ToastTarget, ViewLayout,
};

fn is_background_completion_transition(prev_state: AgentState, new_state: AgentState) -> bool {
    matches!(new_state, AgentState::Idle)
        && matches!(prev_state, AgentState::Working | AgentState::Blocked)
}

fn is_completion_transition(change: &EffectiveStateChange) -> bool {
    is_completion_transition_parts(
        change.previous_state,
        change.state,
        change.previous_agent_label.as_deref(),
        change.agent_label.as_deref(),
    )
}

fn public_tab_id_for_index(ws: &crate::workspace::Workspace, tab_idx: usize) -> Option<String> {
    let tab_number = ws.public_tab_number(tab_idx)?;
    Some(crate::workspace::public_tab_id_for_number(
        &ws.id, tab_number,
    ))
}

pub fn is_completion_transition_parts(
    previous_state: AgentState,
    state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> bool {
    is_background_completion_transition(previous_state, state)
        || (previous_state == AgentState::Unknown
            && state == AgentState::Idle
            && previous_agent_label.is_some()
            && previous_agent_label == agent_label)
}

pub fn active_tab_suppresses_notifications(
    is_active_tab: bool,
    outer_terminal_focus: Option<bool>,
) -> bool {
    is_active_tab && outer_terminal_focus != Some(false)
}

#[cfg(test)]
pub fn notification_sound_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_background_completion_transition(prev_state, new_state)
                && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_sound_for_state_change_with_agent_labels(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_completion_transition_parts(
                prev_state,
                new_state,
                previous_agent_label,
                agent_label,
            ) && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

fn notification_sound_for_effective_state_change(
    suppress_active_tab_notifications: bool,
    change: &EffectiveStateChange,
) -> Option<crate::sound::Sound> {
    if change.state == change.previous_state {
        return None;
    }

    match change.state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_completion_transition(change) && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_toast_for_state_change_with_agent_labels(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle
            if is_completion_transition_parts(
                prev_state,
                new_state,
                previous_agent_label,
                agent_label,
            ) =>
        {
            Some(ToastKind::Finished)
        }
        _ => None,
    }
}

fn notification_toast_for_effective_state_change(
    suppress_active_tab_notifications: bool,
    change: &EffectiveStateChange,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || change.state == change.previous_state {
        return None;
    }

    match change.state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle if is_completion_transition(change) => Some(ToastKind::Finished),
        _ => None,
    }
}

pub fn notification_toast_for_pane_state_update(
    suppress_active_tab_notifications: bool,
    update: &PaneStateUpdate,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || update.state == update.previous_state {
        return None;
    }

    notification_toast_for_state_change_with_agent_labels(
        suppress_active_tab_notifications,
        update.previous_state,
        update.state,
        update.previous_agent_label.as_deref(),
        update.agent_label.as_deref(),
    )
}

fn toast_agent_label(agent_label: &str) -> &str {
    agent_label
}

fn toast_event_text(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::NeedsAttention => "needs attention",
        ToastKind::Finished => "finished",
        ToastKind::UpdateInstalled => "updated",
    }
}

fn sound_for_toast_kind(
    kind: ToastKind,
    suppress_active_tab_notifications: bool,
) -> Option<crate::sound::Sound> {
    match kind {
        ToastKind::NeedsAttention => Some(crate::sound::Sound::Request),
        ToastKind::Finished if !suppress_active_tab_notifications => {
            Some(crate::sound::Sound::Done)
        }
        ToastKind::Finished | ToastKind::UpdateInstalled => None,
    }
}

pub fn notification_context(
    ws: &crate::workspace::Workspace,
    workspace_label: &str,
    ws_idx: usize,
    pane_id: PaneId,
) -> String {
    let mut context = format!("{} · {}", workspace_label, ws_idx + 1);
    if ws.tabs.len() > 1 {
        if let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) {
            if let Some(label) = ws.tab_display_name(tab_idx) {
                context.push_str(&format!(" · {label}"));
            }
        }
    }
    context
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneStateUpdate {
    pub pane_id: PaneId,
    pub ws_idx: usize,
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_seen: bool,
    pub previous_presentation: crate::terminal::EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub presentation: crate::terminal::EffectivePresentation,
    pub agent_name_changed: bool,
    pub agent_released: bool,
    pub agent_release_status: Option<crate::api::schema::AgentStatus>,
}

// ---------------------------------------------------------------------------
// Navigator operations
// ---------------------------------------------------------------------------

impl AppState {
    pub(crate) fn current_pane_focus_target(&self) -> Option<PaneFocusTarget> {
        let ws_idx = self.active?;
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        Some(PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        })
    }

    pub(crate) fn pane_focus_target_indices(
        &self,
        target: &PaneFocusTarget,
    ) -> Option<(usize, usize)> {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.id == target.workspace_id)?;
        let tab_idx = self.workspaces[ws_idx].find_tab_index_for_pane(target.pane_id)?;
        Some((ws_idx, tab_idx))
    }

    pub(crate) fn record_pane_focus_change(
        &mut self,
        previous: Option<PaneFocusTarget>,
        ws_idx: usize,
        pane_id: PaneId,
    ) {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return;
        };
        let target = PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        };
        if previous.as_ref() != Some(&target) {
            self.previous_pane_focus = previous;
        }
    }

    fn record_pane_focus_after_navigation(&mut self, previous: Option<PaneFocusTarget>) {
        let current = self.current_pane_focus_target();
        if previous != current {
            self.previous_pane_focus = previous;
        }
    }

    fn sync_selection_after_focus_navigation(&mut self) {
        if self.copy_mode.is_some() {
            self.sync_copy_mode_with_focus();
        } else {
            self.clear_selection();
        }
    }

    pub(crate) fn focus_pane_in_workspace(&mut self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) else {
            return false;
        };
        let previous = self.current_pane_focus_target();
        let target = PaneFocusTarget {
            workspace_id: ws.id.clone(),
            pane_id,
        };
        if previous.as_ref() == Some(&target) {
            return false;
        }

        if self.copy_mode.is_some() {
            self.clear_copy_mode_selection();
        }
        self.switch_workspace_tab(ws_idx, tab_idx);
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            tab.layout.focus_pane(pane_id);
            self.previous_pane_focus = previous;
            self.mark_session_dirty();
            self.sync_copy_mode_with_focus();
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn open_navigator(&mut self) {
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        self.open_navigator_from(&terminal_runtimes);
    }

    pub(crate) fn open_navigator_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        self.navigator.query.clear();
        self.navigator.search_focused = false;
        self.navigator.state_filter = None;
        self.navigator.scroll = 0;
        self.navigator.expanded_workspaces.clear();

        for ws in &self.workspaces {
            self.navigator.expanded_workspaces.insert(ws.id.clone());
        }

        self.mode = Mode::Navigator;
        self.navigator.selected = self
            .current_navigator_row_index_from(terminal_runtimes)
            .unwrap_or(0);
        self.ensure_navigator_selection_visible_from(terminal_runtimes);
    }

    #[cfg(test)]
    pub(crate) fn navigator_rows(&self) -> Vec<NavigatorRow> {
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        self.navigator_rows_from(&terminal_runtimes)
    }

    pub(crate) fn navigator_rows_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> Vec<NavigatorRow> {
        let query = self.navigator.query.trim().to_lowercase();
        let query_kind = navigator_query_kind(&query, self.navigator.state_filter);
        let mut rows = Vec::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            let workspace_label = ws.display_name_from(&self.terminals, terminal_runtimes);
            let activity = workspace_activity_summary(ws, &self.terminals);
            let workspace_search_text = format!("{workspace_label} {activity}").to_lowercase();
            let workspace_matches = match query_kind {
                NavigatorQueryKind::Empty => true,
                NavigatorQueryKind::State(filter) => {
                    let (state, seen) = ws.aggregate_state(&self.terminals);
                    navigator_state_filter_matches(filter, state, seen)
                }
                NavigatorQueryKind::Text => navigator_matches(&query, &workspace_search_text),
            };

            let child_rows =
                self.navigator_child_rows(ws_idx, query_kind, &query, workspace_matches);
            if !workspace_matches && child_rows.is_empty() {
                continue;
            }

            let expanded = !matches!(query_kind, NavigatorQueryKind::Empty)
                || self.navigator.expanded_workspaces.contains(&ws.id);
            let (state, seen) = ws.aggregate_state(&self.terminals);
            let pane_count = ws.tabs.iter().map(|tab| tab.panes.len()).sum::<usize>();
            rows.push(NavigatorRow {
                target: NavigatorTarget::Workspace { ws_idx },
                depth: 0,
                label: format!("{workspace_label} ({pane_count})"),
                meta: activity,
                status: state,
                seen,
                is_current: self.active == Some(ws_idx),
                is_workspace: true,
                is_tab: false,
                expanded,
                search_text: workspace_search_text,
                matched: workspace_matches,
            });
            if expanded {
                rows.extend(child_rows);
            }
        }
        rows
    }

    fn navigator_child_rows(
        &self,
        ws_idx: usize,
        query_kind: NavigatorQueryKind,
        query: &str,
        workspace_matches: bool,
    ) -> Vec<NavigatorRow> {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let multi_tab = ws.tabs.len() > 1;
        let mut rows = Vec::new();
        for tab_idx in 0..ws.tabs.len() {
            let mut tab_row = multi_tab.then(|| self.navigator_tab_row(ws_idx, tab_idx));
            let tab_matches = tab_row.as_ref().is_some_and(|row| match query_kind {
                NavigatorQueryKind::Empty => true,
                NavigatorQueryKind::State(filter) => {
                    navigator_state_filter_matches(filter, row.status, row.seen)
                }
                NavigatorQueryKind::Text => navigator_matches(query, &row.search_text),
            });
            if let Some(tab_row) = tab_row.as_mut() {
                tab_row.matched = tab_matches;
            }
            let mut pane_rows = self.navigator_pane_rows_for_tab(ws_idx, tab_idx, multi_tab);
            let filtered_panes = match query_kind {
                NavigatorQueryKind::Empty => pane_rows,
                NavigatorQueryKind::State(filter) => pane_rows
                    .into_iter()
                    .filter(|row| navigator_state_filter_matches(filter, row.status, row.seen))
                    .collect::<Vec<_>>(),
                // A matching workspace or tab shows its whole subtree; panes
                // keep their own match flag so context rows can be dimmed.
                NavigatorQueryKind::Text if workspace_matches || tab_matches => {
                    for row in pane_rows.iter_mut() {
                        row.matched = navigator_matches(query, &row.search_text);
                    }
                    pane_rows
                }
                NavigatorQueryKind::Text => pane_rows
                    .into_iter()
                    .filter(|row| navigator_matches(query, &row.search_text))
                    .collect::<Vec<_>>(),
            };

            if let Some(tab_row) = tab_row {
                if tab_matches || !filtered_panes.is_empty() {
                    rows.push(tab_row);
                }
            }
            rows.extend(filtered_panes);
        }
        rows
    }

    fn navigator_tab_row(&self, ws_idx: usize, tab_idx: usize) -> NavigatorRow {
        let ws = &self.workspaces[ws_idx];
        let tab = &ws.tabs[tab_idx];
        let label = ws
            .tab_display_name(tab_idx)
            .unwrap_or_else(|| (tab_idx + 1).to_string());
        let (status, seen) = tab_aggregate_state(tab, &self.terminals);
        let activity = tab_activity_summary(tab, &self.terminals);
        let pane_count = tab.panes.len();
        let meta = if activity.is_empty() {
            format!("{pane_count} panes")
        } else {
            format!("{pane_count} panes · {activity}")
        };
        let search_text = format!("{label} {meta}").to_lowercase();
        NavigatorRow {
            target: NavigatorTarget::Tab { ws_idx, tab_idx },
            depth: 1,
            label,
            meta,
            status,
            seen,
            is_current: false,
            is_workspace: false,
            is_tab: true,
            expanded: true,
            search_text,
            matched: true,
        }
    }

    fn navigator_pane_rows_for_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        multi_tab: bool,
    ) -> Vec<NavigatorRow> {
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return Vec::new();
        };
        let Some(tab) = ws.tabs.get(tab_idx) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for pane_id in tab.layout.pane_ids() {
            let Some(pane) = tab.panes.get(&pane_id) else {
                continue;
            };
            let terminal = self.terminals.get(&pane.attached_terminal_id);
            let pane_number = ws.public_pane_number(pane_id).unwrap_or(0);
            let label = terminal
                .and_then(|terminal| terminal.effective_title())
                .or_else(|| {
                    terminal
                        .and_then(|terminal| terminal.manual_label.as_deref().map(str::to_string))
                })
                .or_else(|| {
                    terminal.and_then(|terminal| terminal.agent_name.as_deref().map(str::to_string))
                })
                .or_else(|| {
                    terminal
                        .and_then(|terminal| terminal.effective_agent_label().map(str::to_string))
                })
                .or_else(|| {
                    launch_label(terminal.and_then(|terminal| terminal.launch_argv.as_ref()))
                })
                .unwrap_or_else(|| format!("pane {pane_number}"));
            let display_agent = terminal.and_then(|terminal| terminal.effective_display_agent());
            let agent_label = display_agent.as_deref().or_else(|| {
                terminal
                    .and_then(|terminal| terminal.agent_name.as_deref())
                    .or_else(|| terminal.and_then(|terminal| terminal.effective_agent_label()))
            });
            let state = terminal
                .map(|terminal| terminal.state)
                .unwrap_or(AgentState::Unknown);
            let status_label = terminal
                .map(|terminal| terminal.effective_presentation().state_labels)
                .and_then(|labels| labels.get(state_label_text(state, pane.seen)).cloned());
            let status = status_label
                .or_else(|| agent_label.map(|_| state_label_text(state, pane.seen).to_string()));
            let meta = match (agent_label, status.as_deref()) {
                (Some(agent_label), Some(status)) => format!("{agent_label} · {status}"),
                (Some(agent_label), None) => agent_label.to_string(),
                (None, _) => "shell".to_string(),
            };
            let is_current = self.is_active_pane(ws_idx, tab_idx, pane_id);
            let search_text = format!("{label} {meta}").to_lowercase();
            rows.push(NavigatorRow {
                target: NavigatorTarget::Pane {
                    ws_idx,
                    tab_idx,
                    pane_id,
                },
                depth: if multi_tab { 2 } else { 1 },
                label,
                meta,
                status: state,
                seen: pane.seen,
                is_current,
                is_workspace: false,
                is_tab: false,
                expanded: false,
                search_text,
                matched: true,
            });
        }
        rows
    }

    fn current_navigator_row_index_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> Option<usize> {
        let rows = self.navigator_rows_from(terminal_runtimes);
        rows.iter()
            .position(|row| matches!(row.target, NavigatorTarget::Pane { .. }) && row.is_current)
            .or_else(|| rows.iter().position(|row| row.is_current))
    }

    pub(crate) fn ensure_navigator_selection_visible_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        let body = self.navigator_body_rect();
        let viewport = body.height as usize;
        if viewport == 0 {
            self.navigator.scroll = 0;
            return;
        }
        let lines = navigator_display_lines(&self.navigator_rows_from(terminal_runtimes));
        let max_scroll = lines.len().saturating_sub(viewport);
        let selected_line =
            navigator_display_index_of_row(&lines, self.navigator.selected).unwrap_or(0);
        if selected_line < self.navigator.scroll {
            self.navigator.scroll = selected_line;
        } else if selected_line >= self.navigator.scroll.saturating_add(viewport) {
            self.navigator.scroll = selected_line.saturating_add(1).saturating_sub(viewport);
        }
        self.navigator.scroll = self.navigator.scroll.min(max_scroll);
    }

    pub(crate) fn navigator_max_scroll_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        viewport: usize,
    ) -> usize {
        if viewport == 0 {
            return 0;
        }
        navigator_display_lines(&self.navigator_rows_from(terminal_runtimes))
            .len()
            .saturating_sub(viewport)
    }

    /// After a mouse-wheel scroll, snap the selection to the first selectable
    /// row at or below the top of the viewport.
    pub(crate) fn align_navigator_selection_to_scroll_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        let lines = navigator_display_lines(&self.navigator_rows_from(terminal_runtimes));
        if let Some(row_idx) = navigator_first_row_at_or_after(&lines, self.navigator.scroll) {
            self.navigator.selected = row_idx;
        }
        self.clamp_navigator_selection_from(terminal_runtimes);
    }

    pub(crate) fn move_navigator_selection_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        delta: isize,
    ) {
        let count = self.navigator_rows_from(terminal_runtimes).len();
        if count == 0 {
            self.navigator.selected = 0;
            self.navigator.scroll = 0;
            return;
        }
        let current = self.navigator.selected.min(count - 1) as isize;
        self.navigator.selected = (current + delta).clamp(0, count as isize - 1) as usize;
        self.ensure_navigator_selection_visible_from(terminal_runtimes);
    }

    /// Move the selection by a distance measured in display lines (used for
    /// half-page jumps), landing on the nearest selectable row in the move
    /// direction.
    pub(crate) fn move_navigator_selection_by_lines_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        delta_lines: isize,
    ) {
        let rows = self.navigator_rows_from(terminal_runtimes);
        if rows.is_empty() {
            self.navigator.selected = 0;
            self.navigator.scroll = 0;
            return;
        }
        let lines = navigator_display_lines(&rows);
        let current_line =
            navigator_display_index_of_row(&lines, self.navigator.selected.min(rows.len() - 1))
                .unwrap_or(0);
        let target_line =
            (current_line as isize + delta_lines).clamp(0, lines.len() as isize - 1) as usize;
        let row_idx = if delta_lines >= 0 {
            navigator_first_row_at_or_after(&lines, target_line)
        } else {
            lines[..=target_line]
                .iter()
                .rev()
                .find_map(|line| match line {
                    super::state::NavigatorDisplayLine::Row(idx) => Some(*idx),
                    super::state::NavigatorDisplayLine::Spacer => None,
                })
        };
        if let Some(row_idx) = row_idx {
            self.navigator.selected = row_idx;
        }
        self.ensure_navigator_selection_visible_from(terminal_runtimes);
    }

    /// After the query or state filter changes, select the first row that
    /// itself matched the filter, so enter immediately accepts the best match.
    /// State filters prefer pane matches over aggregate workspace/tab matches.
    pub(crate) fn select_first_navigator_match_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        let query = self.navigator.query.trim().to_lowercase();
        let query_kind = navigator_query_kind(&query, self.navigator.state_filter);
        if !matches!(query_kind, NavigatorQueryKind::Empty) {
            let rows = self.navigator_rows_from(terminal_runtimes);
            let idx = if matches!(query_kind, NavigatorQueryKind::State(_)) {
                rows.iter()
                    .position(|row| {
                        row.matched && matches!(row.target, NavigatorTarget::Pane { .. })
                    })
                    .or_else(|| rows.iter().position(|row| row.matched))
            } else {
                rows.iter().position(|row| row.matched)
            };
            if let Some(idx) = idx {
                self.navigator.selected = idx;
            }
        }
        self.clamp_navigator_selection_from(terminal_runtimes);
    }

    pub(crate) fn clamp_navigator_selection_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        let count = self.navigator_rows_from(terminal_runtimes).len();
        self.navigator.selected = self.navigator.selected.min(count.saturating_sub(1));
        self.ensure_navigator_selection_visible_from(terminal_runtimes);
    }

    pub(crate) fn toggle_selected_navigator_workspace_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) {
        let Some(row) = self
            .navigator_rows_from(terminal_runtimes)
            .get(self.navigator.selected)
            .cloned()
        else {
            return;
        };
        let NavigatorTarget::Workspace { ws_idx } = row.target else {
            return;
        };
        let Some(workspace_id) = self.workspaces.get(ws_idx).map(|ws| ws.id.clone()) else {
            return;
        };
        if self.navigator.expanded_workspaces.contains(&workspace_id) {
            self.navigator.expanded_workspaces.remove(&workspace_id);
        } else {
            self.navigator.expanded_workspaces.insert(workspace_id);
        }
        self.clamp_navigator_selection_from(terminal_runtimes);
    }

    #[cfg(test)]
    pub(crate) fn accept_navigator_selection(&mut self) -> bool {
        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        self.accept_navigator_selection_from(&terminal_runtimes)
    }

    pub(crate) fn accept_navigator_selection_from(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        let Some(row) = self
            .navigator_rows_from(terminal_runtimes)
            .get(self.navigator.selected)
            .cloned()
        else {
            return false;
        };
        self.focus_navigator_target(row.target)
    }

    pub(crate) fn focus_navigator_target(&mut self, target: NavigatorTarget) -> bool {
        match target {
            NavigatorTarget::Workspace { ws_idx } => {
                if ws_idx >= self.workspaces.len() {
                    return false;
                }
                self.switch_workspace(ws_idx);
                self.mode = Mode::Terminal;
                true
            }
            NavigatorTarget::Tab { ws_idx, tab_idx } => {
                if ws_idx >= self.workspaces.len() {
                    return false;
                }
                let tab_exists = self
                    .workspaces
                    .get(ws_idx)
                    .is_some_and(|ws| tab_idx < ws.tabs.len());
                if !tab_exists {
                    return false;
                }
                self.switch_workspace_tab(ws_idx, tab_idx);
                self.mode = Mode::Terminal;
                true
            }
            NavigatorTarget::Pane {
                ws_idx,
                tab_idx,
                pane_id,
            } => {
                if ws_idx >= self.workspaces.len() {
                    return false;
                }
                if self
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.tabs.get(tab_idx))
                    .is_some_and(|tab| tab.panes.contains_key(&pane_id))
                {
                    self.focus_pane_in_workspace(ws_idx, pane_id);
                    self.mode = Mode::Terminal;
                    return true;
                }
                false
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigatorQueryKind {
    Empty,
    Text,
    State(NavigatorStateFilter),
}

fn navigator_query_kind(
    query: &str,
    state_filter: Option<NavigatorStateFilter>,
) -> NavigatorQueryKind {
    if let Some(filter) = state_filter {
        return NavigatorQueryKind::State(filter);
    }
    if query.is_empty() {
        NavigatorQueryKind::Empty
    } else {
        NavigatorQueryKind::Text
    }
}

fn navigator_state_filter_matches(
    filter: NavigatorStateFilter,
    state: AgentState,
    seen: bool,
) -> bool {
    match filter {
        NavigatorStateFilter::Blocked => state == AgentState::Blocked,
        NavigatorStateFilter::Working => state == AgentState::Working,
        NavigatorStateFilter::Idle => state == AgentState::Idle && seen,
        NavigatorStateFilter::Done => state == AgentState::Idle && !seen,
    }
}

fn navigator_matches(query: &str, text: &str) -> bool {
    text_matches_query(query, text)
}

fn launch_label(argv: Option<&Vec<String>>) -> Option<String> {
    let argv = argv?;
    let command = argv.first()?;
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .or_else(|| Some(command.clone()))
}

fn state_label_text(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn tab_aggregate_state(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
) -> (AgentState, bool) {
    let mut aggregate = AgentState::Unknown;
    let mut seen = true;
    for pane in tab.panes.values() {
        let Some(terminal) = terminals.get(&pane.attached_terminal_id) else {
            continue;
        };
        if state_priority(terminal.state, pane.seen) > state_priority(aggregate, seen) {
            aggregate = terminal.state;
            seen = pane.seen;
        }
    }
    (aggregate, seen)
}

fn state_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 5,
        (AgentState::Working, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Idle, true) => 2,
        (AgentState::Unknown, _) => 1,
    }
}

fn tab_activity_summary(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
) -> String {
    activity_summary_for_panes(tab.panes.values(), terminals)
}

fn workspace_activity_summary(
    ws: &crate::workspace::Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
) -> String {
    activity_summary_for_panes(ws.tabs.iter().flat_map(|tab| tab.panes.values()), terminals)
}

fn activity_summary_for_panes<'a>(
    panes: impl Iterator<Item = &'a crate::pane::PaneState>,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
) -> String {
    let mut blocked = 0usize;
    let mut working = 0usize;
    let mut done = 0usize;
    for pane in panes {
        let Some(terminal) = terminals.get(&pane.attached_terminal_id) else {
            continue;
        };
        match (terminal.state, pane.seen) {
            (AgentState::Blocked, _) => blocked += 1,
            (AgentState::Working, _) => working += 1,
            (AgentState::Idle, false) => done += 1,
            _ => {}
        }
    }

    let mut parts = Vec::new();
    if blocked > 0 {
        parts.push(format!("{blocked} blocked"));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if done > 0 {
        parts.push(format!("{done} done"));
    }
    parts.join(" · ")
}

// ---------------------------------------------------------------------------
// Workspace operations
// ---------------------------------------------------------------------------

impl AppState {
    pub(crate) fn next_agent_metadata_expiry(&self) -> Option<std::time::Instant> {
        self.terminals
            .values()
            .filter_map(|terminal| terminal.next_agent_metadata_expiry())
            .chain(
                self.terminals
                    .values()
                    .filter_map(|terminal| terminal.metadata_tokens.next_expiry()),
            )
            .chain(
                self.workspaces
                    .iter()
                    .filter_map(|workspace| workspace.metadata_tokens.next_expiry()),
            )
            .min()
    }

    pub(crate) fn expire_agent_metadata_at(
        &mut self,
        scheduled_deadline: std::time::Instant,
        now: std::time::Instant,
    ) -> Vec<PaneStateUpdate> {
        let pane_terminals: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| {
                            ws.pane_state(pane_id)
                                .map(|pane| (ws_idx, pane_id, pane.attached_terminal_id.clone()))
                        })
                })
            })
            .collect();
        pane_terminals
            .into_iter()
            .filter_map(|(ws_idx, pane_id, terminal_id)| {
                let previous_seen = self.workspaces[ws_idx].pane_state(pane_id)?.seen;
                let mutation = self
                    .terminals
                    .get_mut(&terminal_id)?
                    .expire_agent_metadata_at(scheduled_deadline, now)?;
                let change = mutation.effective_state_change?;
                let seen = self.apply_pane_state_change(ws_idx, pane_id, &change)?;
                let update = PaneStateUpdate {
                    pane_id,
                    ws_idx,
                    previous_agent_label: change.previous_agent_label.clone(),
                    previous_known_agent: change.previous_known_agent,
                    previous_state: change.previous_state,
                    previous_seen,
                    previous_presentation: change.previous_presentation.clone(),
                    agent_label: change.agent_label.clone(),
                    known_agent: change.known_agent,
                    state: change.state,
                    seen,
                    presentation: change.presentation.clone(),
                    agent_name_changed: false,
                    agent_released: false,
                    agent_release_status: None,
                };
                Some(update)
            })
            .collect()
    }

    pub(crate) fn expire_metadata_tokens(
        &mut self,
        now: std::time::Instant,
    ) -> (Vec<(usize, PaneId)>, Vec<usize>) {
        let pane_terminals = self
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                workspace.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| {
                            workspace
                                .pane_state(pane_id)
                                .map(|pane| (ws_idx, pane_id, pane.attached_terminal_id.clone()))
                        })
                })
            })
            .collect::<Vec<_>>();
        let changed_panes = pane_terminals
            .into_iter()
            .filter_map(|(ws_idx, pane_id, terminal_id)| {
                let terminal = self.terminals.get_mut(&terminal_id)?;
                terminal.metadata_tokens.expire_at(now).then(|| {
                    terminal.revision = terminal.revision.saturating_add(1);
                    (ws_idx, pane_id)
                })
            })
            .collect();
        let changed_workspaces = self
            .workspaces
            .iter_mut()
            .enumerate()
            .filter_map(|(ws_idx, workspace)| {
                workspace.metadata_tokens.expire_at(now).then_some(ws_idx)
            })
            .collect();
        (changed_panes, changed_workspaces)
    }

    pub(crate) fn pane_is_in_active_tab(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if active_ws_idx != ws_idx {
            return false;
        }
        self.workspaces[ws_idx]
            .find_tab_index_for_pane(pane_id)
            .is_some_and(|tab_idx| tab_idx == self.workspaces[ws_idx].active_tab)
    }

    pub fn switch_workspace(&mut self, idx: usize) {
        if idx < self.workspaces.len() {
            let previous_focus = self.current_pane_focus_target();
            self.active = Some(idx);
            self.selected = idx;
            let workspace_id = self.workspaces[idx].id.clone();
            crate::logging::workspace_focused(&workspace_id);
            self.mark_session_dirty();
            self.ensure_workspace_visible(idx);
            if let Some(ws) = self.workspaces.get_mut(idx) {
                let active_tab = ws.active_tab;
                ws.switch_tab(active_tab);
                let tab_id =
                    public_tab_id_for_index(ws, active_tab).unwrap_or_else(|| workspace_id.clone());
                crate::logging::tab_focused(&workspace_id, &tab_id);
            }
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
            self.record_pane_focus_after_navigation(previous_focus);
            self.sync_selection_after_focus_navigation();
        }
    }

    pub(crate) fn switch_workspace_tab(&mut self, ws_idx: usize, tab_idx: usize) -> bool {
        if ws_idx >= self.workspaces.len() {
            return false;
        }
        if self
            .workspaces
            .get(ws_idx)
            .is_none_or(|ws| tab_idx >= ws.tabs.len())
        {
            return false;
        }

        let previous_focus = self.current_pane_focus_target();
        let workspace_changed = self.active != Some(ws_idx);
        self.active = Some(ws_idx);
        self.selected = ws_idx;
        let workspace_id = self.workspaces[ws_idx].id.clone();
        if workspace_changed {
            crate::logging::workspace_focused(&workspace_id);
        }
        self.mark_session_dirty();
        self.ensure_workspace_visible(ws_idx);
        if let Some(ws) = self.workspaces.get_mut(ws_idx) {
            ws.switch_tab(tab_idx);
            let tab_id =
                public_tab_id_for_index(ws, tab_idx).unwrap_or_else(|| workspace_id.clone());
            crate::logging::tab_focused(&workspace_id, &tab_id);
        }
        self.tab_scroll_follow_active = true;
        self.refresh_tab_bar_view();
        self.record_pane_focus_after_navigation(previous_focus);
        self.sync_selection_after_focus_navigation();
        true
    }

    pub(crate) fn ensure_workspace_visible(&mut self, idx: usize) {
        if idx >= self.workspaces.len() {
            return;
        }

        if self.view.layout == ViewLayout::Mobile && self.mode == Mode::Navigate {
            self.ensure_mobile_workspace_visible(idx);
            return;
        }

        if self.sidebar_collapsed {
            return;
        }

        let entries = crate::ui::workspace_list_entries(self);
        let Some(target_entry_idx) = entries.iter().position(|entry| {
            matches!(
                entry,
                crate::ui::WorkspaceListEntry::Workspace { ws_idx, .. } if *ws_idx == idx
            )
        }) else {
            return;
        };

        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        let mut cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
        if cards.iter().any(|card| card.ws_idx == idx) {
            return;
        }

        if target_entry_idx < self.workspace_scroll {
            self.workspace_scroll = target_entry_idx;
            return;
        }

        while !cards.iter().any(|card| card.ws_idx == idx) {
            let previous_scroll = self.workspace_scroll;
            self.workspace_scroll = self.workspace_scroll.saturating_add(1);
            if self.workspace_scroll == previous_scroll {
                break;
            }
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            if self.workspace_scroll == previous_scroll {
                break;
            }
            cards = crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect);
            if cards.is_empty() {
                break;
            }
        }
    }

    fn ensure_mobile_workspace_visible(&mut self, idx: usize) {
        let viewport = crate::ui::mobile_switcher_areas(self).viewport;
        if viewport.height == 0 {
            return;
        }

        let row_range = crate::ui::mobile_switcher_workspace_doc_range(self, idx);
        let visible_start = self.mobile_switcher_scroll;
        let visible_end = visible_start.saturating_add(viewport.height as usize);
        if row_range.start < visible_start {
            self.mobile_switcher_scroll = row_range.start;
        } else if row_range.end > visible_end {
            self.mobile_switcher_scroll = row_range.end.saturating_sub(viewport.height as usize);
        }
        self.mobile_switcher_scroll = self
            .mobile_switcher_scroll
            .min(crate::ui::mobile_switcher_max_scroll(self));
    }

    #[cfg(test)]
    pub fn switch_tab(&mut self, idx: usize) {
        if let Some(ws_idx) = self.active {
            let previous_focus = self.current_pane_focus_target();
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return;
            };
            ws.switch_tab(idx);
            let workspace_id = ws.id.clone();
            let tab_id = public_tab_id_for_index(ws, idx).unwrap_or_else(|| workspace_id.clone());
            crate::logging::tab_focused(&workspace_id, &tab_id);
            self.mark_session_dirty();
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
            self.record_pane_focus_after_navigation(previous_focus);
            self.sync_selection_after_focus_navigation();
        }
    }

    pub(crate) fn mark_active_tab_seen(&mut self) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(crate::workspace::Workspace::active_tab_mut)
        else {
            return false;
        };

        let mut changed = false;
        for pane in tab.panes.values_mut() {
            if !pane.seen {
                pane.seen = true;
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn visible_workspace_order(&self) -> Vec<usize> {
        // Mobile always shows the worktree tree expanded, so its visible order
        // must ignore collapse state to match what the switcher renders.
        let entries = if self.view.layout == ViewLayout::Mobile {
            crate::ui::workspace_list_entries_expanded(self)
        } else {
            crate::ui::workspace_list_entries(self)
        };
        let order = entries
            .into_iter()
            .map(|entry| match entry {
                crate::ui::WorkspaceListEntry::Workspace { ws_idx, .. } => ws_idx,
            })
            .collect::<Vec<_>>();
        if order.is_empty() {
            (0..self.workspaces.len()).collect()
        } else {
            order
        }
    }

    pub(crate) fn workspace_at_visible_position(&self, position: usize) -> Option<usize> {
        self.visible_workspace_order().get(position).copied()
    }

    pub(crate) fn move_selected_workspace_by_visible_delta(&mut self, delta: isize) {
        if self.workspaces.is_empty() {
            return;
        }
        let order = self.visible_workspace_order();
        let current_pos = order
            .iter()
            .position(|idx| *idx == self.selected)
            .unwrap_or(0);
        let target_pos = current_pos
            .saturating_add_signed(delta)
            .min(order.len().saturating_sub(1));
        if let Some(ws_idx) = order.get(target_pos).copied() {
            self.selected = ws_idx;
            self.ensure_workspace_visible(ws_idx);
        }
    }

    #[cfg(test)]
    pub fn next_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(self.selected);
        let order = self.visible_workspace_order();
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let next = order[(current_pos + 1) % order.len()];
        self.switch_workspace(next);
    }

    #[cfg(test)]
    pub fn previous_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(self.selected);
        let order = self.visible_workspace_order();
        let current_pos = order.iter().position(|idx| *idx == current).unwrap_or(0);
        let prev = if current_pos == 0 {
            order[order.len() - 1]
        } else {
            order[current_pos - 1]
        };
        self.switch_workspace(prev);
    }

    pub fn move_workspace(&mut self, source_idx: usize, insert_idx: usize) -> bool {
        if source_idx >= self.workspaces.len() || insert_idx > self.workspaces.len() {
            return false;
        }

        let target_idx = if source_idx < insert_idx {
            insert_idx - 1
        } else {
            insert_idx
        };
        if source_idx == target_idx {
            return false;
        }

        self.mark_session_dirty();

        let active_id = self.active.map(|idx| self.workspaces[idx].id.clone());
        let selected_id = self
            .workspaces
            .get(self.selected)
            .map(|workspace| workspace.id.clone());

        let workspace = self.workspaces.remove(source_idx);
        self.workspaces.insert(target_idx, workspace);

        self.active = active_id.and_then(|id| self.workspaces.iter().position(|ws| ws.id == id));
        self.selected = selected_id
            .and_then(|id| self.workspaces.iter().position(|ws| ws.id == id))
            .unwrap_or(0);
        self.ensure_workspace_visible(self.selected);
        true
    }

    pub fn scroll_tabs_left(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_sub(1);
        self.refresh_tab_bar_view();
    }

    pub fn scroll_tabs_right(&mut self) {
        self.tab_scroll_follow_active = false;
        self.tab_scroll = self.tab_scroll.saturating_add(1);
        self.refresh_tab_bar_view();
    }

    #[cfg(test)]
    pub fn next_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let next = (ws.active_tab + 1) % ws.tabs.len();
                self.switch_tab(next);
            }
        }
    }

    #[cfg(test)]
    pub fn previous_tab(&mut self) {
        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
            if !ws.tabs.is_empty() {
                let prev = if ws.active_tab == 0 {
                    ws.tabs.len() - 1
                } else {
                    ws.active_tab - 1
                };
                self.switch_tab(prev);
            }
        }
    }

    #[cfg(test)]
    pub fn next_agent(&mut self) {
        self.cycle_agent_entry(true);
    }

    #[cfg(test)]
    pub fn previous_agent(&mut self) {
        self.cycle_agent_entry(false);
    }

    #[cfg(test)]
    pub fn focus_agent_entry(&mut self, idx: usize) -> bool {
        let entries = crate::ui::agent_panel_entries(self);
        let Some(target) = entries.get(idx) else {
            return false;
        };
        let ws_idx = target.ws_idx;
        let pane_id = target.pane_id;

        if self.active == Some(ws_idx) && self.workspaces[ws_idx].focused_pane_id() == Some(pane_id)
        {
            self.ensure_agent_panel_entry_visible(idx);
            return true;
        }

        if self.focus_pane_in_workspace(ws_idx, pane_id) {
            self.ensure_agent_panel_entry_visible(idx);
            return true;
        }
        false
    }

    #[cfg(test)]
    fn cycle_agent_entry(&mut self, forward: bool) {
        let entries = crate::ui::agent_panel_entries(self);
        if entries.is_empty() {
            return;
        }

        let focused = self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .and_then(crate::workspace::Workspace::focused_pane_id);
        let current_idx =
            focused.and_then(|pane_id| entries.iter().position(|entry| entry.pane_id == pane_id));
        let target_idx = match (current_idx, forward) {
            (Some(idx), true) => (idx + 1) % entries.len(),
            (Some(0), false) => entries.len() - 1,
            (Some(idx), false) => idx - 1,
            (None, true) => 0,
            (None, false) => entries.len() - 1,
        };

        self.focus_agent_entry(target_idx);
    }

    pub(crate) fn ensure_agent_panel_entry_visible(&mut self, idx: usize) {
        if self.sidebar_collapsed {
            return;
        }

        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            self.view.sidebar_rect,
            self.sidebar_section_split,
        );
        self.agent_panel_scroll = crate::ui::agent_panel_scroll_for_target(
            self,
            detail_area,
            self.agent_panel_scroll,
            idx,
        );
    }

    pub(crate) fn terminal_ids_for_workspace(
        &self,
        ws_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)
            .into_iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect()
    }

    pub(crate) fn pane_ids_for_workspace(&self, ws_idx: usize) -> Vec<PaneId> {
        self.workspaces
            .get(ws_idx)
            .into_iter()
            .flat_map(|ws| &ws.tabs)
            .flat_map(|tab| tab.layout.pane_ids())
            .collect()
    }

    pub(crate) fn terminal_ids_for_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Vec<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .into_iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| pane.attached_terminal_id.clone())
            .collect()
    }

    pub(crate) fn pane_ids_for_tab(&self, ws_idx: usize, tab_idx: usize) -> Vec<PaneId> {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default()
    }

    pub(crate) fn terminal_id_for_pane(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<crate::terminal::TerminalId> {
        self.workspaces
            .get(ws_idx)?
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
    }

    pub(crate) fn remove_unattached_terminal_ids(
        &mut self,
        terminal_ids: impl IntoIterator<Item = crate::terminal::TerminalId>,
    ) {
        for terminal_id in terminal_ids {
            let still_attached = self.workspaces.iter().any(|ws| {
                ws.tabs.iter().any(|tab| {
                    tab.panes
                        .values()
                        .any(|pane| pane.attached_terminal_id == terminal_id)
                })
            });
            if !still_attached
                && self.terminals.remove(&terminal_id).is_some()
                && !self.terminal_runtime_shutdowns.contains(&terminal_id)
            {
                self.terminal_runtime_shutdowns.push(terminal_id);
            }
        }
    }

    pub(crate) fn remove_plugin_pane_records(
        &mut self,
        pane_ids: impl IntoIterator<Item = PaneId>,
    ) {
        let pane_ids = pane_ids.into_iter().collect::<Vec<_>>();
        self.clear_copy_mode_for_removed_panes(pane_ids.iter().copied());
        if self
            .previous_pane_focus
            .as_ref()
            .is_some_and(|focus| pane_ids.contains(&focus.pane_id))
        {
            self.previous_pane_focus = None;
        }
        for pane_id in pane_ids {
            self.plugin_panes.remove(&pane_id);
            self.pane_graphics_layers.remove(&pane_id);
            self.pane_graphics_streams.remove(&pane_id);
        }
    }

    pub fn close_selected_workspace(&mut self) {
        if self.workspaces.is_empty() {
            return;
        }
        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let close_indices = self
            .workspaces
            .get(self.selected)
            .and_then(|ws| ws.worktree_space())
            .filter(|space| !space.is_linked_worktree)
            .map(|space| {
                self.workspaces
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, ws)| {
                        ws.worktree_space()
                            .is_some_and(|member| member.key == space.key)
                            .then_some(idx)
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|indices| indices.len() >= 2)
            .unwrap_or_else(|| vec![self.selected]);

        let mut terminal_ids = Vec::new();
        let mut pane_ids = Vec::new();
        for idx in &close_indices {
            terminal_ids.extend(self.terminal_ids_for_workspace(*idx));
            pane_ids.extend(self.pane_ids_for_workspace(*idx));
            if let Some(workspace_id) = self.workspaces.get(*idx).map(|ws| ws.id.clone()) {
                crate::logging::workspace_closed(&workspace_id);
            }
        }
        self.remove_plugin_pane_records(pane_ids);
        for idx in close_indices.iter().rev() {
            self.workspaces.remove(*idx);
        }
        self.remove_unattached_terminal_ids(terminal_ids);
        if self.workspaces.is_empty() {
            self.active = None;
            self.selected = 0;
            self.workspace_scroll = 0;
            self.tab_scroll = 0;
            self.tab_scroll_follow_active = true;
        } else {
            if self.selected >= self.workspaces.len() {
                self.selected = self.workspaces.len() - 1;
            }
            self.active = Some(self.selected);
            self.workspace_scroll = self
                .workspace_scroll
                .min(self.workspaces.len().saturating_sub(1));
            self.ensure_workspace_visible(self.selected);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
    }

    pub(crate) fn refresh_tab_bar_view(&mut self) {
        let area = self.view.tab_bar_rect;
        let Some(ws) = self.active.and_then(|idx| self.workspaces.get(idx)) else {
            self.tab_scroll = 0;
            self.view.tab_hit_areas.clear();
            self.view.tab_scroll_left_hit_area = ratatui::layout::Rect::default();
            self.view.tab_scroll_right_hit_area = ratatui::layout::Rect::default();
            self.view.new_tab_hit_area = ratatui::layout::Rect::default();
            return;
        };

        let layout = crate::ui::compute_tab_bar_view(
            ws,
            area,
            self.tab_scroll,
            self.tab_scroll_follow_active,
            self.mouse_capture,
        );
        self.tab_scroll = layout.scroll;
        self.view.tab_hit_areas = layout.tab_hit_areas;
        self.view.tab_scroll_left_hit_area = layout.scroll_left_hit_area;
        self.view.tab_scroll_right_hit_area = layout.scroll_right_hit_area;
        self.view.new_tab_hit_area = layout.new_tab_hit_area;
    }
}

// ---------------------------------------------------------------------------
// Pane operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneZoomCommand {
    Toggle,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneZoomNoopReason {
    SinglePane,
    AlreadyZoomed,
    AlreadyUnzoomed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneZoomOutcome {
    pub changed: bool,
    pub focus_changed: bool,
    pub reason: Option<PaneZoomNoopReason>,
    pub zoomed: bool,
}

impl AppState {
    #[cfg(test)]
    pub fn navigate_pane(&mut self, direction: NavDirection) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return;
        };
        let panes = if tab.zoomed {
            tab.layout.panes(self.view.terminal_area)
        } else {
            self.view.pane_infos.clone()
        };

        if let Some(focused) = panes.iter().find(|p| p.is_focused) {
            if let Some(target) = find_in_direction(focused, direction, &panes) {
                self.focus_pane_in_workspace(ws_idx, target);
            }
        }
    }

    #[cfg(test)]
    pub fn swap_pane(&mut self, direction: NavDirection) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return false;
        };
        let panes = if tab.zoomed {
            tab.layout.panes(self.view.terminal_area)
        } else {
            self.view.pane_infos.clone()
        };

        let Some(focused) = panes.iter().find(|p| p.is_focused) else {
            return false;
        };
        let Some(target) = find_in_direction(focused, direction, &panes) else {
            return false;
        };
        let source = focused.id;
        let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.active_tab_mut())
        else {
            return false;
        };
        if tab.layout.swap_panes(source, target) {
            self.mark_session_dirty();
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub fn resize_pane(&mut self, direction: NavDirection) {
        if let Some(first) = self.view.pane_infos.first() {
            let area = self
                .view
                .pane_infos
                .iter()
                .fold(first.rect, |acc, p| acc.union(p.rect));
            if let Some(tab) = self
                .active
                .and_then(|i| self.workspaces.get_mut(i))
                .and_then(|ws| ws.active_tab_mut())
            {
                tab.layout.resize_focused(direction, 0.05, area);
                self.mark_session_dirty();
            }
        }
    }

    #[cfg(test)]
    pub fn cycle_pane(&mut self, reverse: bool) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(tab) = self.workspaces.get(ws_idx).and_then(|ws| ws.active_tab()) else {
            return;
        };
        let ids = tab.layout.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == tab.layout.focused()) {
            let target = if reverse {
                ids[(pos + ids.len() - 1) % ids.len()]
            } else {
                ids[(pos + 1) % ids.len()]
            };
            self.focus_pane_in_workspace(ws_idx, target);
        }
    }

    #[cfg(test)]
    pub fn last_pane(&mut self) {
        let Some(target) = self.previous_pane_focus.clone() else {
            return;
        };
        let Some((ws_idx, tab_idx)) = self.pane_focus_target_indices(&target) else {
            self.previous_pane_focus = None;
            return;
        };
        let current = self.current_pane_focus_target();
        if current.as_ref() == Some(&target) {
            self.previous_pane_focus = None;
            return;
        }

        self.switch_workspace_tab(ws_idx, tab_idx);
        if let Some(tab) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            tab.layout.focus_pane(target.pane_id);
            self.previous_pane_focus = current;
            self.mark_session_dirty();
        }
    }

    pub(crate) fn apply_pane_zoom(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        command: PaneZoomCommand,
    ) -> Option<PaneZoomOutcome> {
        let tab_idx = self
            .workspaces
            .get(ws_idx)?
            .find_tab_index_for_pane(pane_id)?;
        let focus_changed = self.focus_pane_in_workspace(ws_idx, pane_id);
        let tab = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))?;
        if tab.layout.pane_count() <= 1 {
            return Some(PaneZoomOutcome {
                changed: false,
                focus_changed,
                reason: Some(PaneZoomNoopReason::SinglePane),
                zoomed: tab.zoomed,
            });
        }

        let desired = match command {
            PaneZoomCommand::Toggle => !tab.zoomed,
            PaneZoomCommand::On => true,
            PaneZoomCommand::Off => false,
        };
        let reason = match (command, tab.zoomed) {
            (PaneZoomCommand::On, true) => Some(PaneZoomNoopReason::AlreadyZoomed),
            (PaneZoomCommand::Off, false) => Some(PaneZoomNoopReason::AlreadyUnzoomed),
            _ => None,
        };
        if reason.is_some() {
            return Some(PaneZoomOutcome {
                changed: false,
                focus_changed,
                reason,
                zoomed: tab.zoomed,
            });
        }

        tab.zoomed = desired;
        let zoomed = tab.zoomed;
        self.mark_session_dirty();
        Some(PaneZoomOutcome {
            changed: true,
            focus_changed,
            reason: None,
            zoomed,
        })
    }

    #[cfg(test)]
    pub fn toggle_zoom(&mut self) {
        let Some(ws_idx) = self.active else {
            return;
        };
        let Some(pane_id) = self
            .workspaces
            .get(ws_idx)
            .and_then(crate::workspace::Workspace::focused_pane_id)
        else {
            return;
        };
        self.apply_pane_zoom(ws_idx, pane_id, PaneZoomCommand::Toggle);
    }

    pub(crate) fn workspace_close_would_close_worktree_group(&self, ws_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space())
            .filter(|space| !space.is_linked_worktree)
            .is_some_and(|space| {
                self.workspaces
                    .iter()
                    .filter(|ws| {
                        ws.worktree_space()
                            .is_some_and(|member| member.key == space.key)
                    })
                    .count()
                    >= 2
            })
    }

    pub(crate) fn confirm_implicit_worktree_group_close(&mut self, ws_idx: usize) -> bool {
        if self.confirm_close && self.workspace_close_would_close_worktree_group(ws_idx) {
            self.selected = ws_idx;
            self.mode = Mode::ConfirmClose;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn close_focused_pane_would_close_workspace(&self, ws_idx: usize) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|ws| {
            let pane_count = ws
                .active_tab()
                .map(|tab| tab.layout.pane_count())
                .unwrap_or(0);
            pane_count <= 1 && ws.tabs.len() <= 1
        })
    }

    pub(crate) fn close_pane_would_close_workspace(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|ws| {
            ws.find_tab_index_for_pane(pane_id).is_some_and(|tab_idx| {
                ws.tabs[tab_idx].layout.pane_count() <= 1 && ws.tabs.len() <= 1
            })
        })
    }

    #[cfg(test)]
    /// Close the focused pane. Returns true when the close was deferred to confirmation.
    pub fn close_pane(&mut self) -> bool {
        let active = self.active;
        if active.is_some_and(|ws_idx| {
            self.close_focused_pane_would_close_workspace(ws_idx)
                && self.workspace_close_would_close_worktree_group(ws_idx)
        }) {
            if let Some(ws_idx) = active {
                if self.confirm_implicit_worktree_group_close(ws_idx) {
                    return true;
                }
            }
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let terminal_ids = active
            .and_then(|i| {
                self.workspaces
                    .get(i)
                    .and_then(|ws| ws.focused_pane_id().map(|pane_id| (i, pane_id)))
            })
            .and_then(|(i, pane_id)| self.terminal_id_for_pane(i, pane_id))
            .into_iter()
            .collect::<Vec<_>>();
        let pane_ids = active
            .and_then(|i| self.workspaces.get(i).and_then(|ws| ws.focused_pane_id()))
            .into_iter()
            .collect::<Vec<_>>();
        let should_close_workspace = active
            .and_then(|i| self.workspaces.get_mut(i))
            .is_some_and(|ws| ws.close_focused());
        self.remove_plugin_pane_records(pane_ids);
        if should_close_workspace {
            if let Some(active) = active {
                self.selected = active;
            }
            self.close_selected_workspace();
        } else {
            self.remove_unattached_terminal_ids(terminal_ids);
        }
        false
    }

    #[cfg(test)]
    /// Close the active tab. Returns true when the close was deferred to confirmation.
    pub fn close_tab(&mut self) -> bool {
        if self.active.is_some_and(|ws_idx| {
            self.workspaces
                .get(ws_idx)
                .is_some_and(|ws| ws.tabs.len() <= 1)
                && self.workspace_close_would_close_worktree_group(ws_idx)
        }) {
            if let Some(ws_idx) = self.active {
                if self.confirm_implicit_worktree_group_close(ws_idx) {
                    return true;
                }
            }
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.mark_session_dirty();
        let should_close_workspace = self
            .active
            .and_then(|i| self.workspaces.get(i))
            .is_some_and(|ws| ws.tabs.len() <= 1);
        if should_close_workspace {
            if let Some(active) = self.active {
                self.selected = active;
            }
            self.close_selected_workspace();
            return false;
        }
        if let Some(ws_idx) = self.active {
            let terminal_ids = self
                .workspaces
                .get(ws_idx)
                .map(|ws| self.terminal_ids_for_tab(ws_idx, ws.active_tab))
                .unwrap_or_default();
            let pane_ids = self
                .workspaces
                .get(ws_idx)
                .map(|ws| self.pane_ids_for_tab(ws_idx, ws.active_tab))
                .unwrap_or_default();
            let Some(ws) = self.workspaces.get_mut(ws_idx) else {
                return false;
            };
            let workspace_id = ws.id.clone();
            let closing_tab_id =
                public_tab_id_for_index(ws, ws.active_tab).unwrap_or_else(|| workspace_id.clone());
            ws.close_active_tab();
            self.remove_plugin_pane_records(pane_ids);
            self.remove_unattached_terminal_ids(terminal_ids);
            crate::logging::tab_closed(&workspace_id, &closing_tab_id);
            self.tab_scroll_follow_active = true;
            self.refresh_tab_bar_view();
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

impl AppState {
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_autoscroll = None;
    }

    pub(crate) fn stop_selection_autoscroll_state(&mut self) {
        self.selection_autoscroll = None;
    }

    pub(crate) fn copy_word_at_pane_cell(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        viewport_row: u16,
        col: u16,
    ) -> bool {
        // Resolve the active pane cell the double-click landed on.
        let Some(ws_idx) = self
            .active
            .filter(|idx| self.workspaces.get(*idx).is_some())
        else {
            return false;
        };

        let Some(info) = self.pane_info_by_id(pane_id) else {
            return false;
        };
        if viewport_row >= info.inner_rect.height || col >= info.inner_rect.width {
            return false;
        }

        // Leave mouse input to terminal apps that requested it.
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
        else {
            return false;
        };
        if rt
            .input_state()
            .is_some_and(crate::pane::InputState::mouse_reporting_enabled)
        {
            return false;
        }

        // Read the visible row and identify the clicked token bounds.
        let metrics = self.pane_scroll_metrics(terminal_runtimes, pane_id);
        let row_selection = Selection::range(
            pane_id,
            viewport_row,
            0,
            info.inner_rect.width.saturating_sub(1),
            metrics,
        );
        let Some(row_text) = rt.extract_selection(&row_selection) else {
            return false;
        };
        let Some((start_col, end_col)) = word_bounds_at_column(&row_text, col) else {
            return false;
        };

        // Copy the token and keep its selection visible as short-lived feedback.
        let mut selection = Selection::range(pane_id, viewport_row, start_col, end_col, metrics);
        if !selection.finish() {
            return false;
        }

        let Some(text) = rt
            .extract_selection(&selection)
            .filter(|text| !text.is_empty())
        else {
            self.clear_selection();
            return false;
        };
        self.request_clipboard_write = Some(text.into_bytes());
        self.selection = Some(selection);
        self.selection_autoscroll = None;
        info!("copied double-clicked token to clipboard");
        true
    }

    pub(crate) fn url_at_pane_cell(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        viewport_row: u16,
        col: u16,
    ) -> Option<String> {
        let ws_idx = self
            .active
            .filter(|idx| self.workspaces.get(*idx).is_some())?;
        let info = self.pane_info_by_id(pane_id)?;
        if viewport_row >= info.inner_rect.height || col >= info.inner_rect.width {
            return None;
        }

        let rt = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)?;
        let screen_col = info.inner_rect.x.saturating_add(col);
        let screen_row = info.inner_rect.y.saturating_add(viewport_row);
        if let Some((_, _, uri)) = rt
            .visible_hyperlinks(info.inner_rect)
            .into_iter()
            .find(|((x, y), _, _)| *x == screen_col && *y == screen_row)
        {
            return safe_web_url(&uri).map(str::to_owned);
        }

        let metrics = self.pane_scroll_metrics(terminal_runtimes, pane_id);
        let visible_selection = Selection::line_range(
            pane_id,
            Selection::absolute_row_for_viewport(0, metrics),
            Selection::absolute_row_for_viewport(info.inner_rect.height.saturating_sub(1), metrics),
            info.inner_rect.width.saturating_sub(1),
        );
        let visible_text = rt.extract_selection(&visible_selection)?;
        let logical_cell =
            logical_cell_for_visible_cell(&visible_text, info.inner_rect.width, viewport_row, col)?;
        let line_start = visible_text[..logical_cell.byte_index]
            .rfind('\n')
            .map_or(0, |idx| idx + 1);
        let line_end = visible_text[logical_cell.byte_index..]
            .find('\n')
            .map_or(visible_text.len(), |idx| logical_cell.byte_index + idx);
        let line = visible_text.get(line_start..line_end)?;
        url_at_column(line, logical_cell.logical_col).map(str::to_owned)
    }

    pub fn copy_selection(&mut self, terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry) {
        let mut sel = match self.selection.take() {
            Some(sel) => sel,
            None => return,
        };
        if !sel.finish() {
            return;
        }

        let ws_idx = match self.active {
            Some(ws_idx) if self.workspaces.get(ws_idx).is_some() => ws_idx,
            _ => return,
        };

        let text = self
            .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, sel.pane_id)
            .and_then(|rt| rt.extract_selection(&sel));
        if let Some(text) = text {
            if !text.is_empty() {
                self.request_clipboard_write = Some(text.into_bytes());
                info!("copied selection to clipboard");
            }
        }

        self.clear_selection();
    }
}

pub(crate) fn safe_web_url(url: &str) -> Option<&str> {
    (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextCell {
    ch: char,
    start_col: u16,
    end_col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellSpan {
    start: usize,
    end: usize,
}

impl CellSpan {
    fn contains(self, idx: usize) -> bool {
        idx >= self.start && idx <= self.end
    }

    fn columns(self, cells: &[TextCell]) -> (u16, u16) {
        (cells[self.start].start_col, cells[self.end].end_col)
    }
}

/// Finds the terminal display-column bounds for the token under a double-click.
///
/// The algorithm first maps text to terminal cells so wide characters and
/// zero-width marks use display columns, then prefers structured spans that
/// users expect to copy whole (URLs and quoted paths), and finally falls back
/// to a separator-delimited token.
fn word_bounds_at_column(row: &str, col: u16) -> Option<(u16, u16)> {
    // Map the row into display cells before doing any word-boundary work.
    let cells = text_cells(row);
    let clicked_idx = cell_index_at_column(&cells, col)?;

    // Prefer spans that can legally include punctuation or spaces.
    let span = url_span_at_column(&cells, clicked_idx)
        .or_else(|| quoted_path_span_at_column(&cells, clicked_idx))
        .or_else(|| token_span_at_column(&cells, clicked_idx))?;

    // Convert the internal cell span back to inclusive terminal columns.
    Some(span.columns(&cells))
}

pub(crate) fn url_at_column(row: &str, col: u16) -> Option<&str> {
    let cells = text_cells(row);
    let clicked_idx = cell_index_at_column(&cells, col)?;
    let span = url_spans(&cells)
        .into_iter()
        .find(|span| span.contains(clicked_idx))?;
    let start_byte = byte_index_for_cell(row, span.start);
    let end_byte = byte_index_after_cell(row, span.end);
    safe_web_url(row.get(start_byte..end_byte)?)
}

fn url_spans(cells: &[TextCell]) -> Vec<CellSpan> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < cells.len() {
        if starts_with_chars(&cells[start..], "http://")
            || starts_with_chars(&cells[start..], "https://")
        {
            let mut end = start;
            while end + 1 < cells.len() && !cells[end + 1].ch.is_whitespace() {
                end += 1;
            }
            if let Some(span) = trim_url_edges(cells, CellSpan { start, end }) {
                spans.push(span);
            }
            start = end + 1;
        } else {
            start += 1;
        }
    }
    spans
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisibleTextCell {
    pub(crate) byte_index: usize,
    pub(crate) ch: char,
    pub(crate) logical_col: u16,
    pub(crate) screen_row: u16,
    pub(crate) screen_col: u16,
}

pub(crate) fn visible_text_cells(text: &str, pane_width: u16) -> Vec<VisibleTextCell> {
    if pane_width == 0 {
        return Vec::new();
    }

    let mut cells = Vec::new();
    let mut screen_row = 0u16;
    let mut screen_col = 0u16;
    let mut logical_col = 0u16;
    let mut pending_wrap = false;
    for (byte_index, ch) in text.char_indices() {
        if ch == '\n' {
            screen_row = screen_row.saturating_add(1);
            screen_col = 0;
            logical_col = 0;
            pending_wrap = false;
            continue;
        }
        if pending_wrap {
            screen_row = screen_row.saturating_add(1);
            screen_col = 0;
            pending_wrap = false;
        }

        let width = u16::from(crate::ghostty::unicode_codepoint_width(ch as u32));
        cells.push(VisibleTextCell {
            byte_index,
            ch,
            logical_col,
            screen_row,
            screen_col,
        });

        logical_col = logical_col.saturating_add(width);
        screen_col = screen_col.saturating_add(width);
        while screen_col > pane_width {
            screen_col -= pane_width;
            screen_row = screen_row.saturating_add(1);
        }
        if width > 0 && screen_col == pane_width {
            pending_wrap = true;
            screen_col = pane_width.saturating_sub(1);
        }
    }
    cells
}

pub(crate) fn logical_cell_for_visible_cell(
    text: &str,
    pane_width: u16,
    target_row: u16,
    target_col: u16,
) -> Option<VisibleTextCell> {
    visible_text_cells(text, pane_width)
        .into_iter()
        .find(|cell| {
            let width = u16::from(crate::ghostty::unicode_codepoint_width(cell.ch as u32));
            cell.screen_row == target_row
                && if width == 0 {
                    target_col == cell.screen_col
                } else {
                    target_col >= cell.screen_col
                        && target_col < cell.screen_col.saturating_add(width)
                }
        })
}

fn token_span_at_column(cells: &[TextCell], clicked_idx: usize) -> Option<CellSpan> {
    if is_word_separator(cells[clicked_idx].ch) {
        return None;
    }

    let mut start = clicked_idx;
    while start > 0 && !is_word_separator(cells[start - 1].ch) {
        start -= 1;
    }

    let mut end = clicked_idx;
    while end + 1 < cells.len() && !is_word_separator(cells[end + 1].ch) {
        end += 1;
    }

    trim_token_edges(cells, CellSpan { start, end }).filter(|span| span.contains(clicked_idx))
}

fn text_cells(row: &str) -> Vec<TextCell> {
    let mut next_col = 0u16;
    row.chars()
        .map(|ch| {
            let width = u16::from(crate::ghostty::unicode_codepoint_width(ch as u32));
            let start_col = if width == 0 {
                next_col.saturating_sub(1)
            } else {
                next_col
            };
            if width > 0 {
                next_col = next_col.saturating_add(width);
            }
            TextCell {
                ch,
                start_col,
                end_col: next_col.saturating_sub(1),
            }
        })
        .collect()
}

fn cell_index_at_column(cells: &[TextCell], col: u16) -> Option<usize> {
    cells
        .iter()
        .position(|cell| cell.start_col <= col && col <= cell.end_col)
}

fn byte_index_for_cell(row: &str, cell_idx: usize) -> usize {
    row.char_indices()
        .nth(cell_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(row.len())
}

fn byte_index_after_cell(row: &str, cell_idx: usize) -> usize {
    row.char_indices()
        .nth(cell_idx.saturating_add(1))
        .map(|(idx, _)| idx)
        .unwrap_or(row.len())
}

fn url_span_at_column(cells: &[TextCell], clicked_idx: usize) -> Option<CellSpan> {
    let mut start = 0;
    while start < cells.len() {
        if starts_with_chars(&cells[start..], "http://")
            || starts_with_chars(&cells[start..], "https://")
        {
            let mut end = start;
            while end + 1 < cells.len() && !cells[end + 1].ch.is_whitespace() {
                end += 1;
            }
            if clicked_idx >= start && clicked_idx <= end {
                let span = trim_url_edges(cells, CellSpan { start, end })?;
                return span.contains(clicked_idx).then_some(span);
            }
            start = end + 1;
        } else {
            start += 1;
        }
    }
    None
}

fn trim_url_edges(cells: &[TextCell], span: CellSpan) -> Option<CellSpan> {
    let start = span.start;
    let mut end = span.end;
    while start <= end && should_trim_trailing_url_cell(cells, start, end) {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    (start <= end).then_some(CellSpan { start, end })
}

fn should_trim_trailing_url_cell(cells: &[TextCell], start: usize, end: usize) -> bool {
    match cells[end].ch {
        '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?' => true,
        ')' => !trailing_url_closer_is_balanced(cells, start, end, '(', ')'),
        ']' => !trailing_url_closer_is_balanced(cells, start, end, '[', ']'),
        '}' => !trailing_url_closer_is_balanced(cells, start, end, '{', '}'),
        _ => false,
    }
}

fn trailing_url_closer_is_balanced(
    cells: &[TextCell],
    start: usize,
    end: usize,
    open: char,
    close: char,
) -> bool {
    let mut balance = 0i32;
    for cell in &cells[start..end] {
        if cell.ch == open {
            balance += 1;
        } else if cell.ch == close {
            balance -= 1;
        }
    }
    balance > 0
}

fn quoted_path_span_at_column(cells: &[TextCell], clicked_idx: usize) -> Option<CellSpan> {
    let clicked = cells.get(clicked_idx)?.ch;
    if clicked == '"' || clicked == '\'' || clicked == '`' {
        return None;
    }

    for quote in ['"', '\'', '`'] {
        let mut start = None;
        for (idx, cell) in cells.iter().copied().enumerate() {
            let ch = cell.ch;
            if ch != quote || is_escaped(cells, idx) {
                continue;
            }
            if let Some(open) = start {
                if clicked_idx > open
                    && clicked_idx < idx
                    && cells[open + 1..idx].iter().any(|cell| cell.ch == '/')
                {
                    return Some(CellSpan {
                        start: open + 1,
                        end: idx - 1,
                    });
                }
                start = None;
            } else {
                start = Some(idx);
            }
        }
    }
    None
}

fn is_escaped(cells: &[TextCell], idx: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = idx;
    while cursor > 0 && cells[cursor - 1].ch == '\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn starts_with_chars(cells: &[TextCell], prefix: &str) -> bool {
    prefix
        .chars()
        .enumerate()
        .all(|(idx, expected)| cells.get(idx).is_some_and(|cell| cell.ch == expected))
}

fn is_word_separator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '|' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '!'
        )
}

fn trim_token_edges(cells: &[TextCell], span: CellSpan) -> Option<CellSpan> {
    let mut start = span.start;
    let mut end = span.end;
    while start <= end && is_leading_token_wrapper(cells[start].ch) {
        start += 1;
    }
    if start < end && cells[end].ch == '$' && is_trailing_token_wrapper(cells[end - 1].ch) {
        end -= 1;
    }
    while start <= end && is_trailing_token_wrapper(cells[end].ch) {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    (start <= end).then_some(CellSpan { start, end })
}

fn is_leading_token_wrapper(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '`')
}

fn is_trailing_token_wrapper(ch: char) -> bool {
    matches!(
        ch,
        ')' | ']' | '}' | '>' | '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?'
    )
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl AppState {
    pub fn apply_workspace_git_statuses(
        &mut self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
        results: Vec<WorkspaceGitStatus>,
    ) -> bool {
        let mut changed = false;
        for result in results {
            let Some(ws_idx) = self
                .workspaces
                .iter()
                .position(|ws| ws.id == result.workspace_id)
            else {
                continue;
            };

            if self.workspaces[ws_idx]
                .resolved_identity_cwd_from(&self.terminals, terminal_runtimes)
                .as_ref()
                != Some(&result.resolved_identity_cwd)
            {
                continue;
            }

            let ws = &mut self.workspaces[ws_idx];
            if ws.cached_git_branch != result.branch {
                ws.cached_git_branch = result.branch;
                changed = true;
            }
            if ws.cached_git_ahead_behind != result.ahead_behind {
                ws.cached_git_ahead_behind = result.ahead_behind;
                changed = true;
            }
            if ws.cached_git_space != result.space {
                ws.cached_git_space = result.space;
                changed = true;
            }
        }
        changed
    }

    pub fn handle_app_event(&mut self, event: AppEvent) -> Vec<PaneStateUpdate> {
        match event {
            AppEvent::PaneDied { pane_id } => {
                self.handle_pane_died(pane_id);
                Vec::new()
            }
            AppEvent::UpdateReady {
                version,
                install_command,
            } => {
                self.update_available = Some(version.clone());
                self.update_install_command = install_command.clone();
                self.latest_release_notes_available = true;
                self.update_dismissed = true;
                if matches!(
                    self.toast_config.delivery,
                    crate::config::ToastDelivery::Herdr
                ) {
                    self.toast = Some(ToastNotification {
                        kind: ToastKind::UpdateInstalled,
                        title: format!("v{version} available"),
                        context: crate::update::update_install_instruction(&install_command),
                        position: None,
                        target: None,
                    });
                }
                Vec::new()
            }
            AppEvent::AgentDetectionManifestsUpdated { updated, status } => {
                self.agent_manifest_update_status = status;
                self.refresh_agent_manifest_summaries();
                if !updated.is_empty()
                    && matches!(
                        self.toast_config.delivery,
                        crate::config::ToastDelivery::Herdr
                    )
                {
                    let agent_list = updated
                        .iter()
                        .map(|item| {
                            format!(
                                "{} {}",
                                crate::detect::agent_label(item.agent),
                                item.version
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.toast = Some(ToastNotification {
                        kind: ToastKind::UpdateInstalled,
                        title: "Agent detection rules updated".to_string(),
                        context: agent_list,
                        position: None,
                        target: None,
                    });
                }
                Vec::new()
            }
            AppEvent::StateChanged {
                pane_id,
                agent,
                state,
                visible_blocker,
                visible_working,
                process_exited,
                observed_at,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    Some(terminal.set_detected_state_with_screen_signals_at(
                        agent,
                        state,
                        visible_blocker,
                        false,
                        visible_working,
                        process_exited,
                        observed_at,
                    ))
                })
                .into_iter()
                .collect(),
            AppEvent::HookStateReported {
                pane_id,
                source,
                agent_label,
                state,
                message,
                seq,
                session_ref,
            } => {
                if crate::agent_resume::is_reserved_native_state_source(&source, &agent_label) {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.set_agent_session_ref(source, agent_label, session_ref, seq)
                    })
                    .into_iter()
                    .collect()
                } else {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.set_hook_authority_with_session_ref(
                            source,
                            agent_label,
                            state,
                            message,
                            session_ref,
                            seq,
                        )
                    })
                    .into_iter()
                    .collect()
                }
            }
            AppEvent::AgentSessionReported {
                pane_id,
                source,
                agent_label,
                seq,
                session_ref,
                session_start_source,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.set_agent_session_ref_for_session_start(
                        source,
                        agent_label,
                        session_ref,
                        seq,
                        session_start_source,
                    )
                })
                .into_iter()
                .collect(),
            AppEvent::HookMetadataReported {
                pane_id,
                source,
                agent_label,
                applies_to_source,
                title,
                display_agent,
                state_labels,
                clear_title,
                clear_display_agent,
                clear_state_labels,
                seq,
                ttl,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.set_agent_metadata(crate::terminal::AgentMetadataReport {
                        source,
                        agent_label,
                        applies_to_source,
                        title,
                        display_agent,
                        state_labels,
                        clear_title,
                        clear_display_agent,
                        clear_state_labels,
                        ttl,
                        seq,
                    })
                })
                .into_iter()
                .collect(),
            AppEvent::HookAuthorityCleared {
                pane_id,
                source,
                seq,
            } => self
                .update_terminal_state(pane_id, |terminal| {
                    terminal.clear_hook_authority_with_mutation(source.as_deref(), seq)
                })
                .into_iter()
                .collect(),
            AppEvent::HookAgentReleased {
                pane_id,
                source,
                agent_label,
                seq,
                ..
            } => {
                if crate::agent_resume::is_reserved_native_state_source(&source, &agent_label) {
                    Vec::new()
                } else {
                    self.update_terminal_state(pane_id, |terminal| {
                        terminal.release_agent_with_mutation(&source, &agent_label, seq)
                    })
                    .into_iter()
                    .collect()
                }
            }
            // Both intercepted before this dispatch — in App::handle_internal_event (monolithic)
            // or via HeadlessServer forwarding to the foreground client (server); never touch
            // AppState. Kept for AppEvent exhaustiveness.
            AppEvent::ClipboardWrite { .. } => Vec::new(),
            AppEvent::PrefixInputSource { .. } => Vec::new(),
            AppEvent::TerminalCwdReported { pane_id, cwd } => {
                if !cwd.is_absolute() || !cwd.is_dir() {
                    return Vec::new();
                }
                let Some(terminal_id) = self.workspaces.iter().find_map(|ws| {
                    ws.pane_state(pane_id)
                        .map(|pane| pane.attached_terminal_id.clone())
                }) else {
                    return Vec::new();
                };
                let Some(terminal) = self.terminals.get_mut(&terminal_id) else {
                    return Vec::new();
                };
                if terminal.cwd != cwd {
                    terminal.cwd = cwd;
                    self.mark_session_dirty();
                }
                Vec::new()
            }
            AppEvent::GitStatusRefreshed {
                results,
                cache_updates,
            } => {
                let _ = results;
                let _ = cache_updates;
                Vec::new()
            }
            AppEvent::WorktreeAddFinished(_) => Vec::new(),
            AppEvent::WorktreeRemoveFinished(_) => Vec::new(),
            AppEvent::PluginCommandFinished { .. } => Vec::new(),
        }
    }

    fn update_terminal_state<F>(&mut self, pane_id: PaneId, update: F) -> Option<PaneStateUpdate>
    where
        F: FnOnce(&mut crate::terminal::TerminalState) -> Option<TerminalStateMutation>,
    {
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.pane_state(pane_id).is_some())?;
        let terminal_id = self.workspaces[ws_idx]
            .pane_state(pane_id)?
            .attached_terminal_id
            .clone();
        let previous_seen = self.workspaces[ws_idx].pane_state(pane_id)?.seen;
        let now = Instant::now();
        let (mutation, managed_changed, agent_name_changed, unchanged_change) = {
            let terminal = self.terminals.get_mut(&terminal_id)?;
            let previous_agent_name = terminal.agent_name.clone();
            let mutation = update(terminal)?;
            let managed_changed = terminal.reconcile_managed_agent_at(now, false);
            let agent_name_changed = terminal.agent_name != previous_agent_name;
            let unchanged_change = (mutation.agent_released || agent_name_changed)
                .then(|| terminal.unchanged_effective_state_change_at(now));
            (
                mutation,
                managed_changed,
                agent_name_changed,
                unchanged_change,
            )
        };
        if mutation.session_ref_changed || managed_changed || agent_name_changed {
            self.mark_session_dirty();
        }
        let agent_released = mutation.agent_released;
        let change = mutation.effective_state_change.or(unchanged_change)?;
        if change.previous_state != change.state {
            self.next_agent_state_change_seq += 1;
            if let Some(terminal) = self.terminals.get_mut(&terminal_id) {
                terminal.last_agent_state_change_seq = Some(self.next_agent_state_change_seq);
            }
        }
        let seen = self.apply_pane_state_change(ws_idx, pane_id, &change)?;
        let update = PaneStateUpdate {
            pane_id,
            ws_idx,
            previous_agent_label: change.previous_agent_label.clone(),
            previous_known_agent: change.previous_known_agent,
            previous_state: change.previous_state,
            previous_seen,
            previous_presentation: change.previous_presentation.clone(),
            agent_label: change.agent_label.clone(),
            known_agent: change.known_agent,
            state: change.state,
            seen,
            presentation: change.presentation.clone(),
            agent_name_changed,
            agent_released,
            agent_release_status: agent_released.then(|| pane_agent_status(change.state, seen)),
        };
        Some(update)
    }

    pub(crate) fn next_managed_agent_deadline(&self) -> Option<Instant> {
        self.terminals
            .values()
            .filter_map(crate::terminal::TerminalState::next_managed_agent_deadline)
            .min()
    }

    pub(crate) fn reconcile_managed_agents_at(&mut self, now: Instant) -> Vec<(usize, PaneId)> {
        let mut changed_terminals = std::collections::HashSet::new();
        for (terminal_id, terminal) in &mut self.terminals {
            if terminal.reconcile_managed_agent_at(now, false) {
                changed_terminals.insert(terminal_id.clone());
            }
        }
        if changed_terminals.is_empty() {
            return Vec::new();
        }
        self.mark_session_dirty();
        self.workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                let changed_terminals = &changed_terminals;
                workspace.tabs.iter().flat_map(move |tab| {
                    tab.panes.iter().filter_map(move |(&pane_id, pane)| {
                        changed_terminals
                            .contains(&pane.attached_terminal_id)
                            .then_some((ws_idx, pane_id))
                    })
                })
            })
            .collect()
    }

    pub(crate) fn publish_pane_process_exit_if_agent(
        &mut self,
        pane_id: PaneId,
    ) -> Option<PaneStateUpdate> {
        let observed_at = std::time::Instant::now();
        let update = self.update_terminal_state(pane_id, |terminal| {
            let agent = terminal.effective_known_agent().or(terminal.detected_agent);
            if agent.is_none() && !terminal.full_lifecycle_hook_authority_active() {
                return None;
            }
            Some(terminal.set_detected_state_with_screen_signals_at(
                agent,
                AgentState::Idle,
                false,
                true,
                false,
                true,
                observed_at,
            ))
        })?;
        update.agent_released.then_some(update)
    }

    fn apply_pane_state_change(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        change: &EffectiveStateChange,
    ) -> Option<bool> {
        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);
        let pane = self.workspaces[ws_idx]
            .tabs
            .iter_mut()
            .find_map(|tab| tab.panes.get_mut(&pane_id))?;

        if change.state != AgentState::Idle {
            pane.seen = true;
        } else if is_completion_transition(change) {
            pane.seen = suppress_active_tab_notifications;
        }
        let seen = pane.seen;

        if let Some(delivery) = self.record_or_deliver_agent_notification(ws_idx, pane_id, change) {
            self.apply_agent_notification_delivery(&delivery);
        }

        Some(seen)
    }

    fn record_or_deliver_agent_notification(
        &mut self,
        ws_idx: usize,
        pane_id: PaneId,
        change: &EffectiveStateChange,
    ) -> Option<AgentNotificationDelivery> {
        self.pending_agent_notifications.remove(&pane_id);

        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);

        let client_notification_kind = notification_toast_for_effective_state_change(
            suppress_active_tab_notifications,
            change,
        );
        let sound = notification_sound_for_effective_state_change(
            suppress_active_tab_notifications,
            change,
        );
        if client_notification_kind.is_none() && sound.is_none() {
            return None;
        }

        let agent_label = change.agent_label.clone()?;
        let kind = client_notification_kind.unwrap_or(match sound {
            Some(crate::sound::Sound::Request) => ToastKind::NeedsAttention,
            Some(crate::sound::Sound::Done) | None => ToastKind::Finished,
        });
        let workspace_id = self.workspaces[ws_idx].id.clone();

        if self.toast_config.delay_seconds == 0 {
            return self.agent_notification_delivery(
                ws_idx,
                pane_id,
                workspace_id,
                agent_label,
                change.known_agent,
                kind,
                change.state,
            );
        }

        self.pending_agent_notifications.insert(
            pane_id,
            PendingAgentNotification {
                pane_id,
                workspace_id,
                agent_label,
                known_agent: change.known_agent,
                kind,
                state: change.state,
                deadline: {
                    let now = std::time::Instant::now();
                    let delay_seconds = self
                        .toast_config
                        .delay_seconds
                        .min(crate::config::MAX_TOAST_DELAY_SECONDS);
                    now.checked_add(std::time::Duration::from_secs(delay_seconds))
                        .unwrap_or(now)
                },
            },
        );
        None
    }

    fn agent_notification_delivery(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
        workspace_id: String,
        agent_label: String,
        known_agent: Option<Agent>,
        kind: ToastKind,
        expected_state: AgentState,
    ) -> Option<AgentNotificationDelivery> {
        let terminal_state = self
            .workspaces
            .get(ws_idx)?
            .pane_state(pane_id)
            .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))?;
        if terminal_state.state != expected_state {
            return None;
        }
        if terminal_state.effective_agent_label() != Some(agent_label.as_str()) {
            return None;
        }

        let is_active_tab = self.pane_is_in_active_tab(ws_idx, pane_id);
        let suppress_active_tab_notifications =
            active_tab_suppresses_notifications(is_active_tab, self.outer_terminal_focus);
        let sound = sound_for_toast_kind(kind, suppress_active_tab_notifications)
            .filter(|_| self.sound.allows(known_agent));
        let build_toast = || {
            let workspace_label = self.workspaces[ws_idx].display_name();
            let context =
                notification_context(&self.workspaces[ws_idx], &workspace_label, ws_idx, pane_id);
            ToastNotification {
                kind,
                title: format!(
                    "{} {}",
                    toast_agent_label(&agent_label),
                    toast_event_text(kind)
                ),
                context,
                position: None,
                target: Some(ToastTarget {
                    workspace_id: workspace_id.clone(),
                    pane_id,
                }),
            }
        };
        let toast = (!is_active_tab).then(build_toast);
        let client_notification = (!suppress_active_tab_notifications).then(build_toast);

        if toast.is_none() && client_notification.is_none() && sound.is_none() {
            return None;
        }

        Some(AgentNotificationDelivery {
            pane_id,
            workspace_id,
            agent_label,
            known_agent,
            kind,
            toast,
            client_notification,
            sound,
        })
    }

    fn apply_agent_notification_delivery(&mut self, delivery: &AgentNotificationDelivery) {
        if self.local_sound_playback {
            if let Some(sound) = delivery.sound {
                crate::sound::play(sound, &self.sound);
            }
        }

        if matches!(
            self.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        ) {
            if let Some(toast) = delivery.toast.clone() {
                self.toast = Some(toast);
            }
        }
    }

    pub fn next_pending_agent_notification_deadline(&self) -> Option<std::time::Instant> {
        self.pending_agent_notifications
            .values()
            .map(|pending| pending.deadline)
            .min()
    }

    pub fn drain_due_agent_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> Vec<AgentNotificationDelivery> {
        let due_panes: Vec<PaneId> = self
            .pending_agent_notifications
            .iter()
            .filter_map(|(&pane_id, pending)| (pending.deadline <= now).then_some(pane_id))
            .collect();
        let mut deliveries = Vec::new();

        for pane_id in due_panes {
            let Some(pending) = self.pending_agent_notifications.remove(&pane_id) else {
                continue;
            };
            let Some(ws_idx) = self
                .workspaces
                .iter()
                .position(|ws| ws.id == pending.workspace_id)
            else {
                continue;
            };
            let Some(delivery) = self.agent_notification_delivery(
                ws_idx,
                pending.pane_id,
                pending.workspace_id,
                pending.agent_label,
                pending.known_agent,
                pending.kind,
                pending.state,
            ) else {
                continue;
            };
            self.apply_agent_notification_delivery(&delivery);
            deliveries.push(delivery);
        }

        deliveries
    }

    fn handle_pane_died(&mut self, pane_id: PaneId) {
        self.pending_agent_notifications.remove(&pane_id);
        self.remove_plugin_pane_records([pane_id]);
        let ws_idx = self
            .workspaces
            .iter()
            .position(|ws| ws.find_tab_index_for_pane(pane_id).is_some());

        let Some(ws_idx) = ws_idx else {
            warn!(pane = pane_id.raw(), "PaneDied for unknown pane");
            return;
        };

        if self
            .selection
            .as_ref()
            .is_some_and(|s| s.pane_id == pane_id)
        {
            self.selection = None;
            self.selection_autoscroll = None;
        }

        let pane_terminal_id = self.terminal_id_for_pane(ws_idx, pane_id);
        let workspace_terminal_ids = self.terminal_ids_for_workspace(ws_idx);
        self.pane_id_aliases.retain(|_, alias| *alias != pane_id);
        self.public_pane_id_aliases
            .retain(|_, alias| *alias != pane_id);
        let should_close_workspace = {
            let ws = &mut self.workspaces[ws_idx];
            ws.remove_pane(pane_id)
        };
        self.mark_session_dirty();

        if should_close_workspace {
            self.workspaces.remove(ws_idx);
            self.remove_unattached_terminal_ids(workspace_terminal_ids);
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
            self.remove_unattached_terminal_ids(pane_terminal_id);
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
        state.toast_config.delay_seconds = 0;
        for name in names {
            let ws = Workspace::test_new(name);
            state.workspaces.push(ws);
        }
        state.ensure_test_terminals();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.mode = Mode::Terminal;
        }
        state
    }

    fn insert_test_pane_graphics_layer(state: &mut AppState, pane_id: PaneId) {
        state.pane_graphics_layers.insert(
            pane_id,
            crate::app::state::PaneGraphicsLayer::new(
                crate::api::schema::PaneGraphicsFormat::Rgba,
                1,
                1,
                vec![1, 2, 3, 4],
                crate::api::schema::PaneGraphicsPlacementParams::default(),
            ),
        );
    }

    fn insert_test_pane_graphics_state(state: &mut AppState, pane_id: PaneId) {
        insert_test_pane_graphics_layer(state, pane_id);
        state
            .pane_graphics_streams
            .insert(pane_id, "test-stream".into());
    }

    fn mark_linked_worktree(state: &mut AppState, ws_idx: usize) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: true,
        });
    }

    fn mark_parent_worktree(state: &mut AppState, ws_idx: usize) {
        state.workspaces[ws_idx].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
    }

    #[test]
    fn notification_context_formats_resolved_workspace_label() {
        let state = app_with_workspaces(&["stale"]);
        let root = state.workspaces[0].tabs[0].root_pane;

        assert_eq!(
            notification_context(&state.workspaces[0], "__herdr_projects__", 0, root),
            "__herdr_projects__ · 1"
        );
    }

    fn selected_word(row: &str, col: u16) -> Option<String> {
        let (start, end) = word_bounds_at_column(row, col)?;
        Some(text_in_cell_range(row, start, end))
    }

    fn selected_url<'a>(row: &'a str, click: &str) -> Option<&'a str> {
        url_at_column(row, col_of(row, click))
    }

    fn text_in_cell_range(row: &str, start_col: u16, end_col: u16) -> String {
        text_cells(row)
            .into_iter()
            .filter(|cell| cell.start_col >= start_col && cell.end_col <= end_col)
            .map(|cell| cell.ch)
            .collect()
    }

    fn col_of(row: &str, needle: &str) -> u16 {
        let byte_idx = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not found in {row:?}"));
        let prefix = &row[..byte_idx];
        prefix
            .chars()
            .map(|ch| u16::from(crate::ghostty::unicode_codepoint_width(ch as u32)))
            .sum()
    }

    fn assert_selects(row: &str, click: &str, expected: &str) {
        assert_eq!(
            selected_word(row, col_of(row, click)).as_deref(),
            Some(expected),
            "row={row:?}, click={click:?}"
        );
    }

    fn assert_selects_nothing(row: &str, click: &str) {
        assert_eq!(
            selected_word(row, col_of(row, click)),
            None,
            "row={row:?}, click={click:?}"
        );
    }

    #[test]
    fn double_click_word_bounds_cover_terminal_text() {
        let cases = [
            (
                "see https://example.com/a-b_c?q=x@y.",
                "example.com",
                "https://example.com/a-b_c?q=x@y",
            ),
            (
                "open \"https://example.com/a,b;c?q=x\";",
                "example.com",
                "https://example.com/a,b;c?q=x",
            ),
            (
                "see https://en.wikipedia.org/wiki/Foo_(bar_(baz)),",
                "wikipedia",
                "https://en.wikipedia.org/wiki/Foo_(bar_(baz))",
            ),
            (
                "see https://example.com/a(b[c{d}e]f),",
                "example.com",
                "https://example.com/a(b[c{d}e]f)",
            ),
            (
                "see (https://example.com/a(b(c)d)))",
                "example.com",
                "https://example.com/a(b(c)d)",
            ),
            (
                "open /tmp/foo-bar/baz_qux/",
                "foo-bar",
                "/tmp/foo-bar/baz_qux/",
            ),
            (
                "open ./src/app/actions.rs:795",
                "actions",
                "./src/app/actions.rs:795",
            ),
            (
                "open ../herdr-worktrees/issue-1",
                "herdr",
                "../herdr-worktrees/issue-1",
            ),
            (
                "edit src/app/actions.rs,then",
                "actions",
                "src/app/actions.rs",
            ),
            (
                "cat \"/tmp/build output/log.txt\"",
                "output",
                "/tmp/build output/log.txt",
            ),
            (
                "cat '/Users/me/Library/Application Support/app/config.json'",
                "Support",
                "/Users/me/Library/Application Support/app/config.json",
            ),
            ("echo 你好-world done", "好", "你好-world"),
            ("先跑 cargo test", "cargo", "cargo"),
            (
                "export PATH=$HOME/.cargo/bin:$PATH",
                "$HOME",
                "PATH=$HOME/.cargo/bin:$PATH",
            ),
            (
                "git checkout feature/foo-bar_baz",
                "foo",
                "feature/foo-bar_baz",
            ),
            ("refs #123 and @owner/name", "#123", "#123"),
            ("refs #123 and @owner/name", "owner", "@owner/name"),
            ("cargo test --package=herdr", "--package", "--package=herdr"),
            (
                "cargo test app::actions::tests",
                "app::",
                "app::actions::tests",
            ),
            (
                "image ghcr.io/org/app:latest",
                "ghcr",
                "ghcr.io/org/app:latest",
            ),
            ("ERROR [worker-1] request_id=abc-123", "worker", "worker-1"),
            (
                "tmux|newhoo|fixhoo|newmoo|notification|window_bell|herdr",
                "newhoo",
                "newhoo",
            ),
            (
                "render_status_line(app, area)",
                "render",
                "render_status_line",
            ),
            ("render_status_line(app, area)", "app", "app"),
            ("render_status_line(app, area)", "area", "area"),
            ("if !enabled {", "enabled", "enabled"),
            ("println!(\"hi\")", "println", "println"),
            ("( master)$", "master", "master"),
            ("regex foo$", "foo", "foo$"),
        ];

        for (row, click, expected) in cases {
            assert_selects(row, click, expected);
        }

        let row = "echo 你好-world done";
        assert_eq!(
            selected_word(row, col_of(row, "好") + 1).as_deref(),
            Some("你好-world")
        );
    }

    #[test]
    fn double_click_word_bounds_ignore_delimiters() {
        for (row, click) in [
            (
                "tmux|newhoo|fixhoo|newmoo|notification|window_bell|herdr",
                "|",
            ),
            ("alpha,beta;gamma", ","),
            ("alpha,beta;gamma", ";"),
            ("render_status_line(app, area)", "("),
            ("render_status_line(app, area)", ")"),
            ("if !enabled {", "!"),
            ("if !enabled {", "{"),
            ("(done).", "("),
            ("(done).", "."),
        ] {
            assert_selects_nothing(row, click);
        }
    }

    #[test]
    fn url_at_column_returns_safe_visible_url_only() {
        assert_eq!(
            selected_url("see https://example.com/a(b)c.", "example"),
            Some("https://example.com/a(b)c")
        );
        assert_eq!(
            selected_url("[docs](https://example.com/docs),", "example"),
            Some("https://example.com/docs")
        );
        assert_eq!(
            selected_url("[docs](https://example.com/docs)", "docs"),
            None
        );
        assert_eq!(selected_url("open file:///tmp/report", "file"), None);
    }

    #[test]
    fn navigator_rows_show_tab_nodes_only_for_multi_tab_workspaces() {
        let mut state = app_with_workspaces(&["single", "multi"]);
        state.workspaces[1].test_add_tab(Some("tests"));
        state.ensure_test_terminals();

        state.open_navigator();
        let rows = state.navigator_rows();

        assert!(!rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Tab { ws_idx: 0, .. }
        )));
        assert!(rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Tab {
                ws_idx: 1,
                tab_idx: 0
            }
        )));
        assert!(rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Tab {
                ws_idx: 1,
                tab_idx: 1
            }
        )));
    }

    #[tokio::test]
    async fn navigator_rows_match_live_root_runtime_cwd_workspace_label() {
        let unique = format!(
            "herdr-navigator-runtime-cwd-{}-{}",
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

        let mut state = AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;
        state.workspaces = vec![workspace];
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].terminal_id(pane).cloned().unwrap();
        state.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = crate::terminal::TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        state.open_navigator_from(&runtime_registry);
        state.navigator.query = "herdr".into();
        let rows = state.navigator_rows_from(&runtime_registry);

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        // The workspace matched by its live cwd label; its subtree cascades in
        // as context.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "herdr (1)");
        assert!(rows[0].matched);
        assert!(!rows[1].matched);
    }

    #[test]
    fn navigator_rows_include_shell_and_agent_panes() {
        let mut state = app_with_workspaces(&["one"]);
        let shell = state.workspaces[0].tabs[0].root_pane;
        let agent = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();

        let agent_terminal_id = state.workspaces[0].terminal_id(agent).cloned().unwrap();
        let terminal = state.terminals.get_mut(&agent_terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);

        state.open_navigator();
        let rows = state.navigator_rows();

        assert!(rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == shell
        )));
        assert!(rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == agent
        ) && row.meta.contains("claude")));
    }

    #[test]
    fn opening_navigator_selects_current_pane_and_expands_attention_workspaces() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let blocked = state.workspaces[1].tabs[0].root_pane;
        let blocked_terminal_id = state.workspaces[1].terminal_id(blocked).cloned().unwrap();
        state
            .terminals
            .get_mut(&blocked_terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Blocked);

        state.open_navigator();
        let selected = state.navigator_rows()[state.navigator.selected].clone();

        assert!(selected.is_current);
        assert!(state
            .navigator
            .expanded_workspaces
            .contains(&state.workspaces[0].id));
        assert!(state
            .navigator
            .expanded_workspaces
            .contains(&state.workspaces[1].id));
    }

    #[test]
    fn accepting_navigator_pane_switches_workspace_tab_and_focus() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let target = state.workspaces[1].tabs[0].root_pane;
        state.open_navigator();
        state
            .navigator
            .expanded_workspaces
            .insert(state.workspaces[1].id.clone());
        state.navigator.selected = state
            .navigator_rows()
            .iter()
            .position(|row| {
                matches!(
                    row.target,
                    crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == target
                )
            })
            .unwrap();

        assert!(state.accept_navigator_selection());

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(target));
        assert_eq!(state.mode, Mode::Terminal);
    }

    #[test]
    fn navigator_idle_search_matches_idle_agents_not_plain_shells() {
        let mut state = app_with_workspaces(&["one"]);
        let shell = state.workspaces[0].tabs[0].root_pane;
        let agent = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();

        let agent_terminal_id = state.workspaces[0].terminal_id(agent).cloned().unwrap();
        state
            .terminals
            .get_mut(&agent_terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        state.open_navigator();
        state.navigator.query = "idle".into();
        let rows = state.navigator_rows();

        assert!(rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == agent
        )));
        assert!(!rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == shell
        )));
    }

    #[test]
    fn navigator_search_only_matches_visible_row_text() {
        let mut state = app_with_workspaces(&["one"]);
        state.workspaces[0].identity_cwd = "/tmp/herdr-worktrees/issue-work".into();

        state.open_navigator();
        state.navigator.query = "work".into();

        assert!(state.navigator_rows().is_empty());
    }

    #[test]
    fn navigator_state_filter_is_separate_from_text_search() {
        let mut state = app_with_workspaces(&["one"]);
        let shell = state.workspaces[0].tabs[0].root_pane;
        let working = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();

        let shell_terminal_id = state.workspaces[0].terminal_id(shell).cloned().unwrap();
        state
            .terminals
            .get_mut(&shell_terminal_id)
            .unwrap()
            .set_manual_label("wheel notes".into());
        let working_terminal_id = state.workspaces[0].terminal_id(working).cloned().unwrap();
        state
            .terminals
            .get_mut(&working_terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        state.open_navigator();
        state.navigator.state_filter = Some(NavigatorStateFilter::Working);
        let state_rows = state.navigator_rows();

        assert!(state_rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == working
        )));
        assert!(!state_rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == shell
        )));

        state.navigator.state_filter = None;
        state.navigator.query = "w".into();
        let text_rows = state.navigator_rows();

        assert!(text_rows.iter().any(|row| matches!(
            row.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == shell
        )));
        assert!(
            text_rows.iter().any(|row| matches!(
                row.target,
                crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == working
            )),
            "literal one-letter search may still match visible state text"
        );
    }

    #[test]
    fn navigator_search_filters_panes_but_keeps_workspace_context() {
        let mut state = app_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].terminal_id(root).cloned().unwrap();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("weekly review".into());
        state.open_navigator();
        state.navigator.query = "weekly".into();

        let rows = state.navigator_rows();

        assert!(rows.iter().any(|row| row.is_workspace));
        assert!(rows
            .iter()
            .any(|row| !row.is_workspace && row.label.contains("weekly")));
    }

    #[test]
    fn navigator_workspace_match_cascades_full_subtree() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let extra = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        for pane in [root, extra] {
            let terminal_id = state.workspaces[0].terminal_id(pane).cloned().unwrap();
            state
                .terminals
                .get_mut(&terminal_id)
                .unwrap()
                .set_manual_label("unrelated".into());
        }

        state.open_navigator();
        state.navigator.query = "one".into();
        let rows = state.navigator_rows();

        // Both panes cascade in even though only the workspace label matched,
        // and only the workspace carries the matched flag.
        let pane_rows: Vec<_> = rows.iter().filter(|row| !row.is_workspace).collect();
        assert_eq!(pane_rows.len(), 2);
        assert!(pane_rows.iter().all(|row| !row.matched));
        assert!(rows.iter().any(|row| row.is_workspace && row.matched));
        assert!(!rows.iter().any(|row| row.label.starts_with("two")));
    }

    #[test]
    fn navigator_search_selects_first_self_match() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let pane = state.workspaces[1].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[1].terminal_id(pane).cloned().unwrap();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("pi ui build".into());

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.open_navigator_from(&terminal_runtimes);
        state.navigator.query = "ui".into();
        state.select_first_navigator_match_from(&terminal_runtimes);

        let rows = state.navigator_rows_from(&terminal_runtimes);
        let selected = &rows[state.navigator.selected];
        assert!(matches!(
            selected.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == pane
        ));
    }

    #[test]
    fn navigator_state_filter_selects_matching_pane_over_workspace() {
        let mut state = app_with_workspaces(&["one"]);
        let working = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].terminal_id(working).cloned().unwrap();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        state.open_navigator_from(&terminal_runtimes);
        state.navigator.state_filter = Some(NavigatorStateFilter::Working);
        state.select_first_navigator_match_from(&terminal_runtimes);

        let rows = state.navigator_rows_from(&terminal_runtimes);
        let selected = &rows[state.navigator.selected];
        assert!(matches!(
            selected.target,
            crate::app::state::NavigatorTarget::Pane { pane_id, .. } if pane_id == working
        ));
    }

    #[test]
    fn apply_workspace_git_statuses_updates_matching_workspace() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_id = state.workspaces[0].id.clone();
        let first_cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        let second_id = state.workspaces[1].id.clone();

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id: first_id,
                resolved_identity_cwd: first_cwd,
                branch: Some("main".into()),
                ahead_behind: Some((2, 1)),
                space: None,
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("main"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((2, 1)));
        assert_eq!(state.workspaces[1].id, second_id);
        assert_eq!(state.workspaces[1].git_ahead_behind(), None);
    }

    #[test]
    fn apply_workspace_git_statuses_ignores_stale_cwd() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        state.workspaces[0].cached_git_branch = Some("old".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 0));

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: std::path::PathBuf::from("/definitely/not/current"),
                branch: Some("main".into()),
                ahead_behind: Some((0, 1)),
                space: None,
            }],
        );

        assert!(!changed);
        assert_eq!(state.workspaces[0].branch().as_deref(), Some("old"));
        assert_eq!(state.workspaces[0].git_ahead_behind(), Some((1, 0)));
    }

    #[test]
    fn apply_workspace_git_statuses_clears_missing_git_status() {
        let mut state = app_with_workspaces(&["one"]);
        let workspace_id = state.workspaces[0].id.clone();
        let cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        state.workspaces[0].cached_git_branch = Some("main".into());
        state.workspaces[0].cached_git_ahead_behind = Some((1, 2));

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd,
                branch: None,
                ahead_behind: None,
                space: None,
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].branch(), None);
        assert_eq!(state.workspaces[0].git_ahead_behind(), None);
    }

    #[test]
    fn apply_workspace_git_statuses_does_not_change_worktree_membership() {
        let mut state = app_with_workspaces(&["one"]);
        mark_linked_worktree(&mut state, 0);
        let workspace_id = state.workspaces[0].id.clone();
        let cwd = state.workspaces[0].resolved_identity_cwd().unwrap();
        let membership = state.workspaces[0].worktree_space().cloned();

        let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let changed = state.apply_workspace_git_statuses(
            &terminal_runtimes,
            vec![WorkspaceGitStatus {
                workspace_id,
                resolved_identity_cwd: cwd,
                branch: Some("scratch".into()),
                ahead_behind: None,
                space: Some(crate::workspace::GitSpaceMetadata {
                    key: "other-repo-key".into(),
                    checkout_key: "/other/checkout".into(),
                    label: "other".into(),
                    repo_root: "/other/repo".into(),
                    is_linked_worktree: false,
                }),
            }],
        );

        assert!(changed);
        assert_eq!(state.workspaces[0].worktree_space().cloned(), membership);
    }

    fn mark_agent(state: &mut AppState, ws_idx: usize, tab_idx: usize, pane_id: PaneId) {
        set_agent_state(state, ws_idx, tab_idx, pane_id, AgentState::Idle);
    }

    fn set_agent_state(
        state: &mut AppState,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
        agent_state: AgentState,
    ) {
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        if let Some(terminal) = state.terminals.get_mut(&terminal_id) {
            terminal.set_detected_state(Some(Agent::Pi), agent_state);
        }
    }

    fn transition_agent_state(state: &mut AppState, pane_id: PaneId, agent_state: AgentState) {
        state
            .update_terminal_state(pane_id, |terminal| {
                Some(terminal.set_detected_state_with_screen_signals_at(
                    Some(Agent::Pi),
                    agent_state,
                    matches!(agent_state, AgentState::Blocked),
                    false,
                    false,
                    false,
                    std::time::Instant::now(),
                ))
            })
            .expect("agent state transition should update pane state");
    }

    #[test]
    fn next_agent_cycles_agent_panel_entries() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_root);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 0, 0, first_second);
        mark_agent(&mut state, 1, 0, second_root);

        state.next_agent();
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_second));

        state.next_agent();
        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));

        state.previous_agent();
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_second));
        state.assert_invariants_for_test();
    }

    #[test]
    fn focus_agent_entry_uses_agent_panel_order() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_root);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        mark_agent(&mut state, 0, 0, first_root);
        mark_agent(&mut state, 0, 0, first_second);
        mark_agent(&mut state, 1, 0, second_root);

        assert!(state.focus_agent_entry(2));

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
        state.assert_invariants_for_test();
    }

    #[test]
    fn focus_agent_entry_succeeds_for_already_focused_agent() {
        let mut state = app_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        mark_agent(&mut state, 0, 0, root);

        assert!(state.focus_agent_entry(0));
        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        state.assert_invariants_for_test();
    }

    #[test]
    fn next_agent_cycles_priority_sorted_agent_panel_entries() {
        let mut first = Workspace::test_new("one");
        let first_root = first.tabs[0].root_pane;
        let first_second = first.test_split(Direction::Horizontal);
        first.tabs[0].layout.focus_pane(first_root);
        let second = Workspace::test_new("two");
        let second_root = second.tabs[0].root_pane;

        let mut state = AppState::test_new();
        state.workspaces = vec![first, second];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;
        set_agent_state(&mut state, 0, 0, first_root, AgentState::Idle);
        set_agent_state(&mut state, 0, 0, first_second, AgentState::Working);
        set_agent_state(&mut state, 1, 0, second_root, AgentState::Blocked);

        state.next_agent();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
        state.assert_invariants_for_test();
    }

    #[test]
    fn priority_sort_keeps_recently_changed_idle_agent_above_older_idle_agent() {
        let mut workspace = Workspace::test_new("one");
        let first = workspace.tabs[0].root_pane;
        let second = workspace.test_split(Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(first);

        let mut state = AppState::test_new();
        state.workspaces = vec![workspace];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        state.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        transition_agent_state(&mut state, first, AgentState::Idle);
        transition_agent_state(&mut state, second, AgentState::Working);
        assert_eq!(crate::ui::agent_panel_entries(&state)[0].pane_id, second);

        transition_agent_state(&mut state, second, AgentState::Idle);

        assert_eq!(crate::ui::agent_panel_entries(&state)[0].pane_id, second);
        state.assert_invariants_for_test();
    }

    #[test]
    fn previous_agent_keeps_wrapped_target_visible_in_agent_panel() {
        let mut workspace = Workspace::test_new("one");
        let root = workspace.tabs[0].root_pane;
        for idx in 1..8 {
            workspace.test_add_tab(Some(&format!("tab-{idx}")));
        }

        let mut state = AppState::test_new();
        state.workspaces = vec![workspace];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Terminal;
        for tab_idx in 0..state.workspaces[0].tabs.len() {
            let pane_id = state.workspaces[0].tabs[tab_idx].root_pane;
            mark_agent(&mut state, 0, tab_idx, pane_id);
        }
        state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 14));

        state.previous_agent();

        let last_idx = state.workspaces[0].tabs.len() - 1;
        assert_eq!(state.workspaces[0].active_tab, last_idx);
        assert!(state.agent_panel_scroll > 0);
        state.assert_invariants_for_test();
    }

    #[test]
    fn switch_workspace_updates_active_and_selected() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        state.switch_workspace(2);
        assert_eq!(state.active, Some(2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn last_pane_toggles_to_previous_focus_in_active_tab() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);

        state.focus_pane_in_workspace(0, root);
        state.focus_pane_in_workspace(0, right);
        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));

        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn removing_background_pane_preserves_last_pane_history() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        let background = state.workspaces[0].test_split(Direction::Horizontal);

        state.focus_pane_in_workspace(0, root);
        state.focus_pane_in_workspace(0, right);
        state.workspaces[0].remove_pane(background);
        state.last_pane();

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[test]
    fn last_pane_jumps_across_workspaces_and_tabs() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        let second_tab_root = state.workspaces[1].tabs[second_tab].root_pane;

        state.focus_pane_in_workspace(0, first_root);
        state.focus_pane_in_workspace(1, second_tab_root);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, 0);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].active_tab, second_tab);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_tab_root));
    }

    #[test]
    fn last_pane_tracks_tab_and_workspace_switches() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let first_second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        let first_second_root = state.workspaces[0].tabs[first_second_tab].root_pane;
        let second_root = state.workspaces[1].tabs[0].root_pane;

        state.switch_tab(first_second_tab);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, 0);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        state.switch_workspace(1);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].active_tab, first_second_tab);
        assert_eq!(
            state.workspaces[0].focused_pane_id(),
            Some(first_second_root)
        );

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_root));
    }

    #[test]
    fn last_pane_tracks_cross_workspace_tab_selection() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let first_root = state.workspaces[0].tabs[0].root_pane;
        let second_first_root = state.workspaces[1].tabs[0].root_pane;
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        let second_tab_root = state.workspaces[1].tabs[second_tab].root_pane;

        state.switch_workspace_tab(1, second_tab);
        state.last_pane();

        assert_eq!(state.active, Some(0));
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(first_root));

        state.last_pane();

        assert_eq!(state.active, Some(1));
        assert_eq!(state.workspaces[1].active_tab, second_tab);
        assert_eq!(state.workspaces[1].focused_pane_id(), Some(second_tab_root));
        assert_ne!(second_first_root, second_tab_root);
    }

    #[test]
    fn switch_workspace_keeps_selected_visible_in_scrolled_sidebar() {
        let mut state = app_with_workspaces(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 14));

        state.switch_workspace(7);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 80, 14));

        assert!(state
            .view
            .workspace_card_areas
            .iter()
            .any(|card| card.ws_idx == 7));
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
    fn move_workspace_reorders_without_changing_logical_selection() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);
        let active_id = state.workspaces[1].id.clone();
        let selected_id = state.workspaces[2].id.clone();
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 2);
        assert_eq!(state.workspaces[state.active.unwrap()].id, active_id);
        assert_eq!(state.workspaces[state.selected].id, selected_id);
    }

    #[test]
    fn move_workspace_accepts_insert_at_end() {
        let mut state = app_with_workspaces(&["a", "b", "c"]);

        state.move_workspace(0, state.workspaces.len());

        let names: Vec<_> = state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "c", "a"]);
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
    fn close_parent_worktree_workspace_closes_group() {
        let mut state = app_with_workspaces(&["main", "issue", "notes"]);
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        });
        state.workspaces[1].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        state.selected = 0;
        state.active = Some(0);

        state.close_selected_workspace();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "notes");
        assert_eq!(state.active, Some(0));
        assert_eq!(state.selected, 0);
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
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_died_last_workspace_enters_navigate() {
        let mut state = app_with_workspaces(&["only"]);
        state.mode = Mode::Terminal;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_pane_died(pane_id);

        assert!(state.workspaces.is_empty());
        assert_eq!(state.mode, Mode::Navigate);
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_died_multi_pane_keeps_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();

        state.handle_pane_died(second_id);

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].panes.len(), 1);
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_died_unknown_pane_is_noop() {
        let mut state = app_with_workspaces(&["test"]);
        let fake_id = PaneId::from_raw(9999);

        state.handle_pane_died(fake_id);

        assert_eq!(state.workspaces.len(), 1);
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_died_unrelated_pane_preserves_selection() {
        // Two workspaces; user is selecting text in workspace 0.
        // A pane in workspace 1 dies — selection must be preserved.
        let mut state = app_with_workspaces(&["active", "bg"]);
        let active_pane = *state.workspaces[0].panes.keys().next().unwrap();
        let bg_pane = *state.workspaces[1].panes.keys().next().unwrap();

        state.selection = Some(crate::selection::Selection::anchor(active_pane, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(bg_pane);

        assert!(state.selection.is_some());
        assert!(state.selection_autoscroll.is_some());
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_died_same_pane_clears_selection() {
        let mut state = app_with_workspaces(&["test"]);
        let first_id = state.workspaces[0].tabs[0].root_pane;
        let second_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();

        state.selection = Some(crate::selection::Selection::anchor(second_id, 0, 0, None));
        state.selection_autoscroll = Some(crate::app::state::SelectionAutoscroll {
            direction: crate::app::state::SelectionAutoscrollDirection::Down,
            last_mouse_screen_col: 0,
            last_mouse_screen_row: 23,
            inner_rect: ratatui::layout::Rect::new(0, 0, 80, 24),
        });

        state.handle_pane_died(second_id);

        // first_id still alive, workspace stays, but selection was on the dying pane
        assert!(state.selection.is_none());
        assert!(state.selection_autoscroll.is_none());
        assert_eq!(state.workspaces[0].panes.len(), 1);
        assert_eq!(state.workspaces[0].panes.keys().next().unwrap(), &first_id);
        state.assert_invariants_for_test();
    }

    #[test]
    fn state_changed_updates_pane() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
    }

    #[test]
    fn state_changed_idle_in_background_marks_unseen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        // First set it to Working
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        // Now transition to Idle while in background
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(!pane.seen);
        assert!(matches!(
            state.toast.as_ref().map(|toast| toast.kind),
            Some(ToastKind::Finished)
        ));
    }

    #[test]
    fn active_tab_completion_marks_pane_seen() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(true);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&terminal_id).unwrap().state = AgentState::Working;
        state.workspaces[0].panes.get_mut(&pane_id).unwrap().seen = false;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        let pane = state.workspaces[0].panes.get(&pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn initial_idle_in_background_stays_seen() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(pane.seen);
    }

    #[test]
    fn idle_after_known_unknown_agent_in_background_marks_done() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.active = Some(0);
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let pane = state.workspaces[1].panes.get(&bg_pane_id).unwrap();
        assert!(!pane.seen);
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
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Unknown, AgentState::Idle),
            None
        );
    }

    #[test]
    fn background_waiting_sets_attention_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn delayed_background_waiting_schedules_before_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
        assert!(state.pending_agent_notifications.contains_key(&bg_pane_id));

        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        let deliveries = state.drain_due_agent_notifications(deadline);
        assert_eq!(deliveries.len(), 1);

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2");
        assert!(state.pending_agent_notifications.is_empty());
    }

    #[test]
    fn delayed_background_waiting_cancels_when_agent_resumes_working() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.pending_agent_notifications.is_empty());
        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_background_waiting_is_suppressed_if_pane_becomes_active() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        state.active = Some(1);

        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_active_tab_unfocused_keeps_client_notification_available() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(false);
        state.toast_config.delivery = crate::config::ToastDelivery::System;
        state.toast_config.delay_seconds = 1;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        let deliveries = state.drain_due_agent_notifications(deadline);

        assert_eq!(deliveries.len(), 1);
        assert!(deliveries[0].toast.is_none());
        assert!(deliveries[0].client_notification.is_some());
        assert!(state.toast.is_none());
    }

    #[test]
    fn delayed_background_waiting_is_cleared_when_pane_dies() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.toast_config.delay_seconds = 1;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        let deadline = state.next_pending_agent_notification_deadline().unwrap();
        state.handle_app_event(AppEvent::PaneDied {
            pane_id: bg_pane_id,
        });

        assert!(state.pending_agent_notifications.is_empty());
        assert!(state.drain_due_agent_notifications(deadline).is_empty());
        assert!(state.toast.is_none());
    }

    #[test]
    fn hook_reported_unknown_agent_sets_toast_title_from_label() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::HookStateReported {
            pane_id: bg_pane_id,
            source: "custom:hermes".into(),
            agent_label: "hermes".into(),
            state: AgentState::Blocked,
            message: None,
            seq: None,
            session_ref: None,
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "hermes needs attention");
        assert_eq!(toast.context, "background · 2");
    }

    #[test]
    fn visible_blocker_overrides_hook_working_and_notifies() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id: bg_pane_id,
            source: "herdr:codex".into(),
            agent_label: "codex".into(),
            state: AgentState::Working,
            message: None,
            seq: Some(1),
            session_ref: None,
        });
        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Codex),
            state: AgentState::Blocked,
            visible_blocker: true,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&bg_terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Blocked);
        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "codex needs attention");
    }

    #[test]
    fn reserved_native_state_report_does_not_override_screen_state() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            state: AgentState::Blocked,
            message: None,
            seq: Some(1),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session"),
        });
        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.persisted_agent_session.is_some());

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(state.toast.is_none());
    }

    #[test]
    fn reserved_native_release_report_does_not_clear_screen_state() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Working,
            visible_blocker: false,
            visible_working: true,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookAgentReleased {
            pane_id,
            source: "herdr:claude".into(),
            agent_label: "claude".into(),
            known_agent: Some(Agent::Claude),
            seq: Some(1),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.detected_agent, Some(Agent::Claude));
    }

    #[test]
    fn devin_state_report_refreshes_session_without_overriding_screen_state() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .panes
            .get(&pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Devin),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
        state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "herdr:devin".into(),
            agent_label: "devin".into(),
            state: AgentState::Working,
            message: None,
            seq: Some(1),
            session_ref: crate::agent_resume::AgentSessionRef::id("devin-session"),
        });

        let terminal = state.terminals.get(&terminal_id).unwrap();
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn hidden_custom_session_ref_only_update_marks_session_dirty_without_visible_update() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let test_dir = std::env::current_dir().unwrap();
        let first_session = test_dir.join("one.jsonl").display().to_string();
        let second_session = test_dir.join("two.jsonl").display().to_string();

        let first_updates = state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "custom:pi".into(),
            agent_label: "pi".into(),
            state: AgentState::Working,
            message: None,
            seq: Some(20),
            session_ref: crate::agent_resume::AgentSessionRef::path(first_session),
        });
        assert_eq!(first_updates.len(), 1);
        state.session_dirty = false;

        let second_updates = state.handle_app_event(AppEvent::HookStateReported {
            pane_id,
            source: "custom:pi".into(),
            agent_label: "pi".into(),
            state: AgentState::Working,
            message: None,
            seq: Some(21),
            session_ref: crate::agent_resume::AgentSessionRef::path(second_session),
        });

        assert!(second_updates.is_empty());
        assert!(state.session_dirty);
    }

    #[test]
    fn releasing_an_agent_alias_marks_the_session_dirty() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_agent_name("reviewer".into());
        state.session_dirty = false;

        state.handle_app_event(AppEvent::HookAgentReleased {
            pane_id,
            source: "herdr:pi".into(),
            agent_label: "pi".into(),
            known_agent: Some(Agent::Pi),
            seq: Some(1),
        });

        assert!(state.terminals[&terminal_id].agent_name.is_none());
        assert!(state.session_dirty);
    }

    #[test]
    fn terminal_cwd_report_updates_terminal_cwd_and_marks_session_dirty() {
        let mut state = app_with_workspaces(&["active"]);
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();
        let terminal_id = state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let cwd =
            std::env::temp_dir().join(format!("herdr-cwd-report-test-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        state.session_dirty = false;

        let updates = state.handle_app_event(AppEvent::TerminalCwdReported {
            pane_id,
            cwd: cwd.clone(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.terminals.get(&terminal_id).unwrap().cwd, cwd);
        assert!(state.session_dirty);
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn background_idle_sets_finished_toast() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let bg_pane_id = *state.workspaces[1].panes.keys().next().unwrap();
        let bg_terminal_id = state.workspaces[1]
            .panes
            .get(&bg_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&bg_terminal_id).unwrap().state = AgentState::Working;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Droid),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::Finished);
        assert_eq!(toast.title, "droid finished");
        assert_eq!(toast.context, "background · 2");
        let target = toast.target.as_ref().expect("toast target");
        assert_eq!(&target.workspace_id, &state.workspaces[1].id);
        assert_eq!(target.pane_id, bg_pane_id);
    }

    #[test]
    fn background_toast_includes_tab_name_when_workspace_has_multiple_tabs() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[1].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[1].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[1].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "background · 2 · logs");
    }

    #[test]
    fn background_tab_in_active_workspace_still_sets_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.workspaces[0].tabs[0].set_custom_name("main".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        let bg_pane_id = state.workspaces[0].tabs[second_tab].root_pane;

        state.handle_app_event(AppEvent::StateChanged {
            pane_id: bg_pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        let toast = state.toast.as_ref().unwrap();
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert_eq!(toast.title, "pi needs attention");
        assert_eq!(toast.context, "active · 1 · logs");
    }

    #[test]
    fn active_workspace_active_tab_does_not_set_toast() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_workspace_active_tab_keeps_herdr_toast_suppressed_when_outer_terminal_is_unfocused() {
        let mut state = app_with_workspaces(&["active"]);
        state.active = Some(0);
        state.outer_terminal_focus = Some(false);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let pane_id = *state.workspaces[0].panes.keys().next().unwrap();

        state.handle_app_event(AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Pi),
            state: AgentState::Blocked,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });

        assert!(state.toast.is_none());
    }

    #[test]
    fn active_tab_suppression_preserves_unknown_focus_behavior() {
        assert!(active_tab_suppresses_notifications(true, None));
        assert!(active_tab_suppresses_notifications(true, Some(true)));
        assert!(!active_tab_suppresses_notifications(true, Some(false)));
        assert!(!active_tab_suppresses_notifications(false, None));
    }

    #[test]
    fn update_ready_sets_manual_update_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        let updates = state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "herdr update".into(),
        });

        assert!(updates.is_empty());
        assert_eq!(state.update_available.as_deref(), Some("0.5.0"));
        assert!(state.latest_release_notes_available);
        assert!(state.update_dismissed);
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(toast.kind, ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "v0.5.0 available");
        assert_eq!(
            toast.context,
            "detach, run `herdr update`, then follow its restart guidance"
        );
    }

    #[test]
    fn update_ready_uses_event_install_command_in_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;

        state.handle_app_event(AppEvent::UpdateReady {
            version: "0.5.0".into(),
            install_command: "brew update && brew upgrade herdr".into(),
        });

        assert_eq!(
            state.update_install_command,
            "brew update && brew upgrade herdr"
        );
        let toast = state.toast.as_ref().expect("update toast");
        assert_eq!(
            toast.context,
            "detach, run `brew update && brew upgrade herdr`, then restart this Herdr session when ready"
        );
    }

    #[test]
    fn agent_detection_manifest_update_event_updates_status_and_toast() {
        let mut state = AppState::test_new();
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        let status = crate::detect::manifest_update::ManifestUpdateStatus {
            last_result: Some("checked".to_string()),
            ..Default::default()
        };

        let updates = state.handle_app_event(AppEvent::AgentDetectionManifestsUpdated {
            updated: vec![crate::detect::manifest_update::ManifestUpdateCommit {
                agent: Agent::Codex,
                version: crate::detect::manifest_update::ManifestVersion::parse("2026.06.10.1")
                    .unwrap(),
            }],
            status,
        });

        assert!(updates.is_empty());
        assert_eq!(
            state.agent_manifest_update_status.last_result.as_deref(),
            Some("checked")
        );
        let toast = state.toast.as_ref().expect("manifest update toast");
        assert_eq!(toast.kind, ToastKind::UpdateInstalled);
        assert_eq!(toast.title, "Agent detection rules updated");
        assert_eq!(toast.context, "codex 2026.06.10.1");
    }

    #[test]
    fn toggle_zoom_works() {
        let mut state = app_with_workspaces(&["test"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        assert!(!state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(state.workspaces[0].zoomed);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn toggle_zoom_single_pane_noop() {
        let mut state = app_with_workspaces(&["test"]);
        state.toggle_zoom();
        assert!(!state.workspaces[0].zoomed);
    }

    #[test]
    fn navigate_pane_changes_focus_while_zoomed() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, root);

        state.navigate_pane(NavDirection::Right);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(right));
        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, right);
        assert!(state.view.pane_infos[0].inner_rect.x > state.view.pane_infos[0].rect.x);
    }

    #[test]
    fn swap_pane_direction_preserves_focus_and_swaps_layout_cells() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let before_root_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == root)
            .unwrap()
            .rect;
        let before_right_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == right)
            .unwrap()
            .rect;

        assert!(state.swap_pane(NavDirection::Right));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(
            state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == root)
                .unwrap()
                .rect,
            before_right_rect
        );
        assert_eq!(
            state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == right)
                .unwrap()
                .rect,
            before_root_rect
        );
    }

    #[test]
    fn swap_pane_direction_stays_zoomed_and_mutates_hidden_layout() {
        let mut state = app_with_workspaces(&["test"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let right = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        state.workspaces[0].zoomed = true;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.swap_pane(NavDirection::Right));
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));

        assert!(state.workspaces[0].zoomed);
        assert_eq!(state.workspaces[0].focused_pane_id(), Some(root));
        assert_eq!(state.view.pane_infos.len(), 1);
        assert_eq!(state.view.pane_infos[0].id, root);

        state.workspaces[0].zoomed = false;
        crate::ui::compute_view(&mut state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == root)
            .unwrap()
            .rect;
        let right_rect = state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == right)
            .unwrap()
            .rect;

        assert!(root_rect.x > right_rect.x);
    }

    #[test]
    fn close_pane_removes_from_workspace() {
        let mut state = app_with_workspaces(&["test"]);
        let closed = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        assert_eq!(state.workspaces[0].panes.len(), 2);
        state.plugin_panes.insert(
            closed,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
            },
        );
        insert_test_pane_graphics_state(&mut state, closed);

        state.close_pane();
        assert_eq!(state.workspaces[0].panes.len(), 1);
        assert!(!state.plugin_panes.contains_key(&closed));
        assert!(!state.pane_graphics_layers.contains_key(&closed));
        assert!(!state.pane_graphics_streams.contains_key(&closed));
        state.assert_invariants_for_test();
    }

    #[test]
    fn pane_process_exit_publish_marks_agent_idle_before_pane_removal() {
        let mut state = app_with_workspaces(&["active", "background"]);
        state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        state.active = Some(1);
        state.ensure_test_terminals();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Working);
        assert_eq!(
            state.terminals.get(&terminal_id).unwrap().state,
            AgentState::Working
        );

        let update = state
            .publish_pane_process_exit_if_agent(pane_id)
            .expect("process exit update");

        assert!(!state.pane_is_in_active_tab(update.ws_idx, pane_id));
        assert_eq!(update.previous_state, AgentState::Working);
        assert_eq!(update.state, AgentState::Idle);
        assert_eq!(update.agent_label.as_deref(), Some("pi"));
        assert_eq!(update.known_agent, Some(Agent::Pi));
        assert!(update.agent_released);
        assert_eq!(
            update.agent_release_status,
            Some(crate::api::schema::AgentStatus::Done)
        );
        assert!(matches!(
            state.toast.as_ref().map(|toast| toast.kind),
            Some(ToastKind::Finished)
        ));
    }

    #[test]
    fn close_pane_removes_unattached_terminal_state() {
        let mut state = app_with_workspaces(&["test"]);
        let pane_id = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();

        state.close_pane();

        assert!(!state.terminals.contains_key(&terminal_id));
        state.assert_invariants_for_test();
    }

    #[test]
    fn close_tab_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["test"]);
        let tab_idx = state.workspaces[0].test_add_tab(Some("logs"));
        state.ensure_test_terminals();
        state.workspaces[0].switch_tab(tab_idx);
        let pane_id = state.workspaces[0].tabs[tab_idx].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        state.plugin_panes.insert(
            pane_id,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
            },
        );
        insert_test_pane_graphics_state(&mut state, pane_id);

        state.close_tab();

        assert!(!state.terminals.contains_key(&terminal_id));
        assert!(!state.plugin_panes.contains_key(&pane_id));
        assert!(!state.pane_graphics_layers.contains_key(&pane_id));
        assert!(!state.pane_graphics_streams.contains_key(&pane_id));
        state.assert_invariants_for_test();
    }

    #[test]
    fn close_workspace_removes_unattached_terminal_states() {
        let mut state = app_with_workspaces(&["one", "two"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.terminal_id_for_pane(0, pane_id).unwrap();
        state.plugin_panes.insert(
            pane_id,
            crate::app::state::PluginPaneRecord {
                plugin_id: "example.pane".into(),
                entrypoint: "board".into(),
            },
        );
        insert_test_pane_graphics_state(&mut state, pane_id);

        state.close_selected_workspace();

        assert!(!state.terminals.contains_key(&terminal_id));
        assert!(!state.plugin_panes.contains_key(&pane_id));
        assert!(!state.pane_graphics_layers.contains_key(&pane_id));
        assert!(!state.pane_graphics_streams.contains_key(&pane_id));
        state.assert_invariants_for_test();
    }

    #[test]
    fn close_tab_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_tab();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
        state.assert_invariants_for_test();
    }

    #[test]
    fn close_pane_last_pane_closes_active_workspace_not_selected_workspace() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        let active_terminal_id = state
            .terminal_id_for_pane(1, state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        state.active = Some(1);
        state.selected = 0;

        state.close_pane();

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
        assert!(!state.terminals.contains_key(&active_terminal_id));
        state.assert_invariants_for_test();
    }

    #[test]
    fn close_pane_last_pane_in_parent_worktree_group_prompts() {
        let mut state = app_with_workspaces(&["parent", "child"]);
        mark_parent_worktree(&mut state, 0);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(0);
        state.selected = 1;

        let deferred = state.close_pane();

        assert!(deferred);
        assert_eq!(state.mode, Mode::ConfirmClose);
        assert_eq!(state.selected, 0);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn close_tab_in_linked_worktree_closes_workspace_only() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(1);
        state.selected = 0;

        state.close_tab();

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
    }

    #[test]
    fn close_tab_last_tab_in_parent_worktree_group_prompts() {
        let mut state = app_with_workspaces(&["parent", "child"]);
        mark_parent_worktree(&mut state, 0);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(0);
        state.selected = 1;

        let deferred = state.close_tab();

        assert!(deferred);
        assert_eq!(state.mode, Mode::ConfirmClose);
        assert_eq!(state.selected, 0);
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn close_pane_last_pane_in_linked_worktree_closes_workspace_only() {
        let mut state = app_with_workspaces(&["selected", "active"]);
        mark_linked_worktree(&mut state, 1);
        state.active = Some(1);
        state.selected = 0;

        state.close_pane();

        assert_eq!(state.request_remove_linked_worktree, None);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "selected");
    }

    #[test]
    fn close_pane_last_pane_in_parent_worktree_group_closes_when_confirmation_disabled() {
        let mut state = app_with_workspaces(&["parent", "child", "notes"]);
        mark_parent_worktree(&mut state, 0);
        mark_linked_worktree(&mut state, 1);
        state.confirm_close = false;
        state.active = Some(0);
        state.selected = 0;

        let deferred = state.close_pane();

        assert!(!deferred);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].display_name(), "notes");
    }
}
