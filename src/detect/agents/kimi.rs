use super::super::AgentState;

pub(super) fn detect(content: &str) -> AgentState {
    if has_kimi_blocked_prompt(content) {
        return AgentState::Blocked;
    }

    if has_kimi_working_status(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

fn has_kimi_blocked_prompt(content: &str) -> bool {
    let lower = content.to_lowercase();
    lower.contains("requesting approval")
        && (lower.contains("approve once") || lower.contains("approve for this session"))
        && lower.contains("reject")
        && (lower.contains("1/2/3/4 choose") || lower.contains("↵ confirm"))
}

fn has_kimi_working_status(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        if matches!(
            trimmed,
            "🌕" | "🌖" | "🌗" | "🌘" | "🌑" | "🌒" | "🌓" | "🌔"
        ) {
            return true;
        }

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
            .trim_start()
            .to_lowercase();
        rest.starts_with("thinking...") || rest.starts_with("using ")
    })
}
