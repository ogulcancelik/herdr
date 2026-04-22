use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    action_button_width, modal_stack_areas, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell,
};
use crate::app::{
    state::{Palette, ReleaseNotesState},
    AppState,
};

pub(crate) const RELEASE_NOTES_MODAL_SIZE: (u16, u16) = (80, 24);

pub(super) fn render_release_notes_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(notes) = &app.release_notes else {
        return;
    };

    super::dim_background(frame, area);

    let Some(inner) = render_modal_shell(
        frame,
        area,
        RELEASE_NOTES_MODAL_SIZE.0,
        RELEASE_NOTES_MODAL_SIZE.1,
        &app.palette,
    ) else {
        return;
    };
    if inner.height < 8 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    let header_title_area = Rect::new(
        header_rows[0].x + 1,
        header_rows[0].y,
        header_rows[0].width.saturating_sub(2),
        header_rows[0].height,
    );
    let header_subtitle_area = Rect::new(
        header_rows[1].x + 1,
        header_rows[1].y,
        header_rows[1].width.saturating_sub(2),
        header_rows[1].height,
    );

    render_modal_header(
        frame,
        header_title_area,
        &format!("v{}", notes.version),
        &app.palette,
    );
    let subtitle = if notes.preview {
        "update ready"
    } else {
        "what's new in this release"
    };
    frame.render_widget(
        Paragraph::new(subtitle).style(Style::default().fg(app.palette.overlay1)),
        header_subtitle_area,
    );
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

    let sections = release_notes_sections(stack.content, notes.preview);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app.release_notes_max_scroll().saturating_sub(notes.scroll) as usize,
        max_offset_from_bottom: app.release_notes_max_scroll() as usize,
        viewport_rows: sections.notes_body.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(sections.notes_body, metrics);
    let notes_text_area = track
        .map(|_| {
            Rect::new(
                sections.notes_body.x,
                sections.notes_body.y,
                sections.notes_body.width.saturating_sub(1),
                sections.notes_body.height,
            )
        })
        .unwrap_or(sections.notes_body);

    if let Some(instructions_area) = sections.instructions {
        render_release_notes_preview_panel(frame, instructions_area, &notes.version, &app.palette);
    }

    let body = Paragraph::new(
        release_notes_display_lines(notes, &app.palette)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((notes.scroll, 0));
    frame.render_widget(body, notes_text_area);
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
            Span::styled(" esc / enter ", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}

fn release_notes_inline_spans<'a>(
    text: &str,
    base_style: Style,
    code_style: Style,
) -> (usize, Vec<Span<'a>>) {
    let mut spans = Vec::new();
    let mut width = 0;
    let mut remaining = text;

    while let Some(start) = remaining.find('`') {
        let (before, after_start) = remaining.split_at(start);
        if !before.is_empty() {
            width += before.chars().count();
            spans.push(Span::styled(before.to_string(), base_style));
        }

        let after_start = &after_start[1..];
        let Some(end) = after_start.find('`') else {
            let literal = format!("`{after_start}");
            width += literal.chars().count();
            spans.push(Span::styled(literal, base_style));
            remaining = "";
            break;
        };

        let (code, after_end) = after_start.split_at(end);
        width += code.chars().count();
        if !code.is_empty() {
            spans.push(Span::styled(code.to_string(), code_style));
        }
        remaining = &after_end[1..];
    }

    if !remaining.is_empty() {
        width += remaining.chars().count();
        spans.push(Span::styled(remaining.to_string(), base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    (width, spans)
}

pub(crate) fn release_notes_lines<'a>(body: &'a str, p: &Palette) -> Vec<(usize, Line<'a>)> {
    let mut lines = Vec::new();
    let mut in_fenced_code_block = false;
    let text_style = Style::default().fg(p.text);
    let inline_code_style = Style::default()
        .fg(p.accent)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);

    for raw in body.lines() {
        let trimmed = raw.trim_end();
        if trimmed.trim_start().starts_with("```") {
            in_fenced_code_block = !in_fenced_code_block;
            continue;
        }

        if in_fenced_code_block {
            let code_bg = p.surface1;
            let gutter_style = Style::default().fg(p.accent).bg(code_bg);
            let code_style = Style::default().fg(p.text).bg(code_bg);
            let width = 2 + trimmed.chars().count();
            let mut spans = vec![
                Span::styled("▏", gutter_style),
                Span::styled(" ", code_style),
            ];
            if !trimmed.is_empty() {
                spans.push(Span::styled(trimmed.to_string(), code_style));
            }
            lines.push((width, Line::from(spans)));
            continue;
        }

        if trimmed.is_empty() {
            lines.push((0, Line::raw("")));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let text = rest.trim().to_string();
            if text.is_empty() {
                lines.push((0, Line::raw("")));
                continue;
            }
            let width = 1 + text.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        text.to_uppercase(),
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            let (text_width, mut spans) =
                release_notes_inline_spans(rest, text_style, inline_code_style);
            let width = 3 + text_width;
            let mut line_spans = vec![Span::styled(" • ", Style::default().fg(p.accent))];
            line_spans.append(&mut spans);
            lines.push((width, Line::from(line_spans)));
            continue;
        }

        let (text_width, mut spans) =
            release_notes_inline_spans(trimmed, text_style, inline_code_style);
        let width = 1 + text_width;
        let mut line_spans = vec![Span::raw(" ")];
        line_spans.append(&mut spans);
        lines.push((width, Line::from(line_spans)));
    }

    lines
}

pub(crate) struct ReleaseNotesSections {
    pub instructions: Option<Rect>,
    pub notes_body: Rect,
}

pub(crate) fn release_notes_sections(area: Rect, preview: bool) -> ReleaseNotesSections {
    if preview && area.height >= 6 {
        let rows = Layout::vertical([Constraint::Length(5), Constraint::Min(0)]).areas::<2>(area);
        ReleaseNotesSections {
            instructions: Some(rows[0]),
            notes_body: rows[1],
        }
    } else {
        ReleaseNotesSections {
            instructions: None,
            notes_body: area,
        }
    }
}

pub(super) fn release_notes_preview_lines<'a>(_version: &str, p: &Palette) -> Vec<Line<'a>> {
    let title_style = Style::default().fg(p.text).add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(p.text);
    let code_style = Style::default()
        .fg(p.accent)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);

    vec![
        Line::from(vec![
            Span::styled(
                "●",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" update ready", title_style),
        ]),
        Line::from(vec![
            Span::styled("detach from this session, then run ", text_style),
            Span::styled("herdr update", code_style),
            Span::styled(" in your shell", text_style),
        ]),
    ]
}

fn render_release_notes_preview_panel(frame: &mut Frame, area: Rect, _version: &str, p: &Palette) {
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<4>(area);

    let text_area = Rect::new(
        rows[0].x + 1,
        rows[0].y,
        rows[0].width.saturating_sub(2),
        rows[0].height,
    );
    frame.render_widget(
        Paragraph::new(release_notes_preview_lines(_version, p)).wrap(Wrap { trim: false }),
        text_area,
    );

    let divider_area = Rect::new(rows[2].x + 1, rows[2].y, rows[2].width.saturating_sub(2), 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "─".repeat(divider_area.width as usize),
            Style::default().fg(p.surface1),
        )])),
        divider_area,
    );
}

pub(crate) fn release_notes_display_lines<'a>(
    notes: &'a ReleaseNotesState,
    p: &Palette,
) -> Vec<(usize, Line<'a>)> {
    release_notes_lines(notes.body.as_str(), p)
}

pub(crate) fn release_notes_close_button_rect(area: Rect) -> Rect {
    let width = action_button_width(Some("esc"), "close");
    Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1)
}
