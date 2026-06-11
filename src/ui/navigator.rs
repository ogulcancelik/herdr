use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::{
    scrollbar::{render_scrollbar, should_show_scrollbar},
    status::{agent_icon, state_label_color},
    widgets::{panel_contrast_fg, render_panel_shell},
};
use crate::app::state::{AppState, NavigatorRow, NavigatorStateFilter, NavigatorTarget};

pub(super) fn render_navigator_overlay(app: &AppState, frame: &mut Frame) {
    let popup = app.navigator_popup_rect();
    let Some(inner) = render_panel_shell(frame, popup, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };

    let search = app.navigator_search_rect();
    let body = app.navigator_body_rect();
    let detail = app.navigator_detail_rect();
    let footer = app.navigator_footer_rect();
    render_search(app, frame, search);

    if body.height > 0 {
        render_separator(frame, Rect::new(inner.x, search.y + 1, inner.width, 1), app);
        render_rows(app, frame, body);
        render_navigator_scrollbar(app, frame, body);
    }
    render_detail(app, frame, detail);
    render_footer(app, frame, footer);
}

fn render_search(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let focus_style = if app.navigator.search_focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    let count = app
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.tabs.iter())
        .map(|tab| tab.panes.len())
        .sum::<usize>();
    let mut spans = vec![Span::styled(" / ", focus_style)];
    let query = app.navigator.query.trim();
    match app.navigator.state_filter {
        Some(NavigatorStateFilter::Blocked) => push_state_chip(
            &mut spans,
            crate::detect::AgentState::Blocked,
            true,
            app.spinner_tick,
            "blocked",
            app,
        ),
        Some(NavigatorStateFilter::Working) => push_state_chip(
            &mut spans,
            crate::detect::AgentState::Working,
            true,
            app.spinner_tick,
            "working",
            app,
        ),
        Some(NavigatorStateFilter::Idle) => push_state_chip(
            &mut spans,
            crate::detect::AgentState::Idle,
            true,
            app.spinner_tick,
            "idle",
            app,
        ),
        Some(NavigatorStateFilter::Done) => push_state_chip(
            &mut spans,
            crate::detect::AgentState::Idle,
            false,
            app.spinner_tick,
            "done",
            app,
        ),
        None if query.is_empty() => spans.push(Span::styled(
            "search panes",
            Style::default().fg(p.overlay0),
        )),
        None => spans.push(Span::styled(query.to_string(), Style::default().fg(p.text))),
    }
    spans.push(Span::styled(
        format!(
            "{count:>width$} panes",
            width = area.width.saturating_sub(16) as usize
        ),
        Style::default().fg(p.overlay0),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_state_chip(
    spans: &mut Vec<Span<'static>>,
    state: crate::detect::AgentState,
    seen: bool,
    tick: u32,
    label: &'static str,
    app: &AppState,
) {
    let (icon, icon_style) = agent_icon(state, seen, tick, &app.palette);
    spans.push(Span::styled(icon, icon_style.add_modifier(Modifier::BOLD)));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(state_label_color(state, seen, &app.palette))
            .add_modifier(Modifier::BOLD),
    ));
}

fn render_separator(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(app.palette.surface1)),
        area,
    );
}

fn render_rows(app: &AppState, frame: &mut Frame, body: Rect) {
    let rows = app.navigator_rows();
    let start = app.navigator.scroll.min(rows.len());
    let end = rows.len().min(start.saturating_add(body.height as usize));
    for (visible_idx, row) in rows[start..end].iter().enumerate() {
        let idx = start + visible_idx;
        let y = body.y + visible_idx as u16;
        let rect = Rect::new(body.x, y, body.width, 1);
        let selected = idx == app.navigator.selected;
        render_row(app, frame, rect, row, selected);
    }
}

fn render_row(app: &AppState, frame: &mut Frame, rect: Rect, row: &NavigatorRow, selected: bool) {
    let p = &app.palette;
    frame.render_widget(Clear, rect);
    let base_style = if selected {
        Style::default().bg(p.accent).fg(panel_contrast_fg(p))
    } else {
        Style::default().bg(p.panel_bg).fg(p.text)
    };
    let dim_style = if selected {
        base_style
    } else {
        Style::default().fg(p.overlay0).bg(p.panel_bg)
    };
    let text_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else if row.is_current {
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.subtext0).bg(p.panel_bg)
    };
    let (status_icon, status_style) = agent_icon(row.status, row.seen, app.spinner_tick, p);
    let status_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else {
        status_style.bg(p.panel_bg)
    };

    let prefix = if row.is_workspace {
        if row.expanded {
            "▾"
        } else {
            "▸"
        }
    } else if row.depth > 0 {
        "├─"
    } else {
        "  "
    };
    let current = if row.is_current { "◆" } else { " " };
    let marker = if selected { "→" } else { " " };
    let indent = "  ".repeat(row.depth as usize);
    let left_fixed = format!(" {indent}{prefix} {marker} {current} ");
    let meta_width = metadata_width(rect.width);
    let left_budget = rect
        .width
        .saturating_sub(meta_width)
        .saturating_sub(left_fixed.chars().count() as u16)
        .saturating_sub(3) as usize;
    let title = truncate_text(&row.label, left_budget);

    let spans = vec![
        Span::styled(left_fixed, dim_style),
        Span::styled(status_icon, status_style),
        Span::raw(" "),
        Span::styled(title, text_style),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)).style(base_style), rect);

    if meta_width > 0 {
        let meta_rect = Rect::new(
            rect.x + rect.width.saturating_sub(meta_width),
            rect.y,
            meta_width,
            1,
        );
        let meta = truncate_text(&row.meta, meta_width.saturating_sub(2) as usize);
        let meta_style = if selected {
            base_style
        } else if row.is_workspace || row.is_tab {
            Style::default().fg(p.overlay0).bg(p.panel_bg)
        } else {
            Style::default()
                .fg(state_label_color(row.status, row.seen, p))
                .bg(p.panel_bg)
        };
        frame.render_widget(
            Paragraph::new(format!(" {meta}")).style(meta_style),
            meta_rect,
        );
    }
}

fn render_navigator_scrollbar(app: &AppState, frame: &mut Frame, body: Rect) {
    if body.width <= 1 || body.height == 0 {
        return;
    }
    let rows = app.navigator_rows().len();
    let viewport = body.height as usize;
    if rows <= viewport {
        return;
    }
    let metrics = crate::pane::ScrollMetrics {
        viewport_rows: viewport,
        offset_from_bottom: rows
            .saturating_sub(viewport)
            .saturating_sub(app.navigator.scroll),
        max_offset_from_bottom: rows.saturating_sub(viewport),
    };
    if !should_show_scrollbar(metrics) {
        return;
    }
    let track = Rect::new(body.x + body.width - 1, body.y, 1, body.height);
    render_scrollbar(
        frame,
        metrics,
        track,
        app.palette.surface_dim,
        app.palette.overlay0,
        "▕",
    );
}

fn metadata_width(width: u16) -> u16 {
    if width >= 90 {
        28
    } else if width >= 68 {
        20
    } else if width >= 52 {
        14
    } else {
        0
    }
}

fn render_detail(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    render_separator(frame, area, app);
    let detail = selected_detail(app);
    if detail.is_empty() {
        return;
    }
    let text = middle_elide(&detail, area.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(format!(" {text}")).style(Style::default().fg(app.palette.overlay0)),
        area,
    );
}

fn selected_detail(app: &AppState) -> String {
    let rows = app.navigator_rows();
    let Some(row) = rows.get(app.navigator.selected) else {
        return String::new();
    };
    match row.target {
        NavigatorTarget::Workspace { ws_idx } => workspace_detail(app, ws_idx),
        NavigatorTarget::Tab { ws_idx, tab_idx } => tab_detail(app, ws_idx, tab_idx),
        NavigatorTarget::Pane {
            ws_idx,
            tab_idx,
            pane_id,
        } => pane_detail(app, ws_idx, tab_idx, pane_id),
    }
}

fn workspace_detail(app: &AppState, ws_idx: usize) -> String {
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return String::new();
    };
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    let label = ws.display_name_from(&app.terminals, &terminal_runtimes);
    let pane_count = ws.tabs.iter().map(|tab| tab.panes.len()).sum::<usize>();
    let mut parts = vec![label, format!("{pane_count} panes")];
    if !rowless_workspace_activity(app, ws_idx).is_empty() {
        parts.push(rowless_workspace_activity(app, ws_idx));
    }
    parts.join(" · ")
}

fn tab_detail(app: &AppState, ws_idx: usize, tab_idx: usize) -> String {
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return String::new();
    };
    let Some(tab) = ws.tabs.get(tab_idx) else {
        return String::new();
    };
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    let mut parts = vec![
        ws.display_name_from(&app.terminals, &terminal_runtimes),
        format!("tab: {}", tab.display_name()),
        format!("{} panes", tab.panes.len()),
    ];
    let rows = app.navigator_rows();
    if let Some(meta) = rows
        .into_iter()
        .find(|row| matches!(row.target, NavigatorTarget::Tab { ws_idx: row_ws_idx, tab_idx: row_tab_idx } if row_ws_idx == ws_idx && row_tab_idx == tab_idx))
        .map(|row| row.meta)
        .filter(|meta| !meta.is_empty())
    {
        parts.push(meta);
    }
    parts.join(" · ")
}

fn pane_detail(
    app: &AppState,
    ws_idx: usize,
    tab_idx: usize,
    pane_id: crate::layout::PaneId,
) -> String {
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return String::new();
    };
    let Some(tab) = ws.tabs.get(tab_idx) else {
        return String::new();
    };
    let terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::new();
    let mut parts = vec![ws.display_name_from(&app.terminals, &terminal_runtimes)];
    if ws.tabs.len() > 1 {
        parts.push(format!("tab: {}", tab.display_name()));
    }
    if let Some(pane_number) = ws.public_pane_number(pane_id) {
        parts.push(format!("pane {pane_number}"));
    }
    if let Some(terminal_id) = tab.terminal_id(pane_id) {
        if let Some(terminal) = app.terminals.get(terminal_id) {
            let presentation = terminal.effective_presentation();
            if let Some(title) = presentation.title {
                parts.push(title);
            }
            let display_agent = terminal.effective_display_agent();
            if let Some(agent) = display_agent.as_deref().or_else(|| {
                terminal
                    .agent_name
                    .as_deref()
                    .or_else(|| terminal.effective_agent_label())
            }) {
                parts.push(agent.to_string());
                let seen = tab
                    .panes
                    .get(&pane_id)
                    .map(|pane| pane.seen)
                    .unwrap_or(true);
                let state = row_state(app, ws_idx, tab_idx, pane_id);
                let status = presentation
                    .state_labels
                    .get(display_state(state, seen))
                    .cloned()
                    .unwrap_or_else(|| display_state(state, seen).to_string());
                parts.push(status);
            } else {
                parts.push("shell".to_string());
            }
            if let Some(status) = terminal.effective_custom_status() {
                parts.push(status.to_string());
            }
            if let Some(summary) = crate::terminal::compact_header_fields(
                &terminal.active_header_fields(),
                crate::app::state::AppState::NAVIGATOR_HEADER_FIELD_VALUE_COLS,
            ) {
                parts.push(summary);
            }
        }
    }
    parts.join(" · ")
}

fn rowless_workspace_activity(app: &AppState, ws_idx: usize) -> String {
    app.navigator_rows()
        .into_iter()
        .find(|row| matches!(row.target, NavigatorTarget::Workspace { ws_idx: row_ws_idx } if row_ws_idx == ws_idx))
        .map(|row| row.meta)
        .unwrap_or_default()
}

fn row_state(
    app: &AppState,
    ws_idx: usize,
    tab_idx: usize,
    pane_id: crate::layout::PaneId,
) -> crate::detect::AgentState {
    app.workspaces
        .get(ws_idx)
        .and_then(|ws| ws.tabs.get(tab_idx))
        .and_then(|tab| tab.terminal_id(pane_id))
        .and_then(|terminal_id| app.terminals.get(terminal_id))
        .map(|terminal| terminal.state)
        .unwrap_or(crate::detect::AgentState::Unknown)
}

fn display_state(state: crate::detect::AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (crate::detect::AgentState::Blocked, _) => "blocked",
        (crate::detect::AgentState::Working, _) => "working",
        (crate::detect::AgentState::Idle, false) => "done",
        (crate::detect::AgentState::Idle, true) => "idle",
        (crate::detect::AgentState::Unknown, _) => "unknown",
    }
}

fn middle_elide(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let left = max_width.saturating_sub(1) / 2;
    let right = max_width.saturating_sub(1).saturating_sub(left);
    let prefix: String = text.chars().take(left).collect();
    let suffix: String = text
        .chars()
        .rev()
        .take(right)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn render_footer(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    let key = Style::default().fg(p.accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(p.overlay0);
    let line = Line::from(vec![
        Span::styled(" enter", key),
        Span::styled(" switch  ", dim),
        Span::styled("/", key),
        Span::styled(" search  ", dim),
        Span::styled("b/w/i/d/a", key),
        Span::styled(" states  ", dim),
        Span::styled("j/k/↑↓", key),
        Span::styled(" move  ", dim),
        Span::styled("esc", key),
        Span::styled(" close", dim),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn truncate_text(text: &str, max_width: usize) -> String {
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
