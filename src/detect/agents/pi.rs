use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    // pi shows "Working..." when the agent is processing
    if content.contains("Working...") {
        return AgentState::Working;
    }
    AgentState::Idle
}
