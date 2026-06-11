use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::sidebar::{agent_panel_entries, agent_panel_entries_from, AgentPanelEntry};
use super::status::{agent_icon, state_dot};
use crate::app::state::{Palette, ToastKind, ToastNotification};
use crate::app::AppState;
use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::TerminalRuntimeRegistry;

const SWITCH_BUTTON_WIDTH: u16 = 10;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MobileHeaderHitAreas {
    pub menu: Rect,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MobileSwitcherAreas {
    pub close: Rect,
    pub viewport: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MobileSwitcherTarget {
    NewWorkspace,
    Workspace(usize),
    NewTab,
    Tab(usize),
    Agent {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    },
    Menu(usize),
}

pub(crate) fn is_mobile_width(area: Rect, threshold: u16) -> bool {
    area.width > 0 && area.width <= threshold
}

pub(crate) fn compute_mobile_header_hit_areas(_app: &AppState, area: Rect) -> MobileHeaderHitAreas {
    if area.width == 0 || area.height == 0 {
        return MobileHeaderHitAreas::default();
    }

    let width = SWITCH_BUTTON_WIDTH.min(area.width);
    let switch = Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y,
        width,
        area.height,
    );

    MobileHeaderHitAreas { menu: switch }
}

pub(crate) fn mobile_switcher_areas(app: &AppState) -> MobileSwitcherAreas {
    let screen = mobile_screen_rect(app);
    if screen.width == 0 || screen.height <= 2 {
        return MobileSwitcherAreas::default();
    }

    let header_h = screen.height.min(2);
    let close_w = 10u16.min(screen.width);
    let close = Rect::new(
        screen.x + screen.width.saturating_sub(close_w),
        screen.y,
        close_w,
        header_h,
    );
    let viewport = Rect::new(
        screen.x,
        screen.y + header_h + 1,
        screen.width,
        screen.height.saturating_sub(header_h + 1),
    );

    MobileSwitcherAreas { close, viewport }
}

pub(crate) fn mobile_switcher_max_scroll_for_height(app: &AppState, viewport_height: u16) -> usize {
    mobile_switcher_content_height(app).saturating_sub(viewport_height as usize)
}

pub(crate) fn mobile_switcher_workspace_doc_range(idx: usize) -> std::ops::Range<usize> {
    let start = 2 + idx * 2;
    start..start + 2
}

pub(crate) fn mobile_switcher_max_scroll(app: &AppState) -> usize {
    mobile_switcher_max_scroll_for_height(app, mobile_switcher_areas(app).viewport.height)
}

pub(crate) fn mobile_switcher_target_at(
    app: &AppState,
    col: u16,
    row: u16,
) -> Option<MobileSwitcherTarget> {
    let areas = mobile_switcher_areas(app);
    let content = inset_for_left_scrollbar(areas.viewport);
    if !rect_contains(content, col, row) {
        return None;
    }

    let doc_row = app
        .mobile_switcher_scroll
        .saturating_add(row.saturating_sub(areas.viewport.y) as usize);
    let mut cursor = 0usize;

    cursor += 1; // spaces title
    if doc_row == cursor {
        return Some(MobileSwitcherTarget::NewWorkspace);
    }
    cursor += 1;
    let spaces_end = cursor + app.workspaces.len() * 2;
    if doc_row >= cursor && doc_row < spaces_end {
        return Some(MobileSwitcherTarget::Workspace((doc_row - cursor) / 2));
    }
    cursor = spaces_end;

    if let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) {
        cursor += 1; // tabs title
        if doc_row == cursor {
            return Some(MobileSwitcherTarget::NewTab);
        }
        cursor += 1;
        let tabs_end = cursor + ws.tabs.len();
        if doc_row >= cursor && doc_row < tabs_end {
            return Some(MobileSwitcherTarget::Tab(doc_row - cursor));
        }
        cursor = tabs_end;
    }

    cursor += 1; // agents title
    let agents = agent_panel_entries(app);
    let agents_end = cursor + agents.len() * 2;
    if doc_row >= cursor && doc_row < agents_end {
        let idx = (doc_row - cursor) / 2;
        return agents.get(idx).map(|entry| MobileSwitcherTarget::Agent {
            ws_idx: entry.ws_idx,
            tab_idx: entry.tab_idx,
            pane_id: entry.pane_id,
        });
    }
    cursor = agents_end;

    cursor += 1; // menu title
    let menu_idx = doc_row.checked_sub(cursor)?;
    (menu_idx < app.global_menu_labels().len()).then_some(MobileSwitcherTarget::Menu(menu_idx))
}

pub(crate) fn render_mobile_header(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.panel_bg));

    let switch = app.view.mobile_menu_hit_area;
    let status_w = switch.x.saturating_sub(area.x).saturating_sub(1);
    let status = Rect::new(area.x, area.y, status_w, area.height);

    render_header_status(app, terminal_runtimes, frame, status);
    render_switch_button(app, frame, switch);
}

pub(crate) fn mobile_toast_banner_rect(area: Rect, offset_for_warning: bool) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let y = area.y
        + area
            .height
            .saturating_sub(1 + if offset_for_warning { 1 } else { 0 });
    Rect::new(area.x, y, area.width, 1)
}

pub(crate) fn render_mobile_toast_banner(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    p: &Palette,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let banner = mobile_toast_banner_rect(area, offset_for_warning);
    let bg = p.surface0;

    frame.render_widget(Clear, banner);
    fill_rect(frame, banner, Style::default().bg(bg));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled("●", Style::default().fg(dot_color).bg(bg)),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                mobile_toast_title(toast),
                Style::default()
                    .fg(p.text)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(p.overlay0).bg(bg)),
            Span::styled(&toast.context, Style::default().fg(p.overlay0).bg(bg)),
        ])),
        banner,
    );
}

pub(crate) fn render_mobile_panel(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    frame.render_widget(Clear, area);
    fill_rect(frame, area, Style::default().bg(p.panel_bg));

    let areas = mobile_switcher_areas(app);
    frame.render_widget(
        Paragraph::new(" switch").style(
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y, areas.close.x.saturating_sub(area.x), 1),
    );
    render_close_button(app, frame, areas.close);

    if area.height > areas.close.height {
        draw_horizontal_rule(
            frame,
            Rect::new(area.x, area.y + areas.close.height, area.width, 1),
            p,
        );
    }

    render_mobile_switcher_content(app, terminal_runtimes, frame, areas.viewport);
}

fn render_header_status(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) else {
        frame.render_widget(Paragraph::new(" no workspace"), area);
        return;
    };

    let (state, seen) = ws.aggregate_state(&app.terminals);
    let (dot, dot_style) = if matches!(state, AgentState::Working) {
        (
            super::spinner_frame(app.spinner_tick),
            Style::default().fg(p.yellow),
        )
    } else {
        state_dot(state, seen, p)
    };
    let tab_label = format!("tab {}/{}", ws.active_tab + 1, ws.tabs.len());
    let row1 = Rect::new(area.x, area.y, area.width, 1);
    let tab_w = (tab_label.chars().count() as u16 + 1).min(area.width);
    let name_w = area.width.saturating_sub(tab_w);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(dot, dot_style.bg(p.panel_bg)),
            Span::raw(" "),
            Span::styled(
                truncate(
                    &ws.display_name_from(&app.terminals, terminal_runtimes),
                    name_w.saturating_sub(4) as usize,
                ),
                Style::default()
                    .fg(p.text)
                    .bg(p.panel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        Rect::new(row1.x, row1.y, name_w, 1),
    );
    frame.render_widget(
        Paragraph::new(tab_label)
            .style(Style::default().fg(p.overlay1).bg(p.panel_bg))
            .alignment(Alignment::Right),
        Rect::new(row1.x + name_w, row1.y, tab_w, 1),
    );

    if area.height > 1 {
        frame.render_widget(
            Paragraph::new(agent_priority_label(app))
                .style(Style::default().fg(p.overlay1).bg(p.panel_bg)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

fn render_switch_button(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.surface0));
    for y in area.y..area.y + area.height {
        frame.buffer_mut()[(area.x, y)]
            .set_symbol("│")
            .set_style(Style::default().fg(p.surface_dim).bg(p.surface0));
    }
    let label_y = if area.height > 1 { area.y + 1 } else { area.y };
    frame.render_widget(
        Paragraph::new("switch")
            .style(
                Style::default()
                    .fg(p.text)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center),
        Rect::new(area.x + 1, label_y, area.width.saturating_sub(1), 1),
    );
}

fn render_close_button(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = &app.palette;
    fill_rect(frame, area, Style::default().bg(p.surface0));
    for y in area.y..area.y + area.height {
        frame.buffer_mut()[(area.x, y)]
            .set_symbol("│")
            .set_style(Style::default().fg(p.surface_dim).bg(p.surface0));
    }
    frame.render_widget(
        Paragraph::new("close")
            .style(
                Style::default()
                    .fg(p.overlay1)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center),
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), 1),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new("×")
                .style(
                    Style::default()
                        .fg(p.text)
                        .bg(p.surface0)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(Alignment::Center),
            Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(1), 1),
        );
    }
}

fn mobile_switcher_content_height(app: &AppState) -> usize {
    let spaces_h = 2 + app.workspaces.len() * 2;
    let tabs_h = app
        .active
        .and_then(|idx| app.workspaces.get(idx))
        .map(|ws| 2 + ws.tabs.len())
        .unwrap_or(0);
    let agents_h = 1 + agent_panel_entries(app).len() * 2;
    let menu_h = 1 + app.global_menu_labels().len();
    spaces_h + tabs_h + agents_h + menu_h
}

fn render_mobile_switcher_content(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    viewport: Rect,
) {
    if viewport.width == 0 || viewport.height == 0 {
        return;
    }

    let p = &app.palette;
    let total_height = mobile_switcher_content_height(app);
    render_left_scrollbar(
        frame,
        viewport,
        total_height,
        viewport.height as usize,
        app.mobile_switcher_scroll,
        p,
    );
    let content = inset_for_left_scrollbar(viewport);
    if content == Rect::default() {
        return;
    }

    let mut doc_y = 0usize;
    render_section_title_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        "spaces",
        p,
    );
    doc_y += 1;
    render_action_row_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        "+ new workspace",
        p,
    );
    doc_y += 1;
    for (idx, ws) in app.workspaces.iter().enumerate() {
        let active = Some(idx) == app.active;
        let selected = idx == app.selected;
        let bg = mobile_item_bg(selected, active, p);
        let (state, seen) = ws.aggregate_state(&app.terminals);
        let (dot, dot_style) = state_dot(state, seen, p);
        let title = Line::from(vec![
            Span::styled("  ", Style::default().bg(bg)),
            Span::styled(dot, dot_style.bg(bg)),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                truncate(
                    &ws.display_name_from(&app.terminals, terminal_runtimes),
                    content.width.saturating_sub(5) as usize,
                ),
                Style::default()
                    .fg(p.text)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let detail = format!(
            "  {} · tab {}/{}",
            ws.branch().unwrap_or_else(|| "shell".into()),
            ws.active_tab + 1,
            ws.tabs.len()
        );
        render_two_line_item(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            bg,
            title,
            truncate(&detail, content.width as usize),
            p.overlay0,
        );
        doc_y += 2;
    }

    if let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) {
        render_section_title_at(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            "tabs",
            p,
        );
        doc_y += 1;
        render_action_row_at(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            "+ new tab",
            p,
        );
        doc_y += 1;
        for (idx, tab) in ws.tabs.iter().enumerate() {
            let active = idx == ws.active_tab;
            let bg = mobile_item_bg(false, active, p);
            let label = if tab.is_auto_named() {
                format!("tab {}", idx + 1)
            } else {
                format!("{} · {}", idx + 1, tab.display_name())
            };
            let title = Line::from(vec![
                Span::styled("  ", Style::default().bg(bg)),
                Span::styled(
                    truncate(&label, content.width.saturating_sub(3) as usize),
                    Style::default()
                        .fg(p.text)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            render_one_line_item(
                frame,
                viewport,
                content,
                doc_y,
                app.mobile_switcher_scroll,
                bg,
                title,
            );
            doc_y += 1;
        }
    }

    let focused_agent = app.active.and_then(|ws_idx| {
        let ws = app.workspaces.get(ws_idx)?;
        ws.focused_pane_id()
            .map(|pane_id| (ws_idx, ws.active_tab, pane_id))
    });
    let entries = agent_panel_entries_from(app, terminal_runtimes);
    render_section_title_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        "agents",
        p,
    );
    doc_y += 1;
    for entry in &entries {
        let active = focused_agent.is_some_and(|(ws_idx, tab_idx, pane_id)| {
            entry.ws_idx == ws_idx && entry.tab_idx == tab_idx && entry.pane_id == pane_id
        });
        let bg = mobile_item_bg(false, active, p);
        let (icon, icon_style) = agent_icon(entry.state, entry.seen, app.spinner_tick, p);
        let title = Line::from(vec![
            Span::styled("  ", Style::default().bg(bg)),
            Span::styled(icon, icon_style.bg(bg)),
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                truncate(
                    &entry.primary_label,
                    content.width.saturating_sub(5) as usize,
                ),
                Style::default()
                    .fg(p.text)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let detail = mobile_agent_detail(entry);
        render_two_line_item(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            bg,
            title,
            truncate(&detail, content.width as usize),
            p.overlay0,
        );
        doc_y += 2;
    }

    render_section_title_at(
        frame,
        viewport,
        content,
        doc_y,
        app.mobile_switcher_scroll,
        "menu",
        p,
    );
    doc_y += 1;
    for label in app.global_menu_labels() {
        if let Some(y) = visible_y(viewport, app.mobile_switcher_scroll, doc_y) {
            frame.render_widget(
                Paragraph::new(format!("  {label}"))
                    .style(Style::default().fg(p.overlay1).bg(p.panel_bg)),
                Rect::new(content.x, y, content.width, 1),
            );
        }
        doc_y += 1;
    }
}

fn mobile_agent_detail(entry: &AgentPanelEntry) -> String {
    let mut parts = Vec::new();
    if let Some(tab_label) = entry.primary_tab_label.as_deref() {
        parts.push(tab_label.to_string());
    }
    let status = entry
        .state_labels
        .get(super::sidebar::agent_panel_status_key(
            entry.state,
            entry.seen,
        ))
        .cloned()
        .unwrap_or_else(|| super::status::state_label(entry.state, entry.seen).to_string());
    parts.push(status);
    if let Some(agent_label) = entry.agent_label.as_deref() {
        parts.push(agent_label.to_string());
    }
    if let Some(custom_status) = entry.custom_status.as_deref() {
        parts.push(custom_status.to_string());
    }
    // Promoted header fields, narrowest mobile budget (header > agent
    // panel > nav list).
    if let Some(summary) = crate::terminal::compact_header_fields(
        &entry.header_fields,
        crate::app::state::AppState::NAVIGATOR_HEADER_FIELD_VALUE_COLS,
    ) {
        parts.push(summary);
    }

    format!("  {}", parts.join(" · "))
}

fn render_section_title_at(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    title: &str,
    p: &Palette,
) {
    let Some(y) = visible_y(viewport, scroll, doc_y) else {
        return;
    };
    render_section_title(
        frame,
        Rect::new(content.x, y, content.width.saturating_sub(1), 1),
        title,
        p,
    );
}

fn render_action_row_at(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    label: &str,
    p: &Palette,
) {
    let Some(y) = visible_y(viewport, scroll, doc_y) else {
        return;
    };
    render_action_row(frame, Rect::new(content.x, y, content.width, 1), label, p);
}

fn render_one_line_item(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    bg: ratatui::style::Color,
    title: Line<'_>,
) {
    fill_visible_doc_rect(
        frame,
        viewport,
        content,
        doc_y,
        1,
        Style::default().bg(bg),
        scroll,
    );
    if let Some(y) = visible_y(viewport, scroll, doc_y) {
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(content.x, y, content.width, 1),
        );
    }
}

fn render_two_line_item(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    scroll: usize,
    bg: ratatui::style::Color,
    title: Line<'_>,
    detail: String,
    detail_fg: ratatui::style::Color,
) {
    fill_visible_doc_rect(
        frame,
        viewport,
        content,
        doc_y,
        2,
        Style::default().bg(bg),
        scroll,
    );
    if let Some(y) = visible_y(viewport, scroll, doc_y) {
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(content.x, y, content.width, 1),
        );
    }
    if let Some(y) = visible_y(viewport, scroll, doc_y + 1) {
        frame.render_widget(
            Paragraph::new(detail).style(Style::default().fg(detail_fg).bg(bg)),
            Rect::new(content.x, y, content.width, 1),
        );
    }
}

fn visible_y(viewport: Rect, scroll: usize, doc_y: usize) -> Option<u16> {
    let offset = doc_y.checked_sub(scroll)?;
    (offset < viewport.height as usize).then_some(viewport.y + offset as u16)
}

fn fill_visible_doc_rect(
    frame: &mut Frame,
    viewport: Rect,
    content: Rect,
    doc_y: usize,
    height: usize,
    style: Style,
    scroll: usize,
) {
    for offset in 0..height {
        if let Some(y) = visible_y(viewport, scroll, doc_y + offset) {
            fill_rect(frame, Rect::new(content.x, y, content.width, 1), style);
        }
    }
}

fn mobile_item_bg(selected: bool, active: bool, p: &Palette) -> ratatui::style::Color {
    if selected {
        p.surface0
    } else if active {
        p.surface_dim
    } else {
        p.panel_bg
    }
}

fn inset_for_left_scrollbar(area: Rect) -> Rect {
    if area.width <= 1 {
        return Rect::default();
    }
    Rect::new(area.x + 1, area.y, area.width - 1, area.height)
}

fn render_left_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total_rows: usize,
    visible_rows: usize,
    scroll: usize,
    p: &Palette,
) {
    if area.width == 0 || area.height == 0 || visible_rows == 0 || total_rows <= visible_rows {
        return;
    }

    let track = Rect::new(area.x, area.y, 1, area.height);
    let max_scroll = total_rows.saturating_sub(visible_rows);
    let thumb_len = ((track.height as usize * visible_rows).div_ceil(total_rows))
        .max(1)
        .min(track.height as usize) as u16;
    let travel = track.height.saturating_sub(thumb_len);
    let thumb_top = track.y + ((travel as usize * scroll.min(max_scroll)) / max_scroll) as u16;

    for y in track.y..track.y + track.height {
        let is_thumb = y >= thumb_top && y < thumb_top + thumb_len;
        frame.buffer_mut()[(track.x, y)]
            .set_symbol(if is_thumb { "▌" } else { "│" })
            .set_style(
                Style::default()
                    .fg(if is_thumb { p.accent } else { p.surface_dim })
                    .bg(p.panel_bg),
            );
    }
}

fn render_section_title(frame: &mut Frame, area: Rect, title: &str, p: &Palette) {
    frame.render_widget(
        Paragraph::new(format!(" {title} ")).style(
            Style::default()
                .fg(p.overlay1)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_action_row(frame: &mut Frame, area: Rect, label: &str, p: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(format!("  {label}")).style(
            Style::default()
                .fg(p.accent)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn mobile_screen_rect(app: &AppState) -> Rect {
    let header = app.view.mobile_header_rect;
    let terminal = app.view.terminal_area;
    let x = header.x.min(terminal.x);
    let y = header.y.min(terminal.y);
    let right = (header.x + header.width).max(terminal.x + terminal.width);
    let bottom = (header.y + header.height).max(terminal.y + terminal.height);
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

fn agent_priority_label(app: &AppState) -> String {
    let Some(ws) = app.active.and_then(|idx| app.workspaces.get(idx)) else {
        return " no agents".to_string();
    };
    let mut blocked = 0usize;
    let mut working = 0usize;
    let mut done = 0usize;
    for detail in ws.pane_details(&app.terminals) {
        match (detail.state, detail.seen) {
            (AgentState::Blocked, _) => blocked += 1,
            (AgentState::Working, _) => working += 1,
            (AgentState::Idle, false) => done += 1,
            _ => {}
        }
    }
    if blocked > 0 {
        format!(" ◉ {blocked} blocked")
    } else if working > 0 {
        format!(" {working} working")
    } else if done > 0 {
        format!(" {done} done")
    } else {
        " all idle".to_string()
    }
}

fn mobile_toast_title(toast: &ToastNotification) -> String {
    match toast.kind {
        ToastKind::NeedsAttention => toast
            .title
            .strip_suffix(" needs attention")
            .map(|agent| format!("{agent} waiting"))
            .unwrap_or_else(|| toast.title.clone()),
        ToastKind::Finished => toast
            .title
            .strip_suffix(" finished")
            .map(|agent| format!("{agent} done"))
            .unwrap_or_else(|| toast.title.clone()),
        ToastKind::UpdateInstalled => "update ready".to_string(),
    }
}

fn fill_rect(frame: &mut Frame, area: Rect, style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_symbol(" ");
            buf[(x, y)].set_style(style);
        }
    }
}

fn draw_horizontal_rule(frame: &mut Frame, area: Rect, p: &Palette) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = frame.buffer_mut();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)]
            .set_symbol("─")
            .set_style(Style::default().fg(p.surface_dim).bg(p.panel_bg));
    }
}

fn truncate(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}
#[cfg(test)]
mod tests {
    use super::*;

    fn agent_entry(primary_tab_label: Option<&str>, agent_label: Option<&str>) -> AgentPanelEntry {
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: PaneId::from_raw(1),
            primary_label: "herdr".into(),
            primary_tab_label: primary_tab_label.map(str::to_string),
            agent_label: agent_label.map(str::to_string),
            state: AgentState::Idle,
            seen: true,
            custom_status: None,
            header_fields: Vec::new(),
            state_labels: std::collections::HashMap::new(),
            server: "mba22".into(),
            project: Some("herdr".into()),
            target: "main".into(),
            remote: None,
        }
    }

    #[test]
    fn mobile_agent_detail_includes_tab_context_when_available() {
        let entry = agent_entry(Some("mobile-state"), Some("pi"));

        assert_eq!(mobile_agent_detail(&entry), "  mobile-state · idle · pi");
    }

    #[test]
    fn mobile_agent_detail_keeps_existing_compact_detail_without_tab_context() {
        let entry = agent_entry(None, Some("pi"));

        assert_eq!(mobile_agent_detail(&entry), "  idle · pi");
    }

    #[test]
    fn mobile_agent_detail_appends_promoted_header_fields() {
        let mut entry = agent_entry(None, Some("pi"));
        entry.header_fields = vec![
            ("build".to_string(), "73%".to_string()),
            ("pg".to_string(), "up".to_string()),
        ];

        assert_eq!(
            mobile_agent_detail(&entry),
            "  idle · pi · build 73% · pg up"
        );
    }

    #[tokio::test]
    async fn mobile_header_uses_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-mobile-header-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().cwd = stale_cwd;
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_menu_hit_area = Rect::new(30, 0, 10, 2);

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let backend = ratatui::backend::TestBackend::new(40, 2);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_header(&app, &runtime_registry, frame, Rect::new(0, 0, 40, 2))
            })
            .unwrap();
        let row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert!(row.contains("herdr"), "header row: {row:?}");
        assert!(
            !row.contains("issue-264-nix-support"),
            "header row: {row:?}"
        );
    }
}
