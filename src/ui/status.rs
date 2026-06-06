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

/// One-line machine HUD: cpu · mem · disk · battery · net · gpu. Metrics the
/// sampler could not read are omitted. Utilization colors shift at 60/85%.
pub(super) fn render_status_line(app: &crate::app::AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width < 10 {
        return;
    }
    let p = &app.palette;
    let dim = Style::default().fg(p.overlay0);
    let mut spans: Vec<Span> = Vec::new();
    let sep = " \u{b7} ";

    let utilization_style = |percent: f32| {
        if percent >= 85.0 {
            Style::default().fg(p.red)
        } else if percent >= 60.0 {
            Style::default().fg(p.yellow)
        } else {
            Style::default().fg(p.green)
        }
    };
    let push_metric = |spans: &mut Vec<Span>, label: &str, value: String, style: Style| {
        if !spans.is_empty() {
            spans.push(Span::styled(sep.to_string(), dim));
        }
        spans.push(Span::styled(format!("{label} "), dim));
        spans.push(Span::styled(value, style));
    };

    let Some(stats) = app.system_stats.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(" gathering system stats\u{2026}", dim)),
            area,
        );
        return;
    };

    if let Some(cpu) = stats.cpu_percent {
        push_metric(
            &mut spans,
            "cpu",
            format!("{cpu:.0}%"),
            utilization_style(cpu),
        );
    }
    if let (Some(used), Some(total)) = (stats.mem_used, stats.mem_total) {
        let percent = if total > 0 {
            used as f32 / total as f32 * 100.0
        } else {
            0.0
        };
        push_metric(
            &mut spans,
            "mem",
            format!(
                "{}/{}",
                crate::system_stats::human_bytes(used),
                crate::system_stats::human_bytes(total)
            ),
            utilization_style(percent),
        );
    }
    if let Some(free) = stats.disk_free {
        push_metric(
            &mut spans,
            "disk",
            format!("{} free", crate::system_stats::human_bytes(free)),
            Style::default().fg(p.text),
        );
    }
    if let Some(percent) = stats.battery_percent {
        let icon = match stats.battery_charging {
            Some(true) => "\u{26a1}",
            _ => "\u{1f50b}",
        };
        let style = if percent <= 15 {
            Style::default().fg(p.red)
        } else {
            Style::default().fg(p.text)
        };
        push_metric(&mut spans, icon, format!("{percent}%"), style);
    }
    if let (Some(rx), Some(tx)) = (stats.net_rx_per_sec, stats.net_tx_per_sec) {
        push_metric(
            &mut spans,
            "net",
            format!(
                "\u{25bc}{} \u{25b2}{}",
                crate::system_stats::human_bytes(rx),
                crate::system_stats::human_bytes(tx)
            ),
            Style::default().fg(p.teal),
        );
    }
    if let Some(gpu) = stats.gpu_percent {
        push_metric(
            &mut spans,
            "gpu",
            format!("{gpu}%"),
            utilization_style(gpu as f32),
        );
    }

    if spans.is_empty() {
        return;
    }
    spans.insert(0, Span::styled(" ".to_string(), dim));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        area,
    );
}
