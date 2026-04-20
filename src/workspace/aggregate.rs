use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;

use super::{Tab, Workspace};

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub tab_label: String,
    pub label: String,
    pub agent_label: String,
    #[allow(dead_code)]
    pub agent: Option<Agent>,
    pub state: AgentState,
    pub seen: bool,
}

impl Tab {
    pub fn has_working_pane(&self) -> bool {
        self.panes
            .values()
            .any(|pane| pane.state == AgentState::Working)
    }

    pub fn pane_details(&self) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .filter_map(|id| {
                let pane = self.panes.get(id)?;
                let agent_label = pane.effective_agent_label()?.to_string();
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx: self.number.saturating_sub(1),
                    tab_label: self.display_name(),
                    label: agent_label.clone(),
                    agent_label,
                    agent: pane.effective_known_agent(),
                    state: pane.state,
                    seen: pane.seen,
                })
            })
            .collect()
    }
}

fn pane_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

impl Workspace {
    pub fn aggregate_state(&self) -> (AgentState, bool) {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .map(|pane| (pane.state, pane.seen))
            .max_by_key(|(state, seen)| pane_attention_priority(*state, *seen))
            .unwrap_or((AgentState::Unknown, true))
    }

    pub fn has_working_pane(&self) -> bool {
        self.tabs.iter().any(Tab::has_working_pane)
    }

    pub fn pane_details(&self) -> Vec<PaneDetail> {
        let multi_tab = self.tabs.len() > 1;
        self.tabs
            .iter()
            .flat_map(Tab::pane_details)
            .map(|mut detail| {
                if multi_tab {
                    detail.label = format!("{}·{}", detail.tab_label, detail.agent_label);
                }
                detail
            })
            .collect()
    }
}
