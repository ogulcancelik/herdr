use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    if content.to_lowercase().contains("esc interrupt") {
        return AgentState::Working;
    }

    super::opencode::detect(content)
}
