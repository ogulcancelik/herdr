use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Palette, ToastKind, ToastNotification},
    detect::AgentState,
};

pub(crate) fn copy_feedback_rect(area: Rect, feedback: &CopyFeedback, offset_rows: u16) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height + offset_rows);
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
) -> Rect {
    let content_width = (toast.title.len().max(toast.context.len()) as u16) + 4;
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = area.x + area.width.saturating_sub(width);
    let y = area.y
        + area
            .height
            .saturating_sub(height + if offset_for_warning { 1 } else { 0 });
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

/// Action feedback banner: same placement as the config banner, but the
/// message renders verbatim (no "config warning:" framing).
pub(super) fn render_action_notice(
    frame: &mut Frame,
    area: Rect,
    row_offset: u16,
    message: &str,
    p: &Palette,
) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.peach)
        .add_modifier(Modifier::BOLD);
    let text = format!(" {message} ");
    let width = (text.len() as u16).min(area.width);
    let notif_area = Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + row_offset,
        width,
        1,
    );
    if notif_area.y < area.y + area.height {
        frame.render_widget(
            ratatui::widgets::Paragraph::new(text).style(style),
            notif_area,
        );
    }
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" config warning: {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_dot(state: AgentState, seen: bool, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn agent_icon(
    state: AgentState,
    seen: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (super::spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

/// Utilization color: green below 60%, yellow from 60%, red from 85%.
/// Shared glyph language between the status line and the sidebar servers
/// band so machine health reads the same everywhere.
pub(super) fn utilization_style(percent: f32, p: &Palette) -> Style {
    if percent >= 85.0 {
        Style::default().fg(p.red)
    } else if percent >= 60.0 {
        Style::default().fg(p.yellow)
    } else {
        Style::default().fg(p.green)
    }
}

/// Append one `label value` metric, separated from any prior spans by `sep`.
/// The label renders dim; the value carries the caller's style.
fn push_metric_with_sep(
    spans: &mut Vec<Span<'static>>,
    sep: &str,
    label: &str,
    value: String,
    style: Style,
    p: &Palette,
) {
    let dim = Style::default().fg(p.overlay0);
    if !spans.is_empty() {
        spans.push(Span::styled(sep.to_string(), dim));
    }
    spans.push(Span::styled(format!("{label} "), dim));
    spans.push(Span::styled(value, style));
}

/// Append one `label value` metric, `·`-separated from any prior spans —
/// the status line's roomy form.
pub(super) fn push_metric(
    spans: &mut Vec<Span<'static>>,
    label: &str,
    value: String,
    style: Style,
    p: &Palette,
) {
    push_metric_with_sep(spans, " \u{b7} ", label, value, style, p);
}

/// The servers-band variant of [`push_metric`]: single-space separated —
/// the dots cost width for nothing at the band's density (#41).
pub(super) fn push_band_metric(
    spans: &mut Vec<Span<'static>>,
    label: &str,
    value: String,
    style: Style,
    p: &Palette,
) {
    push_metric_with_sep(spans, " ", label, value, style, p);
}

/// CPU/GPU value in the band's fixed-width discipline: right-aligned
/// width-3 (`  8%`, ` 42%`, `100%`) so the columns hold still across
/// refreshes.
pub(super) fn format_percent3(percent: f32) -> String {
    format!("{percent:>3.0}%")
}

/// `used/total` memory with used padded to the width of total
/// (` 92G/512G`) so the slash column does not jitter.
pub(super) fn format_mem_ratio(used: u64, total: u64) -> String {
    let used = crate::system_stats::human_bytes(used);
    let total = crate::system_stats::human_bytes(total);
    format!("{used:>width$}/{total}", width = total.len())
}

/// `▼rx ▲tx` network throughput, bytes/sec.
pub(super) fn format_net_io(rx: u64, tx: u64) -> String {
    format!(
        "\u{25bc}{} \u{25b2}{}",
        crate::system_stats::human_bytes(rx),
        crate::system_stats::human_bytes(tx)
    )
}

/// Battery glyph: the charging bolt when on AC, else the level glyph by
/// quintile.
pub(super) fn battery_icon(percent: u8, charging: Option<bool>) -> &'static str {
    // nf-md-battery_charging, else level glyph by quintile
    match charging {
        Some(true) => "\u{f0084}",
        _ => match percent {
            0..=20 => "\u{f007a}",
            21..=40 => "\u{f007c}",
            41..=60 => "\u{f007e}",
            61..=80 => "\u{f0080}",
            _ => "\u{f0079}",
        },
    }
}

/// Battery value color: red once the charge gets critical.
pub(super) fn battery_style(percent: u8, p: &Palette) -> Style {
    if percent <= 15 {
        Style::default().fg(p.red)
    } else {
        Style::default().fg(p.text)
    }
}

/// Memory utilization as a percentage, for [`utilization_style`].
pub(super) fn mem_percent(used: u64, total: u64) -> f32 {
    if total > 0 {
        used as f32 / total as f32 * 100.0
    } else {
        0.0
    }
}

/// One-line machine HUD: cpu · mem · disk · battery · net · gpu. Metrics the
/// sampler could not read are omitted. Utilization colors shift at 60/85%.
pub(super) fn render_status_line(app: &crate::app::AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width < 10 {
        return;
    }
    let p = &app.palette;
    let dim = Style::default().fg(p.overlay0);
    let mut spans: Vec<Span> = Vec::new();

    let Some(stats) = app.system_stats.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(" gathering system stats\u{2026}", dim)),
            area,
        );
        return;
    };

    if let Some(host) = stats.host.as_deref() {
        spans.push(Span::styled(
            host.to_string(),
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(cpu) = stats.cpu_percent {
        push_metric(
            &mut spans,
            "\u{f0ee0}", // nf-md-cpu_64_bit
            format!("{cpu:.0}%"),
            utilization_style(cpu, p),
            p,
        );
    }
    if let (Some(used), Some(total)) = (stats.mem_used, stats.mem_total) {
        push_metric(
            &mut spans,
            "\u{f035b}", // nf-md-memory
            format!(
                "{}/{}",
                crate::system_stats::human_bytes(used),
                crate::system_stats::human_bytes(total)
            ),
            utilization_style(mem_percent(used, total), p),
            p,
        );
    }
    if let Some(free) = stats.disk_free {
        push_metric(
            &mut spans,
            "\u{f02ca}", // nf-md-harddisk
            format!("{} free", crate::system_stats::human_bytes(free)),
            Style::default().fg(p.text),
            p,
        );
    }
    if let Some(percent) = stats.battery_percent {
        push_metric(
            &mut spans,
            battery_icon(percent, stats.battery_charging),
            format!("{percent}%"),
            battery_style(percent, p),
            p,
        );
    }
    if let (Some(rx), Some(tx)) = (stats.net_rx_per_sec, stats.net_tx_per_sec) {
        push_metric(
            &mut spans,
            "\u{f06f3}", // nf-md-network
            format_net_io(rx, tx),
            Style::default().fg(p.teal),
            p,
        );
    }
    if let Some(gpu) = stats.gpu_percent {
        push_metric(
            &mut spans,
            "\u{f08ae}", // nf-md-expansion_card
            format!("{gpu}%"),
            utilization_style(gpu as f32, p),
            p,
        );
    }

    if spans.is_empty() {
        return;
    }
    spans.insert(0, Span::styled(" ".to_string(), dim));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_percent3_right_aligns_to_three_digits() {
        assert_eq!(format_percent3(8.0), "  8%");
        assert_eq!(format_percent3(42.0), " 42%");
        assert_eq!(format_percent3(100.0), "100%");
        // Rounded, not truncated — 99.6 reads as full.
        assert_eq!(format_percent3(99.6), "100%");
    }

    #[test]
    fn format_mem_ratio_pads_used_to_total_width() {
        const G: u64 = 1024 * 1024 * 1024;
        // Used padded to the width of total so the slash column is stable.
        assert_eq!(format_mem_ratio(92 * G, 512 * G), " 92G/512G");
        assert_eq!(format_mem_ratio(110 * G, 512 * G), "110G/512G");
        // Equal widths need no padding; `human_bytes` keeps one decimal
        // under 10, so a small used value is already at total's width.
        assert_eq!(format_mem_ratio(13 * G, 16 * G), "13G/16G");
        assert_eq!(format_mem_ratio(8 * G, 17 * G), "8.0G/17G");
    }

    #[test]
    fn band_metrics_join_with_spaces_status_metrics_with_dots() {
        let p = crate::app::state::AppState::test_new().palette;
        let style = Style::default();

        let mut band: Vec<Span<'static>> = Vec::new();
        push_band_metric(&mut band, "a", "1".into(), style, &p);
        push_band_metric(&mut band, "b", "2".into(), style, &p);
        let band: String = band.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(band, "a 1 b 2");

        let mut status: Vec<Span<'static>> = Vec::new();
        push_metric(&mut status, "a", "1".into(), style, &p);
        push_metric(&mut status, "b", "2".into(), style, &p);
        let status: String = status.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(status, "a 1 \u{b7} b 2");
    }

    #[test]
    fn battery_icon_picks_quintile_or_charging_bolt() {
        assert_eq!(battery_icon(50, Some(true)), "\u{f0084}");
        assert_eq!(battery_icon(10, None), "\u{f007a}");
        assert_eq!(battery_icon(50, Some(false)), "\u{f007e}");
        assert_eq!(battery_icon(95, None), "\u{f0079}");
    }
}
