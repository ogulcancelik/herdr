//! Centered overlay renderer for the ephemeral floating pane.
//!
//! The float is a post-pass over the pane field (no `Mode` variant): when the
//! active workspace has a visible float, paint a centered ~80%x70% panel and
//! render the float terminal's screen through the same runtime cell-painting
//! path layout panes use.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear},
    Frame,
};

use crate::app::{AppState, Mode};
use crate::terminal::TerminalRuntimeRegistry;

use super::panes::pane_is_scrolled_back;
use super::widgets::centered_popup_rect;

const FLOAT_WIDTH_PERCENT: u32 = 80;
const FLOAT_HEIGHT_PERCENT: u32 = 70;

/// Outer rect of the float overlay, centered in the terminal area.
/// None when the area is too small to host a usable overlay.
pub(crate) fn float_overlay_rect(terminal_area: Rect) -> Option<Rect> {
    let width = (u32::from(terminal_area.width) * FLOAT_WIDTH_PERCENT / 100) as u16;
    let height = (u32::from(terminal_area.height) * FLOAT_HEIGHT_PERCENT / 100) as u16;
    centered_popup_rect(terminal_area, width, height)
}

/// Inner (terminal screen) rect of the float overlay: the outer rect minus
/// its border. Used both for rendering and for sizing the float's PTY.
pub(crate) fn float_overlay_inner_rect(terminal_area: Rect) -> Option<Rect> {
    let area = float_overlay_rect(terminal_area)?;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    (inner.width > 0 && inner.height > 0).then_some(inner)
}

/// Keep the float's PTY sized to its overlay rect. Mirrors the per-frame
/// `rt.resize` reconciliation layout panes get in `compute_pane_infos`.
pub(super) fn resize_float_runtime(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let Some(float) = app.visible_float_for_active_workspace() else {
        return;
    };
    let Some(inner) = float_overlay_inner_rect(terminal_area) else {
        return;
    };
    if app.direct_attach_resize_locks.contains(&float.terminal_id) {
        return;
    }
    if let Some(rt) = terminal_runtimes.get(&float.terminal_id) {
        rt.resize(
            inner.height,
            inner.width,
            cell_size.width_px,
            cell_size.height_px,
        );
    }
}

/// Paint the active workspace's visible float above the pane field.
pub(super) fn render_float_overlay(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    terminal_area: Rect,
) {
    let Some(float) = app.visible_float_for_active_workspace() else {
        return;
    };
    let Some(rt) = terminal_runtimes.get(&float.terminal_id) else {
        return;
    };
    let Some(area) = float_overlay_rect(terminal_area) else {
        return;
    };

    // Themes with panel_bg = Reset keep transparent chrome elsewhere, but an
    // overlay must occlude the panes underneath — fall back to surface_dim.
    let bg = match app.palette.panel_bg {
        Color::Reset => app.palette.surface_dim,
        color => color,
    };
    let border_style = Style::default().fg(app.palette.accent);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .style(Style::default().bg(bg))
        .title(Line::from(Span::styled(
            float_title(app, area.width),
            border_style,
        )));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let show_cursor = app.mode == Mode::Terminal && !pane_is_scrolled_back(rt);
    rt.render(frame, inner, show_cursor);
}

/// Border title: the float's cwd when known, truncated to the overlay width.
fn float_title(app: &AppState, overlay_width: u16) -> String {
    let cwd = app
        .visible_float_for_active_workspace()
        .and_then(|float| app.terminals.get(&float.terminal_id))
        .map(|terminal| terminal.cwd.display().to_string())
        .filter(|cwd| !cwd.is_empty());
    let label = cwd.unwrap_or_else(|| "float".to_string());
    let max = usize::from(overlay_width.saturating_sub(4));
    if max == 0 {
        return String::new();
    }
    let truncated: String = if label.chars().count() > max {
        let tail: Vec<char> = label.chars().collect();
        let start = tail.len() - max.saturating_sub(1);
        std::iter::once('…')
            .chain(tail[start..].iter().copied())
            .collect()
    } else {
        label
    };
    format!(" {truncated} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_rect_is_centered_and_proportional() {
        let area = Rect::new(20, 5, 100, 40);
        let rect = float_overlay_rect(area).expect("rect for a roomy area");
        assert_eq!(rect.width, 80);
        assert_eq!(rect.height, 28);
        // Centered: equal margins on each side (within rounding).
        assert_eq!(rect.x - area.x, area.x + area.width - (rect.x + rect.width));
        assert_eq!(
            rect.y - area.y,
            area.y + area.height - (rect.y + rect.height)
        );
    }

    #[test]
    fn overlay_rect_degrades_to_none_for_tiny_areas() {
        assert!(float_overlay_rect(Rect::new(0, 0, 5, 4)).is_none());
        assert!(float_overlay_rect(Rect::default()).is_none());
        assert!(float_overlay_inner_rect(Rect::new(0, 0, 6, 5)).is_none());
    }

    #[test]
    fn inner_rect_subtracts_the_border() {
        let area = Rect::new(0, 0, 100, 40);
        let outer = float_overlay_rect(area).unwrap();
        let inner = float_overlay_inner_rect(area).unwrap();
        assert_eq!(inner.width, outer.width - 2);
        assert_eq!(inner.height, outer.height - 2);
    }
}
