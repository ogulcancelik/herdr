use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Blocked
    if has_cursor_blocked_prompt(content, &lower) {
        return AgentState::Blocked;
    }

    // Working
    if lower.contains("ctrl+c to stop") {
        return AgentState::Working;
    }
    if has_cursor_spinner(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_cursor_blocked_prompt(content: &str, lower: &str) -> bool {
    if lower.contains("waiting for approval") || lower.contains("run this command?") {
        return true;
    }

    if lower.contains("(y) (enter)")
        || lower.contains("keep (n)")
        || lower.contains("skip (esc or n)")
    {
        return true;
    }

    content.lines().any(|line| {
        let line = line.trim().to_lowercase();
        let has_yes_action = line.contains("(y)");
        has_yes_action
            && (line.contains("allow")
                || line.contains("run (once)")
                || line.contains("→ run")
                || line.starts_with("run "))
    })
}

/// Cursor status line: spinner glyphs followed by a live action label.
pub(in crate::detect) fn has_cursor_spinner(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        let rest = chars.as_str().trim_start();

        if matches!(first, '⬡' | '⬢') {
            return cursor_status_word_is_active(rest);
        }

        if ('\u{2800}'..='\u{28FF}').contains(&first) {
            let rest = rest.trim_start_matches(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
            return cursor_status_word_is_active(rest.trim_start());
        }

        false
    })
}

fn cursor_status_word_is_active(rest: &str) -> bool {
    let Some(word) = rest.split_whitespace().next() else {
        return false;
    };
    word.trim_end_matches(|c: char| !c.is_alphabetic())
        .to_ascii_lowercase()
        .ends_with("ing")
}
