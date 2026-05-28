//! Status-line composition: assemble `status-left`, the (single) window-status
//! segment, and `status-right` into a width-filled ratatui [`Line`], mirroring
//! tmux's left/right justification.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::expand::{expand_str, render_segments, FormatResolver};
use super::style::apply_style_spec;

/// Format strings + length limits for one status line. Field names mirror tmux
/// options. Lengths of `0` mean "unlimited" (as in tmux).
pub struct StatusSpec<'a> {
    pub style: &'a str,
    pub left: &'a str,
    pub right: &'a str,
    pub window: &'a str,
    pub left_length: usize,
    pub right_length: usize,
}

/// Compose a status line of exactly `width` columns.
pub fn compose(spec: &StatusSpec, width: u16, r: &dyn FormatResolver) -> Line<'static> {
    let width = width as usize;
    let base = apply_style_spec(&expand_str(spec.style, r), Style::default(), Style::default());

    let mut left = render_segments(spec.left, base, r);
    if spec.left_length > 0 {
        truncate_segments(&mut left, spec.left_length);
    }
    let window = render_segments(spec.window, base, r);
    let mut right = render_segments(spec.right, base, r);
    if spec.right_length > 0 {
        truncate_segments(&mut right, spec.right_length);
    }

    let right_width = segments_width(&right);
    let mut left_part = left;
    left_part.extend(window);

    // Reserve space for the right segment; truncate the left part if needed.
    let left_budget = width.saturating_sub(right_width);
    truncate_segments(&mut left_part, left_budget);
    let left_width = segments_width(&left_part);

    let pad = width.saturating_sub(left_width + right_width);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(left_part.len() + right.len() + 1);
    for (text, style) in left_part {
        spans.push(Span::styled(text, style));
    }
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), base));
    }
    for (text, style) in right {
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

fn segments_width(segs: &[(String, Style)]) -> usize {
    segs.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum()
}

/// Truncate styled segments to at most `max` display columns (column-accurate).
fn truncate_segments(segs: &mut Vec<(String, Style)>, max: usize) {
    use unicode_width::UnicodeWidthChar;
    let mut width = 0usize;
    let mut keep = Vec::new();
    'outer: for (text, style) in segs.drain(..) {
        let mut piece = String::new();
        for ch in text.chars() {
            let w = ch.width().unwrap_or(0);
            if width + w > max {
                if !piece.is_empty() {
                    keep.push((piece, style));
                }
                break 'outer;
            }
            width += w;
            piece.push(ch);
        }
        if !piece.is_empty() {
            keep.push((piece, style));
        }
    }
    *segs = keep;
}
