use super::super::{has_braille_spinner, AgentState};

/// Grok Build detection.
///
/// Blocked permission prompts display a whitelist scope selector with choices
/// like "Yes, proceed" and "No, reject". Working turns show a braille spinner
/// status line such as "⠋ Waiting… 1.8s" plus live controls like
/// "Ctrl+c:cancel" and "Ctrl+Enter:interject".
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    if lower.contains("use ← → to choose permission whitelist scope")
        || lower.contains("yes, proceed")
        || lower.contains("no, reject")
        || lower.contains("ctrl+o:yolo")
        || lower.contains(":scope")
    {
        return AgentState::Blocked;
    }

    if has_braille_spinner(content)
        && (lower.contains("waiting")
            || lower.contains("run ")
            || lower.contains("read ")
            || lower.contains("search ")
            || lower.contains("list "))
    {
        return AgentState::Working;
    }

    if lower.contains("ctrl+c:cancel") && lower.contains("ctrl+enter:interject") {
        return AgentState::Working;
    }

    AgentState::Idle
}
