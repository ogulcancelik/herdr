use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::widgets::{
    action_button_row_rects, action_button_width, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_header, render_modal_shell, ActionButtonSpec,
};
use crate::app::AppState;

const ONBOARDING_PREFIX_LABEL: &str = "ctrl+b";

pub(super) fn render_onboarding_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);

    match app.onboarding_step {
        0 => render_onboarding_welcome(app, frame, area),
        _ => render_onboarding_notifications(app, frame, area),
    }
}

pub(crate) fn onboarding_welcome_continue_rect(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        action_button_width(Some("↵"), "continue"),
        1,
    )
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
            .fg(panel_contrast_fg(&app.palette))
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
                .fg(panel_contrast_fg(&app.palette))
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
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}
