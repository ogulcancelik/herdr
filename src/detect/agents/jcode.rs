use super::super::AgentState;

/// Jcode detection.
///
/// Working: spinner activity, tool execution, embedding indicators
/// Blocked: approval prompts, session waiting
/// Idle: prompt box visible, no active indicators
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked: approval/wait patterns specific to jcode
    // Session waiting or manual input required
    if lower.contains("waiting for session") || lower.contains("awaiting input") {
        return AgentState::Blocked;
    }
    // Approval or confirmation prompts
    if lower.contains("approve?") || lower.contains("confirm action") {
        return AgentState::Blocked;
    }

    // Working: active spinner or processing indicators
    // Braille spinner characters at line start are common in agent TUIs
    if super::super::has_braille_spinner(content) {
        return AgentState::Working;
    }
    // Processing indicators
    if lower.contains("processing") || lower.contains("embedding") {
        return AgentState::Working;
    }
    // Tool execution indicators
    if lower.contains("running tool") || lower.contains("executing") {
        return AgentState::Working;
    }
    // Session activity markers
    if lower.contains("session uuid:") || lower.contains("context:") {
        // These appear during active sessions
        if lower.contains("tokens") || lower.contains("working") {
            return AgentState::Working;
        }
    }

    // Idle: session ready marker or prompt
    if lower.contains("session ready") || lower.contains("ready for input") {
        return AgentState::Idle;
    }
    // Idle prompt box patterns
    if lower.contains("❯") && !lower.contains("processing") {
        return AgentState::Idle;
    }

    // Default to Unknown for unrecognized content
    // This keeps jcode from incorrectly matching other agents
    AgentState::Unknown
}
