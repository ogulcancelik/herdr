use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use super::widgets::{panel_contrast_fg, render_panel_shell};
use crate::app::AppState;

fn render_bottom_bar(frame: &mut Frame, area: Rect, line: Line<'_>, bg: ratatui::style::Color) {
    frame.render_widget(Clear, area);
    let buf = frame.buffer_mut();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)].set_style(Style::default().bg(bg));
    }
    frame.render_widget(Paragraph::new(line), area);
}

pub(super) fn render_navigate_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let mode_style = Style::default()
        .fg(panel_contrast_fg(&app.palette))
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);

    let kb = &app.keybinds;
    let line = Line::from(vec![
        Span::styled(" NAVIGATE ", mode_style),
        Span::raw(" "),
        Span::styled("esc", key),
        Span::styled(" back  ", dim),
        Span::styled("↑↓", key),
        Span::styled(" ws  ", dim),
        Span::styled("⇥", key),
        Span::styled(" pane  ", dim),
        Span::styled(kb.new_tab_label.as_str(), key),
        Span::styled(" new tab  ", dim),
        Span::styled(kb.split_vertical_label.as_str(), key),
        Span::styled(" split│  ", dim),
        Span::styled(kb.split_horizontal_label.as_str(), key),
        Span::styled(" split─  ", dim),
        Span::styled(kb.close_pane_label.as_str(), key),
        Span::styled(" close  ", dim),
        Span::styled(kb.fullscreen_label.as_str(), key),
        Span::styled(" full  ", dim),
        Span::styled(kb.resize_mode_label.as_str(), key),
        Span::styled(" resize  ", dim),
        Span::styled("?", key),
        Span::styled(" keybinds  ", dim),
        Span::styled("s", key),
        Span::styled(" settings  ", dim),
        Span::styled("q", key),
        Span::styled(" quit", dim),
    ]);

    let overlay_y = area.y + area.height.saturating_sub(1);
    let overlay_area = Rect::new(area.x, overlay_y, area.width, 1);
    render_bottom_bar(frame, overlay_area, line, app.palette.panel_bg);

    if app.update_available.is_some() {
        let status = Line::from(vec![Span::styled(
            " update ready",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        )]);
        let width = 13u16.min(overlay_area.width);
        let status_area = Rect::new(
            overlay_area.x + overlay_area.width.saturating_sub(width),
            overlay_area.y,
            width,
            overlay_area.height,
        );
        frame.render_widget(Clear, status_area);
        frame.render_widget(
            Paragraph::new(status).alignment(Alignment::Right),
            status_area,
        );
    }
}

pub(super) fn render_global_launcher_menu(app: &AppState, frame: &mut Frame) {
    let rect = app.global_menu_rect();
    let Some(inner) = render_panel_shell(frame, rect, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };

    let items = app.global_menu_labels();
    for (idx, item) in items.iter().enumerate() {
        let y = inner.y + idx as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = idx == app.global_menu.highlighted;
        let rect = Rect::new(inner.x, y, inner.width, 1);

        if *item == "update ready" {
            let line = if selected {
                Line::from(vec![
                    Span::styled(
                        " update ready ",
                        Style::default()
                            .fg(panel_contrast_fg(&app.palette))
                            .bg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "● ",
                        Style::default()
                            .fg(panel_contrast_fg(&app.palette))
                            .bg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled(" update ready ", Style::default().fg(app.palette.text)),
                    Span::styled(
                        "● ",
                        Style::default()
                            .fg(app.palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            };
            frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), rect);
            continue;
        }

        let style = if selected {
            Style::default()
                .fg(panel_contrast_fg(&app.palette))
                .bg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.text)
        };
        frame.render_widget(
            Paragraph::new(format!(" {item} "))
                .style(style)
                .alignment(Alignment::Left),
            rect,
        );
    }
}

pub(super) fn render_resize_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let mode_style = Style::default()
        .fg(panel_contrast_fg(&app.palette))
        .bg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);

    let line = Line::from(vec![
        Span::styled(" RESIZE ", mode_style),
        Span::raw("  "),
        Span::styled("h/l", key),
        Span::styled(" width  ", dim),
        Span::styled("j/k", key),
        Span::styled(" height  ", dim),
        Span::styled("esc", key),
        Span::styled(" done", dim),
    ]);

    let overlay_y = area.y + area.height.saturating_sub(1);
    let overlay_area = Rect::new(area.x, overlay_y, area.width, 1);
    render_bottom_bar(frame, overlay_area, line, app.palette.panel_bg);
}

pub(super) fn render_context_menu(app: &AppState, frame: &mut Frame) {
    let Some(menu) = &app.context_menu else {
        return;
    };

    let p = &app.palette;
    let Some(menu_rect) = app.context_menu_rect() else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, menu_rect, p.accent, p.panel_bg) else {
        return;
    };

    let items: Vec<ListItem> = menu
        .items()
        .iter()
        .map(|item| ListItem::new(Line::from(*item)))
        .collect();
    let list = List::new(items)
        .style(Style::default().fg(p.text))
        .highlight_style(
            Style::default()
                .bg(p.accent)
                .fg(panel_contrast_fg(p))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ");
    let mut state = ListState::default().with_selected(Some(menu.list.highlighted));
    frame.render_stateful_widget(list, inner, &mut state);
}
