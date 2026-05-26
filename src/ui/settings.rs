use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_choice_list, render_panel_shell, ActionButtonSpec,
};
use crate::{
    app::{
        state::{
            ordered_sidebar_agent_items, ordered_sidebar_space_items, Palette, SettingsSection,
            SidebarConfigGroup, SidebarLine,
        },
        AppState,
    },
    config::ToastDelivery,
};

pub(crate) const SETTINGS_POPUP_WIDTH: u16 = 96;
pub(crate) const SETTINGS_POPUP_HEIGHT: u16 = 32;

pub(super) fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let Some(popup) = centered_popup_rect(area, SETTINGS_POPUP_WIDTH, SETTINGS_POPUP_HEIGHT) else {
        return;
    };

    super::dim_background(frame, area);

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

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " settings",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );

    let tab_labels = SettingsSection::ALL.iter().map(|section| {
        if app.settings_section_has_badge(*section) {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(section.label()),
            ])
        } else {
            Line::from(section.label())
        }
    });
    let tabs = Tabs::new(tab_labels)
        .select(
            SettingsSection::ALL
                .iter()
                .position(|section| *section == app.settings.section)
                .unwrap_or(0),
        )
        .style(Style::default().fg(p.overlay1))
        .highlight_style(
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ")
        .padding(" ", " ");
    frame.render_widget(tabs, header_rows[1]);

    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[2],
    );

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
            render_modal_choice_list(
                frame,
                content_area,
                "notification popups",
                "choose where background popup notifications should appear",
                &[
                    ("off", ToastDelivery::Off),
                    ("inside herdr", ToastDelivery::Herdr),
                    ("via terminal", ToastDelivery::Terminal),
                    ("via system", ToastDelivery::System),
                ],
                app.toast_delivery(),
                app.settings.list.selected,
                p,
                2,
            );
        }
        SettingsSection::PaneLabels => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                "agent border labels",
                "show detected agent names in split pane borders",
                app.agent_border_labels_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Sidebar => {
            render_settings_sidebar_config(app, frame, content_area);
        }
        SettingsSection::Experiments => {
            render_settings_experiments(app, frame, content_area);
        }
        SettingsSection::Integrations => {
            render_settings_integrations(app, frame, content_area);
        }
    }

    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let primary_label = settings_primary_button_label(app.settings.section);
        let show_primary = settings_show_primary_action(app);
        let (apply_rect, close_rect) =
            settings_button_rects(inner, app.settings.section, show_primary);
        if let Some(apply_rect) = apply_rect {
            render_action_button(
                frame,
                apply_rect,
                Some("↵"),
                primary_label,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
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

        frame.render_widget(Paragraph::new(settings_footer_hint(app)), footer_rows[0]);
    }
}

fn settings_footer_hint(app: &AppState) -> Line<'static> {
    let p = &app.palette;
    if app.settings.section == SettingsSection::Sidebar {
        if app.settings.sidebar_config_editing {
            return Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(" reorder  ", Style::default().fg(p.overlay1)),
                Span::styled("c", Style::default().fg(p.overlay0)),
                Span::styled(" color  ", Style::default().fg(p.overlay1)),
                Span::styled("enter", Style::default().fg(p.overlay0)),
                Span::styled(" done  ", Style::default().fg(p.overlay1)),
                Span::styled("tab", Style::default().fg(p.overlay0)),
                Span::styled(" section", Style::default().fg(p.overlay1)),
            ]);
        }
        return Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
            Span::styled(" select  ", Style::default().fg(p.overlay1)),
            Span::styled("←→", Style::default().fg(p.overlay0)),
            Span::styled(" group  ", Style::default().fg(p.overlay1)),
            Span::styled("space", Style::default().fg(p.overlay0)),
            Span::styled(" toggle  ", Style::default().fg(p.overlay1)),
            Span::styled("c", Style::default().fg(p.overlay0)),
            Span::styled(" color  ", Style::default().fg(p.overlay1)),
            Span::styled("enter", Style::default().fg(p.overlay0)),
            Span::styled(" edit  ", Style::default().fg(p.overlay1)),
            Span::styled("tab", Style::default().fg(p.overlay0)),
            Span::styled(" section", Style::default().fg(p.overlay1)),
        ]);
    }

    Line::from(vec![
        Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
        Span::styled(" select  ", Style::default().fg(p.overlay1)),
        Span::styled("tab", Style::default().fg(p.overlay0)),
        Span::styled(" section", Style::default().fg(p.overlay1)),
    ])
}

pub(crate) fn settings_primary_button_label(
    section: crate::app::state::SettingsSection,
) -> &'static str {
    match section {
        crate::app::state::SettingsSection::Integrations => "install",
        _ => "apply",
    }
}

pub(crate) fn settings_show_primary_action(app: &AppState) -> bool {
    app.settings.section != crate::app::state::SettingsSection::Integrations
        || app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
}

pub(crate) fn settings_button_rects(
    inner: Rect,
    section: crate::app::state::SettingsSection,
    show_primary: bool,
) -> (Option<Rect>, Rect) {
    if !show_primary {
        let rects = action_button_row_rects(
            inner,
            &[ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            }],
            2,
            inner.height.saturating_sub(1),
        );
        return (None, rects[0]);
    }

    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: settings_primary_button_label(section),
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (Some(rects[0]), rects[1])
}

fn render_settings_integrations(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<4>(area);

    frame.render_widget(
        Paragraph::new("agent integrations")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(
            "let agents report state directly instead of relying only on process detection",
        )
        .style(Style::default().fg(p.overlay1))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let mut lines = Vec::new();
    for item in &app.integration_recommendations {
        let marker = match item.state {
            crate::integration::IntegrationStatusKind::Current => "✓",
            crate::integration::IntegrationStatusKind::Outdated => "↻",
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => "+",
            crate::integration::IntegrationStatusKind::NotInstalled => "–",
        };
        let marker_style = match item.state {
            crate::integration::IntegrationStatusKind::Current => Style::default().fg(p.green),
            crate::integration::IntegrationStatusKind::Outdated => Style::default().fg(p.yellow),
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => {
                Style::default().fg(p.accent)
            }
            crate::integration::IntegrationStatusKind::NotInstalled => {
                Style::default().fg(p.overlay0)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                format!("{:<9}", item.label),
                Style::default().fg(p.subtext0),
            ),
            Span::styled(item.status_label(), Style::default().fg(p.overlay1)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no integration targets available",
            Style::default().fg(p.overlay1),
        )));
    }

    if !app.integration_install_messages.is_empty() {
        lines.push(Line::from(""));
        for message in &app.integration_install_messages {
            lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(p.overlay1),
            )));
        }
    } else {
        lines.push(Line::from(""));
        let found_any = app.integration_recommendations.iter().any(|item| {
            item.available || item.state != crate::integration::IntegrationStatusKind::NotInstalled
        });
        let hint = if app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
        {
            " press install to add available or outdated integrations"
        } else if found_any {
            " all detected integrations are installed"
        } else {
            " no supported agent CLIs found on PATH"
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(p.overlay1),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows[3]);
}

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

fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    render_modal_choice_list(
        frame,
        area,
        title,
        description,
        &[("on", true), ("off", false)],
        current_value,
        selected_idx,
        p,
        1,
    );
}

fn render_settings_sidebar_config(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [desc_area, _, list_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(area);

    let title = format!(
        "{} - sidebar config",
        app.settings.sidebar_config_group.label()
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ))),
        Rect::new(desc_area.x, desc_area.y, desc_area.width, 1),
    );
    frame.render_widget(
        Paragraph::new("configure sidebar navigation").style(Style::default().fg(p.overlay1)),
        Rect::new(
            desc_area.x,
            desc_area.y.saturating_add(1),
            desc_area.width,
            1,
        ),
    );

    let mut rows: Vec<(Line<'static>, Option<usize>, bool)> = Vec::new();
    match app.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => {
            let ordered = ordered_sidebar_space_items(&app.sidebar_space);
            let line_count =
                SidebarConfigGroup::Spaces.settings_line_count(app.sidebar_space.lines.len());
            for line in (0..line_count).map(SidebarLine::from_index) {
                rows.push((Line::from(format!(" {}", line.label())), None, true));
                let start_len = rows.len();
                for (idx, item) in ordered
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, item)| item.line(&app.sidebar_space) == line)
                {
                    let marker = if item.enabled(&app.sidebar_space) {
                        "[✓]"
                    } else {
                        "[ ]"
                    };
                    rows.push((
                        sidebar_config_item_row(
                            format!("   {marker} {}", item.label()),
                            item.color(&app.sidebar_space),
                            p,
                        ),
                        Some(idx),
                        false,
                    ));
                }
                if rows.len() == start_len {
                    rows.push((Line::from("   (empty)"), None, false));
                }
            }
        }
        SidebarConfigGroup::Agents => {
            let ordered = ordered_sidebar_agent_items(&app.sidebar_agent);
            let line_count =
                SidebarConfigGroup::Agents.settings_line_count(app.sidebar_agent.lines.len());
            for line in (0..line_count).map(SidebarLine::from_index) {
                rows.push((Line::from(format!(" {}", line.label())), None, true));
                let start_len = rows.len();
                for (idx, item) in ordered
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, item)| item.line(&app.sidebar_agent) == line)
                {
                    let marker = if item.enabled(&app.sidebar_agent) {
                        "[✓]"
                    } else {
                        "[ ]"
                    };
                    rows.push((
                        sidebar_config_item_row(
                            format!("   {marker} {}", item.label()),
                            item.color(&app.sidebar_agent),
                            p,
                        ),
                        Some(idx),
                        false,
                    ));
                }
                if rows.len() == start_len {
                    rows.push((Line::from("   (empty)"), None, false));
                }
            }
        }
    };

    let list_bottom = list_area.y.saturating_add(list_area.height);
    let mut rendered_rows = 0u16;
    for (idx, (text, selected_idx, is_header)) in rows.iter().enumerate() {
        let row = Rect::new(list_area.x, list_area.y + idx as u16, list_area.width, 1);
        if row.y >= list_bottom {
            break;
        }
        let style = if selected_idx == &Some(app.settings.list.selected) {
            let style = Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD);
            if app.settings.sidebar_config_editing {
                style.fg(p.accent)
            } else {
                style
            }
        } else if *is_header {
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD)
        } else if selected_idx.is_none() {
            Style::default().fg(p.overlay1)
        } else {
            Style::default().fg(p.subtext0)
        };
        frame.render_widget(Paragraph::new(text.clone()).style(style), row);
        rendered_rows = rendered_rows.saturating_add(1);
    }

    if rows.len() > rendered_rows as usize {
        return;
    }

    let demo_y = list_area.y + rendered_rows + 1;
    if demo_y >= list_area.y + list_area.height {
        return;
    }
    let demo_title = match app.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => "demo spaces",
        SidebarConfigGroup::Agents => "demo agents",
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            demo_title,
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )),
        Rect::new(list_area.x, demo_y, list_area.width, 1),
    );

    let demo_width = match app.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => list_area.width,
        SidebarConfigGroup::Agents => super::sidebar::settings_sidebar_agent_demo_width(app)
            .max(1)
            .min(list_area.width),
    };
    let demo_lines = match app.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => super::sidebar::settings_sidebar_space_demo_lines(app),
        SidebarConfigGroup::Agents => {
            super::sidebar::settings_sidebar_agent_demo_lines(app, demo_width)
        }
    };
    for (idx, line) in demo_lines.into_iter().enumerate() {
        let y = demo_y + 1 + idx as u16;
        if y >= list_area.y + list_area.height {
            break;
        }
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(list_area.x, y, demo_width, 1),
        );
    }
}

fn sidebar_config_item_row(
    label: String,
    color: crate::config::SidebarColorPreset,
    p: &Palette,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(label),
        Span::raw("  "),
        Span::styled("■", Style::default().fg(p.sidebar_color(color, p.subtext0))),
        Span::styled(
            format!(" {}", color.as_str()),
            Style::default().fg(p.overlay1),
        ),
    ])
}

fn render_settings_experiments(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [desc_area, _, list_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(area);

    super::widgets::render_modal_description(
        frame,
        desc_area,
        "optional and experimental behavior",
        Style::default().fg(p.overlay1),
    );

    let rows = [(
        "pane screen history",
        app.pane_history_persistence_enabled(),
    )];
    for (idx, (label, enabled)) in rows.into_iter().enumerate() {
        let marker = if enabled { "[✓]" } else { "[ ]" };
        let style = if app.settings.list.selected == idx {
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let row = Rect::new(list_area.x, list_area.y + idx as u16, list_area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(" {label} {marker}")).style(style),
            row,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{state::SettingsSection, Mode};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn experiments_pane_history_uses_settings_checkmark_marker() {
        let mut app = AppState::test_new();
        app.pane_history_persistence = true;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("pane screen history [✓]"));
        assert!(!rendered.contains("agent panel details"));
        assert!(!rendered.contains("[x]"));
    }

    #[test]
    fn sidebar_config_renders_spaces_checkbox_rows_and_demo_preview() {
        let mut app = AppState::test_new();
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Name.set_enabled(&mut app.sidebar_space, true);
        crate::app::state::SidebarSpaceItem::Branch.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::BranchStatus.set_enabled(&mut app.sidebar_space, true);
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Spaces;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("spaces - sidebar config"));
        assert!(rendered.contains("1st line"));
        assert!(rendered.contains("[ ] space status"));
        assert!(rendered.contains("■ default"));
        assert!(rendered.contains("[✓] space name"));
        assert!(rendered.contains("2nd line"));
        assert!(rendered.contains("[ ] space branch"));
        assert!(rendered.contains("[✓] space branch status"));
        assert!(!rendered.contains("1st line 1"));
        assert!(rendered.contains("demo spaces"));
        assert!(rendered.contains("↑2"));
    }

    #[test]
    fn sidebar_config_keeps_empty_line_groups_visible() {
        let mut app = AppState::test_new();
        for item in crate::app::state::SIDEBAR_SPACE_ITEMS {
            item.set_line(
                &mut app.sidebar_space,
                crate::app::state::SidebarLine::Second,
            );
        }
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Spaces;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(100, 34)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 100, 34)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("1st line"));
        assert!(rendered.contains("(empty)"));
        assert!(rendered.contains("2nd line"));
        assert!(rendered.contains("[✓] space status"));
        assert!(rendered.contains("[✓] space name"));
    }

    #[test]
    fn sidebar_config_renders_agents_checkbox_rows_and_demo_preview() {
        let mut app = AppState::test_new();
        crate::app::state::SidebarAgentItem::PaneName.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::TabName.set_enabled(&mut app.sidebar_agent, false);
        crate::app::state::SidebarAgentItem::SpaceName.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::AgentStatus.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::Status.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::Time.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::AgentName.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, false);
        app.sidebar_width = 60;
        app.view.sidebar_rect = Rect::new(0, 0, 60, 40);
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Agents;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(110, 34)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 110, 34)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("agents - sidebar config"));
        assert!(rendered.contains("1st line"));
        assert!(rendered.contains("[✓] agent status"));
        assert!(rendered.contains("■ default"));
        assert!(rendered.contains("[✓] agent pane name"));
        assert!(rendered.contains("[ ] agent tab name"));
        assert!(rendered.contains("[✓] space name"));
        assert!(rendered.contains("2nd line"));
        assert!(rendered.contains("[✓] status text"));
        assert!(rendered.contains("[✓] agent time"));
        assert!(rendered.contains("[✓] custom status"));
        assert!(rendered.contains("[ ] right-alignment"));
        assert!(rendered.contains("[✓] agent name"));
        assert!(rendered.contains("3rd line"));
        assert!(rendered.contains("(empty)"));
        assert!(!rendered.contains("layout"));
        assert!(!rendered.contains("right-align agent name"));
        assert!(!rendered.contains("1st line 1"));
        assert!(rendered.contains("demo agents"));
        assert!(rendered.contains("claude"));
        assert!(rendered.contains("codex"));
    }

    #[test]
    fn sidebar_config_agent_demo_uses_sidebar_width_for_right_alignment() {
        let mut app = AppState::test_new();
        app.view.sidebar_rect = Rect::new(0, 0, 26, 40);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, true);
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Agents;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(120, 36)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 120, 36)))
            .expect("settings overlay should render");
        let buffer = terminal.backend().buffer();
        let content = app.settings_content_rect();
        let row = (content.y..content.y + content.height)
            .map(|y| {
                (0..120)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|row| row.contains("working") && row.contains("claude"))
            .expect("working claude demo row should render");
        let claude_col = row
            .find("claude")
            .unwrap_or_else(|| panic!("claude should render in row: {row:?}"))
            as u16;

        let expected_right_edge = row
            .find("working")
            .map(|idx| idx as u16 - 2 + 26)
            .expect("working text should render");
        assert_eq!(claude_col + "claude".len() as u16, expected_right_edge);
    }

    #[test]
    fn sidebar_config_footer_shows_navigation_hints() {
        let mut app = AppState::test_new();
        app.settings.section = SettingsSection::Sidebar;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(110, 28)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 110, 28)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("←→ group"));
        assert!(rendered.contains("enter edit"));
        assert!(rendered.contains("space toggle"));
        assert!(rendered.contains("c color"));
    }

    #[test]
    fn sidebar_config_footer_shows_editing_hints() {
        let mut app = AppState::test_new();
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_editing = true;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(110, 28)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 110, 28)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("↑↓ reorder"));
        assert!(!rendered.contains("←→ line"));
        assert!(!rendered.contains("space toggle"));
        assert!(rendered.contains("c color"));
        assert!(rendered.contains("enter done"));
    }

    #[test]
    fn sidebar_config_rows_do_not_render_past_content_area() {
        let mut app = AppState::test_new();
        app.sidebar_space.lines = vec![Vec::new(); 24];
        app.view.sidebar_rect = Rect::new(0, 0, 110, 28);
        app.view.terminal_area = Rect::new(0, 0, 110, 28);
        app.settings.section = SettingsSection::Sidebar;
        app.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Spaces;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(110, 28)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 110, 28)))
            .expect("settings overlay should render");
        let buffer = terminal.backend().buffer();
        let content = app.settings_content_rect();

        for y in content.y + content.height..28 {
            let row = (0..110)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            assert!(!row.contains("line"), "row {y} leaked config rows: {row:?}");
            assert!(
                !row.contains("space branch"),
                "row {y} leaked config rows: {row:?}"
            );
        }
    }
}
