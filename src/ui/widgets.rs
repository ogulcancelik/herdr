use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::state::Palette;

pub(super) fn render_panel_shell(
    frame: &mut Frame,
    area: Rect,
    border_color: Color,
    bg: Color,
) -> Option<Rect> {
    if area.width < 2 || area.height < 2 {
        return None;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .border_set(ratatui::symbols::border::PLAIN)
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    Some(inner)
}

pub(super) fn panel_contrast_fg(p: &Palette) -> Color {
    match p.panel_bg {
        Color::Reset => p.surface_dim,
        color => color,
    }
}

pub(crate) fn centered_popup_rect(area: Rect, popup_w: u16, popup_h: u16) -> Option<Rect> {
    let popup_w = popup_w.min(area.width.saturating_sub(4));
    let popup_h = popup_h.min(area.height.saturating_sub(2));
    if popup_w < 4 || popup_h < 4 {
        return None;
    }

    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    Some(Rect::new(popup_x, popup_y, popup_w, popup_h))
}

pub(super) fn render_modal_shell(
    frame: &mut Frame,
    area: Rect,
    popup_w: u16,
    popup_h: u16,
    p: &Palette,
) -> Option<Rect> {
    let popup = centered_popup_rect(area, popup_w, popup_h)?;
    render_panel_shell(frame, popup, p.accent, p.panel_bg)
}

pub(super) fn render_modal_header(frame: &mut Frame, area: Rect, title: &str, p: &Palette) {
    let line = Line::from(vec![
        Span::styled(
            title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("⬆", Style::default().fg(p.accent)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ModalStackAreas {
    pub header: Rect,
    pub content: Rect,
    pub footer: Option<Rect>,
    pub actions: Option<Rect>,
}

pub(crate) fn modal_stack_areas(
    inner: Rect,
    header_height: u16,
    footer_height: u16,
    actions_height: u16,
    gap: u16,
) -> ModalStackAreas {
    #[derive(Clone, Copy)]
    enum Slot {
        Header,
        Content,
        Footer,
        Actions,
    }

    let mut constraints = Vec::new();
    let mut slots = Vec::new();
    let mut push = |slot: Slot, constraint: Constraint| {
        if !slots.is_empty() {
            constraints.push(Constraint::Length(gap));
        }
        constraints.push(constraint);
        slots.push(slot);
    };

    push(Slot::Header, Constraint::Length(header_height));
    push(Slot::Content, Constraint::Min(0));
    if footer_height > 0 {
        push(Slot::Footer, Constraint::Length(footer_height));
    }
    if actions_height > 0 {
        push(Slot::Actions, Constraint::Length(actions_height));
    }

    let areas = Layout::vertical(constraints).split(inner);
    let mut header = Rect::default();
    let mut content = Rect::default();
    let mut footer = None;
    let mut actions = None;

    for (slot, area) in slots.into_iter().zip(areas.iter().step_by(2).copied()) {
        match slot {
            Slot::Header => header = area,
            Slot::Content => content = area,
            Slot::Footer => footer = Some(area),
            Slot::Actions => actions = Some(area),
        }
    }

    ModalStackAreas {
        header,
        content,
        footer,
        actions,
    }
}

pub(crate) fn action_button_text(hint: Option<&str>, label: &str) -> String {
    match hint {
        Some(hint) => format!(" {hint} {label} "),
        None => format!(" {label} "),
    }
}

pub(crate) fn action_button_width(hint: Option<&str>, label: &str) -> u16 {
    action_button_text(hint, label).chars().count() as u16
}

pub(crate) struct ActionButtonSpec<'a> {
    pub hint: Option<&'a str>,
    pub label: &'a str,
}

pub(crate) fn action_button_row_rects(
    area: Rect,
    buttons: &[ActionButtonSpec<'_>],
    gap: u16,
    row_offset: u16,
) -> Vec<Rect> {
    let widths: Vec<u16> = buttons
        .iter()
        .map(|button| action_button_width(button.hint, button.label))
        .collect();
    centered_button_row(area, &widths, gap, row_offset)
}

pub(super) fn render_action_button(
    frame: &mut Frame,
    rect: Rect,
    hint: Option<&str>,
    label: &str,
    style: Style,
) {
    frame.render_widget(
        Paragraph::new(action_button_text(hint, label))
            .style(style)
            .alignment(Alignment::Center),
        rect,
    );
}

pub(super) fn centered_button_row(
    inner: Rect,
    widths: &[u16],
    gap: u16,
    row_offset: u16,
) -> Vec<Rect> {
    let total_w = widths
        .iter()
        .copied()
        .sum::<u16>()
        .saturating_add(gap.saturating_mul(widths.len().saturating_sub(1) as u16));
    let mut x = inner.x + inner.width.saturating_sub(total_w) / 2;
    let y = inner.y + row_offset.min(inner.height.saturating_sub(1));
    widths
        .iter()
        .map(|w| {
            let rect = Rect::new(
                x,
                y,
                (*w).min(inner.width.saturating_sub(x.saturating_sub(inner.x))),
                1,
            );
            x = x.saturating_add(*w).saturating_add(gap);
            rect
        })
        .collect()
}
