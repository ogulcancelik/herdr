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
    text::{display_width_u16, middle_elide, truncate_end},
    widgets::{panel_contrast_fg, render_panel_shell},
};
use crate::app::state::{
    navigator_display_lines, AppState, NavigatorDisplayLine, NavigatorRow, NavigatorStateFilter,
    NavigatorTarget,
};
use crate::terminal::TerminalRuntimeRegistry;

pub(super) fn render_navigator_overlay(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
) {
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
        let rows = app.navigator_rows_from(terminal_runtimes);
        let lines = navigator_display_lines(&rows);
        render_separator(frame, Rect::new(inner.x, search.y + 1, inner.width, 1), app);
        render_rows(app, &rows, &lines, frame, body);
        render_navigator_scrollbar(app, lines.len(), frame, body);
    }
    render_detail(app, terminal_runtimes, frame, detail);
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

fn render_rows(
    app: &AppState,
    rows: &[NavigatorRow],
    lines: &[NavigatorDisplayLine],
    frame: &mut Frame,
    body: Rect,
) {
    let start = app.navigator.scroll.min(lines.len());
    let end = lines.len().min(start.saturating_add(body.height as usize));
    for (visible_idx, line) in lines[start..end].iter().enumerate() {
        let NavigatorDisplayLine::Row(idx) = *line else {
            continue;
        };
        let y = body.y + visible_idx as u16;
        let rect = Rect::new(body.x, y, body.width, 1);
        let selected = idx == app.navigator.selected;
        render_row(app, frame, rect, rows, idx, selected);
    }
}

fn render_row(
    app: &AppState,
    frame: &mut Frame,
    rect: Rect,
    rows: &[NavigatorRow],
    idx: usize,
    selected: bool,
) {
    let row = &rows[idx];
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
    let filter_active =
        app.navigator.state_filter.is_some() || !app.navigator.query.trim().is_empty();
    let context_only = filter_active && !row.matched;
    let text_style = if selected {
        base_style.add_modifier(Modifier::BOLD)
    } else if context_only {
        let dimmed = Style::default().fg(p.overlay0).bg(p.panel_bg);
        if row.is_workspace {
            dimmed.add_modifier(Modifier::BOLD)
        } else {
            dimmed
        }
    } else if row.is_workspace {
        Style::default()
            .fg(p.accent)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD)
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
    } else if context_only {
        Style::default().fg(p.overlay0).bg(p.panel_bg)
    } else {
        status_style.bg(p.panel_bg)
    };

    let prefix = tree_prefix(rows, idx);
    let current = if row.is_current { "◆" } else { " " };
    let gutter = format!(" {current} ");
    let gutter_style = if selected {
        base_style
    } else if row.is_current {
        Style::default().fg(p.accent).bg(p.panel_bg)
    } else {
        dim_style
    };
    // Branch glyphs recede one shade below the workspace caret so the
    // structure stays behind the labels.
    let tree_style = if selected {
        base_style
    } else if row.is_workspace {
        dim_style
    } else {
        Style::default().fg(p.surface1).bg(p.panel_bg)
    };
    let meta_width = metadata_width(rect.width);
    let left_budget = rect
        .width
        .saturating_sub(meta_width)
        .saturating_sub(display_width_u16(&format!("{gutter}{prefix} ")))
        .saturating_sub(3) as usize;
    let title = truncate_end(&row.label, left_budget);

    let spans = vec![
        Span::styled(gutter, gutter_style),
        Span::styled(prefix, tree_style),
        Span::styled(" ", base_style),
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
        let meta = truncate_end(&row.meta, meta_width.saturating_sub(2) as usize);
        let meta_style = if selected {
            base_style
        } else if context_only || row.is_workspace || row.is_tab {
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

/// Tree prefix for a navigator row: expand caret for workspaces, connected
/// branch glyphs for children (`├──`, `└──` for the last sibling, with `│`
/// continuation lines under ancestors that have more siblings below).
fn tree_prefix(rows: &[NavigatorRow], idx: usize) -> String {
    let row = &rows[idx];
    if row.is_workspace {
        return if row.expanded { "▾" } else { "▸" }.to_string();
    }
    if row.depth == 0 {
        return "  ".to_string();
    }
    let mut prefix = String::new();
    for level in 1..row.depth {
        prefix.push_str(if has_following_sibling_at_depth(rows, idx, level) {
            "│  "
        } else {
            "   "
        });
    }
    prefix.push_str(if has_following_sibling_at_depth(rows, idx, row.depth) {
        "├──"
    } else {
        "└──"
    });
    prefix
}

/// Whether another row at `depth` follows `idx` before the subtree at that
/// depth ends (a row shallower than `depth` closes the subtree).
fn has_following_sibling_at_depth(rows: &[NavigatorRow], idx: usize, depth: u8) -> bool {
    rows[idx + 1..]
        .iter()
        .take_while(|row| row.depth >= depth)
        .any(|row| row.depth == depth)
}

fn render_navigator_scrollbar(app: &AppState, line_count: usize, frame: &mut Frame, body: Rect) {
    if body.width <= 1 || body.height == 0 {
        return;
    }
    let viewport = body.height as usize;
    if line_count <= viewport {
        return;
    }
    let metrics = crate::pane::ScrollMetrics {
        viewport_rows: viewport,
        offset_from_bottom: line_count
            .saturating_sub(viewport)
            .saturating_sub(app.navigator.scroll),
        max_offset_from_bottom: line_count.saturating_sub(viewport),
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

fn render_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    render_separator(frame, area, app);
    let detail = selected_detail(app, terminal_runtimes);
    if detail.is_empty() {
        return;
    }
    let text = middle_elide(&detail, area.width.saturating_sub(2) as usize);
    frame.render_widget(
        Paragraph::new(format!(" {text}")).style(Style::default().fg(app.palette.overlay0)),
        area,
    );
}

fn selected_detail(app: &AppState, terminal_runtimes: &TerminalRuntimeRegistry) -> String {
    let rows = app.navigator_rows_from(terminal_runtimes);
    let Some(row) = rows.get(app.navigator.selected) else {
        return String::new();
    };
    match row.target {
        NavigatorTarget::Workspace { ws_idx } => workspace_detail(app, terminal_runtimes, ws_idx),
        NavigatorTarget::Tab { ws_idx, tab_idx } => {
            tab_detail(app, terminal_runtimes, ws_idx, tab_idx)
        }
        NavigatorTarget::Pane {
            ws_idx,
            tab_idx,
            pane_id,
        } => pane_detail(app, terminal_runtimes, ws_idx, tab_idx, pane_id),
    }
}

fn workspace_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) -> String {
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return String::new();
    };
    let label = ws.display_name_from(&app.terminals, terminal_runtimes);
    let pane_count = ws.tabs.iter().map(|tab| tab.panes.len()).sum::<usize>();
    let mut parts = vec![label, format!("{pane_count} panes")];
    if !rowless_workspace_activity(app, terminal_runtimes, ws_idx).is_empty() {
        parts.push(rowless_workspace_activity(app, terminal_runtimes, ws_idx));
    }
    parts.join(" · ")
}

fn tab_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
    tab_idx: usize,
) -> String {
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return String::new();
    };
    let Some(tab) = ws.tabs.get(tab_idx) else {
        return String::new();
    };
    let mut parts = vec![
        ws.display_name_from(&app.terminals, terminal_runtimes),
        format!(
            "tab: {}",
            ws.tab_display_name(tab_idx)
                .unwrap_or_else(|| (tab_idx + 1).to_string())
        ),
        format!("{} panes", tab.panes.len()),
    ];
    let rows = app.navigator_rows_from(terminal_runtimes);
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
    terminal_runtimes: &TerminalRuntimeRegistry,
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
    let mut parts = vec![ws.display_name_from(&app.terminals, terminal_runtimes)];
    if ws.tabs.len() > 1 {
        parts.push(format!(
            "tab: {}",
            ws.tab_display_name(tab_idx)
                .unwrap_or_else(|| (tab_idx + 1).to_string())
        ));
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
        }
    }
    parts.join(" · ")
}

fn rowless_workspace_activity(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    ws_idx: usize,
) -> String {
    app.navigator_rows_from(terminal_runtimes)
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

fn render_footer(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    let p = &app.palette;
    let key = Style::default().fg(p.accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(p.overlay0);
    let line = if app.navigator.search_focused {
        Line::from(vec![
            Span::styled(" enter", key),
            Span::styled(" switch  ", dim),
            Span::styled("↑↓", key),
            Span::styled(" move  ", dim),
            Span::styled("ctrl+u", key),
            Span::styled(" clear  ", dim),
            Span::styled("esc", key),
            Span::styled(" back", dim),
        ])
    } else {
        Line::from(vec![
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
        ])
    };
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentState;

    fn row(depth: u8, is_workspace: bool) -> NavigatorRow {
        NavigatorRow {
            target: NavigatorTarget::Workspace { ws_idx: 0 },
            depth,
            label: String::new(),
            meta: String::new(),
            status: AgentState::Idle,
            seen: true,
            is_current: false,
            is_workspace,
            is_tab: false,
            expanded: true,
            search_text: String::new(),
            matched: true,
        }
    }

    fn multi_tab_rows() -> Vec<NavigatorRow> {
        vec![
            row(0, true),  // workspace
            row(1, false), // tab a
            row(2, false), // pane
            row(2, false), // pane (last in tab a)
            row(1, false), // tab b (last tab)
            row(2, false), // pane (last in tab b)
            row(0, true),  // workspace
            row(1, false), // pane (single child)
        ]
    }

    #[test]
    fn workspace_rows_use_expand_caret() {
        let rows = multi_tab_rows();
        assert_eq!(tree_prefix(&rows, 0), "▾");
        let mut collapsed = rows.clone();
        collapsed[0].expanded = false;
        assert_eq!(tree_prefix(&collapsed, 0), "▸");
    }

    #[test]
    fn middle_children_get_branch_glyph() {
        let rows = multi_tab_rows();
        assert_eq!(tree_prefix(&rows, 1), "├──");
        assert_eq!(tree_prefix(&rows, 2), "│  ├──");
    }

    #[test]
    fn last_children_get_terminator_glyph() {
        let rows = multi_tab_rows();
        assert_eq!(tree_prefix(&rows, 3), "│  └──");
        assert_eq!(tree_prefix(&rows, 4), "└──");
        assert_eq!(tree_prefix(&rows, 7), "└──");
    }

    #[test]
    fn spine_stops_after_last_ancestor_sibling() {
        let rows = multi_tab_rows();
        assert_eq!(tree_prefix(&rows, 5), "   └──");
    }

    #[test]
    fn next_workspace_does_not_extend_previous_subtree() {
        // The pane at idx 5 is last in its workspace even though another
        // workspace with children follows.
        let rows = multi_tab_rows();
        assert!(!has_following_sibling_at_depth(&rows, 5, 1));
        assert!(!has_following_sibling_at_depth(&rows, 5, 2));
    }
}
