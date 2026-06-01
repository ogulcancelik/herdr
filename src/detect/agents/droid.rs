use super::super::{has_braille_spinner, AgentState};

/// Droid detection.
///
/// Working: braille spinner line (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) + "Thinking..." + "(Press ESC to stop)"
/// Blocked: EXECUTE prompt with selection box ("Yes, allow" / "No, cancel") +
///          "Use ↑↓ to navigate, Enter to select"
/// Idle: prompt box visible, no spinner, no selection prompt
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked: EXECUTE approval prompt with selection UI chrome
    // Primary (AND): structural keyword + chrome text = certain
    let has_execute = content.contains("EXECUTE");
    let has_selection_chrome = lower.contains("enter to select")
        || lower.contains("↑↓ to navigate")
        || lower.contains("esc to cancel");
    let has_selection_options = lower.contains("> yes, allow") || lower.contains("> no, cancel");

    if has_execute && (has_selection_chrome || has_selection_options) {
        return AgentState::Blocked;
    }
    // Secondary: selection chrome + options together (no EXECUTE needed)
    if has_selection_chrome && has_selection_options {
        return AgentState::Blocked;
    }

    // Working: braille spinner character at start of a line + "Thinking..."
    // The braille chars (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏) are very specific — won't appear in normal content
    if has_braille_spinner(content) && lower.contains("esc to stop") {
        return AgentState::Working;
    }
    // Fallback: "ESC to stop" alone is still a strong signal (it's UI chrome)
    if lower.contains("esc to stop") {
        return AgentState::Working;
    }

    AgentState::Idle
}
