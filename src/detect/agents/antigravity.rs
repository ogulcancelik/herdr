use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    let has_permission_request = lower.contains("requesting permission for:");
    let has_permission_question = lower.contains("do you want to proceed?");
    let has_permission_controls = lower.contains("tab amend") && lower.contains("edit command");
    if has_permission_request && (has_permission_question || has_permission_controls) {
        return AgentState::Blocked;
    }

    if has_antigravity_spinner(content) || has_antigravity_background_tasks(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_antigravity_spinner(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        let mut chars = trimmed.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !('\u{2800}'..='\u{28FF}').contains(&first) {
            return false;
        }

        let rest = chars
            .as_str()
            .trim_start_matches(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
            .trim_start();
        status_word_is_active(rest)
    })
}

fn has_antigravity_background_tasks(content: &str) -> bool {
    let bottom_lines: Vec<&str> = content
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(5)
        .collect();

    bottom_lines.into_iter().any(|line| {
        let line = line.trim().to_lowercase();
        line.contains("/tasks") && antigravity_task_count(&line).is_some_and(|count| count > 0)
    })
}

fn antigravity_task_count(line: &str) -> Option<u32> {
    for marker in [" task(s)", " tasks", " task"] {
        let Some((before, _)) = line.split_once(marker) else {
            continue;
        };
        let raw_count = before.split_whitespace().last()?.trim_matches(|c| c == '·');
        if let Ok(count) = raw_count.parse() {
            return Some(count);
        }
    }
    None
}

fn status_word_is_active(rest: &str) -> bool {
    let Some(word) = rest.split_whitespace().next() else {
        return false;
    };
    word.trim_end_matches(|c: char| !c.is_alphabetic())
        .to_ascii_lowercase()
        .ends_with("ing")
}
