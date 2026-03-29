use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use tui_term::widget::PseudoTerminal;

use crate::app::state::{ToastKind, ToastNotification};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::layout::PaneInfo;

const COLLAPSED_WIDTH: u16 = 4; // num + space + dot + separator
const MIN_SIDEBAR_WIDTH: u16 = 18;
const MAX_SIDEBAR_WIDTH: u16 = 36;

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
    } else {
        compute_sidebar_width(app)
    };

    let [sidebar_area, main_area] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(1)]).areas(area);

    let terminal_area = main_area;

    // Compute split borders
    let split_borders = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| ws.layout.splits(terminal_area))
        .unwrap_or_default();

    // Compute pane layout + reconcile sizes
    let pane_infos = compute_pane_infos(app, terminal_area);

    app.view = crate::app::ViewState {
        sidebar_rect: sidebar_area,
        terminal_area,
        pane_infos,
        split_borders,
    };
}

/// Render the UI — reads AppState but does not mutate it.
pub fn render(app: &AppState, frame: &mut Frame) {
    let sidebar_area = app.view.sidebar_rect;
    let terminal_area = app.view.terminal_area;

    if app.sidebar_collapsed {
        render_sidebar_collapsed(app, frame, sidebar_area);
    } else {
        render_sidebar(app, frame, sidebar_area);
    }
    render_panes(app, frame, terminal_area);

    match app.mode {
        Mode::Onboarding => render_onboarding_overlay(app, frame, frame.area()),
        Mode::Navigate => render_navigate_overlay(app, frame, terminal_area),
        Mode::Resize => render_resize_overlay(app, frame, terminal_area),
        Mode::ConfirmClose => render_confirm_close_overlay(app, frame, terminal_area),
        Mode::ContextMenu => {
            render_navigate_overlay(app, frame, terminal_area);
            render_context_menu(app, frame);
        }
        Mode::Settings => render_settings_overlay(app, frame, frame.area()),
        Mode::RenameSession => {}
        Mode::Terminal => {}
    }

    // Notifications (rendered on top of everything)
    if let Some(version) = &app.update_available {
        if !app.update_dismissed {
            render_update_notification(frame, terminal_area, version, &app.palette);
        }
    }
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

/// Compute pane layout info and resize pane runtimes to match.
fn compute_pane_infos(app: &AppState, area: Rect) -> Vec<PaneInfo> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return Vec::new();
    };

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        if let Some(rt) = ws.runtimes.get(&focused_id) {
            rt.resize(area.height, area.width);
        }
        return vec![PaneInfo {
            id: focused_id,
            rect: area,
            inner_rect: area,
            is_focused: true,
        }];
    }

    let multi_pane = ws.layout.pane_count() > 1;
    let terminal_active = app.mode == Mode::Terminal;
    let mut pane_infos = ws.layout.panes(area);

    for info in &mut pane_infos {
        let inner = if multi_pane {
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
        info.inner_rect = inner;

        if let Some(rt) = ws.runtimes.get(&info.id) {
            rt.resize(inner.height, inner.width);
        }
    }

    pane_infos
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
fn compute_sidebar_width(app: &AppState) -> u16 {
    if app.workspaces.is_empty() {
        return app.sidebar_width;
    }
    let max_line = app
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let name_len = ws.display_name().len();
            let number_len = (i + 1).to_string().len();
            let pane_dots = if ws.layout.pane_count() > 1 {
                ws.layout.pane_count()
            } else {
                1
            };
            // marker + number + space + name + spaces + dots
            let line1 = 3 + number_len + name_len + 2 + pane_dots;
            // branch line: "  branch"
            let line2 = ws.branch().map(|b| 3 + b.len()).unwrap_or(0);
            line1.max(line2)
        })
        .max()
        .unwrap_or(12);
    ((max_line as u16) + 2).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

/// Collapsed sidebar: pure glance mode.
fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameSession
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
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
            | Mode::RenameSession
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
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
        let has_second_line =
            ws.branch().is_some() || (app.mode == Mode::RenameSession && i == app.selected);
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

        // Line 1: marker-space + number + name + state dots
        let marker = if is_active { " " } else { " " }; // accent bar handles active indicator
        let mut line1 = vec![
            Span::styled(marker, Style::default()),
            Span::styled(format!("{} ", i + 1), num_style),
            Span::styled(ws.display_name(), name_style),
            Span::styled(" ", Style::default()),
        ];

        // State dots (one per pane, or single aggregate)
        if ws.layout.pane_count() == 1 {
            let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
            line1.push(Span::styled(icon, icon_style));
        } else {
            for (pane_state, pane_seen) in ws.pane_states() {
                let (icon, icon_style) = state_dot(pane_state, pane_seen, p);
                line1.push(Span::styled(icon, icon_style));
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1)),
            Rect::new(area.x, row_y, area.width, 1),
        );
        row_y += 1;

        // Line 2: branch or rename input
        if row_y < list_bottom {
            if app.mode == Mode::RenameSession && i == app.selected {
                let text = format!("   {}\u{2588}", app.name_input);
                frame.render_widget(Clear, Rect::new(area.x, row_y, area.width, 1));
                frame.render_widget(
                    Paragraph::new(text).style(Style::default().fg(p.yellow)),
                    Rect::new(area.x, row_y, area.width, 1),
                );
                row_y += 1;
            } else if let Some(branch) = ws.branch() {
                let max_branch_len = (area.width as usize).saturating_sub(5);
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
                let line2 = Line::from(vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(branch_display, Style::default().fg(branch_color)),
                ]);
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

    // "new" at the bottom of workspace section
    if list_bottom > area.y {
        frame.render_widget(
            Paragraph::new(Span::styled("new", Style::default().fg(p.overlay0))),
            Rect::new(area.x, list_bottom, area.width, 1),
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
        if row_y >= area.y + area.height {
            break;
        }

        let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
        let label_color = state_label_color(detail.state, detail.seen, p);
        let label = state_label(detail.state, detail.seen);

        // Agent name in soft white, state label right-aligned in its color
        let name_style = Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD);

        // Calculate padding to right-align state label
        let used = 3 + detail.label.len() + 1; // " icon name "
        let label_len = label.len();
        let avail = (area.width as usize).saturating_sub(used + label_len);
        let padding = " ".repeat(avail);

        let line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(icon, icon_style),
            Span::styled(" ", Style::default()),
            Span::styled(&detail.label, name_style),
            Span::styled(padding, Style::default()),
            Span::styled(
                label,
                Style::default().fg(label_color).add_modifier(Modifier::DIM),
            ),
        ]);

        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x, row_y, area.width, 1),
        );
        row_y += 1;
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
        render_empty(frame, area, &app.palette);
        return;
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        render_empty(frame, area, &app.palette);
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
                let pt = PseudoTerminal::new(parser.screen());
                frame.render_widget(pt, info.inner_rect);
            }

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

fn render_empty(frame: &mut Frame, area: Rect, p: &Palette) {
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  No active workspace",
            Style::default().fg(p.overlay0),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(p.overlay0)),
            Span::styled(
                "new",
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

fn render_modal_shell(
    frame: &mut Frame,
    area: Rect,
    popup_w: u16,
    popup_h: u16,
    p: &Palette,
) -> Option<Rect> {
    let popup_w = popup_w.min(area.width.saturating_sub(4));
    let popup_h = popup_h.min(area.height.saturating_sub(2));
    if popup_w < 4 || popup_h < 4 {
        return None;
    }

    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);
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

fn render_onboarding_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    dim_background(frame, area);

    match app.onboarding_step {
        0 => render_onboarding_welcome(app, frame, area),
        _ => render_onboarding_notifications(app, frame, area),
    }
}

fn render_onboarding_welcome(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_modal_shell(frame, area, 64, 15, &app.palette) else {
        return;
    };
    if inner.height < 10 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<11>(inner);

    frame.render_widget(
        Paragraph::new("  herdr").style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new("  workspace manager for coding agents")
            .style(Style::default().fg(app.palette.overlay0)),
        rows[2],
    );

    let line1 = Line::from(vec![
        Span::styled(
            format!("  {}", ONBOARDING_PREFIX_LABEL),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" navigate ", Style::default().fg(app.palette.overlay1)),
        Span::styled("·", Style::default().fg(app.palette.overlay0)),
        Span::styled(" click sidebar ", Style::default().fg(app.palette.overlay1)),
        Span::styled("·", Style::default().fg(app.palette.overlay0)),
        Span::styled(" scroll panes", Style::default().fg(app.palette.overlay1)),
    ]);
    frame.render_widget(Paragraph::new(line1), rows[4]);

    let line2 = Line::from(vec![
        Span::styled(
            "  ↑↓",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " switch workspace ",
            Style::default().fg(app.palette.overlay1),
        ),
        Span::styled("·", Style::default().fg(app.palette.overlay0)),
        Span::styled(" drag borders ", Style::default().fg(app.palette.overlay1)),
        Span::styled("·", Style::default().fg(app.palette.overlay0)),
        Span::styled(
            " ⇥",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" pane", Style::default().fg(app.palette.overlay1)),
    ]);
    frame.render_widget(Paragraph::new(line2), rows[5]);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ● ", Style::default().fg(app.palette.red)),
            Span::styled("needs you    ", Style::default().fg(app.palette.overlay1)),
            Span::styled("○ ", Style::default().fg(app.palette.yellow)),
            Span::styled("working    ", Style::default().fg(app.palette.overlay1)),
            Span::styled("◌ ", Style::default().fg(app.palette.overlay0)),
            Span::styled("no agent", Style::default().fg(app.palette.overlay1)),
        ])),
        rows[7],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  continue  ",
                Style::default()
                    .fg(app.palette.panel_bg)
                    .bg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "   readme has config and more",
                Style::default().fg(app.palette.overlay0),
            ),
        ])),
        rows[9],
    );
}

fn render_onboarding_notifications(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_modal_shell(frame, area, 52, 10, &app.palette) else {
        return;
    };

    if inner.height < 7 {
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<7>(inner);

    render_modal_header(frame, rows[0], "choose notification style", &app.palette);
    frame.render_widget(
        Paragraph::new(" herdr can alert you when background work needs attention or finishes.")
            .style(Style::default().fg(app.palette.overlay1)),
        rows[1],
    );

    let options = [
        "quiet        no sound, no visual toasts",
        "visual only  top-right toasts, no sound",
        "sound only   sound alerts, no toasts",
        "both         sound and visual toasts",
    ];

    for (idx, option) in options.iter().enumerate() {
        let selected = idx == app.onboarding_selected;
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
            rows[idx + 2],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" [ back ] ", Style::default().fg(app.palette.overlay0)),
            Span::raw("  "),
            Span::styled(
                " [ save ] ",
                Style::default()
                    .fg(app.palette.panel_bg)
                    .bg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        rows[6],
    );
}

/// Floating overlay for navigate mode — appears at bottom of terminal area.
fn render_navigate_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.palette.overlay0);
    let label = Style::default().fg(app.palette.text);

    let kb = &app.keybinds;
    let line1 = Line::from(vec![
        Span::styled(format!(" {}", kb.new_workspace_label), key),
        Span::styled(" new  ", dim),
        Span::styled(kb.rename_workspace_label.as_str(), key),
        Span::styled(" rename  ", dim),
        Span::styled(kb.close_workspace_label.as_str(), key),
        Span::styled(" close ws  ", dim),
        Span::styled(kb.split_vertical_label.as_str(), key),
        Span::styled(" split│  ", dim),
        Span::styled(kb.split_horizontal_label.as_str(), key),
        Span::styled(" split─  ", dim),
        Span::styled(kb.close_pane_label.as_str(), key),
        Span::styled(" close pane  ", dim),
        Span::styled(kb.fullscreen_label.as_str(), key),
        Span::styled(" fullscreen", dim),
    ]);

    let ws_name = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .map(|ws| ws.display_name())
        .unwrap_or_else(|| "—".to_string());

    let pane_info = app
        .active
        .and_then(|i| app.workspaces.get(i))
        .filter(|ws| ws.layout.pane_count() > 1)
        .map(|ws| {
            let ids = ws.layout.pane_ids();
            let pos = ids
                .iter()
                .position(|id| *id == ws.layout.focused())
                .unwrap_or(0);
            format!(" [{}/{}]", pos + 1, ids.len())
        })
        .unwrap_or_default();

    let mode_style = Style::default()
        .fg(app.palette.panel_bg)
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);

    let line2 = Line::from(vec![
        Span::styled(" NAVIGATE ", mode_style),
        Span::raw(" "),
        Span::styled(ws_name, label),
        Span::styled(&pane_info, dim),
        Span::raw("  "),
        Span::styled("esc", key),
        Span::styled(" back  ", dim),
        Span::styled("↑↓", key),
        Span::styled(" ws  ", dim),
        Span::styled("⇥", key),
        Span::styled(" pane  ", dim),
        Span::styled(kb.resize_mode_label.as_str(), key),
        Span::styled(" resize  ", dim),
        Span::styled(kb.toggle_sidebar_label.as_str(), key),
        Span::styled(" sidebar  ", dim),
        Span::styled("s", key),
        Span::styled(" settings  ", dim),
        Span::styled("⏎", key),
        Span::styled(" open  ", dim),
        Span::styled("q", key),
        Span::styled(" quit", dim),
    ]);

    let overlay_height = 2;
    let overlay_y = area.y + area.height.saturating_sub(overlay_height);
    let overlay_area = Rect::new(area.x, overlay_y, area.width, overlay_height);

    // Clear the area behind the overlay
    frame.render_widget(Clear, overlay_area);

    let bg = Style::default().bg(app.palette.panel_bg);
    let buf = frame.buffer_mut();
    for y in overlay_area.y..overlay_area.y + overlay_area.height {
        for x in overlay_area.x..overlay_area.x + overlay_area.width {
            buf[(x, y)].set_style(bg);
        }
    }

    let [row1, row2] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(overlay_area);
    frame.render_widget(Paragraph::new(line1), row1);
    frame.render_widget(Paragraph::new(line2), row2);
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
    let popup_w = 44u16.min(area.width.saturating_sub(4));
    let popup_h = 6u16;
    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);

    let key = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
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

    if inner.height >= 4 {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas::<4>(inner);

        frame.render_widget(Paragraph::new(title_line), rows[0]);
        frame.render_widget(Paragraph::new(detail_line), rows[1]);

        let (confirm_rect, cancel_rect) = confirm_close_button_rects(inner);
        frame.render_widget(
            Paragraph::new(" confirm ").style(
                Style::default()
                    .fg(app.palette.panel_bg)
                    .bg(app.palette.red)
                    .add_modifier(Modifier::BOLD),
            ),
            confirm_rect,
        );
        frame.render_widget(
            Paragraph::new(" cancel ").style(
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.surface0)
                    .add_modifier(Modifier::BOLD),
            ),
            cancel_rect,
        );

        let hints = Line::from(vec![
            Span::styled(" enter/y", key),
            Span::styled(" confirm  ", dim),
            Span::styled("esc/click", key),
            Span::styled(" cancel", dim),
        ]);
        frame.render_widget(Paragraph::new(hints), rows[3]);
    }
}

fn confirm_close_button_rects(inner: Rect) -> (Rect, Rect) {
    let confirm_w = 9u16;
    let cancel_w = 8u16;
    let gap = 2u16;
    let total_w = confirm_w + gap + cancel_w;
    let x = inner.x + inner.width.saturating_sub(total_w) / 2;
    let y = inner.y + 2.min(inner.height.saturating_sub(1));
    (
        Rect::new(x, y, confirm_w.min(inner.width), 1),
        Rect::new(
            x + confirm_w + gap,
            y,
            cancel_w.min(inner.width.saturating_sub(confirm_w + gap)),
            1,
        ),
    )
}

/// Right-click context menu popup anchored near the click position.
// ---------------------------------------------------------------------------
// Settings overlay
// ---------------------------------------------------------------------------

fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::SettingsSection;

    let p = &app.palette;
    let popup_w: u16 = 56;
    let popup_h: u16 = 20;

    let popup_w = popup_w.min(area.width.saturating_sub(4));
    let popup_h = popup_h.min(area.height.saturating_sub(2));
    if popup_w < 20 || popup_h < 10 {
        return;
    }

    let popup_x = area.x + (area.width.saturating_sub(popup_w)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_h)) / 2;
    let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);

    // Dim everything behind the modal
    dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let mut y = inner.y;

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " settings",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    // Tab bar
    let tabs: Vec<Span> = SettingsSection::ALL
        .iter()
        .map(|s| {
            let active = *s == app.settings.section;
            let label = format!(" {} ", s.label());
            if active {
                Span::styled(
                    label,
                    Style::default()
                        .fg(p.panel_bg)
                        .bg(p.accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(label, Style::default().fg(p.overlay1))
            }
        })
        .collect();
    let mut tab_line: Vec<Span> = Vec::new();
    tab_line.push(Span::styled(" ", Style::default()));
    for (i, tab) in tabs.into_iter().enumerate() {
        if i > 0 {
            tab_line.push(Span::styled(" ", Style::default()));
        }
        tab_line.push(tab);
    }
    frame.render_widget(
        Paragraph::new(Line::from(tab_line)),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    // Separator
    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        Rect::new(inner.x, y, inner.width, 1),
    );
    y += 1;

    // Section content
    let content_area = Rect::new(inner.x, y, inner.width, inner.y + inner.height - y);

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
                app.settings.selected,
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
                app.settings.selected,
            );
        }
    }

    // Footer hints
    let footer_y = inner.y + inner.height - 1;
    if footer_y > y {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(" select  ", Style::default().fg(p.overlay1)),
                Span::styled("tab", Style::default().fg(p.overlay0)),
                Span::styled(" section  ", Style::default().fg(p.overlay1)),
                Span::styled("⏎", Style::default().fg(p.overlay0)),
                Span::styled(" apply  ", Style::default().fg(p.overlay1)),
                Span::styled("esc", Style::default().fg(p.overlay0)),
                Span::styled(" close", Style::default().fg(p.overlay1)),
            ])),
            Rect::new(inner.x, footer_y, inner.width, 1),
        );
    }
}

/// Render the theme picker list inside the settings panel.
fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let max_visible = (area.height as usize).saturating_sub(1);
    let selected = app.settings.selected;

    // Scroll offset
    let scroll = if selected >= max_visible {
        selected - max_visible + 1
    } else {
        0
    };

    for (vi, theme_idx) in (scroll..THEME_NAMES.len()).enumerate() {
        if vi >= max_visible {
            break;
        }
        let name = THEME_NAMES[theme_idx];
        let is_selected = theme_idx == selected;
        let is_current = name.to_lowercase().replace([' ', '_'], "-")
            == app.theme_name.to_lowercase().replace([' ', '_'], "-");

        let prefix = if is_selected { "▸" } else { " " };
        let marker = if is_current { " ✓" } else { "" };

        let row_y = area.y + vi as u16;

        // Background for selected
        if is_selected {
            let buf = frame.buffer_mut();
            for x in area.x..area.x + area.width {
                buf[(x, row_y)].set_style(Style::default().bg(p.surface0));
            }
        }

        let name_style = if is_selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {prefix} "), Style::default().fg(p.overlay0)),
                Span::styled(name, name_style),
                Span::styled(marker, Style::default().fg(p.green)),
            ])),
            Rect::new(area.x, row_y, area.width, 1),
        );
    }
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
    let mut y = area.y;

    // Description — truncate to fit within area
    let max_desc_len = (area.width as usize).saturating_sub(2);
    let desc_text = if description.len() > max_desc_len {
        format!(" {}…", &description[..max_desc_len.saturating_sub(2)])
    } else {
        format!(" {description}")
    };
    frame.render_widget(
        Paragraph::new(Span::styled(desc_text, Style::default().fg(p.overlay1))),
        Rect::new(area.x, y, area.width, 1),
    );
    y += 2;

    let options = ["on", "off"];
    for (i, label) in options.iter().enumerate() {
        if y >= area.y + area.height {
            break;
        }
        let is_selected = i == selected_idx;
        let is_active = (i == 0) == current_value;

        let prefix = if is_selected { "▸" } else { " " };
        let marker = if is_active { " ✓" } else { "" };

        if is_selected {
            let buf = frame.buffer_mut();
            for x in area.x..area.x + area.width {
                buf[(x, y)].set_style(Style::default().bg(p.surface0));
            }
        }

        let label_style = if is_selected {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {prefix} "), Style::default().fg(p.overlay0)),
                Span::styled(format!("{title}: {label}"), label_style),
                Span::styled(marker, Style::default().fg(p.green)),
            ])),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }
}

fn render_context_menu(app: &AppState, frame: &mut Frame) {
    use crate::app::CONTEXT_MENU_ITEMS;

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

    let highlight = Style::default()
        .fg(p.panel_bg)
        .bg(p.accent)
        .add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(p.text).bg(p.panel_bg);

    for (i, item) in CONTEXT_MENU_ITEMS.iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let style = if i == menu.selected {
            highlight
        } else {
            normal
        };
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        frame.render_widget(Paragraph::new(format!(" {item}")).style(style), row);
    }
}

fn render_update_notification(frame: &mut Frame, area: Rect, version: &str, p: &Palette) {
    let text = format!(" ✦ herdr v{version} installed — restart to update ");
    let width = text.len() as u16 + 2;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(3);
    let notif_area = Rect::new(x, y, width.min(area.width), 1);

    frame.render_widget(Clear, notif_area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            text,
            Style::default()
                .fg(p.panel_bg)
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )),
        notif_area,
    );
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
    };
    let content_width = (toast.title.len().max(toast.context.len()) as u16) + 4;
    let width = content_width.saturating_add(2).min(area.width);
    let height = 4u16.min(area.height);
    let x = area.x + area.width.saturating_sub(width);
    let y = area.y + if offset_for_warning { 1 } else { 0 };
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
/// | Busy               | ●    | Yellow |
/// | Done (idle+unseen) | ●    | Blue   |
/// | Idle (seen)        | ○    | Green  |
/// | Unknown            | ·    | Gray   |
///
/// Filled dot = needs attention (working, or finished unseen).
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
        (AgentState::Waiting, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Busy, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
}

/// Rich icon for per-pane agent detail (bottom section).
/// Uses animated spinner for busy state.
fn agent_icon(state: AgentState, seen: bool, tick: u32, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Waiting, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Busy, _) => (spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

/// State label for the agent detail panel.
fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Waiting, _) => "waiting",
        (AgentState::Busy, _) => "running",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

/// Color for the state label text.
fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Waiting, _) => p.red,
        (AgentState::Busy, _) => p.yellow,
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
