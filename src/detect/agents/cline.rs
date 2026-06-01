use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if lower.contains("let cline use this tool") {
        return AgentState::Blocked;
    }
    // [act mode] or [plan mode] followed by "yes"
    if (lower.contains("[act mode]") || lower.contains("[plan mode]")) && lower.contains("yes") {
        return AgentState::Blocked;
    }

    // Idle
    if lower.contains("cline is ready for your message") {
        return AgentState::Idle;
    }

    // Cline defaults to working (unlike most agents that default to idle)
    AgentState::Working
}
