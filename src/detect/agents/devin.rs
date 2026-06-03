use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    if has_visible_blocker(content) {
        return AgentState::Blocked;
    }

    if has_visible_working(content) {
        return AgentState::Working;
    }

    if has_idle_prompt(content) {
        return AgentState::Idle;
    }

    AgentState::Idle
}

pub(super) fn has_visible_blocker(content: &str) -> bool {
    let lower = content.to_lowercase();
    has_workspace_trust_prompt(&lower) || has_permission_prompt(&lower)
}

pub(super) fn has_idle_prompt(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("ask devin to build features, fix bugs, or work on")
        && (lower.contains("looking for plan mode? /plan") || lower.contains("context:"))
}

pub(super) fn has_visible_working(content: &str) -> bool {
    let lower = content.to_lowercase();
    !has_visible_blocker(content)
        && (lower.contains("guide devin while it works")
            || lower.contains("running tools") && lower.contains("esc to interrupt")
            || lower.contains("reading shell ") && lower.contains("timeout:"))
}

fn has_workspace_trust_prompt(lower_content: &str) -> bool {
    lower_content.contains("do you trust the authors of this directory?")
        && lower_content.contains("with untrusted content.")
        && lower_content.contains("yes, trust ")
}

fn has_permission_prompt(lower_content: &str) -> bool {
    lower_content.contains("approve once")
        && lower_content.contains("select")
        && lower_content.contains("confirm")
        && lower_content.contains("esc cancel")
}
