use super::super::AgentState;

/// Kiro CLI detection.
///
/// Kiro exposes reliable working and idle terminal markers. Tool approval
/// prompts render with stable approval wording and an action menu.
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    if has_kiro_blocked_prompt(&lower) {
        return AgentState::Blocked;
    }

    if lower.contains("kiro is working")
        || (lower.contains("esc to cancel") && has_kiro_tool_spinner(content))
    {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_kiro_blocked_prompt(lower_content: &str) -> bool {
    has_tool_approval_prompt(lower_content) || has_subagent_approval_prompt(lower_content)
}

fn has_tool_approval_prompt(lower_content: &str) -> bool {
    let has_approval_request = lower_content.contains("requires approval");
    let has_approval_actions = lower_content.contains("yes, single permission")
        || lower_content.contains("trust, always allow")
        || lower_content.contains("no (tab to edit)")
        || lower_content.contains("esc to close");
    has_approval_request && has_approval_actions
}

fn has_subagent_approval_prompt(lower_content: &str) -> bool {
    let has_approval_request = (lower_content.contains("tool approval")
        || lower_content.contains("tool approvals"))
        && lower_content.contains("pending from subagents");
    let has_approval_actions = lower_content.contains("approve all pending")
        || lower_content.contains("configure individually")
        || lower_content.contains("exit (cancel subagents)");
    has_approval_request && has_approval_actions
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
