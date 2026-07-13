use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::sidebar::{
    agent_panel_entries, agent_panel_entries_from, grouped_child_display_label,
    next_entry_is_indented_workspace, workspace_list_entries_expanded, AgentPanelEntry,
    WorkspaceListEntry,
};
use super::status::{agent_icon, state_dot};
use super::text::{display_width_u16, truncate_end};
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

/// Doc-row height of the agents section (title + two rows per agent), or 0 when
/// there are no agents so we don't show an empty header. The switcher leads with
/// agents, so every section below it is offset by this.
fn mobile_agents_block_height(app: &AppState) -> usize {
    let count = agent_panel_entries(app).len();
    if count == 0 {
        0
    } else {
        1 + count * 2
    }
}

pub(crate) fn mobile_switcher_workspace_doc_range(
    app: &AppState,
    idx: usize,
) -> std::ops::Range<usize> {
    // Spaces render in grouped order, so a workspace's row position is its index
    // in the entry list, not its raw array index.
    let pos = workspace_list_entries_expanded(app)
        .iter()
        .position(|WorkspaceListEntry::Workspace { ws_idx, .. }| *ws_idx == idx)
        .unwrap_or(idx);
    // spaces sit after the agents block, then a title + "new workspace" row.
    let start = mobile_agents_block_height(app) + 2 + pos * 2;
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

    // Agents lead the switcher: the primary job is switching between running
    // agents. Spaces/tabs/create actions follow for navigation and management.
    // The section is omitted entirely when there are no agents.
    let agents = agent_panel_entries(app);
    if !agents.is_empty() {
        cursor += 1; // agents title
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
    }

    cursor += 1; // spaces title
    if doc_row == cursor {
        return Some(MobileSwitcherTarget::NewWorkspace);
    }
    cursor += 1;
    // Spaces render in grouped (worktree-tree) order, which differs from raw
    // array order, so map the clicked row to the entry's real workspace index.
    let space_entries = workspace_list_entries_expanded(app);
    let spaces_end = cursor + space_entries.len() * 2;
    if doc_row >= cursor && doc_row < spaces_end {
        let entry_idx = (doc_row - cursor) / 2;
        return space_entries.get(entry_idx).map(
            |WorkspaceListEntry::Workspace { ws_idx, .. }| MobileSwitcherTarget::Workspace(*ws_idx),
        );
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
    let tab_label = mobile_tab_status(ws);
    let row1 = Rect::new(area.x, area.y, area.width, 1);
    let tab_w = display_width_u16(&tab_label)
        .saturating_add(1)
        .min(area.width);
    let name_w = area.width.saturating_sub(tab_w);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(dot, dot_style.bg(p.panel_bg)),
            Span::raw(" "),
            Span::styled(
                truncate_end(
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
            Paragraph::new(agent_summary_line(app, p, area.width)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

fn mobile_tab_status(ws: &crate::workspace::Workspace) -> String {
    let tab_label = ws
        .tab_display_name(ws.active_tab)
        .unwrap_or_else(|| (ws.active_tab + 1).to_string());
    if ws.tabs.len() <= 1 {
        format!("tab {tab_label}")
    } else {
        format!("tab {tab_label} · {}/{}", ws.active_tab + 1, ws.tabs.len())
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

    // Attention badge: a blocked agent anywhere makes the button itself read as
    // "tap me" without the user reading the summary row.
    if global_agent_counts(app).blocked > 0 {
        let bx = area.x + area.width.saturating_sub(1);
        frame.buffer_mut()[(bx, area.y)]
            .set_symbol("●")
            .set_style(Style::default().fg(p.red).bg(p.surface0));
    }
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
    // Derive spaces height from the same entry list the render/hit-test use so
    // the three never disagree.
    let spaces_h = 2 + workspace_list_entries_expanded(app).len() * 2;
    let tabs_h = app
        .active
        .and_then(|idx| app.workspaces.get(idx))
        .map(|ws| 2 + ws.tabs.len())
        .unwrap_or(0);
    let agents_h = mobile_agents_block_height(app);
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

    let entries = agent_panel_entries_from(app, terminal_runtimes);
    if !entries.is_empty() {
        let focused_agent = app.active.and_then(|ws_idx| {
            let ws = app.workspaces.get(ws_idx)?;
            ws.focused_pane_id()
                .map(|pane_id| (ws_idx, ws.active_tab, pane_id))
        });
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
                    truncate_end(
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
                truncate_end(&detail, content.width as usize),
                p.overlay0,
            );
            doc_y += 2;
        }
    }

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
    let space_entries = workspace_list_entries_expanded(app);
    for (entry_idx, WorkspaceListEntry::Workspace { ws_idx, indented }) in
        space_entries.iter().enumerate()
    {
        let Some(ws) = app.workspaces.get(*ws_idx) else {
            continue;
        };
        let active = Some(*ws_idx) == app.active;
        let selected = *ws_idx == app.selected;
        let bg = mobile_item_bg(selected, active, p);
        let (state, seen) = ws.aggregate_state(&app.terminals);
        let (dot, dot_style) = state_dot(state, seen, p);

        let mut title_spans = vec![Span::styled("  ", Style::default().bg(bg))];
        // Worktrees of the same space render as branches off their parent, so a
        // child gets an L/T connector on its name row and a matching vertical
        // continuation on its detail row.
        let detail_prefix = if *indented {
            let last_child = !next_entry_is_indented_workspace(&space_entries, entry_idx);
            title_spans.push(Span::styled(
                if last_child { "└─ " } else { "├─ " },
                Style::default().fg(p.overlay0).bg(bg),
            ));
            if last_child {
                "       "
            } else {
                "  │    "
            }
        } else {
            "  "
        };

        title_spans.push(Span::styled(dot, dot_style.bg(bg)));
        title_spans.push(Span::styled(" ", Style::default().bg(bg)));
        let raw_label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let name = if *indented {
            grouped_child_display_label(
                &raw_label,
                ws.branch().as_deref(),
                ws.custom_name.is_some(),
            )
        } else {
            raw_label
        };
        let name_budget = content.width.saturating_sub(if *indented { 8 } else { 5 }) as usize;
        title_spans.push(Span::styled(
            truncate_end(&name, name_budget),
            Style::default()
                .fg(p.text)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));

        let detail = format!(
            "{detail_prefix}{} · {}",
            ws.branch().unwrap_or_else(|| "shell".into()),
            mobile_tab_status(ws)
        );
        render_two_line_item(
            frame,
            viewport,
            content,
            doc_y,
            app.mobile_switcher_scroll,
            bg,
            Line::from(title_spans),
            truncate_end(&detail, content.width as usize),
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
            let display_name = ws
                .tab_display_name(idx)
                .unwrap_or_else(|| (idx + 1).to_string());
            let label = if tab.is_auto_named() {
                format!("tab {display_name}")
            } else {
                format!("{} · {display_name}", idx + 1)
            };
            let title = Line::from(vec![
                Span::styled("  ", Style::default().bg(bg)),
                Span::styled(
                    truncate_end(&label, content.width.saturating_sub(3) as usize),
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

/// Agent state counts across every workspace. The mobile header is global on
/// purpose: while you stare at one terminal, a blocked agent anywhere should
/// still surface.
#[derive(Debug, Default, Clone, Copy)]
struct GlobalAgentCounts {
    blocked: usize,
    done: usize,
    working: usize,
    idle: usize,
}

impl GlobalAgentCounts {
    fn total(&self) -> usize {
        self.blocked + self.done + self.working + self.idle
    }

    fn any_pending(&self) -> bool {
        self.blocked > 0 || self.done > 0 || self.working > 0
    }
}

fn global_agent_counts(app: &AppState) -> GlobalAgentCounts {
    let mut counts = GlobalAgentCounts::default();
    for entry in agent_panel_entries(app) {
        match (entry.state, entry.seen) {
            (AgentState::Blocked, _) => counts.blocked += 1,
            (AgentState::Idle, false) => counts.done += 1,
            (AgentState::Working, _) => counts.working += 1,
            (AgentState::Idle, true) => counts.idle += 1,
            (AgentState::Unknown, _) => {}
        }
    }
    counts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryTone {
    Blocked,
    Done,
    Working,
    Idle,
    Muted,
}

/// Ordered, non-zero breakdown for the header roll-up: attention states lead
/// (blocked → done → working → idle). Pure so it can be unit-tested.
fn agent_summary_segments(counts: GlobalAgentCounts) -> Vec<(String, SummaryTone)> {
    if counts.total() == 0 {
        return vec![("no agents".to_string(), SummaryTone::Muted)];
    }
    if !counts.any_pending() {
        return vec![("all idle".to_string(), SummaryTone::Muted)];
    }
    let mut segments = Vec::new();
    if counts.blocked > 0 {
        segments.push((
            format!("◉ {} blocked", counts.blocked),
            SummaryTone::Blocked,
        ));
    }
    if counts.done > 0 {
        segments.push((format!("● {} done", counts.done), SummaryTone::Done));
    }
    if counts.working > 0 {
        segments.push((format!("{} working", counts.working), SummaryTone::Working));
    }
    if counts.idle > 0 {
        segments.push((format!("{} idle", counts.idle), SummaryTone::Idle));
    }
    segments
}

/// Greedily keep the most-urgent segments that fit `max_width` (counting the
/// leading space and " · " separators) and report whether any were dropped.
/// Segments are ordered by urgency, so the dropped tail is always the least
/// important state.
fn fit_summary_segments(
    segments: Vec<(String, SummaryTone)>,
    max_width: usize,
) -> (Vec<(String, SummaryTone)>, bool) {
    let mut shown = Vec::new();
    let mut used = 1usize; // leading space
    for (idx, segment) in segments.iter().enumerate() {
        let sep = if idx > 0 { 3 } else { 0 }; // " · "
        let seg_w = segment.0.chars().count();
        if used + sep + seg_w > max_width {
            break;
        }
        used += sep + seg_w;
        shown.push(segment.clone());
    }
    let truncated = shown.len() < segments.len();
    (shown, truncated)
}

fn agent_summary_line(app: &AppState, p: &Palette, max_width: u16) -> Line<'static> {
    let segments = agent_summary_segments(global_agent_counts(app));
    let (shown, truncated) = fit_summary_segments(segments, max_width as usize);

    let mut spans = vec![Span::styled(" ", Style::default().bg(p.panel_bg))];
    let mut used = 1usize;
    for (idx, (text, tone)) in shown.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(p.overlay0).bg(p.panel_bg),
            ));
            used += 3;
        }
        // Only the leading (most urgent) segment keeps its state color; the
        // rest stay dim so the urgent count is the loud thing.
        let style = if idx == 0 {
            let color = match tone {
                SummaryTone::Blocked => p.red,
                SummaryTone::Done => p.blue,
                SummaryTone::Working => p.yellow,
                SummaryTone::Idle | SummaryTone::Muted => p.overlay1,
            };
            let style = Style::default().fg(color).bg(p.panel_bg);
            if tone == SummaryTone::Muted {
                style
            } else {
                style.add_modifier(Modifier::BOLD)
            }
        } else {
            Style::default().fg(p.overlay1).bg(p.panel_bg)
        };
        used += text.chars().count();
        spans.push(Span::styled(text, style));
    }
    if truncated && used + 2 <= max_width as usize {
        spans.push(Span::styled(
            " …",
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        ));
    }
    Line::from(spans)
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
            pane_label: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_label: agent_label.map(str::to_string),
            agent: agent_label.and_then(crate::detect::parse_agent_label),
            state: AgentState::Idle,
            seen: true,
            last_agent_state_change_seq: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn agent_summary_leads_with_attention_states_in_priority_order() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let segments = agent_summary_segments(counts);
        let labels: Vec<&str> = segments.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            labels,
            vec!["◉ 2 blocked", "● 1 done", "2 working", "1 idle"]
        );
        assert_eq!(segments[0].1, SummaryTone::Blocked);
    }

    #[test]
    fn agent_summary_hides_empty_categories() {
        let counts = GlobalAgentCounts {
            done: 1,
            working: 2,
            ..Default::default()
        };
        let labels: Vec<String> = agent_summary_segments(counts)
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(
            labels,
            vec!["● 1 done".to_string(), "2 working".to_string()]
        );
    }

    #[test]
    fn agent_summary_collapses_to_all_idle_without_attention() {
        let counts = GlobalAgentCounts {
            idle: 3,
            ..Default::default()
        };
        assert_eq!(
            agent_summary_segments(counts),
            vec![("all idle".to_string(), SummaryTone::Muted)]
        );
    }

    #[test]
    fn agent_summary_drops_least_urgent_segments_when_narrow() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let (shown, truncated) = fit_summary_segments(agent_summary_segments(counts), 24);
        let labels: Vec<&str> = shown.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(labels, vec!["◉ 2 blocked", "● 1 done"]);
        assert!(truncated);
    }

    #[test]
    fn agent_summary_keeps_all_segments_when_wide_enough() {
        let counts = GlobalAgentCounts {
            blocked: 2,
            done: 1,
            working: 2,
            idle: 1,
        };
        let (shown, truncated) = fit_summary_segments(agent_summary_segments(counts), 60);
        assert_eq!(shown.len(), 4);
        assert!(!truncated);
    }

    #[test]
    fn agent_summary_reports_no_agents_when_empty() {
        assert_eq!(
            agent_summary_segments(GlobalAgentCounts::default()),
            vec![("no agents".to_string(), SummaryTone::Muted)]
        );
    }

    #[test]
    fn switcher_leads_with_agents_and_shifts_spaces_below() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("agents-first");
        workspace.test_add_tab(None); // two tabs -> two agent panes
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.agent_name = Some("pi".to_string());
            terminal.state = AgentState::Working;
        }
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        assert_eq!(agent_panel_entries(&app).len(), 2);
        // agents title (1) + 2 agents * 2 rows = 5, then spaces title + "new
        // workspace" (2) before the first workspace ribbon at doc row 7.
        assert_eq!(mobile_switcher_workspace_doc_range(&app, 0).start, 7);

        let viewport = mobile_switcher_areas(&app).viewport;
        let agent_hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 1);
        assert!(matches!(
            agent_hit,
            Some(MobileSwitcherTarget::Agent { .. })
        ));
        let workspace_hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 7);
        assert_eq!(workspace_hit, Some(MobileSwitcherTarget::Workspace(0)));
    }

    fn worktree_workspace(name: &str, key: &str, linked: bool) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: linked,
        });
        ws
    }

    #[test]
    fn switcher_spaces_follow_grouped_worktree_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            worktree_workspace("main", "repo-key", false),
            crate::workspace::Workspace::test_new("other"),
            worktree_workspace("feature", "repo-key", true),
        ];
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        // Grouped order pulls the worktree (idx 2) up under its parent (idx 0),
        // ahead of the unrelated "other" workspace (idx 1): rows are main,
        // feature, other.
        assert_eq!(mobile_switcher_workspace_doc_range(&app, 2).start, 4);
        assert_eq!(mobile_switcher_workspace_doc_range(&app, 1).start, 6);

        let viewport = mobile_switcher_areas(&app).viewport;
        // The second space row on screen is the worktree, not workspaces[1].
        let hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 4);
        assert_eq!(hit, Some(MobileSwitcherTarget::Workspace(2)));

        // Mobile ignores collapse: even with the space folded on desktop, the
        // worktree child still renders in the same position.
        app.collapsed_space_keys.insert("repo-key".to_string());
        assert_eq!(mobile_switcher_workspace_doc_range(&app, 2).start, 4);
        let hit = mobile_switcher_target_at(&app, viewport.x + 2, viewport.y + 4);
        assert_eq!(hit, Some(MobileSwitcherTarget::Workspace(2)));
    }

    #[test]
    fn switcher_without_agents_keeps_spaces_first() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("shell-only")];
        app.active = Some(0);
        app.selected = 0;

        // No attached terminals -> no agents -> no agents header, spaces lead.
        assert_eq!(agent_panel_entries(&app).len(), 0);
        assert_eq!(mobile_switcher_workspace_doc_range(&app, 0).start, 2);
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
    fn mobile_tab_status_uses_compact_tab_label_and_position() {
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        assert!(workspace.close_tab(removed_tab));
        workspace.active_tab = 1;

        assert_eq!(mobile_tab_status(&workspace), "tab 2 · 2/2");
    }

    #[test]
    fn mobile_switcher_uses_compact_tab_label_for_auto_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = crate::workspace::Workspace::test_new("mobile-tabs");
        let removed_tab = workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        assert!(workspace.close_tab(removed_tab));
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.view.mobile_header_rect = Rect::new(0, 0, 40, 2);
        app.view.terminal_area = Rect::new(0, 2, 40, 18);

        let backend = ratatui::backend::TestBackend::new(40, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_mobile_panel(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 20),
                )
            })
            .unwrap();

        let row = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 10)].symbol())
            .collect::<String>();

        assert!(row.contains("tab 2"), "mobile tab row: {row:?}");
        assert!(!row.contains("tab 3"), "mobile tab row: {row:?}");
    }

    #[cfg(unix)]
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
            &crate::pane::PaneLaunchEnv::default(),
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
