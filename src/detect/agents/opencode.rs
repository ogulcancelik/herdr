use super::super::{has_interrupt_pattern, AgentState};

pub(super) fn detect(content: &str) -> AgentState {
    // Blocked
    if content.contains("△ Permission required") || has_opencode_question_prompt(content) {
        return AgentState::Blocked;
    }

    // Working
    if has_interrupt_pattern(&content.to_lowercase()) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_opencode_question_prompt(content: &str) -> bool {
    let lower = content.to_lowercase();
    let has_enter_action = lower.contains("enter confirm")
        || lower.contains("enter submit")
        || lower.contains("enter toggle");
    let has_question_nav = content.contains("↑↓ select") || content.contains("⇆ tab");

    lower.contains("esc dismiss") && has_enter_action && has_question_nav
}
