use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::AppState;

fn keybind_label(bindings: &crate::config::ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_string())
}

fn indexed_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        "unset".to_string()
    } else if bindings.len() == 9 {
        let first = &bindings[0].label;
        if first.ends_with('1') {
            format!("{}1..9", first.trim_end_matches('1'))
        } else {
            bindings
                .iter()
                .map(|binding| binding.label.clone())
                .collect::<Vec<_>>()
                .join(" / ")
        }
    } else {
        bindings
            .iter()
            .map(|binding| binding.label.clone())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

pub(super) fn keybind_help_groups(app: &AppState) -> Vec<(&'static str, Vec<(String, String)>)> {
    let kb = &app.keybinds;
    let mut groups = Vec::new();

    groups.push((
        "global",
        vec![
            (
                crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
                "prefix mode".to_string(),
            ),
            (keybind_label(&kb.help), "keybinds".to_string()),
            (keybind_label(&kb.settings), "settings".to_string()),
            (keybind_label(&kb.detach), "detach".to_string()),
            (
                keybind_label(&kb.reload_config),
                "reload config".to_string(),
            ),
            (
                keybind_label(&kb.open_notification_target),
                "open notification target".to_string(),
            ),
        ],
    ));

    groups.push((
        "navigation",
        vec![
            ("esc".to_string(), "back".to_string()),
            (
                format!(
                    "{} / {}",
                    keybind_label(&kb.navigate.workspace_up),
                    keybind_label(&kb.navigate.workspace_down)
                ),
                "workspace list".to_string(),
            ),
            (
                format!(
                    "{} / {} / {} / {} / left / right",
                    keybind_label(&kb.navigate.pane_left),
                    keybind_label(&kb.navigate.pane_down),
                    keybind_label(&kb.navigate.pane_up),
                    keybind_label(&kb.navigate.pane_right)
                ),
                "move focus".to_string(),
            ),
            ("tab / shift+tab".to_string(), "cycle pane".to_string()),
            ("enter".to_string(), "open workspace".to_string()),
            ("1..9".to_string(), "switch workspace".to_string()),
        ],
    ));

    let workspace_tab = vec![
        (
            keybind_label(&kb.workspace_picker),
            "workspace navigation".to_string(),
        ),
        (keybind_label(&kb.goto), "session navigator".to_string()),
        (
            keybind_label(&kb.new_workspace),
            "new workspace".to_string(),
        ),
        (keybind_label(&kb.new_worktree), "new worktree".to_string()),
        (
            keybind_label(&kb.open_worktree),
            "open worktree".to_string(),
        ),
        (
            keybind_label(&kb.remove_worktree),
            "delete worktree checkout".to_string(),
        ),
        (
            keybind_label(&kb.rename_workspace),
            "rename workspace".to_string(),
        ),
        (
            keybind_label(&kb.close_workspace),
            "close workspace".to_string(),
        ),
        (
            keybind_label(&kb.previous_workspace),
            "previous workspace".to_string(),
        ),
        (
            keybind_label(&kb.next_workspace),
            "next workspace".to_string(),
        ),
        (
            indexed_label(&kb.switch_workspace),
            "switch workspace 1-9".to_string(),
        ),
        (
            keybind_label(&kb.previous_agent),
            "previous agent".to_string(),
        ),
        (keybind_label(&kb.next_agent), "next agent".to_string()),
        (
            indexed_label(&kb.focus_agent),
            "focus agent 1-9".to_string(),
        ),
        (keybind_label(&kb.new_tab), "new tab".to_string()),
        (keybind_label(&kb.rename_tab), "rename tab".to_string()),
        (keybind_label(&kb.previous_tab), "previous tab".to_string()),
        (keybind_label(&kb.next_tab), "next tab".to_string()),
        (indexed_label(&kb.switch_tab), "switch tab 1-9".to_string()),
        (keybind_label(&kb.close_tab), "close tab".to_string()),
    ];
    groups.push(("workspaces / tabs", workspace_tab));

    let panes = vec![
        (
            keybind_label(&kb.split_vertical),
            "split vertical".to_string(),
        ),
        (
            keybind_label(&kb.split_horizontal),
            "split horizontal".to_string(),
        ),
        (keybind_label(&kb.close_pane), "close pane".to_string()),
        (keybind_label(&kb.rename_pane), "rename pane".to_string()),
        (
            keybind_label(&kb.edit_scrollback),
            "edit scrollback".to_string(),
        ),
        (keybind_label(&kb.copy_mode), "copy mode".to_string()),
        (keybind_label(&kb.zoom), "zoom pane".to_string()),
        (keybind_label(&kb.resize_mode), "resize mode".to_string()),
        (
            keybind_label(&kb.toggle_sidebar),
            "toggle sidebar".to_string(),
        ),
        (
            keybind_label(&kb.focus_pane_left),
            "focus pane left".to_string(),
        ),
        (
            keybind_label(&kb.focus_pane_down),
            "focus pane down".to_string(),
        ),
        (
            keybind_label(&kb.focus_pane_up),
            "focus pane up".to_string(),
        ),
        (
            keybind_label(&kb.focus_pane_right),
            "focus pane right".to_string(),
        ),
        (
            keybind_label(&kb.cycle_pane_next),
            "cycle pane next".to_string(),
        ),
        (
            keybind_label(&kb.cycle_pane_previous),
            "cycle pane previous".to_string(),
        ),
        (keybind_label(&kb.last_pane), "last pane".to_string()),
    ];
    groups.push(("panes", panes));

    if !kb.custom_commands.is_empty() {
        groups.push((
            "custom",
            kb.custom_commands
                .iter()
                .map(|binding| {
                    let desc = binding.description.as_deref().unwrap_or("custom command");
                    (binding.label.clone(), desc.to_string())
                })
                .collect(),
        ));
    }

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

pub(super) fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    super::dim_background(frame, frame.area());

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
            .fg(panel_contrast_fg(&app.palette))
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
            Span::styled(" esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}
