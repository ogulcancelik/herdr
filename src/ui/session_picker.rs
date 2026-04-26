use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::widgets::{
    action_button_width, centered_popup_rect, modal_stack_areas, panel_contrast_fg,
    render_modal_header, render_modal_shell,
};
use crate::app::AppState;

const MODAL_WIDTH: u16 = 42;
const MODAL_MIN_HEIGHT: u16 = 10;

pub(crate) fn session_picker_modal_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, MODAL_WIDTH, MODAL_MIN_HEIGHT)
}

pub(crate) fn session_picker_visible_row_rects(
    inner: Rect,
    entry_count: usize,
    scroll: usize,
) -> Vec<(usize, Rect)> {
    let mut rows = Vec::new();
    let mut y = inner.y;
    for idx in scroll..entry_count {
        if y >= inner.y + inner.height {
            break;
        }
        rows.push((idx, Rect::new(inner.x, y, inner.width, 1)));
        y = y.saturating_add(1);
    }
    rows
}

pub(crate) fn session_picker_new_button_rect(footer: Rect) -> Rect {
    let width = action_button_width(Some("n"), "new");
    let x = footer.x + footer.width.saturating_sub(width);
    Rect::new(x, footer.y, width.min(footer.width), footer.height)
}

pub fn render_session_picker_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);

    let picker = &app.session_picker;
    let entry_count = picker.sessions.len();
    let content_rows = entry_count.max(1) as u16;
    let creating_row = if picker.creating { 2 } else { 0 };
    let error_row = if picker.error.is_some() { 1 } else { 0 };
    let footer_rows = 1u16;
    let border_rows = 2u16;
    let gap_rows = 2u16;
    let header_rows = 2u16;
    let total_height = (border_rows
        + header_rows
        + gap_rows
        + content_rows
        + creating_row
        + error_row
        + footer_rows)
        .max(MODAL_MIN_HEIGHT)
        .min(area.height.saturating_sub(2));

    let Some(inner) = render_modal_shell(frame, area, MODAL_WIDTH, total_height, &app.palette)
    else {
        return;
    };

    let stack = modal_stack_areas(inner, 2, footer_rows, 0, 1);

    // Header
    render_modal_header(frame, stack.header, "sessions", &app.palette);

    // Content area
    let content = stack.content;
    let visible_rows = session_picker_visible_row_rects(content, entry_count, picker.scroll);

    for (idx, row) in visible_rows {
        let entry = &picker.sessions[idx];
        let is_highlighted = idx == picker.highlighted;
        let style = if is_highlighted {
            Style::default()
                .bg(app.palette.accent)
                .fg(panel_contrast_fg(&app.palette))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .bg(app.palette.panel_bg)
                .fg(app.palette.text)
        };

        let marker = if entry.active { " ● " } else { "   " };
        let label = if picker.pending_delete.is_some() && idx == picker.highlighted {
            format!(
                "{}delete '{}' ? (enter confirm / esc cancel)",
                marker, entry.label
            )
        } else if entry.active {
            format!("{}{}", marker, entry.label)
        } else {
            format!("  {}{}", marker, entry.label)
        };
        frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), row);
    }

    // Empty state
    if picker.sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " no saved sessions",
                Style::default().fg(app.palette.overlay0),
            ))),
            content,
        );
    }

    // Creating input
    if picker.creating {
        let input_y = content.y + content.height.saturating_sub(2);
        let input_area = Rect::new(content.x, input_y, content.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" name: {}", picker.name_input),
                Style::default().fg(app.palette.text),
            ))),
            input_area,
        );
    }

    // Error
    if let Some(error) = &picker.error {
        let err_y = content.y + content.height.saturating_sub(1);
        let err_area = Rect::new(content.x, err_y, content.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" ! {error}"),
                Style::default().fg(app.palette.red),
            ))),
            err_area,
        );
    }

    // Footer hints
    if let Some(footer) = stack.footer {
        let hints = if picker.creating {
            "enter confirm  esc cancel"
        } else if picker.pending_delete.is_some() {
            "enter confirm  esc cancel"
        } else {
            "enter switch  n new  d delete  esc close"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hints,
                Style::default().fg(app.palette.overlay0),
            ))),
            footer,
        );
    }
}
