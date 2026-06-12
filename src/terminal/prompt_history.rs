//! Per-pane prompt + recap history.
//!
//! Today the pane header keeps only the LAST prompt; this module backs the
//! upgrade (issue #96): every `pane.report_prompt` APPENDS a timestamped
//! entry, and `pane.report_recap` appends a visually distinct recap entry
//! from a session's Stop hook. The pane header keeps the collapsed (latest)
//! prompt unchanged; the expanded view becomes a bounded scrollable panel.
//!
//! Ephemeral by design — never persisted into session snapshots.

use std::time::{Duration, Instant};

/// Hard cap on history per pane, measured in rendered lines (not entries).
/// Drop oldest WHOLE entries until the total fits. Sized for ~1000 lines:
/// generous for a long session, small enough to bound render cost.
pub const MAX_PROMPT_HISTORY_LINES: usize = 1000;

/// What kind of entry this is. Recaps render visually distinct from prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptHistoryKind {
    Prompt,
    Recap,
}

/// One history entry. `text` is already sanitized when stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptHistoryEntry {
    pub kind: PromptHistoryKind,
    pub text: String,
    pub recorded_at: Instant,
}

impl PromptHistoryEntry {
    /// Number of rendered lines this entry will occupy, counting one row per
    /// non-empty logical line plus a chrome line for the relative timestamp
    /// header. Empty leading/trailing blank lines are trimmed.
    pub fn rendered_line_count(&self) -> usize {
        // Always at least 1 (the chrome line). Logical text adds one row per
        // non-empty line; an empty body still gets the chrome.
        1 + trimmed_logical_lines(&self.text).len()
    }

    /// Compact "12s ago"-style age string.
    pub fn relative_age(&self, now: Instant) -> String {
        let elapsed = now.saturating_duration_since(self.recorded_at);
        relative_age_for_duration(elapsed)
    }
}

fn trimmed_logical_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty())
        .collect();
    let mut lines = lines;
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn relative_age_for_duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Append `entry`, then drop oldest WHOLE entries until the total rendered
/// line count fits within [`MAX_PROMPT_HISTORY_LINES`]. A single new entry
/// larger than the cap is still kept (it would not fit alone — but losing it
/// is worse than the temporary overflow), matching the "drop oldest whole"
/// invariant.
pub fn append_with_cap(history: &mut Vec<PromptHistoryEntry>, entry: PromptHistoryEntry) {
    history.push(entry);
    let mut total: usize = history
        .iter()
        .map(PromptHistoryEntry::rendered_line_count)
        .sum();
    while total > MAX_PROMPT_HISTORY_LINES && history.len() > 1 {
        let removed = history.remove(0);
        total = total.saturating_sub(removed.rendered_line_count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: PromptHistoryKind, text: &str) -> PromptHistoryEntry {
        PromptHistoryEntry {
            kind,
            text: text.to_string(),
            recorded_at: Instant::now(),
        }
    }

    #[test]
    fn append_below_cap_keeps_everything() {
        let mut history = Vec::new();
        append_with_cap(&mut history, entry(PromptHistoryKind::Prompt, "fix bug"));
        append_with_cap(&mut history, entry(PromptHistoryKind::Recap, "did it"));
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].kind, PromptHistoryKind::Prompt);
        assert_eq!(history[1].kind, PromptHistoryKind::Recap);
    }

    #[test]
    fn append_over_cap_drops_oldest_whole_entries() {
        let mut history = Vec::new();
        // Each entry: 1 chrome + 50 lines = 51 rendered lines.
        let big_body = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        // 21 entries of 51 rows = 1071 rows; cap drops the oldest until <=1000.
        for i in 0..21 {
            let mut e = entry(PromptHistoryKind::Prompt, &big_body);
            // Give each a distinct text so we can identify which survived.
            e.text = format!("entry-{i}\n{big_body}");
            append_with_cap(&mut history, e);
        }
        let total: usize = history.iter().map(|e| e.rendered_line_count()).sum();
        assert!(total <= MAX_PROMPT_HISTORY_LINES);
        // The newest entry must always survive.
        assert!(history.last().unwrap().text.starts_with("entry-20"));
        // The oldest entry must have been dropped.
        assert!(history.iter().all(|e| !e.text.starts_with("entry-0\n")));
    }

    #[test]
    fn append_single_oversized_entry_is_still_kept() {
        let mut history = Vec::new();
        let huge_body = (0..2000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        append_with_cap(&mut history, entry(PromptHistoryKind::Prompt, &huge_body));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn rendered_line_count_trims_blank_borders() {
        let e = entry(PromptHistoryKind::Prompt, "\n\nhello\nworld\n\n");
        // 1 chrome + 2 body lines = 3.
        assert_eq!(e.rendered_line_count(), 3);
    }

    #[test]
    fn relative_age_buckets() {
        assert_eq!(relative_age_for_duration(Duration::from_secs(5)), "5s ago");
        assert_eq!(
            relative_age_for_duration(Duration::from_secs(125)),
            "2m ago"
        );
        assert_eq!(
            relative_age_for_duration(Duration::from_secs(3 * 3600 + 1)),
            "3h ago"
        );
        assert_eq!(
            relative_age_for_duration(Duration::from_secs(2 * 86_400)),
            "2d ago"
        );
    }
}
