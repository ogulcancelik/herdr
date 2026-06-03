use super::super::{has_confirmation_prompt, has_interrupt_pattern, AgentState};

pub(super) fn detect(content: &str) -> AgentState {
    let lower = content.to_lowercase();

    // Strong blocked patterns are structural Codex UI chrome, so they can win
    // even when the prompt region is visible.
    if has_codex_strong_blocked_prompt(&lower) {
        return AgentState::Blocked;
    }

    // Working
    if has_codex_working_status_at_current_prompt(content) {
        return AgentState::Working;
    }

    // Idle
    if has_codex_current_prompt(content) {
        return AgentState::Idle;
    }

    // Weak blocked patterns are too broad to scan above a visible prompt. They
    // only apply when Codex does not currently show an idle prompt region.
    if has_codex_weak_blocked_prompt(&lower) {
        return AgentState::Blocked;
    }

    // Fallback working signals for narrow captures where the footer scrolled
    // out or the working row is the only Codex chrome visible.
    if has_interrupt_pattern(&lower) || has_codex_working_header(content) {
        return AgentState::Working;
    }

    AgentState::Idle
}

pub(super) fn has_visible_blocker(content: &str) -> bool {
    let lower = content.to_lowercase();
    has_codex_strong_blocked_prompt(&lower)
}

pub(super) fn has_prompt(content: &str) -> bool {
    has_codex_current_prompt(content) || content.lines().any(codex_prompt_line)
}

pub(super) fn has_visible_working(content: &str) -> bool {
    has_codex_live_working_at_current_prompt(content)
        || (!has_codex_current_prompt(content) && has_codex_visible_working_without_prompt(content))
}

pub(super) fn is_transcript_viewer(content: &str) -> bool {
    let bottom_lines = bottom_non_empty_lines(content, 3);
    let Some(last_line) = bottom_lines.last() else {
        return false;
    };
    let bottom_text = normalize_lines(&bottom_lines);

    bottom_text.contains("↑/↓ to scroll")
        && bottom_text.contains("pgup/pgdn to page")
        && bottom_text.contains("home/end to jump")
        && bottom_text.contains("q to quit")
        && has_codex_edit_prev_controls(&bottom_text)
        && transcript_control_tail(last_line)
}

fn has_codex_edit_prev_controls(bottom_text: &str) -> bool {
    bottom_text.contains("esc to edit prev") || bottom_text.contains("esc/← to edit prev")
}

fn has_codex_visible_working_without_prompt(content: &str) -> bool {
    let mut recent_lines = content.lines().rev().filter(|line| !line.trim().is_empty());
    let Some(last_line) = recent_lines.next() else {
        return false;
    };

    if codex_live_working_line(last_line) {
        return true;
    }

    codex_status_detail_line(last_line)
        && recent_lines
            .take(4)
            .find(|line| codex_block_marker_line(line))
            .is_some_and(codex_live_working_line)
}

fn has_codex_strong_blocked_prompt(lower_content: &str) -> bool {
    lower_content.contains("press enter to confirm or esc to cancel")
        || lower_content.contains("enter to submit answer")
        || lower_content.contains("enter to submit all")
        || lower_content.contains("allow command?")
}

fn has_codex_weak_blocked_prompt(lower_content: &str) -> bool {
    lower_content.contains("[y/n]")
        || lower_content.contains("yes (y)")
        || has_confirmation_prompt(lower_content)
}

fn has_codex_live_working_at_current_prompt(content: &str) -> bool {
    codex_last_block_marker_before_current_prompt(content).is_some_and(codex_live_working_line)
}

fn has_codex_working_status_at_current_prompt(content: &str) -> bool {
    codex_last_block_marker_before_current_prompt(content).is_some_and(codex_working_status_line)
}

fn codex_last_block_marker_before_current_prompt(content: &str) -> Option<&str> {
    let (lines, prompt_index) = codex_current_prompt_region(content)?;

    lines[..prompt_index]
        .iter()
        .rev()
        .find(|line| codex_block_marker_line(line))
        .copied()
}

fn has_codex_working_header(content: &str) -> bool {
    content.lines().any(codex_working_status_line)
}

fn codex_live_working_line(line: &str) -> bool {
    if codex_queued_input_header_line(line) {
        return true;
    }

    let trimmed = line.trim_start();
    let lower = trimmed.to_lowercase();
    codex_working_status_line(line)
        && (trimmed.contains("Waiting for background terminal")
            || lower.contains("esc to interrupt")
            || lower.contains("esc…")
            || lower.contains("background terminal running")
            || lower.contains("/ps to view")
            || lower.contains("/stop to close"))
}

fn codex_working_status_line(line: &str) -> bool {
    if codex_queued_input_header_line(line) {
        return true;
    }

    let trimmed = line.trim_start();
    let lower = trimmed.to_lowercase();
    trimmed.starts_with('•')
        && (trimmed.contains("Working (")
            || trimmed.contains("Waiting for background terminal (")
            || lower.contains("reviewing approval request (")
            || (lower.contains("reviewing ") && lower.contains(" approval requests ("))
            || trimmed.contains("Booting MCP server:"))
}

fn has_codex_current_prompt(content: &str) -> bool {
    codex_current_prompt_region(content).is_some()
}

fn codex_current_prompt_region(content: &str) -> Option<(Vec<&str>, usize)> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;

    if lines[prompt_index + 1..]
        .iter()
        .any(|line| codex_block_marker_line(line))
    {
        return None;
    }

    Some((lines, prompt_index))
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn codex_status_detail_line(line: &str) -> bool {
    line.trim_start().starts_with('└')
}

fn codex_queued_input_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('•') {
        return false;
    }

    let lower = trimmed.to_lowercase();
    lower.starts_with("• queued follow-up inputs")
        || lower.starts_with("• messages to be submitted after next tool call")
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
    lower.contains("q to quit")
        || lower.contains("esc to edit")
        || lower.contains("esc/← to edit")
        || lower.contains("edit message")
}
