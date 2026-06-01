use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("esc to cancel")
        && (lower.contains("enter to select")
            || lower.contains("enter to confirm")
            || lower.contains("enter to submit"))
    {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("esc to cancel")
        || lower.contains("esc cancel")
        || lower.contains("esc again to cancel")
    {
        return AgentState::Working;
    }

    AgentState::Idle
}
