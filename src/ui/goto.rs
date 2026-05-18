use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::widgets::{centered_popup_rect, panel_contrast_fg};
use crate::app::AppState;

pub(super) fn render_goto_overlay(app: &AppState, frame: &mut Frame) {
    let area = frame.area();
    let target_w = (area.width as f32 * 0.7) as u16;
    let popup_w = target_w.clamp(40, 90).min(area.width.saturating_sub(4));
    let popup_h = (area.height.saturating_sub(4)).min(20).max(8);
    let Some(rect) = centered_popup_rect(area, popup_w, popup_h) else {
        return;
    };

    let p = &app.palette;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.accent))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let [header_area, filter_area, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(inner);

    let header = Line::from(vec![Span::styled(
        " Goto",
        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(Paragraph::new(header), header_area);

    let prompt = Line::from(vec![
        Span::styled(" > ", Style::default().fg(p.accent)),
        Span::styled(app.goto.filter.as_str(), Style::default().fg(p.text)),
        Span::styled("_", Style::default().fg(p.overlay0)),
    ]);
    frame.render_widget(Paragraph::new(prompt), filter_area);

    let total = app.goto.items.len();
    let height = list_area.height as usize;
    if total == 0 || height == 0 {
        let empty = Line::from(Span::styled(
            "  no matches",
            Style::default().fg(p.overlay0),
        ));
        frame.render_widget(Paragraph::new(empty), list_area);
        return;
    }

    let selected = app.goto.list.min(total.saturating_sub(1));
    let start = if selected >= height {
        selected + 1 - height
    } else {
        0
    };
    let end = (start + height).min(total);

    for (row, idx) in (start..end).enumerate() {
        let item = &app.goto.items[idx];
        let y = list_area.y + row as u16;
        let row_rect = Rect::new(list_area.x, y, list_area.width, 1);
        let is_selected = idx == selected;
        let style = if is_selected {
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };
        let marker = if item.is_current { " *" } else { "  " };
        let text = format!(" {}{}", item.label, marker);
        frame.render_widget(Paragraph::new(text).style(style), row_rect);
    }
}
