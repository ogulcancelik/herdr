use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use std::collections::{HashMap, HashSet};

use super::scrollbar::{render_pane_scrollbar, should_show_scrollbar};
use super::widgets::panel_contrast_fg;
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::config::PaneBordersConfig;
use crate::layout::{PaneInfo, SplitBorder};
use crate::terminal::{TerminalRuntime, TerminalRuntimeRegistry};

pub(crate) fn pane_is_scrolled_back(rt: &TerminalRuntime) -> bool {
    rt.scroll_metrics()
        .is_some_and(|metrics| metrics.offset_from_bottom > 0)
}

fn truncate_label(text: &str, max_width: usize) -> String {
    let len = text.chars().count();
    if len <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{prefix}…")
}

fn pane_border_title(label: &str, pane_width: u16) -> Option<String> {
    let label = label.trim();
    if label.is_empty() || pane_width <= 4 {
        return None;
    }
    let max_label_width = pane_width.saturating_sub(4) as usize;
    Some(format!(" {} ", truncate_label(label, max_label_width)))
}

fn stable_terminal_inner_rect(pane_inner: Rect) -> Rect {
    if pane_inner.width <= 4 {
        return pane_inner;
    }

    Rect::new(
        pane_inner.x,
        pane_inner.y,
        pane_inner.width.saturating_sub(1),
        pane_inner.height,
    )
}

fn pane_inner_rect(
    pane_rect: Rect,
    area: Rect,
    multi_pane: bool,
    style: PaneBordersConfig,
) -> Rect {
    if !multi_pane {
        return area;
    }
    match style {
        PaneBordersConfig::Full => Block::default().borders(Borders::ALL).inner(pane_rect),
        PaneBordersConfig::Minimal => minimal_pane_content_rect(pane_rect, area),
    }
}

/// Content rect for a pane in minimal-border mode: trim a single cell off the
/// left/top edges that sit against an internal separator and leave the outer
/// edges flush. The trimmed cell is where the shared separator line is drawn,
/// so each seam costs one cell instead of a per-pane box.
fn minimal_pane_content_rect(pane_rect: Rect, area: Rect) -> Rect {
    let mut content = pane_rect;
    if content.x > area.x {
        content.x = content.x.saturating_add(1);
        content.width = content.width.saturating_sub(1);
    }
    if content.y > area.y {
        content.y = content.y.saturating_add(1);
        content.height = content.height.saturating_sub(1);
    }
    content
}

fn runtime_for_tab_pane<'a>(
    terminal_runtimes: &'a TerminalRuntimeRegistry,
    tab: &'a crate::workspace::Tab,
    pane_id: crate::layout::PaneId,
) -> Option<(&'a crate::terminal::TerminalId, &'a TerminalRuntime)> {
    let terminal_id = tab.terminal_id(pane_id)?;
    #[cfg(test)]
    if let Some(runtime) = tab.runtimes.get(&pane_id) {
        return Some((terminal_id, runtime));
    }
    terminal_runtimes
        .get(terminal_id)
        .map(|runtime| (terminal_id, runtime))
}

fn stable_scrollbar_gutter(rt: &TerminalRuntime, pane_inner: Rect) -> (Rect, Option<Rect>) {
    let inner_rect = stable_terminal_inner_rect(pane_inner);
    if inner_rect == pane_inner {
        return (inner_rect, None);
    }
    let gutter = Rect::new(
        pane_inner.x + pane_inner.width.saturating_sub(1),
        pane_inner.y,
        1,
        pane_inner.height,
    );
    let scrollbar_rect = rt
        .scroll_metrics()
        .filter(|metrics| should_show_scrollbar(*metrics))
        .map(|_| gutter);

    (inner_rect, scrollbar_rect)
}

/// Resize every visible runtime in a tab to the geometry it would receive if the tab were selected.
pub(super) fn resize_tab_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    tab: &crate::workspace::Tab,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let multi_pane = tab.layout.pane_count() > 1;
    let style = app.pane_borders;

    if tab.zoomed {
        let focused_id = tab.layout.focused();
        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, focused_id) {
            let pane_inner = pane_inner_rect(area, area, multi_pane, style);
            let inner_rect = stable_terminal_inner_rect(pane_inner);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
        return;
    }

    for info in tab.layout.panes(area) {
        let pane_inner = pane_inner_rect(info.rect, area, multi_pane, style);

        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, info.id) {
            let inner_rect = stable_terminal_inner_rect(pane_inner);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
    }
}

/// Compute pane layout info and optionally resize pane runtimes to match.
pub(super) fn compute_pane_infos(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Vec<PaneInfo> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return Vec::new();
    };

    let multi_pane = ws.layout.pane_count() > 1;
    let style = app.pane_borders;

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        let pane_inner = pane_inner_rect(area, area, multi_pane, style);
        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, focused_id) {
            (inner_rect, scrollbar_rect) = stable_scrollbar_gutter(rt, pane_inner);
            if resize_panes
                && ws.terminal_id(focused_id).is_some_and(|terminal_id| {
                    !app.direct_attach_resize_locks.contains(terminal_id)
                })
            {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
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
        let pane_inner = pane_inner_rect(info.rect, area, multi_pane, style);

        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            (inner_rect, scrollbar_rect) = stable_scrollbar_gutter(rt, pane_inner);
            if resize_panes
                && ws.terminal_id(info.id).is_some_and(|terminal_id| {
                    !app.direct_attach_resize_locks.contains(terminal_id)
                })
            {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }

        info.inner_rect = inner_rect;
        info.scrollbar_rect = scrollbar_rect;
    }

    pane_infos
}

pub(super) fn render_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
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
    let borders = app.pane_borders;

    // Precompute per-frame fallback colors once so dimming multiple inactive
    // panes doesn't repeat the same derivation.
    let dim_fg_fallback = app
        .host_terminal_theme
        .foreground
        .map(terminal_theme_to_rgb)
        .or_else(|| color_to_rgb(app.palette.text))
        .unwrap_or((205, 214, 244));
    let dim_bg_fallback = app
        .host_terminal_theme
        .background
        .map(terminal_theme_to_rgb)
        .or_else(|| color_to_rgb(selection_palette_background(&app.palette)));

    for info in &app.view.pane_infos {
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            if multi_pane && !borders.draws_separators() {
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

                let mut block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .border_set(border_set);
                if let Some(title) = ws
                    .pane_state(info.id)
                    .and_then(|pane| app.terminals.get(&pane.attached_terminal_id))
                    .and_then(|terminal| {
                        terminal.border_label(app.show_agent_labels_on_pane_borders)
                    })
                    .and_then(|label| pane_border_title(&label, info.rect.width))
                {
                    block = block.title(Line::from(Span::styled(title, border_style)));
                }
                frame.render_widget(block, info.rect);
            }

            let show_cursor = info.is_focused
                && terminal_active
                && !pane_is_scrolled_back(rt)
                && app.pane_exposes_host_cursor(ws_idx, info.id);
            rt.render(frame, info.inner_rect, show_cursor);
            render_pane_scrollbar(app, frame, info, rt);

            // Minimal mode has no border to show focus, so it always dims the
            // inactive panes (including terminal mode) with a wezterm-style HSV
            // transform. Full mode keeps its border as the focus cue and only
            // dims in non-terminal modes, using the terminal's native DIM
            // attribute as before.
            if !info.is_focused && multi_pane {
                if borders.dims_inactive_panes() {
                    dim_pane_content(
                        frame,
                        info.inner_rect,
                        dim_fg_fallback,
                        dim_bg_fallback,
                        &app.host_ansi_palette,
                    );
                } else if !terminal_active {
                    dim_pane_with_modifier(frame, info.inner_rect);
                }
            }

            render_selection_highlight(
                &app.selection,
                frame,
                info.id,
                info.inner_rect,
                rt.scroll_metrics(),
                &app.palette,
                app.host_terminal_theme,
            );
            render_copy_mode_cursor(app, frame, info);
        }
    }

    if borders.draws_separators() && multi_pane && !ws.zoomed {
        // surface1 matches wezterm's subtle split-line color (~#444444).
        render_pane_separators(frame, area, &app.view.split_borders, app.palette.surface1);
    }
}

const SEP_UP: u8 = 1;
const SEP_DOWN: u8 = 1 << 1;
const SEP_LEFT: u8 = 1 << 2;
const SEP_RIGHT: u8 = 1 << 3;

/// Draw single-cell separators between adjacent panes, connecting them with the
/// correct box-drawing junction glyph wherever lines meet.
fn render_pane_separators(frame: &mut Frame, area: Rect, splits: &[SplitBorder], color: Color) {
    let cells = collect_separator_cells(area, splits);
    if cells.is_empty() {
        return;
    }
    let style = Style::default().fg(color);
    let buf = frame.buffer_mut();
    for &(x, y) in &cells {
        let glyph = separator_glyph(separator_bits(x, y, &cells));
        let mut tmp = [0u8; 4];
        buf[(x, y)]
            .set_symbol(glyph.encode_utf8(&mut tmp))
            .set_style(style);
    }
}

fn collect_separator_cells(area: Rect, splits: &[SplitBorder]) -> HashSet<(u16, u16)> {
    let x_end = area.x.saturating_add(area.width);
    let y_end = area.y.saturating_add(area.height);
    let mut cells = HashSet::new();
    for split in splits {
        match split.direction {
            ratatui::layout::Direction::Horizontal => {
                let x = split.pos;
                if x < area.x || x >= x_end {
                    continue;
                }
                let y0 = split.area.y.max(area.y);
                let y1 = split.area.y.saturating_add(split.area.height).min(y_end);
                for y in y0..y1 {
                    cells.insert((x, y));
                }
            }
            ratatui::layout::Direction::Vertical => {
                let y = split.pos;
                if y < area.y || y >= y_end {
                    continue;
                }
                let x0 = split.area.x.max(area.x);
                let x1 = split.area.x.saturating_add(split.area.width).min(x_end);
                for x in x0..x1 {
                    cells.insert((x, y));
                }
            }
        }
    }
    cells
}

fn separator_bits(x: u16, y: u16, cells: &HashSet<(u16, u16)>) -> u8 {
    let mut bits = 0;
    if y > 0 && cells.contains(&(x, y - 1)) {
        bits |= SEP_UP;
    }
    if cells.contains(&(x, y + 1)) {
        bits |= SEP_DOWN;
    }
    if x > 0 && cells.contains(&(x - 1, y)) {
        bits |= SEP_LEFT;
    }
    if cells.contains(&(x + 1, y)) {
        bits |= SEP_RIGHT;
    }
    bits
}

fn separator_glyph(bits: u8) -> char {
    match (
        bits & SEP_UP != 0,
        bits & SEP_DOWN != 0,
        bits & SEP_LEFT != 0,
        bits & SEP_RIGHT != 0,
    ) {
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (false, false, true, true) | (false, false, true, false) | (false, false, false, true) => {
            '─'
        }
        _ => '│',
    }
}

fn render_copy_mode_cursor(app: &AppState, frame: &mut Frame, info: &PaneInfo) {
    if app.mode != Mode::Copy {
        return;
    }
    let Some(copy_mode) = app.copy_mode else {
        return;
    };
    if copy_mode.pane_id != info.id
        || copy_mode.cursor_row >= info.inner_rect.height
        || copy_mode.cursor_col >= info.inner_rect.width
    {
        return;
    }

    let x = info.inner_rect.x + copy_mode.cursor_col;
    let y = info.inner_rect.y + copy_mode.cursor_row;
    let cell = &mut frame.buffer_mut()[(x, y)];
    cell.set_style(
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_selection_highlight(
    selection: &Option<crate::selection::Selection>,
    frame: &mut Frame,
    pane_id: crate::layout::PaneId,
    inner: Rect,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    p: &Palette,
    host_theme: crate::terminal_theme::TerminalTheme,
) {
    if let Some(sel) = selection {
        if sel.is_visible() && sel.pane_id == pane_id {
            let buf = frame.buffer_mut();
            let style = automatic_selection_style(p, host_theme);
            for y in 0..inner.height {
                for x in 0..inner.width {
                    if sel.contains(y, x, scroll_metrics) {
                        let cell = &mut buf[(inner.x + x, inner.y + y)];
                        cell.set_style(style);
                    }
                }
            }
        }
    }
}

type Rgb = (u8, u8, u8);

/// HSV multipliers applied to an inactive pane's cells, mirroring wezterm's
/// `inactive_pane_hsb`: scale saturation and brightness down a little so the
/// focused pane stands out while the dimmed pane keeps its colors (no border).
const INACTIVE_PANE_SATURATION: f32 = 0.9;
const INACTIVE_PANE_BRIGHTNESS: f32 = 0.8;

/// Dim an inactive full-border pane using the terminal's native DIM attribute,
/// matching the pre-minimal-borders behavior for that mode.
fn dim_pane_with_modifier(frame: &mut Frame, inner: Rect) {
    let buf = frame.buffer_mut();
    for y in inner.y..inner.bottom() {
        for x in inner.x..inner.right() {
            let cell = &mut buf[(x, y)];
            cell.set_style(cell.style().add_modifier(Modifier::DIM));
        }
    }
}

fn dim_pane_content(
    frame: &mut Frame,
    inner: Rect,
    fg_fallback: Rgb,
    bg_fallback: Option<Rgb>,
    ansi_palette: &crate::terminal_theme::AnsiPalette,
) {
    // Cache the HSV transform per resolved color: terminal content reuses a
    // small set of colors, so this avoids re-running the sRGB/HSV round-trip
    // (several powf calls) for every cell.
    let mut scaled = HashMap::new();
    let buf = frame.buffer_mut();
    for y in inner.y..inner.bottom() {
        for x in inner.x..inner.right() {
            let cell = &mut buf[(x, y)];
            let style = cell.style();
            let fg = dim_color(
                style.fg.unwrap_or(Color::Reset),
                Some(fg_fallback),
                ansi_palette,
                &mut scaled,
            );
            let bg = dim_color(
                style.bg.unwrap_or(Color::Reset),
                bg_fallback,
                ansi_palette,
                &mut scaled,
            );
            let mut dimmed = style;
            if let Some(fg) = fg {
                dimmed = dimmed.fg(fg);
            }
            if let Some(bg) = bg {
                dimmed = dimmed.bg(bg);
            }
            cell.set_style(dimmed);
        }
    }
}

fn dim_color(
    color: Color,
    fallback: Option<Rgb>,
    ansi_palette: &crate::terminal_theme::AnsiPalette,
    scaled: &mut HashMap<Rgb, Rgb>,
) -> Option<Color> {
    let rgb = resolve_rgb(color, ansi_palette).or(fallback)?;
    let (r, g, b) = *scaled
        .entry(rgb)
        .or_insert_with(|| scale_hsv(rgb, INACTIVE_PANE_SATURATION, INACTIVE_PANE_BRIGHTNESS));
    Some(Color::Rgb(r, g, b))
}

/// Resolve a cell color to concrete RGB so it can be dimmed. Unlike
/// `color_to_rgb`, this expands `Color::Indexed`: the 16 ANSI colors use the
/// host terminal's queried palette when known (so dimmed content matches what
/// the host actually shows), falling back to the standard xterm table; the
/// 256-color cube and grayscale ramp are universal. `Reset` stays unresolved
/// (the caller's host-theme fallback handles it).
fn resolve_rgb(color: Color, ansi_palette: &crate::terminal_theme::AnsiPalette) -> Option<Rgb> {
    match color {
        Color::Indexed(i) => Some(
            ansi_palette
                .get(usize::from(i))
                .copied()
                .flatten()
                .map(|c| (c.r, c.g, c.b))
                .unwrap_or_else(|| xterm_256_to_rgb(i)),
        ),
        other => color_to_rgb(other),
    }
}

/// Standard xterm palette mapping, used only as a fallback when the host's real
/// ANSI palette (OSC 4) is not yet known. The 0..16 entries are generic xterm
/// defaults and may differ from the user's theme; the 16..256 cube and grayscale
/// ramp are universal.
fn xterm_256_to_rgb(index: u8) -> Rgb {
    const BASE16: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    match index {
        0..=15 => color_to_rgb(BASE16[index as usize]).unwrap_or((0, 0, 0)),
        16..=231 => {
            let i = index - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            (gray, gray, gray)
        }
    }
}

/// Scale a color's saturation and value in HSV space, leaving hue intact.
/// Mirrors wezterm's `apply_hsv` (hsv *= transform), including operating in
/// linear-light RGB so the brightness reduction reads the same as wezterm's
/// rather than darkening too aggressively in sRGB space.
fn scale_hsv(rgb: Rgb, saturation: f32, value: f32) -> Rgb {
    let linear = (
        srgb_to_linear(rgb.0),
        srgb_to_linear(rgb.1),
        srgb_to_linear(rgb.2),
    );
    let (h, s, v) = rgb_to_hsv(linear);
    let scaled = hsv_to_rgb(
        h,
        (s * saturation).clamp(0.0, 1.0),
        (v * value).clamp(0.0, 1.0),
    );
    (
        linear_to_srgb(scaled.0),
        linear_to_srgb(scaled.1),
        linear_to_srgb(scaled.2),
    )
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let srgb = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

fn rgb_to_hsv((r, g, b): (f32, f32, f32)) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let saturation = if max == 0.0 { 0.0 } else { delta / max };
    (hue, saturation, max)
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> (f32, f32, f32) {
    let c = value * saturation;
    let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = value - c;
    let (r, g, b) = match hue {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

fn automatic_selection_style(
    p: &Palette,
    host_theme: crate::terminal_theme::TerminalTheme,
) -> Style {
    let bg = automatic_selection_bg(p, host_theme);
    Style::reset().fg(selection_fg_for_bg(bg, p)).bg(bg)
}

fn automatic_selection_bg(p: &Palette, host_theme: crate::terminal_theme::TerminalTheme) -> Color {
    let Some(background) = host_theme.background.map(terminal_theme_to_rgb) else {
        return selection_palette_background(p);
    };

    let target = if relative_luminance(background) < 0.5 {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    let selected = mix_rgb(background, target, 0.28);
    Color::Rgb(selected.0, selected.1, selected.2)
}

fn selection_palette_background(p: &Palette) -> Color {
    if p.panel_bg == Color::Reset {
        p.surface_dim
    } else {
        p.panel_bg
    }
}

fn terminal_theme_to_rgb(color: crate::terminal_theme::RgbColor) -> Rgb {
    (color.r, color.g, color.b)
}

fn selection_fg_for_bg(bg: Color, p: &Palette) -> Color {
    color_to_rgb(bg)
        .map(|bg| {
            if relative_luminance(bg) < 0.5 {
                Color::White
            } else {
                Color::Black
            }
        })
        .unwrap_or_else(|| panel_contrast_fg(p))
}

fn mix_rgb(base: Rgb, target: Rgb, amount: f32) -> Rgb {
    fn channel(base: u8, target: u8, amount: f32) -> u8 {
        (f32::from(base) + (f32::from(target) - f32::from(base)) * amount).round() as u8
    }
    (
        channel(base.0, target.0, amount),
        channel(base.1, target.1, amount),
        channel(base.2, target.2, amount),
    )
}

fn relative_luminance(color: Rgb) -> f32 {
    fn channel(value: u8) -> f32 {
        let value = f32::from(value) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.0) + 0.7152 * channel(color.1) + 0.0722 * channel(color.2)
}

fn color_to_rgb(color: Color) -> Option<Rgb> {
    match color {
        Color::Reset => None,
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((128, 0, 0)),
        Color::Green => Some((0, 128, 0)),
        Color::Yellow => Some((128, 128, 0)),
        Color::Blue => Some((0, 0, 128)),
        Color::Magenta => Some((128, 0, 128)),
        Color::Cyan => Some((0, 128, 128)),
        Color::Gray => Some((192, 192, 192)),
        Color::DarkGray => Some((128, 128, 128)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((0, 0, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(_) => None,
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
                app.keybinds
                    .new_workspace
                    .label()
                    .unwrap_or_else(|| "unset".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::selection::Selection;
    use crate::terminal::TerminalRuntime;
    use crate::workspace::Workspace;

    #[test]
    fn pane_border_title_trims_and_truncates() {
        assert_eq!(
            pane_border_title(" claude ", 20).as_deref(),
            Some(" claude ")
        );
        assert_eq!(pane_border_title("", 20), None);
        assert_eq!(pane_border_title("abcdef", 8).as_deref(), Some(" abc… "));
        assert_eq!(pane_border_title("abcdef", 4), None);
    }

    #[tokio::test]
    async fn pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));
    }

    #[tokio::test]
    async fn zoomed_pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        workspace.zoomed = true;
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));
    }

    #[tokio::test]
    async fn zoomed_multi_pane_keeps_border_space() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let focused_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.zoomed = true;
        workspace.tabs[0].runtimes.insert(
            focused_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.id, focused_pane);
        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn tiny_pane_does_not_reserve_scrollbar_gutter() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(4, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 4, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, area);
    }

    #[tokio::test]
    async fn pane_scrollbar_reserves_last_column_from_terminal_area() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(
                40,
                8,
                1024,
                b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
            ),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, Some(Rect::new(49, 3, 1, 8)));
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));
    }

    #[test]
    fn selection_highlight_uses_one_uniform_style() {
        let palette = Palette::catppuccin();
        let host_theme = crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 12,
                g: 14,
                b: 16,
            }),
        };
        let expected_style = automatic_selection_style(&palette, host_theme);
        let selection = Some(Selection::range(PaneId::from_raw(1), 0, 0, 2, None));
        let backend = ratatui::backend::TestBackend::new(4, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                let buf = frame.buffer_mut();
                buf[(0, 0)].set_style(
                    Style::default()
                        .fg(Color::Rgb(10, 220, 120))
                        .bg(Color::Black),
                );
                buf[(1, 0)].set_style(
                    Style::default()
                        .fg(Color::Rgb(220, 180, 40))
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                );
                buf[(2, 0)].set_style(Style::default().fg(Color::Blue).bg(Color::Reset));
                render_selection_highlight(
                    &selection,
                    frame,
                    PaneId::from_raw(1),
                    Rect::new(0, 0, 4, 1),
                    None,
                    &palette,
                    host_theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first = buffer[(0, 0)].style();
        let second = buffer[(1, 0)].style();
        let third = buffer[(2, 0)].style();

        assert_eq!(first.fg, expected_style.fg);
        assert_eq!(second.fg, expected_style.fg);
        assert_eq!(third.fg, expected_style.fg);
        assert_eq!(first.bg, expected_style.bg);
        assert_eq!(second.bg, expected_style.bg);
        assert_eq!(third.bg, expected_style.bg);
        assert_eq!(first.add_modifier, expected_style.add_modifier);
        assert_eq!(second.add_modifier, expected_style.add_modifier);
        assert_eq!(third.add_modifier, expected_style.add_modifier);
        assert!(!second.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn automatic_selection_background_uses_host_background() {
        let bg = automatic_selection_bg(
            &Palette::terminal(),
            crate::terminal_theme::TerminalTheme {
                foreground: Some(crate::terminal_theme::RgbColor {
                    r: 230,
                    g: 230,
                    b: 230,
                }),
                background: Some(crate::terminal_theme::RgbColor {
                    r: 12,
                    g: 14,
                    b: 16,
                }),
            },
        );

        let Color::Rgb(r, g, b) = bg else {
            panic!("selection background should resolve to rgb");
        };
        assert!(relative_luminance((r, g, b)) > relative_luminance((12, 14, 16)));
    }

    #[test]
    fn dim_color_dims_without_killing_hue_and_resolves_default_via_fallback() {
        let no_palette: crate::terminal_theme::AnsiPalette = [None; 16];
        let mut cache = HashMap::new();
        // A saturated green stays clearly green (hue preserved) but darker.
        let Some(Color::Rgb(r, g, b)) =
            dim_color(Color::Rgb(40, 200, 80), None, &no_palette, &mut cache)
        else {
            panic!("rgb input should dim");
        };
        assert!(g > r && g > b, "green should remain the dominant channel");
        assert!(g < 200, "value should be reduced");

        // A default (Reset) color falls back to the supplied host color.
        assert!(dim_color(Color::Reset, None, &no_palette, &mut cache).is_none());
        assert!(dim_color(Color::Reset, Some((180, 180, 180)), &no_palette, &mut cache).is_some());

        // Indexed (256-color) content keeps its hue instead of graying out.
        let Some(Color::Rgb(r, g, b)) = dim_color(Color::Indexed(2), None, &no_palette, &mut cache)
        else {
            panic!("indexed green should resolve and dim");
        };
        assert!(g > r && g > b, "indexed green stays green");
    }

    #[test]
    fn resolve_rgb_prefers_host_ansi_palette_for_indexed_colors() {
        let mut palette: crate::terminal_theme::AnsiPalette = [None; 16];
        // Host's ANSI blue (index 4) is catppuccin blue, not standard (0,0,128).
        palette[4] = Some(crate::terminal_theme::RgbColor {
            r: 137,
            g: 180,
            b: 250,
        });
        assert_eq!(
            resolve_rgb(Color::Indexed(4), &palette),
            Some((137, 180, 250))
        );
        // Without a host entry, falls back to the standard xterm table.
        let empty: crate::terminal_theme::AnsiPalette = [None; 16];
        assert_eq!(resolve_rgb(Color::Indexed(4), &empty), Some((0, 0, 128)));
        // The 256-color cube is never overridden by the 16-entry ANSI palette.
        assert_eq!(
            resolve_rgb(Color::Indexed(196), &palette),
            Some((255, 0, 0))
        );
    }

    #[test]
    fn xterm_256_palette_resolves_cube_and_grayscale() {
        assert_eq!(xterm_256_to_rgb(0), (0, 0, 0));
        assert_eq!(xterm_256_to_rgb(15), (255, 255, 255));
        // First color-cube entry (16) is black; 196 is pure red.
        assert_eq!(xterm_256_to_rgb(16), (0, 0, 0));
        assert_eq!(xterm_256_to_rgb(196), (255, 0, 0));
        // Grayscale ramp.
        assert_eq!(xterm_256_to_rgb(232), (8, 8, 8));
        assert_eq!(xterm_256_to_rgb(255), (238, 238, 238));
    }

    #[test]
    fn scale_hsv_preserves_hue_order_and_dims_value() {
        // Pure red scaled down keeps red dominant.
        let (r, g, b) = scale_hsv((255, 0, 0), 0.9, 0.8);
        assert!(r > g && r > b);
        assert!(r < 255);
        // A gray (zero saturation) just dims toward darker gray, staying neutral.
        let (r, g, b) = scale_hsv((100, 100, 100), 0.9, 0.8);
        assert_eq!(r, g);
        assert_eq!(g, b);
        assert!(r < 100, "value should be reduced");
    }

    #[test]
    fn minimal_content_rect_trims_only_internal_edges() {
        let area = Rect::new(0, 0, 40, 10);
        assert_eq!(
            minimal_pane_content_rect(Rect::new(0, 0, 20, 10), area),
            Rect::new(0, 0, 20, 10)
        );
        assert_eq!(
            minimal_pane_content_rect(Rect::new(20, 0, 20, 10), area),
            Rect::new(21, 0, 19, 10)
        );
        assert_eq!(
            minimal_pane_content_rect(Rect::new(0, 5, 40, 5), area),
            Rect::new(0, 6, 40, 4)
        );
    }

    #[tokio::test]
    async fn minimal_borders_reclaim_separator_space_for_split_panes() {
        let mut app = AppState::test_new();
        app.pane_borders = crate::config::PaneBordersConfig::Minimal;
        let mut workspace = Workspace::test_new("test");
        workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(0, 0, 40, 10);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );

        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].rect, Rect::new(0, 0, 20, 10));
        assert_eq!(infos[0].inner_rect, Rect::new(0, 0, 20, 10));
        assert_eq!(infos[1].rect, Rect::new(20, 0, 20, 10));
        assert_eq!(infos[1].inner_rect, Rect::new(21, 0, 19, 10));
    }

    #[test]
    fn separator_cells_cover_the_divider_column_only() {
        let area = Rect::new(0, 0, 40, 10);
        let splits = vec![SplitBorder {
            pos: 20,
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            area,
            path: vec![],
        }];
        let cells = collect_separator_cells(area, &splits);
        assert_eq!(cells.len(), 10);
        assert!(cells.contains(&(20, 0)));
        assert!(cells.contains(&(20, 9)));
        assert!(!cells.contains(&(19, 0)));
        assert!(!cells.contains(&(21, 0)));
    }

    #[test]
    fn separator_glyph_maps_each_junction() {
        assert_eq!(separator_glyph(SEP_UP | SEP_DOWN), '│');
        assert_eq!(separator_glyph(SEP_LEFT | SEP_RIGHT), '─');
        assert_eq!(
            separator_glyph(SEP_UP | SEP_DOWN | SEP_LEFT | SEP_RIGHT),
            '┼'
        );
        assert_eq!(separator_glyph(SEP_UP | SEP_DOWN | SEP_RIGHT), '├');
        assert_eq!(separator_glyph(SEP_UP | SEP_DOWN | SEP_LEFT), '┤');
        assert_eq!(separator_glyph(SEP_DOWN | SEP_LEFT | SEP_RIGHT), '┬');
        assert_eq!(separator_glyph(SEP_UP | SEP_LEFT | SEP_RIGHT), '┴');
    }

    #[test]
    fn separator_bits_connect_where_a_horizontal_line_meets_a_vertical_one() {
        let mut cells = HashSet::new();
        for y in 0..4 {
            cells.insert((5u16, y));
        }
        for x in 5..10 {
            cells.insert((x, 2u16));
        }
        assert_eq!(separator_glyph(separator_bits(5, 2, &cells)), '├');
        assert_eq!(separator_glyph(separator_bits(5, 0, &cells)), '│');
        assert_eq!(separator_glyph(separator_bits(7, 2, &cells)), '─');
    }
}
