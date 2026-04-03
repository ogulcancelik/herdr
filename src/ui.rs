use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use crate::app::state::{ToastKind, ToastNotification};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::layout::PaneInfo;

const COLLAPSED_WIDTH: u16 = 4; // num + space + dot + separator
pub(crate) const MIN_SIDEBAR_WIDTH: u16 = 18;
pub(crate) const MAX_SIDEBAR_WIDTH: u16 = 36;

// Braille spinner frames — smooth rotation
const SPINNERS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Map spinner_tick (incremented every frame at ~60fps) to a spinner frame.
/// We want ~8 updates/sec so divide by 8.
fn spinner_frame(tick: u32) -> &'static str {
    SPINNERS[(tick as usize / 8) % SPINNERS.len()]
}

use crate::app::state::Palette;

/// Compute view geometry and reconcile pane sizes.
/// Called before render to separate mutation from drawing.
pub fn compute_view(app: &mut AppState, area: Rect) {
    let sidebar_w = if app.sidebar_collapsed {
        COLLAPSED_WIDTH
    } else if app.sidebar_width_auto {
        compute_sidebar_width(app)
    } else {
        app.sidebar_width
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
    };

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(1)]).areas(area);

    let has_tabs = app.active.and_then(|i| app.workspaces.get(i)).is_some();
    let (tab_bar_rect, terminal_area) = if has_tabs && main_area.height > 1 {
        let [tab_bar_rect, terminal_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(main_area);
        (tab_bar_rect, terminal_area)
    } else {
        (Rect::default(), main_area)
    };

    let tab_hit_areas = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| compute_tab_hit_areas(ws, tab_bar_rect))
        .unwrap_or_default();
    let new_tab_hit_area = compute_new_tab_hit_area(&tab_hit_areas, tab_bar_rect);

    let split_borders = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| ws.layout.splits(terminal_area))
        .unwrap_or_default();

    let pane_infos = compute_pane_infos(app, terminal_area);

    app.view = crate::app::ViewState {
        sidebar_rect: sidebar_area,
        tab_bar_rect,
        tab_hit_areas,
        new_tab_hit_area,
        terminal_area,
        pane_infos,
        split_borders,
    };
}

/// Render the UI — reads AppState but does not mutate it.
pub fn render(app: &AppState, frame: &mut Frame) {
    let sidebar_area = app.view.sidebar_rect;
    let tab_bar_area = app.view.tab_bar_rect;
    let terminal_area = app.view.terminal_area;

    if app.sidebar_collapsed {
        render_sidebar_collapsed(app, frame, sidebar_area);
    } else {
        render_sidebar(app, frame, sidebar_area);
    }
    render_tab_bar(app, frame, tab_bar_area);
    render_panes(app, frame, terminal_area);

    match app.mode {
        Mode::Onboarding => render_onboarding_overlay(app, frame, frame.area()),
        Mode::ReleaseNotes => render_release_notes_overlay(app, frame, frame.area()),
        Mode::Navigate => render_navigate_overlay(app, frame, terminal_area),
        Mode::Resize => render_resize_overlay(app, frame, terminal_area),
        Mode::ConfirmClose => render_confirm_close_overlay(app, frame, terminal_area),
        Mode::ContextMenu => {
            render_navigate_overlay(app, frame, terminal_area);
            render_context_menu(app, frame);
        }
        Mode::Settings => render_settings_overlay(app, frame, frame.area()),
        Mode::RenameWorkspace | Mode::RenameTab => render_rename_overlay(app, frame, frame.area()),
        Mode::GlobalMenu => render_global_launcher_menu(app, frame),
        Mode::KeybindHelp => render_keybind_help_overlay(app, frame),
        Mode::Terminal => {}
    }

    // Notifications (rendered on top of everything)
    let has_config_diagnostic = app.config_diagnostic.is_some();
    if let Some(message) = &app.config_diagnostic {
        render_config_diagnostic(frame, terminal_area, message, &app.palette);
    }
    if let Some(toast) = &app.toast {
        render_toast_notification(
            frame,
            terminal_area,
            toast,
            has_config_diagnostic,
            &app.palette,
        );
    }
}

const MIN_TAB_WIDTH: u16 = 8;
const NEW_TAB_WIDTH: u16 = 3;

fn compute_tab_hit_areas(ws: &crate::workspace::Workspace, area: Rect) -> Vec<Rect> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }

    let mut x = area.x;
    let mut rects = Vec::new();
    let right = area.x + area.width;
    for tab in &ws.tabs {
        if x >= right.saturating_sub(NEW_TAB_WIDTH) {
            break;
        }
        let desired = (tab.display_name().chars().count() as u16 + 4).max(MIN_TAB_WIDTH);
        let remaining = right.saturating_sub(NEW_TAB_WIDTH).saturating_sub(x);
        let width = desired.min(remaining).max(1);
        rects.push(Rect::new(x, area.y, width, 1));
        x = x.saturating_add(width + 1);
    }
    rects
}

fn compute_new_tab_hit_area(tab_hit_areas: &[Rect], area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = tab_hit_areas
        .last()
        .map(|rect| rect.x + rect.width + 1)
        .unwrap_or(area.x)
        .min(area.x + area.width.saturating_sub(NEW_TAB_WIDTH));
    Rect::new(x, area.y, NEW_TAB_WIDTH.min(area.width), 1)
}

fn render_tab_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return;
    };

    let p = &app.palette;

    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(Style::default().bg(p.panel_bg)),
        area,
    );

    for (idx, tab) in ws.tabs.iter().enumerate() {
        let Some(rect) = app.view.tab_hit_areas.get(idx).copied() else {
            break;
        };
        let active = idx == ws.active_tab;
        let style = if active {
            Style::default()
                .fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.overlay1).bg(p.surface0)
        };
        let width = rect.width as usize;
        let name = tab.display_name();
        let text = format!(" {:width$}", name, width = width.saturating_sub(1));
        frame.render_widget(Paragraph::new(text).style(style), rect);
    }

    if app.view.new_tab_hit_area.width > 0 {
        frame.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.overlay1)),
            app.view.new_tab_hit_area,
        );
    }
}

/// Compute pane layout info and resize pane runtimes to match.
fn compute_pane_infos(app: &AppState, area: Rect) -> Vec<PaneInfo> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return Vec::new();
    };

    let multi_pane = ws.layout.pane_count() > 1;
    let terminal_active = app.mode == Mode::Terminal;

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        let mut inner_rect = area;
        let mut scrollbar_rect = None;
        if let Some(rt) = ws.runtimes.get(&focused_id) {
            if rt
                .scroll_metrics()
                .is_some_and(|metrics| should_show_scrollbar(metrics) && area.width > 1)
            {
                inner_rect.width = inner_rect.width.saturating_sub(1);
                scrollbar_rect = Some(Rect::new(
                    area.x + area.width.saturating_sub(1),
                    area.y,
                    1,
                    area.height,
                ));
            }
            rt.resize(inner_rect.height, inner_rect.width);
        }
        return vec![PaneInfo {
            id: focused_id,
            rect: area,
            inner_rect,
            scrollbar_rect,
            is_focused: true,
        }];
    }

    let mut pane_infos = ws.layout.panes(area);

    for info in &mut pane_infos {
        let pane_inner = if multi_pane {
            let border_set = if info.is_focused && terminal_active {
                ratatui::symbols::border::THICK
            } else {
                ratatui::symbols::border::PLAIN
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border_set);
            block.inner(info.rect)
        } else {
            area
        };

        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = ws.runtimes.get(&info.id) {
            if rt
                .scroll_metrics()
                .is_some_and(|metrics| should_show_scrollbar(metrics) && pane_inner.width > 1)
            {
                inner_rect.width = inner_rect.width.saturating_sub(1);
                scrollbar_rect = Some(Rect::new(
                    pane_inner.x + pane_inner.width.saturating_sub(1),
                    pane_inner.y,
                    1,
                    pane_inner.height,
                ));
            }
            rt.resize(inner_rect.height, inner_rect.width);
        }

        info.inner_rect = inner_rect;
        info.scrollbar_rect = scrollbar_rect;
    }

    pane_infos
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
fn compute_sidebar_width(app: &AppState) -> u16 {
    if app.workspaces.is_empty() {
        return app
            .sidebar_width
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
    }
    let max_workspace_line = app
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let name_len = ws.display_name().len();
            let number_len = (i + 1).to_string().len();
            // marker + number + space + name + spaces + aggregate dot
            let line1 = 3 + number_len + name_len + 3;
            // branch line: "  branch"
            let line2 = ws.branch().map(|b| 3 + b.len()).unwrap_or(0);
            line1.max(line2)
        })
        .max()
        .unwrap_or(12);
    let max_agent_line = app
        .workspaces
        .iter()
        .flat_map(|ws| ws.pane_details().into_iter())
        .map(|detail| {
            let name_line = 3 + detail.label.len(); // " icon name"
            let state_line = 2 + state_label(detail.state, detail.seen).len(); // "  state"
            name_line.max(state_line)
        })
        .max()
        .unwrap_or(0);
    ((max_workspace_line.max(max_agent_line) as u16) + 2)
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

/// Collapsed sidebar: pure glance mode.
fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameWorkspace
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
            | Mode::GlobalMenu
            | Mode::KeybindHelp
    );

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let content_w = area.width.saturating_sub(1);
    let bottom_y = area.y + area.height.saturating_sub(1);

    for (i, ws) in app.workspaces.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= bottom_y {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state();
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = i == app.selected && is_navigating;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected {
            let buf = frame.buffer_mut();
            for x in area.x..area.x + content_w {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", i + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(area.x, y, content_w, 1),
        );
    }

    render_sidebar_toggle(frame, area, true, p);
}

fn render_sidebar(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let is_navigating = matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameWorkspace
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
            | Mode::GlobalMenu
            | Mode::KeybindHelp
    );
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    // Right border
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);

    // Determine which workspace to show in the detail panel
    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };

    // Split sidebar in half: workspaces on top, agents on bottom
    let total_h = content.height as usize;
    let ws_h = (total_h + 1) / 2; // top half (ceiling)
    let detail_h = total_h.saturating_sub(ws_h);

    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(
        content.x,
        content.y + ws_h as u16,
        content.width,
        detail_h as u16,
    );

    // --- Top section: Workspaces ---
    render_workspace_list(app, frame, ws_area, is_navigating);

    // --- Bottom section: Agent detail ---
    if let Some(ws_idx) = detail_ws_idx {
        if let Some(ws) = app.workspaces.get(ws_idx) {
            render_agent_detail(app, frame, detail_area, ws);
        }
    }

    render_sidebar_toggle(frame, area, false, p);
}

/// Render the workspace list in the top section of the sidebar.
fn render_workspace_list(app: &AppState, frame: &mut Frame, area: Rect, is_navigating: bool) {
    let p = &app.palette;

    // Reserve last row for "new" button
    let list_bottom = area.y + area.height.saturating_sub(1);
    let mut row_y = area.y;

    for (i, ws) in app.workspaces.iter().enumerate() {
        if row_y + 1 >= list_bottom {
            break;
        }
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let highlighted = selected || is_active; // active always gets a bg
        let (agg_state, agg_seen) = ws.aggregate_state();

        // Determine row height for background fill
        let has_second_line = ws.branch().is_some();
        let row_height: u16 = if has_second_line { 2 } else { 1 };

        // Background fill: selected gets brighter surface, active gets subtle surface
        if highlighted {
            let bg = if selected { p.surface0 } else { p.surface_dim };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in area.x..area.x + area.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        // Accent bar on the left edge for active workspace
        if is_active {
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                buf[(area.x, y)].set_symbol("▌");
                buf[(area.x, y)].set_style(Style::default().fg(p.accent));
            }
        }

        // Styles — active workspace is always visible, selected (nav mode) is brightest
        let name_style = if selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let num_style = if selected || is_active {
            Style::default().fg(p.overlay1)
        } else {
            Style::default().fg(p.overlay0)
        };

        // Line 1: marker-space + number + name + aggregate state dot
        let marker = if is_active { " " } else { " " }; // accent bar handles active indicator
        let mut line1 = vec![
            Span::styled(marker, Style::default()),
            Span::styled(format!("{} ", i + 1), num_style),
            Span::styled(ws.display_name(), name_style),
            Span::styled(" ", Style::default()),
        ];

        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        line1.push(Span::styled(icon, icon_style));

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(area.x, row_y, area.width, 1),
        );
        row_y += 1;

        // Line 2: branch
        if row_y < list_bottom {
            if let Some(branch) = ws.branch() {
                let upstream_label = ws.git_ahead_behind().and_then(|(ahead, behind)| {
                    let mut parts = Vec::new();
                    if ahead > 0 {
                        parts.push((format!("↑{}", ahead), p.green));
                    }
                    if behind > 0 {
                        parts.push((format!("↓{}", behind), p.red));
                    }
                    (!parts.is_empty()).then_some(parts)
                });
                let reserved = upstream_label
                    .as_ref()
                    .map(|parts| {
                        parts.iter().map(|(label, _)| label.len()).sum::<usize>() + parts.len()
                    })
                    .unwrap_or(0);
                let max_branch_len = (area.width as usize).saturating_sub(5 + reserved);
                let branch_display = if branch.len() > max_branch_len {
                    format!("{}…", &branch[..max_branch_len.saturating_sub(1)])
                } else {
                    branch
                };
                let branch_color = if selected || is_active {
                    p.mauve
                } else {
                    p.overlay0
                };
                let mut spans = vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ];
                if let Some(parts) = upstream_label {
                    spans.push(Span::styled(" ", Style::default()));
                    for (idx, (label, color)) in parts.into_iter().enumerate() {
                        if idx > 0 {
                            spans.push(Span::styled(" ", Style::default()));
                        }
                        spans.push(Span::styled(label, Style::default().fg(color)));
                    }
                }
                let line2 = Line::from(spans);
                frame.render_widget(
                    Paragraph::new(line2),
                    Rect::new(area.x, row_y, area.width, 1),
                );
                row_y += 1;
            }
        }

        // Spacing between workspace cards
        row_y += 1;
    }

    // Footer actions for workspace section
    if list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled("new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        frame.render_widget(
            Paragraph::new(Span::styled("menu", Style::default().fg(p.overlay0)))
                .alignment(Alignment::Right),
            menu_rect,
        );
    }
}

/// Render the agent detail panel in the bottom section of the sidebar.
fn render_agent_detail(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    ws: &crate::workspace::Workspace,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let mut row_y = area.y;

    // Horizontal separator
    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, row_y, area.width, 1),
    );
    row_y += 1;

    // Section header
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, row_y, area.width, 1),
    );
    row_y += 1;

    // Blank line for breathing room
    row_y += 1;

    // Per-pane agent entries, sorted by urgency
    let details = ws.pane_details();
    for detail in &details {
        if row_y + 1 >= area.y + area.height {
            break;
        }

        let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
        let label_color = state_label_color(detail.state, detail.seen, p);
        let label = state_label(detail.state, detail.seen);

        let name_style = Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD);
        let status_style = Style::default().fg(label_color).add_modifier(Modifier::DIM);

        let name_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(&detail.label, name_style),
        ]);
        frame.render_widget(
            Paragraph::new(name_line),
            Rect::new(area.x, row_y, area.width, 1),
        );
        row_y += 1;

        if row_y >= area.y + area.height {
            break;
        }

        let status_line = Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(label, status_style),
        ]);
        frame.render_widget(
            Paragraph::new(status_line),
            Rect::new(area.x, row_y, area.width, 1),
        );
        row_y += 1;

        if row_y < area.y + area.height {
            row_y += 1;
        }
    }
}

fn render_sidebar_toggle(frame: &mut Frame, area: Rect, collapsed: bool, p: &Palette) {
    // Toggle button not needed when sidebar has content — skip for now
    // to avoid conflicting with the agent detail panel at the bottom.
    if !collapsed {
        return;
    }
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return;
    }
    let icon = "»";
    let x = area.x + content_w / 2;
    let toggle_area = Rect::new(x, bottom_y, 1, 1);
    frame.render_widget(
        Paragraph::new(Span::styled(icon, Style::default().fg(p.overlay0))),
        toggle_area,
    );
}

fn render_panes(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(ws_idx) = app.active else {
        render_empty(app, frame, area);
        return;
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        render_empty(app, frame, area);
        return;
    };

    let multi_pane = ws.layout.pane_count() > 1;
    let terminal_active = app.mode == Mode::Terminal;

    for info in &app.view.pane_infos {
        if let Some(rt) = ws.runtimes.get(&info.id) {
            // Draw borders for multi-pane layouts
            if multi_pane {
                let (border_style, border_set) = if info.is_focused && terminal_active {
                    (
                        Style::default().fg(app.palette.accent),
                        ratatui::symbols::border::THICK,
                    )
                } else if info.is_focused {
                    (
                        Style::default().fg(app.palette.accent),
                        ratatui::symbols::border::PLAIN,
                    )
                } else {
                    (
                        Style::default().fg(app.palette.overlay0),
                        ratatui::symbols::border::PLAIN,
                    )
                };

                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .border_set(border_set);
                frame.render_widget(block, info.rect);
            }

            // Draw terminal content
            if let Ok(parser) = rt.parser.read() {
                let show_cursor = parser.screen().scrollback() == 0;
                let pt = PseudoTerminal::new(parser.screen())
                    .cursor(tui_term::widget::Cursor::default().visibility(show_cursor));
                frame.render_widget(pt, info.inner_rect);
            }
            render_pane_scrollbar(app, frame, info, rt);

            // Dim unfocused panes only in navigate mode
            let should_dim = !info.is_focused && multi_pane && !terminal_active;
            if should_dim {
                let inner = info.inner_rect;
                let buf = frame.buffer_mut();
                for y in inner.y..inner.y + inner.height {
                    for x in inner.x..inner.x + inner.width {
                        let cell = &mut buf[(x, y)];
                        let style = cell.style();
                        let fg = style.fg.unwrap_or(Color::White);
                        let dimmed_fg = dim_color(fg);
                        cell.set_style(style.fg(dimmed_fg));
                    }
                }
            }

            // Selection highlight
            render_selection_highlight(
                &app.selection,
                frame,
                info.id,
                info.inner_rect,
                &app.palette,
            );
        }
    }
}

/// Render selection highlight for a pane by inverting fg/bg colors.
/// Reduce a color's brightness by blending it toward black.
fn dim_color(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(r / 3, g / 3, b / 3),
        Color::White => Color::DarkGray,
        Color::Gray => Color::DarkGray,
        Color::DarkGray => Color::Rgb(30, 30, 30),
        Color::Red => Color::Rgb(60, 0, 0),
        Color::Green => Color::Rgb(0, 60, 0),
        Color::Yellow => Color::Rgb(60, 60, 0),
        Color::Blue => Color::Rgb(0, 0, 60),
        Color::Magenta => Color::Rgb(60, 0, 60),
        Color::Cyan => Color::Rgb(0, 60, 60),
        Color::LightRed => Color::Rgb(80, 30, 30),
        Color::LightGreen => Color::Rgb(30, 80, 30),
        Color::LightYellow => Color::Rgb(80, 80, 30),
        Color::LightBlue => Color::Rgb(30, 30, 80),
        Color::LightMagenta => Color::Rgb(80, 30, 80),
        Color::LightCyan => Color::Rgb(30, 80, 80),
        // Indexed colors and others: just use DIM modifier as fallback
        _ => Color::DarkGray,
    }
}

pub(crate) fn pane_scrollbar_rect(info: &PaneInfo) -> Option<Rect> {
    info.scrollbar_rect
}

pub(crate) fn release_notes_scrollbar_rect(
    body: Rect,
    metrics: crate::pane::ScrollMetrics,
) -> Option<Rect> {
    (should_show_scrollbar(metrics) && body.width > 1).then_some(Rect::new(
        body.x + body.width - 1,
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn should_show_scrollbar(metrics: crate::pane::ScrollMetrics) -> bool {
    metrics.max_offset_from_bottom > 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollbarThumb {
    pub top: u16,
    pub len: u16,
}

pub(crate) fn scrollbar_thumb(
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
) -> Option<ScrollbarThumb> {
    if metrics.max_offset_from_bottom == 0 || track.height == 0 {
        return None;
    }

    let track_height = track.height as usize;
    let total_rows = metrics.max_offset_from_bottom + metrics.viewport_rows;
    if total_rows == 0 {
        return None;
    }

    let thumb_len = ((metrics.viewport_rows * track_height) as f32 / total_rows as f32)
        .round()
        .max(1.0)
        .min(track_height as f32) as usize;
    let max_thumb_top = track_height.saturating_sub(thumb_len);
    let scrolled_from_top = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let thumb_top = if max_thumb_top == 0 || metrics.max_offset_from_bottom == 0 {
        0
    } else {
        ((scrolled_from_top * max_thumb_top) as f32 / metrics.max_offset_from_bottom as f32)
            .round()
            .clamp(0.0, max_thumb_top as f32) as usize
    };

    Some(ScrollbarThumb {
        top: track.y + thumb_top as u16,
        len: thumb_len as u16,
    })
}

pub(crate) fn scrollbar_thumb_grab_offset(
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    row: u16,
) -> Option<u16> {
    let thumb = scrollbar_thumb(metrics, track)?;
    (row >= thumb.top && row < thumb.top + thumb.len).then_some(row - thumb.top)
}

fn scrollbar_offset_from_thumb_top(
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    thumb_top: usize,
) -> usize {
    if metrics.max_offset_from_bottom == 0 {
        return 0;
    }

    let thumb_len = scrollbar_thumb(metrics, track)
        .map(|thumb| thumb.len as usize)
        .unwrap_or(1);
    let max_thumb_top = track.height as usize - thumb_len.min(track.height as usize);
    if max_thumb_top == 0 {
        return 0;
    }

    let desired_top = thumb_top.min(max_thumb_top);
    let scrolled_from_top = ((desired_top * metrics.max_offset_from_bottom) as f32
        / max_thumb_top as f32)
        .round() as usize;
    metrics
        .max_offset_from_bottom
        .saturating_sub(scrolled_from_top)
}

pub(crate) fn scrollbar_offset_from_row(
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    row: u16,
) -> usize {
    let thumb = match scrollbar_thumb(metrics, track) {
        Some(thumb) => thumb,
        None => return 0,
    };
    let clamped_row = row.clamp(track.y, track.y + track.height.saturating_sub(1));
    let row_offset = clamped_row.saturating_sub(track.y) as usize;
    let thumb_center = (thumb.len as usize) / 2;
    let desired_top = row_offset.saturating_sub(thumb_center);
    scrollbar_offset_from_thumb_top(metrics, track, desired_top)
}

pub(crate) fn scrollbar_offset_from_drag_row(
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    row: u16,
    grab_row_offset: u16,
) -> usize {
    let clamped_row = row.clamp(track.y, track.y + track.height.saturating_sub(1));
    let row_offset = clamped_row.saturating_sub(track.y) as usize;
    let desired_top = row_offset.saturating_sub(grab_row_offset as usize);
    scrollbar_offset_from_thumb_top(metrics, track, desired_top)
}

fn render_scrollbar(
    frame: &mut Frame,
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    track_color: Color,
    thumb_color: Color,
    thumb_symbol: &str,
) {
    if metrics.max_offset_from_bottom == 0 {
        return;
    }

    let Some(thumb) = scrollbar_thumb(metrics, track) else {
        return;
    };

    let buf = frame.buffer_mut();
    for y in track.y..track.y + track.height {
        let cell = &mut buf[(track.x, y)];
        cell.set_symbol("▕");
        cell.set_style(Style::default().fg(track_color));
    }
    for y in thumb.top..thumb.top + thumb.len {
        let cell = &mut buf[(track.x, y)];
        cell.set_symbol(thumb_symbol);
        cell.set_style(Style::default().fg(thumb_color));
    }
}

fn render_pane_scrollbar(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    rt: &crate::pane::PaneRuntime,
) {
    let Some(metrics) = rt.scroll_metrics() else {
        return;
    };
    let Some(track) = pane_scrollbar_rect(info) else {
        return;
    };

    let (track_color, thumb_color, thumb_symbol) = if info.is_focused {
        (app.palette.overlay0, app.palette.overlay1, "▐")
    } else {
        (app.palette.surface_dim, app.palette.overlay0, "▕")
    };

    render_scrollbar(
        frame,
        metrics,
        track,
        track_color,
        thumb_color,
        thumb_symbol,
    );
}

fn render_selection_highlight(
    selection: &Option<crate::selection::Selection>,
    frame: &mut Frame,
    pane_id: crate::layout::PaneId,
    inner: Rect,
    p: &Palette,
) {
    if let Some(sel) = selection {
        if sel.is_visible() && sel.pane_id == pane_id {
            let buf = frame.buffer_mut();
            for y in 0..inner.height {
                for x in 0..inner.width {
                    if sel.contains(y, x) {
                        let cell = &mut buf[(inner.x + x, inner.y + y)];
                        cell.set_style(Style::default().fg(p.panel_bg).bg(p.blue));
                    }
                }
            }
        }
    }
}

fn render_empty(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  No workspaces yet",
            Style::default().fg(p.overlay0),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  A workspace is one project context.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(Span::styled(
            "  Its root pane (top-left) sets the default repo or folder name.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(p.overlay0)),
            Span::styled(
                format!("{}", app.keybinds.new_workspace_label),
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to create one", Style::default().fg(p.overlay0)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(p.surface_dim)),
        ),
        area,
    );
}

const ONBOARDING_PREFIX_LABEL: &str = "ctrl+b";

fn dim_background(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

fn render_panel_shell(
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

fn render_modal_shell(
    frame: &mut Frame,
    area: Rect,
    popup_w: u16,
    popup_h: u16,
    p: &Palette,
) -> Option<Rect> {
    let popup = centered_popup_rect(area, popup_w, popup_h)?;
    render_panel_shell(frame, popup, p.accent, p.panel_bg)
}

fn render_modal_header(frame: &mut Frame, area: Rect, title: &str, p: &Palette) {
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(" {title} "),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )),
        area,
    );
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

fn render_onboarding_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    dim_background(frame, area);

    match app.onboarding_step {
        0 => render_onboarding_welcome(app, frame, area),
        _ => render_onboarding_notifications(app, frame, area),
    }
}

fn render_release_notes_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(notes) = &app.release_notes else {
        return;
    };

    dim_background(frame, area);

    let Some(inner) = render_modal_shell(frame, area, 76, 20, &app.palette) else {
        return;
    };
    if inner.height < 8 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(
        frame,
        header_rows[0],
        &format!("v{}", notes.version),
        &app.palette,
    );
    frame.render_widget(
        Paragraph::new(" what's new in this release")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[1],
    );
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(app.palette.panel_bg)
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app.release_notes_max_scroll().saturating_sub(notes.scroll) as usize,
        max_offset_from_bottom: app.release_notes_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        release_notes_lines(notes.body.as_str(), &app.palette)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((notes.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("wheel ↑↓", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close", Style::default().fg(app.palette.overlay0)),
            Span::styled(" q / esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}

pub(crate) fn release_notes_lines<'a>(body: &'a str, p: &Palette) -> Vec<(usize, Line<'a>)> {
    let mut lines = Vec::new();

    for raw in body.lines() {
        let trimmed = raw.trim_end();
        if trimmed.is_empty() {
            lines.push((0, Line::raw("")));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let text = rest.to_lowercase();
            let width = 1 + text.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        text,
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            let width = 3 + rest.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(" • ", Style::default().fg(p.accent)),
                    Span::styled(rest.to_string(), Style::default().fg(p.text)),
                ]),
            ));
            continue;
        }

        let width = 1 + trimmed.chars().count();
        lines.push((
            width,
            Line::from(vec![
                Span::raw(" "),
                Span::styled(trimmed.to_string(), Style::default().fg(p.text)),
            ]),
        ));
    }

    lines
}

pub(crate) fn release_notes_close_button_rect(area: Rect) -> Rect {
    let width = action_button_width(Some("esc"), "close");
    Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1)
}

pub(crate) fn onboarding_welcome_continue_rect(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        action_button_width(Some("↵"), "continue"),
        1,
    )
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

pub(crate) fn onboarding_notification_button_rects(area: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        area,
        &[
            ActionButtonSpec {
                hint: Some("esc"),
                label: "back",
            },
            ActionButtonSpec {
                hint: Some("↵"),
                label: "start",
            },
        ],
        2,
        0,
    );
    (rects[0], rects[1])
}

fn render_onboarding_welcome(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_modal_shell(frame, area, 64, 16, &app.palette) else {
        return;
    };
    if inner.height < 11 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 0, 1, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);
    let content_rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<4>(stack.content);

    frame.render_widget(
        Paragraph::new("  herdr").style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        header_rows[0],
    );
    frame.render_widget(
        Paragraph::new("  terminal workspace manager for coding agents")
            .style(Style::default().fg(app.palette.overlay0)),
        header_rows[1],
    );

    frame.render_widget(
        Paragraph::new(
            "  this is a mouse-first terminal.\n  click the sidebar to switch workspaces, drag pane\n  borders to resize, right-click for context menus.",
        )
        .style(Style::default().fg(app.palette.overlay1)),
        content_rows[0],
    );

    let key_line = Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            ONBOARDING_PREFIX_LABEL,
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " enters navigate mode · ",
            Style::default().fg(app.palette.overlay1),
        ),
        Span::styled(
            "?",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " shows keybinds and settings",
            Style::default().fg(app.palette.overlay1),
        ),
    ]);
    frame.render_widget(Paragraph::new(key_line), content_rows[2]);

    let continue_rect = onboarding_welcome_continue_rect(stack.actions.unwrap_or_default());
    render_action_button(
        frame,
        continue_rect,
        Some("↵"),
        "continue",
        Style::default()
            .fg(app.palette.panel_bg)
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_onboarding_notifications(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_modal_shell(frame, area, 56, 14, &app.palette) else {
        return;
    };

    if inner.height < 11 {
        return;
    }

    let stack = modal_stack_areas(inner, 3, 0, 1, 1);
    let header_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<3>(stack.header);
    let option_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<5>(stack.content);

    render_modal_header(frame, header_rows[0], "notification style", &app.palette);
    frame.render_widget(
        Paragraph::new(" herdr watches background panes and can alert you")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[1],
    );
    frame.render_widget(
        Paragraph::new(" when agents finish or need attention.")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[2],
    );

    let options = [
        "quiet        no interruptions",
        "visual only  top-right toasts",
        "sound only   sound alerts",
        "both         sound and toasts",
    ];

    for (idx, option) in options.iter().enumerate() {
        let selected = idx == app.onboarding_list.selected;
        let prefix = if selected { "›" } else { " " };
        let style = if selected {
            Style::default()
                .fg(app.palette.panel_bg)
                .bg(app.palette.accent)
        } else {
            Style::default().fg(app.palette.text)
        };
        frame.render_widget(
            Paragraph::new(format!(" {prefix} {}. {option}", idx + 1)).style(style),
            option_rows[idx],
        );
    }

    let (back_rect, save_rect) =
        onboarding_notification_button_rects(stack.actions.unwrap_or_default());
    render_action_button(
        frame,
        back_rect,
        Some("esc"),
        "back",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        save_rect,
        Some("↵"),
        "start",
        Style::default()
            .fg(app.palette.panel_bg)
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

/// Floating overlay for navigate mode — appears at bottom of terminal area.
fn render_navigate_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let mode_style = Style::default()
        .fg(app.palette.panel_bg)
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
        Span::styled(" settings", dim),
    ]);

    let overlay_y = area.y + area.height.saturating_sub(1);
    let overlay_area = Rect::new(area.x, overlay_y, area.width, 1);

    frame.render_widget(Clear, overlay_area);
    let bg = Style::default().bg(app.palette.panel_bg);
    let buf = frame.buffer_mut();
    for x in overlay_area.x..overlay_area.x + overlay_area.width {
        buf[(x, overlay_y)].set_style(bg);
    }
    frame.render_widget(Paragraph::new(line), overlay_area);
}

fn render_global_launcher_menu(app: &AppState, frame: &mut Frame) {
    let rect = app.global_menu_rect();
    let Some(inner) = render_panel_shell(frame, rect, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };

    let items = ["keybinds", "settings"];
    for (idx, item) in items.iter().enumerate() {
        let y = inner.y + idx as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = idx == app.global_menu.highlighted;
        let style = if selected {
            Style::default()
                .fg(app.palette.panel_bg)
                .bg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.palette.text)
        };
        frame.render_widget(
            Paragraph::new(format!(" {item}"))
                .style(style)
                .alignment(Alignment::Left),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }
}

fn optional_keybind_label(label: &Option<String>) -> String {
    label.clone().unwrap_or_else(|| "unset".to_string())
}

fn keybind_help_groups(app: &AppState) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let kb = &app.keybinds;
    let mut groups = Vec::new();

    groups.push((
        "global",
        vec![
            (
                crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
                "navigate mode",
            ),
            ("prefix + ?".to_string(), "keybinds"),
        ],
    ));

    groups.push((
        "navigation",
        vec![
            ("esc".to_string(), "back"),
            ("↑ / ↓".to_string(), "workspace list"),
            ("h j k l / arrows".to_string(), "move focus"),
            ("tab / shift+tab".to_string(), "cycle pane"),
            ("enter".to_string(), "open workspace"),
            ("s".to_string(), "settings"),
            ("q".to_string(), "quit"),
        ],
    ));

    let workspace_tab = vec![
        (kb.new_workspace_label.clone(), "new workspace"),
        (kb.rename_workspace_label.clone(), "rename workspace"),
        (kb.close_workspace_label.clone(), "close workspace"),
        (
            optional_keybind_label(&kb.previous_workspace_label),
            "previous workspace",
        ),
        (
            optional_keybind_label(&kb.next_workspace_label),
            "next workspace",
        ),
        (kb.new_tab_label.clone(), "new tab"),
        (optional_keybind_label(&kb.rename_tab_label), "rename tab"),
        (
            optional_keybind_label(&kb.previous_tab_label),
            "previous tab",
        ),
        (optional_keybind_label(&kb.next_tab_label), "next tab"),
        (optional_keybind_label(&kb.close_tab_label), "close tab"),
    ];
    groups.push(("workspaces / tabs", workspace_tab));

    let panes = vec![
        (kb.split_vertical_label.clone(), "split vertical"),
        (kb.split_horizontal_label.clone(), "split horizontal"),
        (kb.close_pane_label.clone(), "close pane"),
        (kb.fullscreen_label.clone(), "fullscreen"),
        (kb.resize_mode_label.clone(), "resize mode"),
        (kb.toggle_sidebar_label.clone(), "toggle sidebar"),
        (
            optional_keybind_label(&kb.focus_pane_left_label),
            "focus pane left",
        ),
        (
            optional_keybind_label(&kb.focus_pane_down_label),
            "focus pane down",
        ),
        (
            optional_keybind_label(&kb.focus_pane_up_label),
            "focus pane up",
        ),
        (
            optional_keybind_label(&kb.focus_pane_right_label),
            "focus pane right",
        ),
    ];
    groups.push(("panes", panes));

    groups
}

pub(crate) fn keybind_help_lines(app: &AppState) -> Vec<(usize, Line<'static>)> {
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);

    let groups = keybind_help_groups(app);
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);

    let mut lines = Vec::new();

    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(vec![Span::styled(format!(" {group}"), heading_style)]),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {:<width$} ", key, width = key_width);
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(padded_key, key_style),
                    Span::styled(label.to_string(), label_style),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }

    lines
}

fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    dim_background(frame, frame.area());

    let Some(inner) = render_modal_shell(frame, frame.area(), 76, 22, &app.palette) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(frame, header_rows[0], "keybinds", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(app.palette.panel_bg)
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(
        Paragraph::new(" available commands and configured shortcuts")
            .style(Style::default().fg(app.palette.overlay1)),
        header_rows[1],
    );

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app
            .keybind_help_max_scroll()
            .saturating_sub(app.keybind_help.scroll) as usize,
        max_offset_from_bottom: app.keybind_help_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        keybind_help_lines(app)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((app.keybind_help.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("wheel ↑↓", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("jump", Style::default().fg(app.palette.overlay0)),
            Span::styled(" pgup / pgdn ", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close", Style::default().fg(app.palette.overlay0)),
            Span::styled(" q / esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}

/// Floating overlay for resize mode.
fn render_resize_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let mode_style = Style::default()
        .fg(app.palette.panel_bg)
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

    frame.render_widget(Clear, overlay_area);
    let bg = Style::default().bg(app.palette.panel_bg);
    let buf = frame.buffer_mut();
    for x in overlay_area.x..overlay_area.x + overlay_area.width {
        buf[(x, overlay_y)].set_style(bg);
    }
    frame.render_widget(Paragraph::new(line), overlay_area);
}

/// Centered popup confirmation dialog with dimmed background.
pub(crate) fn rename_button_rects(inner: Rect) -> (Rect, Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "save",
            },
            ActionButtonSpec {
                hint: Some("^c"),
                label: "clear",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        3,
    );
    (rects[0], rects[1], rects[2])
}

fn render_rename_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    dim_background(frame, area);

    let title = match app.mode {
        Mode::RenameWorkspace => "rename workspace",
        Mode::RenameTab => "rename tab",
        _ => return,
    };

    let Some(inner) = render_modal_shell(frame, area, 56, 7, &app.palette) else {
        return;
    };
    if inner.height < 4 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<5>(inner);

    render_modal_header(frame, rows[0], title, &app.palette);

    let input_rect = Rect::new(rows[2].x, rows[2].y, rows[2].width, 1);
    frame.render_widget(Clear, input_rect);
    frame.render_widget(
        Paragraph::new(format!(" {}█", app.name_input)).style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0),
        ),
        input_rect,
    );

    let (save_rect, clear_rect, cancel_rect) = rename_button_rects(inner);

    render_action_button(
        frame,
        save_rect,
        Some("↵"),
        "save",
        Style::default()
            .fg(app.palette.panel_bg)
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        clear_rect,
        Some("^c"),
        "clear",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        cancel_rect,
        Some("esc"),
        "cancel",
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_confirm_close_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let ws_name = app
        .workspaces
        .get(app.selected)
        .map(|ws| ws.display_name())
        .unwrap_or_else(|| "?".to_string());
    let pane_count = app
        .workspaces
        .get(app.selected)
        .map(|ws| ws.layout.pane_count())
        .unwrap_or(0);

    let pane_text = if pane_count == 1 {
        "1 pane".to_string()
    } else {
        format!("{pane_count} panes")
    };

    // Dim the entire background
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }

    // Centered popup
    let Some(popup) = confirm_close_popup_rect(area) else {
        return;
    };

    let warn = Style::default()
        .fg(app.palette.red)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);

    let title_line = Line::from(vec![Span::styled(" Close workspace?", warn)]);

    let detail_line = Line::from(vec![
        Span::styled(
            format!(" {ws_name}"),
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" — {pane_text}"), dim),
    ]);

    let Some(inner) = render_panel_shell(frame, popup, app.palette.red, app.palette.panel_bg)
    else {
        return;
    };

    if inner.height >= 3 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas::<3>(inner);

        frame.render_widget(Paragraph::new(title_line), rows[0]);
        frame.render_widget(Paragraph::new(detail_line), rows[1]);

        let (confirm_rect, cancel_rect) = confirm_close_button_rects(inner);
        render_action_button(
            frame,
            confirm_rect,
            Some("↵"),
            "confirm",
            Style::default()
                .fg(app.palette.panel_bg)
                .bg(app.palette.red)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            cancel_rect,
            Some("esc"),
            "cancel",
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
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

fn render_action_button(
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

fn centered_button_row(inner: Rect, widths: &[u16], gap: u16, row_offset: u16) -> Vec<Rect> {
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

pub(crate) fn confirm_close_popup_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, 44, 5)
}

pub(crate) fn confirm_close_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "confirm",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "cancel",
            },
        ],
        2,
        2,
    );
    (rects[0], rects[1])
}

pub(crate) fn settings_button_rects(inner: Rect) -> (Rect, Rect) {
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: "apply",
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (rects[0], rects[1])
}

/// Right-click context menu popup anchored near the click position.
// ---------------------------------------------------------------------------
// Settings overlay
// ---------------------------------------------------------------------------

fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::SettingsSection;

    let p = &app.palette;
    let Some(popup) = centered_popup_rect(area, 56, 20) else {
        return;
    };

    // Dim everything behind the modal
    dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let stack = modal_stack_areas(inner, 3, 2, 0, 1);
    let header_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<3>(stack.header);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " settings",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );

    // Tab bar
    let tabs = Tabs::new(SettingsSection::ALL.iter().map(|s| s.label()))
        .select(
            SettingsSection::ALL
                .iter()
                .position(|section| *section == app.settings.section)
                .unwrap_or(0),
        )
        .style(Style::default().fg(p.overlay1))
        .highlight_style(
            Style::default()
                .fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ")
        .padding(" ", " ");
    frame.render_widget(tabs, header_rows[1]);

    // Separator
    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[2],
    );

    // Section content
    let content_area = stack.content;

    match app.settings.section {
        SettingsSection::Theme => {
            render_settings_theme(app, frame, content_area);
        }
        SettingsSection::Sound => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                "sound alerts",
                "play sounds when agents change state in background",
                app.sound_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Toast => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                "visual toasts",
                "show top-right notifications for background events",
                app.toast_config.enabled,
                app.settings.list.selected,
            );
        }
    }

    // Footer buttons + hints
    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let (apply_rect, close_rect) = settings_button_rects(inner);
        render_action_button(
            frame,
            apply_rect,
            Some("↵"),
            "apply",
            Style::default()
                .fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        );
        render_action_button(
            frame,
            close_rect,
            Some("esc"),
            "close",
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(" select  ", Style::default().fg(p.overlay1)),
                Span::styled("tab", Style::default().fg(p.overlay0)),
                Span::styled(" section", Style::default().fg(p.overlay1)),
            ])),
            footer_rows[0],
        );
    }
}

/// Render the theme picker list inside the settings panel.
fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let items: Vec<ListItem> = THEME_NAMES
        .iter()
        .map(|name| {
            let is_current = name.to_lowercase().replace([' ', '_'], "-")
                == app.theme_name.to_lowercase().replace([' ', '_'], "-");
            let marker = if is_current { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut state = ListState::default().with_selected(Some(app.settings.list.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

/// Reusable toggle widget for boolean settings (sound, toast).
fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &crate::app::state::Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    let [desc_area, _, list_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(2),
    ])
    .areas::<3>(area);

    let max_desc_len = (desc_area.width as usize).saturating_sub(2);
    let desc_text = if description.len() > max_desc_len {
        format!(" {}…", &description[..max_desc_len.saturating_sub(2)])
    } else {
        format!(" {description}")
    };
    frame.render_widget(
        Paragraph::new(Span::styled(desc_text, Style::default().fg(p.overlay1))),
        desc_area,
    );

    let items: Vec<ListItem> = ["on", "off"]
        .into_iter()
        .map(|label| {
            let is_active = (label == "on") == current_value;
            let marker = if is_active { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{title}: {label}"), Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ");
    let mut state = ListState::default().with_selected(Some(selected_idx.min(1)));
    frame.render_stateful_widget(list, list_area, &mut state);
}

fn render_context_menu(app: &AppState, frame: &mut Frame) {
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
                .fg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ");
    let mut state = ListState::default().with_selected(Some(menu.list.highlighted));
    frame.render_stateful_widget(list, inner, &mut state);
}

fn render_toast_notification(
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
    let content_width = (toast.title.len().max(toast.context.len()) as u16) + 4;
    let width = content_width.saturating_add(2).min(area.width);
    let height = 4u16.min(area.height);
    let x = area.x + area.width.saturating_sub(width);
    let y = area.y
        + area
            .height
            .saturating_sub(height + if offset_for_warning { 1 } else { 0 });
    let toast_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 2 {
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
    frame.render_widget(Paragraph::new(context), context_row);
}

/// Visual badge for a pane's state + seen flag.
///
/// | State              | Icon | Color  |
/// |--------------------|------|--------|
/// | Blocked            | ●    | Red    |
/// | Working            | ●    | Yellow |
/// | Done (idle+unseen) | ●    | Blue   |
/// | Idle (seen)        | ○    | Green  |
/// | Unknown            | ·    | Gray   |
///
/// Filled dot = needs attention (blocked/working, or finished unseen).
/// Hollow dot = nothing to do here.
fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let text = format!(" config warning: {message} ");
    let width = text.len() as u16 + 2;
    let notif_area = Rect::new(
        area.x + area.width.saturating_sub(width.min(area.width)),
        area.y,
        width.min(area.width),
        1,
    );

    frame.render_widget(Clear, notif_area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            text,
            Style::default()
                .fg(p.panel_bg)
                .bg(p.yellow)
                .add_modifier(Modifier::BOLD),
        )),
        notif_area,
    );
}

/// Compact dot icon for workspace-level aggregate state (top section).
fn state_dot(state: AgentState, seen: bool, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
}

/// Rich icon for per-pane agent detail (bottom section).
/// Uses animated spinner for the working state.
fn agent_icon(state: AgentState, seen: bool, tick: u32, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

/// State label for the agent detail panel.
fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

/// Color for the state label text.
fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

fn _build_hints(items: &[(&str, &str)], key_style: Style, dim_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (i, (k, desc)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", dim_style));
        }
        spans.push(Span::styled(k.to_string(), key_style));
        spans.push(Span::styled(format!(" {desc}"), dim_style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_scrollbar_rect_uses_rightmost_inner_column() {
        let info = PaneInfo {
            id: crate::layout::PaneId::from_raw(1),
            rect: Rect::new(0, 0, 12, 8),
            inner_rect: Rect::new(1, 1, 9, 6),
            scrollbar_rect: Some(Rect::new(10, 1, 1, 6)),
            is_focused: true,
        };

        assert_eq!(pane_scrollbar_rect(&info), Some(Rect::new(10, 1, 1, 6)));
    }

    #[test]
    fn scrollbar_stays_hidden_without_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: 5,
        };

        assert!(!should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_shows_with_scrollback() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };

        assert!(should_show_scrollbar(metrics));
    }

    #[test]
    fn scrollbar_thumb_reaches_bottom_when_scrolled_to_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        assert_eq!(thumb.top + thumb.len, track.y + track.height);
    }

    #[test]
    fn scrollbar_offset_mapping_hits_top_middle_and_bottom() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 0,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 5);

        assert_eq!(scrollbar_offset_from_row(metrics, track, 4), 20);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 6), 10);
        assert_eq!(scrollbar_offset_from_row(metrics, track, 8), 0);
    }

    #[test]
    fn dragging_from_current_thumb_row_preserves_offset() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 7,
            max_offset_from_bottom: 20,
            viewport_rows: 5,
        };
        let track = Rect::new(9, 4, 1, 8);
        let thumb = scrollbar_thumb(metrics, track).expect("thumb");
        let row = thumb.top + thumb.len / 2;
        let grab = scrollbar_thumb_grab_offset(metrics, track, row).expect("grab");

        assert_eq!(scrollbar_offset_from_drag_row(metrics, track, row, grab), 7);
    }

    #[test]
    fn keybind_help_shows_unset_for_optional_actions() {
        let app = crate::app::state::AppState::test_new();
        let groups = keybind_help_groups(&app);

        let workspace_tab = groups
            .iter()
            .find(|(name, _)| *name == "workspaces / tabs")
            .expect("workspace tab group")
            .1
            .clone();
        let panes = groups
            .iter()
            .find(|(name, _)| *name == "panes")
            .expect("panes group")
            .1
            .clone();

        assert!(workspace_tab.contains(&("unset".to_string(), "previous workspace")));
        assert!(workspace_tab.contains(&("unset".to_string(), "next workspace")));
        assert!(workspace_tab.contains(&("unset".to_string(), "rename tab")));
        assert!(workspace_tab.contains(&("unset".to_string(), "previous tab")));
        assert!(workspace_tab.contains(&("unset".to_string(), "next tab")));
        assert!(workspace_tab.contains(&("unset".to_string(), "close tab")));
        assert!(panes.contains(&("unset".to_string(), "focus pane left")));
        assert!(panes.contains(&("unset".to_string(), "focus pane down")));
        assert!(panes.contains(&("unset".to_string(), "focus pane up")));
        assert!(panes.contains(&("unset".to_string(), "focus pane right")));
    }
}
