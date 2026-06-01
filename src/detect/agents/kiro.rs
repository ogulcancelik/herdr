use super::super::AgentState;

/// Kiro CLI detection.
///
/// Kiro exposes reliable working and idle terminal markers. Tool approval
/// prompts render with a stable "requires approval" line and an action menu.
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    let has_approval_request = lower.contains("requires approval");
    let has_approval_actions = lower.contains("yes, single permission")
        || lower.contains("trust, always allow")
        || lower.contains("no (tab to edit)")
        || lower.contains("esc to close");
    if has_approval_request && has_approval_actions {
        return AgentState::Blocked;
    }

    if lower.contains("kiro is working")
        || (lower.contains("esc to cancel") && has_kiro_tool_spinner(content))
    {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_kiro_tool_spinner(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !matches!(first, '◔' | '◑' | '◕' | '●') {
            return false;
        }
        let rest = chars.as_str().trim_start();
        rest.chars().next().is_some_and(char::is_alphabetic)
    })
}
