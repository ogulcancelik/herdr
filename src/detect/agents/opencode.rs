use super::super::{has_interrupt_pattern, AgentState};

pub(super) fn detect(content: &str) -> AgentState {
    // Blocked
    if content.contains("△ Permission required") || has_opencode_question_prompt(content) {
        return AgentState::Blocked;
    }

    // Working
    if has_interrupt_pattern(&content.to_lowercase())
        || has_opencode_interrupt_footer(content)
        || has_opencode_progress_run(content)
    {
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

fn has_opencode_interrupt_footer(content: &str) -> bool {
    content.lines().any(|line| {
        let lower = line.to_lowercase();
        if !(lower.contains("esc interrupt") || lower.contains("esc again to interrupt")) {
            return false;
        }

        lower.contains("opencode")
    })
}

fn has_opencode_progress_run(line: &str) -> bool {
    let mut run = 0usize;
    for ch in line.chars() {
        if matches!(ch, '■' | '⬝') {
            run += 1;
            if run >= 4 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}
