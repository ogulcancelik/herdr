use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, render_panel_shell, ActionButtonSpec,
};
use crate::app::{state::Palette, AppState, Mode};

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

pub(super) fn render_rename_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);

    let title = match app.mode {
        Mode::RenameWorkspace => "rename workspace",
        Mode::RenameTab if app.creating_new_tab => "new tab",
        Mode::RenameTab => "rename tab",
        Mode::RenamePane => "rename pane",
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
            .fg(panel_contrast_fg(&app.palette))
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

pub(super) fn render_confirm_close_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
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

    super::dim_background(frame, area);

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
                .fg(panel_contrast_fg(&app.palette))
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

pub(super) fn render_new_workspace_picker_overlay(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(picker) = app.new_workspace_picker.as_ref() else {
        return;
    };

    super::dim_background(frame, area);

    let title = match app.mode {
        Mode::NewWorkspaceTypePicker => "new workspace",
        Mode::NewWorkspaceRemotePicker => {
            let provider_name = picker
                .provider_index
                .and_then(|i| app.remote_providers.get(i))
                .map(|p| p.name.as_str())
                .unwrap_or("remote");
            return render_remote_picker(app, frame, area, picker, provider_name);
        }
        _ => return,
    };

    // Reserve space: header, blank, one row per entry (cap at 10), blank, footer.
    let visible = picker.entries.len().min(10) as u16;
    let height = visible + 4;
    let Some(inner) = render_modal_shell(frame, area, 56, height, &app.palette) else {
        return;
    };

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas::<4>(inner);

    render_modal_header(frame, rows[0], title, &app.palette);

    render_picker_list(frame, rows[2], &picker.entries, picker.highlighted, &app.palette);

    let footer = Paragraph::new(Span::styled(
        " ↑↓ select  ↵ confirm  esc cancel",
        Style::default().fg(app.palette.overlay0),
    ));
    frame.render_widget(footer, rows[3]);
}

fn render_remote_picker(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    picker: &crate::app::state::NewWorkspacePickerState,
    provider_name: &str,
) {
    let visible = picker.entries.len().min(12).max(1) as u16;
    let extra = if picker.message.is_some() { 1 } else { 0 };
    let height = visible + 4 + extra;
    let Some(inner) = render_modal_shell(frame, area, 64, height, &app.palette) else {
        return;
    };

    let header_text = format!("remote workspace · {}", provider_name);
    if let Some(message) = &picker.message {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas::<5>(inner);
        render_modal_header(frame, rows[0], &header_text, &app.palette);
        let msg = Paragraph::new(Span::styled(
            format!(" {message}"),
            Style::default().fg(app.palette.red),
        ));
        frame.render_widget(msg, rows[1]);
        render_picker_list(
            frame,
            rows[2],
            &picker.entries,
            picker.highlighted,
            &app.palette,
        );
        let footer = Paragraph::new(Span::styled(
            " ↑↓ select  ↵ connect  esc cancel",
            Style::default().fg(app.palette.overlay0),
        ));
        frame.render_widget(footer, rows[4]);
    } else {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas::<4>(inner);
        render_modal_header(frame, rows[0], &header_text, &app.palette);
        render_picker_list(
            frame,
            rows[2],
            &picker.entries,
            picker.highlighted,
            &app.palette,
        );
        let footer = Paragraph::new(Span::styled(
            " ↑↓ select  ↵ connect  esc cancel",
            Style::default().fg(app.palette.overlay0),
        ));
        frame.render_widget(footer, rows[3]);
    }
}

fn render_picker_list(
    frame: &mut Frame,
    area: Rect,
    entries: &[String],
    highlighted: usize,
    palette: &Palette,
) {
    if entries.is_empty() {
        let empty = Paragraph::new(Span::styled(
            "  (no entries — list_command returned nothing)",
            Style::default().fg(palette.overlay0),
        ));
        frame.render_widget(empty, area);
        return;
    }

    let max_visible = area.height as usize;
    if max_visible == 0 {
        return;
    }

    let start = if highlighted >= max_visible {
        highlighted + 1 - max_visible
    } else {
        0
    };
    let end = (start + max_visible).min(entries.len());

    let lines: Vec<Line> = entries[start..end]
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let idx = start + offset;
            let selected = idx == highlighted;
            let prefix = if selected { " ▶ " } else { "   " };
            let style = if selected {
                Style::default()
                    .fg(panel_contrast_fg(palette))
                    .bg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            Line::from(Span::styled(format!("{prefix}{entry}"), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
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
