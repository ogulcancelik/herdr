use ratatui::{
    layout::Rect,
    style::{Color, Style},
    Frame,
};

use crate::app::AppState;
use crate::layout::PaneInfo;

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

pub(super) fn render_scrollbar(
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

pub(super) fn render_pane_scrollbar(
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
