use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::scrollbar::{render_pane_scrollbar, should_show_scrollbar};
use super::widgets::panel_contrast_fg;
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::layout::PaneInfo;
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

fn pane_inner_rect(area: Rect, framed: bool) -> Rect {
    if framed {
        Block::default().borders(Borders::ALL).inner(area)
    } else {
        area
    }
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
/// Rows reserved for the pane header (context line + prompt section) of this
/// terminal, or 0 when the header is disabled / the pane never hosted an
/// agent / the pane is too small to give up rows.
fn pane_header_rows(
    app: &AppState,
    terminal_id: Option<&crate::terminal::TerminalId>,
    pane_inner: Rect,
) -> u16 {
    if !app.pane_header {
        return 0;
    }
    let Some(terminal) = terminal_id.and_then(|id| app.terminals.get(id)) else {
        return 0;
    };
    if !terminal.header_reserved {
        return 0;
    }
    // context + prompt rows + a hairline divider separating header from content
    let rows = 2 + app.prompt_float_lines;
    // Keep a usable PTY: the header never claims more than it leaves behind.
    if pane_inner.height < rows.saturating_mul(2).saturating_add(4) {
        return 0;
    }
    rows
}

/// Split `pane_inner` into (header strip, remaining content area).
fn carve_pane_header(
    app: &AppState,
    terminal_id: Option<&crate::terminal::TerminalId>,
    pane_inner: Rect,
) -> (Option<Rect>, Rect) {
    let rows = pane_header_rows(app, terminal_id, pane_inner);
    if rows == 0 {
        return (None, pane_inner);
    }
    let header = Rect::new(pane_inner.x, pane_inner.y, pane_inner.width, rows);
    let content = Rect::new(
        pane_inner.x,
        pane_inner.y + rows,
        pane_inner.width,
        pane_inner.height - rows,
    );
    (Some(header), content)
}

pub(super) fn resize_tab_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    tab: &crate::workspace::Tab,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    if tab.zoomed {
        let focused_id = tab.layout.focused();
        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, focused_id) {
            // Always framed — matches the render geometry (single pane
            // keeps the focus outline).
            let pane_inner = pane_inner_rect(area, true);
            let (_, pane_inner) = carve_pane_header(app, Some(terminal_id), pane_inner);
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
        let pane_inner = Block::default().borders(Borders::ALL).inner(info.rect);

        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, info.id) {
            let (_, pane_inner) = carve_pane_header(app, Some(terminal_id), pane_inner);
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

    let terminal_active = app.mode == Mode::Terminal;

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        // Always framed: a lone (or zoomed) pane keeps the focus outline —
        // the same border language as splits (#43 SSoT).
        let pane_inner = pane_inner_rect(area, true);
        let (header_rect, pane_inner) =
            carve_pane_header(app, ws.terminal_id(focused_id), pane_inner);
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
            header_rect,
            is_focused: true,
        }];
    }

    let mut pane_infos = ws.layout.panes(area);

    for info in &mut pane_infos {
        let pane_inner = {
            let border_set = if info.is_focused && terminal_active {
                ratatui::symbols::border::THICK
            } else {
                ratatui::symbols::border::PLAIN
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(border_set);
            block.inner(info.rect)
        };

        let (header_rect, pane_inner) = carve_pane_header(app, ws.terminal_id(info.id), pane_inner);
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
        info.header_rect = header_rect;
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

    for info in &app.view.pane_infos {
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            {
                let border_style =
                    Style::default().fg(pane_focus_color(info.is_focused, &app.palette));
                let border_set = if info.is_focused && terminal_active {
                    ratatui::symbols::border::THICK
                } else {
                    ratatui::symbols::border::PLAIN
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

            let show_cursor = info.is_focused && terminal_active && !pane_is_scrolled_back(rt);
            rt.render(frame, info.inner_rect, show_cursor);
            render_pane_scrollbar(app, frame, info, rt);

            let should_dim = !info.is_focused && multi_pane && !terminal_active;
            if should_dim {
                let inner = info.inner_rect;
                let buf = frame.buffer_mut();
                for y in inner.y..inner.y + inner.height {
                    for x in inner.x..inner.x + inner.width {
                        let cell = &mut buf[(x, y)];
                        cell.set_style(cell.style().add_modifier(Modifier::DIM));
                    }
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
            render_pane_header(app, ws, frame, info);
        }
    }
}

/// Render the reserved pane header: a context line (project · worktree ·
/// branch) and the last submitted prompt, middle-collapsed into the
/// remaining header rows.
/// SSoT for "does this pane have focus" coloring: the pane borders and the
/// header hairline divider speak the same language — accent for the focused
/// pane (a single pane is focused by construction, so it always reads
/// active), the muted overlay for the rest.
fn pane_focus_color(is_focused: bool, p: &crate::app::state::Palette) -> ratatui::style::Color {
    if is_focused {
        p.accent
    } else {
        p.overlay0
    }
}

fn render_pane_header(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    frame: &mut Frame,
    info: &PaneInfo,
) {
    let Some(header) = info.header_rect else {
        return;
    };
    let terminal = ws
        .pane_state(info.id)
        .and_then(|pane| app.terminals.get(&pane.attached_terminal_id));

    let p = &app.palette;
    let bar_bg = Style::default();
    let buf = frame.buffer_mut();
    for y in header.y..header.y + header.height {
        for x in header.x..header.x + header.width {
            buf[(x, y)].set_symbol(" ");
            buf[(x, y)].set_style(bar_bg);
        }
    }
    // Hairline divider — same visual language as the sidebar separators.
    let divider_y = header.y + header.height - 1;
    // Same focus language as the pane borders (accent = focused; a single
    // pane is always focused, so its header always reads active).
    let divider_style = Style::default().fg(pane_focus_color(info.is_focused, p));
    for x in header.x..header.x + header.width {
        buf[(x, divider_y)].set_symbol("\u{2500}");
        buf[(x, divider_y)].set_style(divider_style);
    }

    // Context line: owner/project · worktree · branch.
    let mut spans: Vec<Span> = vec![Span::styled(" ", bar_bg)];
    let project_label = ws
        .worktree_space()
        .map(|space| space.label.clone())
        .unwrap_or_else(|| ws.display_name());
    let project = owner_qualified_project(ws.repo_group_key(), &project_label);
    spans.push(Span::styled(
        project,
        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
    ));
    if let Some(worktree_dir) = ws
        .worktree_space()
        .filter(|space| space.is_linked_worktree)
        .and_then(|space| space.checkout_path.file_name())
        .map(|name| name.to_string_lossy().to_string())
    {
        spans.push(Span::styled(" \u{b7} ", Style::default().fg(p.overlay0)));
        spans.push(Span::styled(worktree_dir, Style::default().fg(p.mauve)));
    }
    if let Some(branch) = ws.branch() {
        spans.push(Span::styled(" \u{b7} ", Style::default().fg(p.overlay0)));
        spans.push(Span::styled(
            format!("\u{e0a0} {branch}"),
            Style::default().fg(p.green),
        ));
        if let Some((ahead, behind)) = ws.ahead_behind() {
            if ahead > 0 {
                spans.push(Span::styled(
                    format!(" \u{2191}{ahead}"),
                    Style::default().fg(p.yellow),
                ));
            }
            if behind > 0 {
                spans.push(Span::styled(
                    format!(" \u{2193}{behind}"),
                    Style::default().fg(p.peach),
                ));
            }
        }
        if let Some(pr) = ws.pr_state() {
            let (glyph, color) = crate::ui::state_signal::pr_state_glyph(pr.state, p);
            spans.push(Span::styled(
                format!(" #{} {glyph}", pr.number),
                Style::default().fg(color),
            ));
        }
    }
    // Session-promoted header field chips, AFTER branch/PR: the project and
    // branch segments win the width fight; chips fit into what remains, in
    // insertion order, middle-truncating the value under pressure.
    if let Some(terminal) = terminal {
        let chips = terminal.active_header_fields();
        if !chips.is_empty() {
            let base_width: usize = spans.iter().map(|span| span.content.chars().count()).sum();
            let avail = (header.width as usize).saturating_sub(base_width);
            for (key, value) in fit_header_field_chips(&chips, avail) {
                spans.push(Span::styled(" \u{b7} ", Style::default().fg(p.overlay0)));
                spans.push(Span::styled(
                    format!("{key} "),
                    Style::default().fg(p.overlay0),
                ));
                spans.push(Span::styled(value, Style::default().fg(p.text)));
            }
        }
    }
    buf.set_line(header.x, header.y, &Line::from(spans), header.width);

    // Prompt section. Reserve BOTH the context row above and the hairline
    // divider row below — the prompt must not render onto the divider's line.
    let prompt_rows = header.height.saturating_sub(2) as usize;
    if prompt_rows == 0 {
        return;
    }
    let width = header.width.saturating_sub(2) as usize;
    let prompt = terminal.and_then(|t| t.last_prompt.as_deref());
    let lines = match prompt {
        Some(prompt) => collapse_prompt_lines(prompt, prompt_rows, width),
        None => Vec::new(),
    };
    let prompt_style = Style::default().fg(p.subtext0);
    let marker_style = Style::default()
        .bg(p.surface_dim)
        .fg(p.overlay0)
        .add_modifier(Modifier::DIM);
    if lines.is_empty() {
        buf.set_stringn(
            header.x + 1,
            header.y + 1,
            "\u{276f} \u{2014}",
            width.max(3),
            marker_style,
        );
        return;
    }
    // Expanded: the full prompt REPLACES the collapsed view in place — same
    // header anchor and colors — extending downward over content, rather than
    // floating below the still-visible minimized header.
    if app.expanded_prompt_pane == Some(info.id) {
        if let Some(prompt) = prompt {
            let full: Vec<String> = prompt
                .lines()
                .map(|line| line.trim_end().to_string())
                .collect();
            let first_y = header.y + 1;
            let bottom = info.inner_rect.y + info.inner_rect.height;
            let avail = bottom.saturating_sub(first_y);
            let shown = (full.len() as u16).min(avail);
            for offset in 0..shown {
                let y = first_y + offset;
                for x in header.x..header.x + header.width {
                    buf[(x, y)].set_symbol(" ");
                    buf[(x, y)].set_style(bar_bg);
                }
                let prefix = if offset == 0 { "\u{276f} " } else { "  " };
                let mut text = format!("{prefix}{}", full[offset as usize]);
                if offset == 0 {
                    text.push_str(" \u{25b4}");
                }
                buf.set_stringn(
                    header.x + 1,
                    y,
                    text,
                    header.width.saturating_sub(1) as usize,
                    prompt_style,
                );
            }
            if (full.len() as u16) > shown && shown > 0 {
                buf.set_stringn(
                    header.x + 1,
                    first_y + shown - 1,
                    format!(
                        "\u{22ef} +{} more lines \u{22ef}",
                        full.len() as u16 - shown
                    ),
                    header.width.saturating_sub(1) as usize,
                    marker_style,
                );
            }
        }
        return;
    }

    let collapsed_content = lines.iter().any(|line| line.is_marker)
        || lines.iter().any(|line| line.text.contains('\u{2026}'));
    for (row, line) in lines.iter().enumerate() {
        let style = if line.is_marker {
            marker_style
        } else {
            prompt_style
        };
        let prefix = if row == 0 { "\u{276f} " } else { "  " };
        let mut text = format!("{prefix}{}", line.text);
        if row == 0 && collapsed_content {
            text = format!("{text} \u{25be}");
        }
        buf.set_stringn(
            header.x + 1,
            header.y + 1 + row as u16,
            text,
            header.width.saturating_sub(1) as usize,
            style,
        );
    }
}

/// "owner/label" when the space key is a normalized origin URL
/// ("github.com/owner/repo") — surfaces the org|person the repo belongs to in
/// the header. `dir:`-fallback keys and origin-less repos keep the bare label.
fn owner_qualified_project(key: Option<&str>, label: &str) -> String {
    let Some(key) = key else {
        return label.to_string();
    };
    if key.starts_with("dir:") {
        return label.to_string();
    }
    let mut segments = key.split('/');
    let (Some(_host), Some(owner)) = (segments.next(), segments.next()) else {
        return label.to_string();
    };
    if owner.is_empty() || segments.next().is_none() {
        return label.to_string();
    }
    format!("{owner}/{label}")
}

/// Minimum columns a chip value must get before the chip is dropped instead
/// of truncated to nothing.
const MIN_HEADER_CHIP_VALUE_COLS: usize = 4;
/// Columns of the " · " separator that precedes every chip.
const HEADER_CHIP_SEPARATOR_COLS: usize = 3;

/// Fit `key value` chips into `avail` columns. Chips are taken in insertion
/// order (earlier chips have priority); the first chip that no longer fits
/// at full width is middle-truncated into the remaining budget when at least
/// [`MIN_HEADER_CHIP_VALUE_COLS`] columns are left for its value, and
/// everything after it is dropped. Each returned chip costs
/// `separator + key + space + value` columns.
fn fit_header_field_chips(chips: &[(String, String)], avail: usize) -> Vec<(String, String)> {
    let mut remaining = avail;
    let mut fitted = Vec::new();
    for (key, value) in chips {
        let fixed = HEADER_CHIP_SEPARATOR_COLS + key.chars().count() + 1;
        let full = fixed + value.chars().count();
        if full <= remaining {
            fitted.push((key.clone(), value.clone()));
            remaining -= full;
            continue;
        }
        if fixed + MIN_HEADER_CHIP_VALUE_COLS <= remaining {
            let value_budget = remaining - fixed;
            fitted.push((key.clone(), middle_truncate(value, value_budget)));
        }
        break;
    }
    fitted
}

struct PromptFloatLine {
    text: String,
    is_marker: bool,
}

/// Middle-truncate a single line to `width` display columns, keeping the
/// start and end ("abcdef", 5 -> "ab\u{2026}ef").
fn middle_truncate(line: &str, width: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= width {
        return line.to_string();
    }
    if width <= 1 {
        return "\u{2026}".to_string();
    }
    let keep = width - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('\u{2026}');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// Collapse a prompt to at most `max_lines` rows of `width` columns: when the
/// prompt has more logical lines than fit, the MIDDLE is elided symmetrically
/// (head lines, a "+N lines" marker, tail lines) — the start and the end of
/// the prompt both survive. Overlong individual lines are middle-truncated.
fn collapse_prompt_lines(prompt: &str, max_lines: usize, width: usize) -> Vec<PromptFloatLine> {
    if max_lines == 0 || width == 0 {
        return Vec::new();
    }
    // The first rendered row carries a 2-col prompt marker prefix.
    let text_width = width.saturating_sub(2).max(1);
    let logical: Vec<&str> = prompt
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .into_iter()
        .skip_while(|line| line.is_empty())
        .collect();
    let logical: Vec<&str> = {
        let mut lines = logical;
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        lines
    };
    if logical.is_empty() {
        return Vec::new();
    }

    let truncated = |line: &str| PromptFloatLine {
        text: middle_truncate(line, text_width),
        is_marker: false,
    };

    if logical.len() <= max_lines {
        return logical.iter().map(|line| truncated(line)).collect();
    }
    if max_lines == 1 {
        // No room for a marker: fuse start and end of the whole prompt.
        let joined = format!(
            "{} \u{2026} {}",
            logical.first().unwrap_or(&""),
            logical.last().unwrap_or(&"")
        );
        return vec![truncated(&joined)];
    }

    let budget = max_lines - 1; // one row for the elision marker
    let head = budget.div_ceil(2);
    let tail = budget - head;
    let elided = logical.len() - head - tail;

    let mut out: Vec<PromptFloatLine> = Vec::with_capacity(max_lines);
    out.extend(logical[..head].iter().map(|line| truncated(line)));
    out.push(PromptFloatLine {
        text: format!("\u{22ef} +{elided} lines \u{22ef}"),
        is_marker: true,
    });
    out.extend(
        logical[logical.len() - tail..]
            .iter()
            .map(|line| truncated(line)),
    );
    out
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

    fn chips(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn header_chips_fit_in_insertion_order_within_budget() {
        let chips = chips(&[("build", "73%"), ("pg", "up")]);
        // " · build 73%" (12) + " · pg up" (8) = 20 columns.
        assert_eq!(
            fit_header_field_chips(&chips, 20),
            vec![
                ("build".to_string(), "73%".to_string()),
                ("pg".to_string(), "up".to_string()),
            ]
        );
    }

    #[test]
    fn header_chips_truncate_the_overflowing_value_and_drop_the_rest() {
        let chips = chips(&[("model", "claude-fable-5-20260120"), ("pg", "up")]);
        // fixed cost for "model" chip: 3 (sep) + 5 (key) + 1 = 9; budget 19
        // leaves 10 columns for the value -> middle-truncated.
        assert_eq!(
            fit_header_field_chips(&chips, 19),
            vec![("model".to_string(), "claud\u{2026}0120".to_string())]
        );
        // Earlier chips have priority: when the first chip cannot get even
        // its minimum value columns, later chips never jump the queue.
        assert_eq!(fit_header_field_chips(&chips, 10), Vec::new());
        // No budget at all -> no chips.
        assert_eq!(fit_header_field_chips(&chips, 0), Vec::new());
    }

    #[test]
    fn owner_qualified_project_parses_origin_keys() {
        assert_eq!(
            owner_qualified_project(Some("github.com/gerchowl/herdr"), "herdr"),
            "gerchowl/herdr"
        );
        // gitlab nested groups: top-level org qualifies
        assert_eq!(
            owner_qualified_project(Some("gitlab.com/group/sub/repo"), "repo"),
            "group/repo"
        );
        // origin-less / dir-fallback / missing keys keep the bare label
        assert_eq!(owner_qualified_project(Some("dir:notes"), "notes"), "notes");
        assert_eq!(owner_qualified_project(None, "scratch"), "scratch");
        // host-only or host/owner (no repo segment) stays bare
        assert_eq!(owner_qualified_project(Some("github.com"), "x"), "x");
        assert_eq!(owner_qualified_project(Some("github.com/solo"), "x"), "x");
    }

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
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
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
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
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
        // Always framed: even a tiny lone pane keeps the focus outline.
        assert_eq!(info.inner_rect, Rect::new(11, 4, 2, 6));
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
        assert_eq!(info.scrollbar_rect, Some(Rect::new(48, 4, 1, 6)));
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
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
    fn collapse_keeps_short_prompts_verbatim() {
        let lines = collapse_prompt_lines("fix the parser bug", 3, 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "fix the parser bug");
        assert!(!lines[0].is_marker);
    }

    #[test]
    fn collapse_elides_the_middle_not_the_end() {
        let prompt = "line one\nline two\nline three\nline four\nline five";
        let lines = collapse_prompt_lines(prompt, 3, 40);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "line one");
        assert!(lines[1].is_marker);
        assert!(lines[1].text.contains("+3 lines"));
        assert_eq!(lines[2].text, "line five");
    }

    #[test]
    fn collapse_distributes_head_heavy_for_even_budgets() {
        let prompt = (1..=10)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = collapse_prompt_lines(&prompt, 4, 40);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].text, "l1");
        assert_eq!(lines[1].text, "l2");
        assert!(lines[2].is_marker);
        assert_eq!(lines[3].text, "l10");
    }

    #[test]
    fn collapse_single_row_fuses_start_and_end() {
        let prompt = "first ask\nmiddle\nfinal ask";
        let lines = collapse_prompt_lines(prompt, 1, 60);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.starts_with("first ask"));
        assert!(lines[0].text.ends_with("final ask"));
    }

    #[test]
    fn middle_truncate_keeps_both_ends_of_long_lines() {
        let out = middle_truncate("abcdefghijklmnopqrstuvwxyz", 11);
        assert_eq!(out.chars().count(), 11);
        assert!(out.starts_with("abcde"));
        assert!(out.ends_with("vwxyz"));
        assert!(out.contains('\u{2026}'));
    }

    #[test]
    fn collapse_trims_blank_padding_lines() {
        let lines = collapse_prompt_lines("\n\n  do the thing  \n\n", 3, 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "  do the thing");
    }
    #[test]
    fn header_carve_reserves_rows_only_for_latched_agent_panes() {
        let mut app = AppState::test_new();
        app.pane_header = true;
        app.prompt_float_lines = 3;
        let ws = crate::workspace::Workspace::test_new("main");
        let pane_id = ws.focused_pane_id().unwrap();
        let terminal_id = ws.pane_state(pane_id).unwrap().attached_terminal_id.clone();
        let terminal = crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
        let pane_inner = Rect::new(0, 0, 80, 30);

        // Not latched: no reservation.
        app.terminals.insert(terminal_id.clone(), terminal);
        let (header, content) = carve_pane_header(&app, Some(&terminal_id), pane_inner);
        assert!(header.is_none());
        assert_eq!(content, pane_inner);

        // Latched: context row + prompt rows reserved, content shifts down.
        app.terminals.get_mut(&terminal_id).unwrap().header_reserved = true;
        let (header, content) = carve_pane_header(&app, Some(&terminal_id), pane_inner);
        let header = header.expect("header should be reserved");
        assert_eq!(header.height, 5);
        assert_eq!(content.y, pane_inner.y + 5);
        assert_eq!(content.height, pane_inner.height - 5);

        // Disabled config: nothing reserved.
        app.pane_header = false;
        let (header, content) = carve_pane_header(&app, Some(&terminal_id), pane_inner);
        assert!(header.is_none());
        assert_eq!(content, pane_inner);
    }

    #[test]
    fn header_carve_skips_tiny_panes() {
        let mut app = AppState::test_new();
        app.pane_header = true;
        app.prompt_float_lines = 3;
        let ws = crate::workspace::Workspace::test_new("main");
        let pane_id = ws.focused_pane_id().unwrap();
        let terminal_id = ws.pane_state(pane_id).unwrap().attached_terminal_id.clone();
        let mut terminal = crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal.header_reserved = true;
        app.terminals.insert(terminal_id.clone(), terminal);

        // 12 rows < 2*5+4: the pane keeps everything.
        let tiny = Rect::new(0, 0, 80, 12);
        let (header, content) = carve_pane_header(&app, Some(&terminal_id), tiny);
        assert!(header.is_none());
        assert_eq!(content, tiny);
    }
}
