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

pub(super) fn has_visible_blocker(content: &str) -> bool {
    has_current_approval_panel(content) || has_question_panel(content)
}

pub(super) fn has_prompt_box(content: &str) -> bool {
    has_editor_prompt_box(content) && has_footer_context(content)
}

pub(super) fn has_visible_working(content: &str) -> bool {
    has_kimi_working_status(content)
}

fn has_kimi_blocked_prompt(content: &str) -> bool {
    if has_visible_blocker(content) {
        return true;
    }

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
        rest.starts_with("thinking...")
            || rest.starts_with("working...")
            || rest.starts_with("using ")
    })
}

fn has_current_approval_panel(content: &str) -> bool {
    let lower = content.to_lowercase();
    has_approval_title(&lower)
        && has_numeric_choose_hint(&lower)
        && lower.contains("↵ confirm")
        && (lower.contains("approve") || lower.contains("reject") || lower.contains("revise"))
}

fn has_approval_title(lower_content: &str) -> bool {
    lower_content.contains("run this command?")
        || lower_content.contains("write this file?")
        || lower_content.contains("apply these edits?")
        || lower_content.contains("stop this task?")
        || lower_content.contains("ready to build with this plan?")
        || lower_content.lines().any(|line| {
            let trimmed = line.trim_start_matches(|c: char| c == '▶' || c.is_whitespace());
            trimmed.starts_with("approve ") && trimmed.ends_with('?')
        })
}

fn has_question_panel(content: &str) -> bool {
    let lower = content.to_lowercase();
    content.lines().any(|line| line.trim() == "question")
        && content
            .lines()
            .any(|line| line.trim_start().starts_with("? "))
        && lower.contains("↑↓ select")
        && (lower.contains("↵ choose") || lower.contains("↵ toggle") || lower.contains("↵ save"))
        && lower.contains("esc cancel")
}

fn has_numeric_choose_hint(lower_content: &str) -> bool {
    lower_content.contains(" choose") && lower_content.contains('1') && lower_content.contains('2')
}

fn has_editor_prompt_box(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();

    for top_index in 0..lines.len() {
        if !is_editor_top_border(lines[top_index]) {
            continue;
        }

        let mut saw_prompt = false;
        for line in lines.iter().skip(top_index + 1) {
            if is_editor_bottom_border(line) {
                if saw_prompt {
                    return true;
                }
                break;
            }

            saw_prompt |= is_editor_prompt_line(line);
        }
    }

    false
}

fn is_editor_top_border(line: &str) -> bool {
    let trimmed = line.trim();
    ((trimmed.starts_with('╭') && trimmed.ends_with('╮'))
        || (trimmed.starts_with('├') && trimmed.ends_with('┤')))
        && trimmed.contains('─')
}

fn is_editor_bottom_border(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('╰') && trimmed.ends_with('╯') && trimmed.contains('─')
}

fn is_editor_prompt_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let inner = trimmed.strip_prefix('│').unwrap_or(trimmed).trim_start();
    inner.starts_with('>')
}

fn has_footer_context(content: &str) -> bool {
    content.to_lowercase().contains("context: ")
}
