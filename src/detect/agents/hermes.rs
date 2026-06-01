use super::super::AgentState;

/// Hermes Agent detection.
///
/// Hermes shows a bottom status bar while turns are active and modal approval
/// dialogs for dangerous terminal commands. Prefer the modal controls for
/// blocked detection, then the live interrupt/status controls for working.
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    let has_approval_options = lower.contains("allow once")
        && lower.contains("allow for this session")
        && lower.contains("deny");
    let has_approval_controls = lower.contains("enter to confirm")
        || lower.contains("↑/↓ to select")
        || lower.contains("show full command");
    if (lower.contains("dangerous command") || has_approval_options) && has_approval_controls {
        return AgentState::Blocked;
    }

    if lower.contains("msg=interrupt") || lower.contains("ctrl+c cancel") {
        return AgentState::Working;
    }

    AgentState::Idle
}
