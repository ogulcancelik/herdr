use super::super::{has_confirmation_prompt, AgentState};

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked — explicit confirmation
    if lower.contains("waiting for user confirmation") {
        return AgentState::Blocked;
    }

    // Blocked — box-drawing confirmation prompts
    if content.contains("│ Apply this change")
        || content.contains("│ Allow execution")
        || content.contains("│ Do you want to proceed")
    {
        return AgentState::Blocked;
    }
    if has_confirmation_prompt(&lower) {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("esc to cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}
