use super::super::AgentState;

/// Amp (Sourcegraph) detection.
///
/// Blocked approval prompts use a shared footer with options like
/// "Approve", "Allow All for This Session", "Allow All for Every Session",
/// "Allow File for Every Session", and "Deny with feedback". The header varies
/// by approval type, for example "Invoke tool ...?", "Run this command?",
/// "Allow editing file:", or "Allow creating file:".
///
/// Working layout:
/// ```text
///   ✓ Search Map the core runtime architecture...
///   ⋯ Oracle ▼
///   ≈ Running tools...         Esc to cancel
/// ```
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    let has_waiting_for_approval = lower.contains("waiting for approval");
    let has_approval_header = lower.contains("invoke tool")
        || lower.contains("run this command?")
        || lower.contains("allow editing file:")
        || lower.contains("allow creating file:")
        || lower.contains("confirm tool call");
    let has_approval_actions = lower.contains("approve")
        && (lower.contains("allow all for this session")
            || lower.contains("allow all for every session")
            || lower.contains("allow file for every session")
            || lower.contains("deny with feedback"));

    if has_approval_actions && (has_waiting_for_approval || has_approval_header) {
        return AgentState::Blocked;
    }

    if lower.contains("esc to cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}
