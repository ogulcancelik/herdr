use super::super::{has_confirmation_prompt, has_selection_prompt, AgentState};

/// Claude Code detection. The most complex — it has a structured prompt box UI.
///
/// Screen layout:
/// ```text
///   (agent output / tool results)
///   ───────────────────────── (top border)
///   ❯ _                      (prompt line)
///   ───────────────────────── (bottom border)
/// ```
pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Search prompt is always idle
    if content.contains("⌕ Search…") {
        return AgentState::Idle;
    }

    // ctrl+r toggle — don't change state
    // (we return Idle as a safe default since we don't have previous state here)
    if lower.contains("ctrl+r to toggle") {
        return AgentState::Idle;
    }

    if has_live_blocked_form(content) {
        return AgentState::Blocked;
    }

    if has_working_chrome(content) {
        return AgentState::Working;
    }

    if !has_prompt_box(content) && has_claude_blocked_prompt(content, &lower) {
        return AgentState::Blocked;
    }

    if has_prompt_box(content) {
        return AgentState::Idle;
    }

    AgentState::Idle
}

pub(super) fn has_visible_blocker(content: &str) -> bool {
    let lower = content.to_lowercase();
    has_live_blocked_form(content)
        || lower.contains("do you want to proceed?")
            && has_claude_yes_no_choice(content)
            && (lower.contains("bash command")
                || lower.contains("bash(")
                || lower.contains("contains expansion")
                || lower.contains("tab to amend")
                || lower.contains("ctrl+e to explain"))
}

pub(super) fn has_working_chrome(content: &str) -> bool {
    let above = content_above_prompt_box(content);
    let above_lower = above.to_lowercase();
    above_lower.contains("esc to interrupt")
        || above_lower.contains("ctrl+c to interrupt")
        || has_running_status_line(above)
        || has_spinner_activity(above)
}

pub(super) fn is_transcript_viewer(content: &str) -> bool {
    let bottom_lines = bottom_non_empty_lines(content, 3);
    let Some(last_line) = bottom_lines.last() else {
        return false;
    };
    let bottom_text = normalize_lines(&bottom_lines);

    bottom_text.contains("showing detailed transcript")
        && bottom_text.contains("ctrl+o to toggle")
        && (bottom_text.contains("ctrl+e to show all")
            || bottom_text.contains("ctrl+e to collapse"))
        && transcript_control_tail(last_line)
}

pub(super) fn has_prompt_box(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top_border_index) = claude_prompt_box_top_border_index(&lines) else {
        return false;
    };

    lines[top_border_index + 1..]
        .iter()
        .take_while(|line| !is_horizontal_rule(line))
        .any(|line| line.trim_start().starts_with('❯'))
}

/// Claude uses the same generic Select and Dialog widgets for both
/// permission flows and ordinary slash/settings menus. Match only the
/// permission and interview prompts that actually need user input.
fn has_claude_blocked_prompt(content: &str, lower_content: &str) -> bool {
    has_confirmation_prompt(lower_content)
        || lower_content.contains("do you want to proceed?")
        || lower_content.contains("would you like to proceed?")
        || lower_content.contains("waiting for permission")
        || lower_content.contains("do you want to allow this connection?")
        || lower_content.contains("tab to amend")
        || lower_content.contains("ctrl+e to explain")
        || lower_content.contains("review your answers")
        || lower_content.contains("skip interview and plan immediately")
        || (has_selection_prompt(content) && has_claude_yes_no_choice(content))
}

fn has_live_blocked_form(content: &str) -> bool {
    let region = content_after_last_horizontal_rule(content);
    region.lines().any(|line| {
        let lower = line.to_lowercase();
        lower.contains("enter to select")
            && lower.contains("esc to cancel")
            && (lower.contains("tab/arrow keys to navigate")
                || lower.contains("arrow keys to navigate")
                || lower.contains("arrows to navigate")
                || lower.contains("↑/↓ to navigate")
                || lower.contains("↑↓ to navigate"))
    })
}

fn has_running_status_line(content_above_prompt: &str) -> bool {
    let Some(line) = content_above_prompt
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
    else {
        return false;
    };

    is_background_agent_wait_line(line) || is_still_running_status_line(line)
}

fn is_background_agent_wait_line(line: &str) -> bool {
    let mut text = line.trim();
    if !text.starts_with("Waiting for ") && !text.starts_with("waiting for ") {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if first.is_alphanumeric() {
            return false;
        }
        text = chars.as_str().trim_start();
    }

    let lower = text.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("waiting for ") else {
        return false;
    };
    let Some((count, rest)) = rest.split_once(' ') else {
        return false;
    };
    if count.parse::<u32>().ok().is_none_or(|count| count == 0) {
        return false;
    }

    rest == "background agent to finish" || rest == "background agents to finish"
}

fn is_still_running_status_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    for (index, word) in words.iter().enumerate() {
        let Ok(count) = word.parse::<u32>() else {
            continue;
        };
        if count == 0 {
            continue;
        }

        if matches!(
            words.get(index + 1..index + 4),
            Some(["shell" | "shells", "still", "running"])
        ) {
            return true;
        }

        if matches!(
            words.get(index + 1..index + 5),
            Some(["local", "agent" | "agents", "still", "running"])
        ) {
            return true;
        }
    }

    false
}

fn has_claude_yes_no_choice(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line
            .trim()
            .trim_start_matches('❯')
            .trim_start()
            .to_lowercase();
        trimmed == "yes"
            || trimmed == "no"
            || trimmed.starts_with("1. yes")
            || trimmed.starts_with("2. no")
            || trimmed.starts_with("yes, and ")
            || trimmed.starts_with("no, and tell claude")
    })
}

/// Claude Code spinner characters + activity label.
/// The verb changes frequently ("Processing…", "Pouncing…", etc.), so rely
/// on the spinner glyph + trailing ellipsis rather than specific wording.
/// Include Claude's narrow-pane middle-dot frame too.
pub(in crate::detect) fn has_spinner_activity(content: &str) -> bool {
    spinner_activity_text(content).is_some()
}

/// The free-text activity label from Claude's spinner line, e.g.
/// "✶ Implementing the parser… (esc to interrupt)" -> "Implementing the parser".
pub(in crate::detect) fn spinner_activity_text(content: &str) -> Option<String> {
    const SPINNER_CHARS: &str = "·✱✲✳✴✵✶✷✸✹✺✻✼✽✾✿❀❁❂❃❇❈❉❊❋✢✣✤✥✦✧✨⊛⊕⊙◉◎◍⁂⁕※⍟☼★☆";
    for line in content.lines() {
        let trimmed = line.trim();
        let mut chars = trimmed.chars();
        if let Some(first) = chars.next() {
            if SPINNER_CHARS.contains(first) {
                let rest: String = chars.collect();
                if rest.starts_with(' ') && rest.contains('\u{2026}') {
                    let text = rest
                        .split('\u{2026}')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if text.chars().any(|c| c.is_alphanumeric()) {
                        return Some(text);
                    }
                }
            }
        }
    }
    None
}

/// Public seam for detect::detect_agent: activity text relative to the live
/// prompt area (ignores transcript scrollback above the prompt box).
pub(in crate::detect) fn live_activity_text(content: &str) -> Option<String> {
    spinner_activity_text(content_above_prompt_box(content))
}

/// Extract content above Claude's prompt box.
/// The prompt box is two ─── border lines with ❯ between them.
pub(in crate::detect) fn content_above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();

    if let Some(i) = claude_prompt_box_top_border_index(&lines) {
        let byte_offset: usize = lines[..i].iter().map(|l| l.len() + 1).sum();
        return &content[..byte_offset.min(content.len())];
    }

    // No prompt box found, return all content
    content
}

fn content_after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }

    &content[last_rule_end..]
}

fn claude_prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;

    for i in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[i]) {
            border_count += 1;
            if border_count == 2 {
                return Some(i);
            }
        }
    }

    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let rule_chars = trimmed.chars().take_while(|&c| c == '─').count();
    if rule_chars == 0 {
        return false;
    }

    let rule_bytes = trimmed
        .char_indices()
        .nth(rule_chars)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();

    suffix.is_empty() || rule_chars >= 3
}

fn bottom_non_empty_lines(content: &str, max_lines: usize) -> Vec<&str> {
    let mut lines: Vec<&str> = content
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .collect();
    lines.reverse();
    lines
}

fn normalize_lines(lines: &[&str]) -> String {
    lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn transcript_control_tail(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("ctrl+e")
        || lower.contains("show all")
        || lower.contains("collapse")
        || lower.contains("verbose")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_box_below(content_above_prompt: &str) -> String {
        format!(
            "{content_above_prompt}\n────────────────────────────────\n❯ \n────────────────────────────────\n"
        )
    }

    #[test]
    fn shell_still_running_status_line_is_working() {
        let content = prompt_box_below(
            "● Started. I'll tell you when it finishes.\n\n✻ Crunched for 7s · 1 shell still running",
        );

        assert_eq!(detect(&content), AgentState::Working);
        assert!(has_working_chrome(&content));
    }

    #[test]
    fn local_agent_still_running_status_line_is_working() {
        let content = prompt_box_below(
            "● Hey. What do you want to work on?\n\n✻ Worked for 4s · 2 local agents still running",
        );

        assert_eq!(detect(&content), AgentState::Working);
        assert!(has_working_chrome(&content));
    }

    #[test]
    fn lower_agent_picker_shell_count_is_not_working_chrome() {
        let content = prompt_box_below("  ~/P/herdr ⎇ master ▱▱▱▱▱ 0%\n  1 shell · ← for agents");

        assert_eq!(detect(&content), AgentState::Idle);
        assert!(!has_working_chrome(&content));
    }

    #[test]
    fn stale_shell_running_line_above_newer_output_is_not_working_chrome() {
        let content = prompt_box_below(
            "● Started. I'll tell you when it finishes.\n\n✻ Crunched for 7s · 1 shell still running\n\n● hi",
        );

        assert_eq!(detect(&content), AgentState::Idle);
        assert!(!has_working_chrome(&content));
    }
}
