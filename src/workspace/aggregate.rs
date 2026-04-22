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

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use super::*;
    use crate::detect::Agent;

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        ws.tabs[0].panes.get_mut(&root_id).unwrap().state = AgentState::Idle;
        ws.tabs[0].panes.get_mut(&id2).unwrap().state = AgentState::Working;

        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.state = AgentState::Idle;
        root.seen = false;
        ws.tabs[0].panes.get_mut(&id2).unwrap().state = AgentState::Working;

        let (state, seen) = ws.aggregate_state();
        assert_eq!(state, AgentState::Idle);
        assert!(!seen);
    }

    #[test]
    fn pane_details_hide_plain_shells() {
        let mut ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0].panes.get_mut(&root_pane).unwrap().detected_agent = Some(Agent::Pi);
        ws.test_split(Direction::Horizontal);

        let details = ws.pane_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].label, "pi");
    }

    #[test]
    fn pane_details_include_tab_context_when_workspace_has_multiple_tabs() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("main".into());
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0].panes.get_mut(&root_pane).unwrap().detected_agent = Some(Agent::Pi);

        let tab_idx = ws.test_add_tab(Some("logs"));
        let second_root_pane = ws.tabs[tab_idx].root_pane;
        ws.tabs[tab_idx]
            .panes
            .get_mut(&second_root_pane)
            .unwrap()
            .detected_agent = Some(Agent::Claude);

        let details = ws.pane_details();
        assert_eq!(details.len(), 2);
        assert!(details.iter().any(|detail| detail.label == "main·pi"));
        assert!(details.iter().any(|detail| detail.label == "logs·claude"));
    }

    #[test]
    fn pane_details_include_hook_reported_unknown_agents() {
        let mut ws = Workspace::test_new("test");
        let root_pane = ws.tabs[0].root_pane;
        ws.tabs[0]
            .panes
            .get_mut(&root_pane)
            .unwrap()
            .set_hook_authority(
                "custom:hermes".into(),
                "hermes".into(),
                AgentState::Working,
                None,
            );

        let details = ws.pane_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].agent_label, "hermes");
        assert_eq!(details[0].agent, None);
    }
}
