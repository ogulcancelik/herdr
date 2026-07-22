use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Mode, Palette, ToastKind, ToastNotification},
    app::AppState,
    config::{ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    platform::status_metrics::{status_metrics, NetKind},
    session,
};

/// Full-width top status row — native parity with the user's tmux powerline.
///
/// Left:  [prefix]  session:ws.pane · host ·  user · cwd ·  branch
/// Right: 󰛳 lan 󰌘 ts  wan ·  [] ↓/↑ ·  mem ·  cpu · battery ·  date · time
///
/// Layout: spans the full client width above the sidebar. On narrow widths,
/// right-side tail segments drop first, then non-essential left segments.
pub(crate) fn render_status_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    let bg = Style::default().bg(p.panel_bg);
    frame.render_widget(Paragraph::new("").style(bg), area);

    let metrics = status_metrics();
    let prefix_active = app.mode == Mode::Prefix;

    let left = left_segments(app, &metrics, prefix_active, p);
    let right = right_segments(&metrics, p);

    // Fit strategy (powerline-ish):
    // 1) drop right-side segments from the tail (time → date → battery → …)
    // 2) if still too wide, drop left-side segments from the tail (branch → cwd)
    //    but never drop the session identity (+ prefix pill when present).
    let width_of = |segs: &[Segment], n: usize| -> usize {
        segs[..n].iter().map(|s| display_width(&s.text)).sum()
    };
    let min_left = {
        let mut n = 0usize;
        for (i, seg) in left.iter().enumerate() {
            if seg.preserve_bg {
                n = i + 1;
                continue;
            }
            if seg.text.contains('') {
                n = i + 1;
                if left.get(i + 1).is_some_and(|s| s.text.starts_with(':')) {
                    n = i + 2;
                }
                break;
            }
            n = i + 1;
            break;
        }
        n.clamp(1, left.len().max(1))
    };
    let mut left_keep = left.len();
    let mut right_keep = right.len();
    loop {
        let total = width_of(&left, left_keep) + width_of(&right, right_keep);
        if total <= area.width as usize {
            break;
        }
        if right_keep > 0 {
            right_keep -= 1;
        } else if left_keep > min_left {
            left_keep -= 1;
        } else {
            break;
        }
    }

    let mut spans: Vec<Span> = Vec::new();
    for seg in &left[..left_keep] {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    let used_left = width_of(&left, left_keep);
    let used_right = width_of(&right, right_keep);
    let pad = (area.width as usize)
        .saturating_sub(used_left)
        .saturating_sub(used_right);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), bg));
    }
    for seg in &right[..right_keep] {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}

struct Segment {
    text: String,
    style: Style,
    /// When true, `style` already carries its own background (prefix pill).
    preserve_bg: bool,
}

fn left_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    prefix_active: bool,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();

    if prefix_active {
        // Match tmux `#{?client_prefix,... § ...}`.
        out.push(Segment {
            text: " § ".into(),
            style: Style::default()
                .fg(p.panel_bg)
                .bg(p.yellow)
                .add_modifier(Modifier::BOLD),
            preserve_bg: true,
        });
    }

    let session = session::active_name()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| session::DEFAULT_SESSION_NAME.to_string());
    let (ws_label, _tab_label, pane_label, cwd, branch) = focused_identity(app);

    // Powerline session_icon: " #S" + dimmed ":#I.#P".
    out.push(Segment {
        text: format!("  {session}"),
        style: Style::default().fg(p.blue),
        preserve_bg: false,
    });
    out.push(Segment {
        text: format!(":{ws_label}.{pane_label} "),
        style: Style::default().fg(p.overlay0),
        preserve_bg: false,
    });

    // hostname_ssh: icon only when remote (SSH/mosh).
    let host_text = if metrics.remote_session {
        format!(" 󰣀 {} ", metrics.hostname)
    } else {
        format!(" {} ", metrics.hostname)
    };
    out.push(Segment {
        text: host_text,
        style: Style::default().fg(p.green),
        preserve_bg: false,
    });

    out.push(Segment {
        text: format!("  {} ", metrics.username),
        style: Style::default().fg(p.teal),
        preserve_bg: false,
    });

    if let Some(cwd) = cwd {
        let display = shorten_path(&cwd, 40);
        out.push(Segment {
            text: format!(" {display} "),
            style: Style::default().fg(p.mauve),
            preserve_bg: false,
        });
    }

    if let Some(branch) = branch {
        let branch = shorten_branch(&branch, 24);
        out.push(Segment {
            text: format!("  {branch} "),
            style: Style::default().fg(p.yellow),
            preserve_bg: false,
        });
    }

    out
}

fn right_segments(
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();

    // network_ips: "󰛳 LAN 󰌘 TS  WAN"
    let mut ip_parts: Vec<String> = Vec::new();
    if let Some(ip) = &metrics.local_ip {
        ip_parts.push(format!("󰛳 {ip}"));
    }
    if let Some(ip) = &metrics.tailscale_ip {
        ip_parts.push(format!("󰌘 {ip}"));
    }
    if let Some(ip) = &metrics.public_ip {
        ip_parts.push(format!(" {ip}"));
    }
    if !ip_parts.is_empty() {
        out.push(Segment {
            text: format!(" {} ", ip_parts.join(" ")),
            style: Style::default().fg(p.teal),
            preserve_bg: false,
        });
    }

    // bandwidth: wifi/eth glyph + optional VPN lock + ↓/↑ KiB/s
    if let (Some(down), Some(up)) = (metrics.net_down_kib, metrics.net_up_kib) {
        let kind_icon = match metrics.net_kind {
            NetKind::Ethernet => "󰈀",
            NetKind::Wifi | NetKind::Unknown => "",
        };
        let vpn = if metrics.vpn_active { " " } else { "" };
        out.push(Segment {
            text: format!(" {kind_icon}{vpn} ↓{down}K/s ↑{up}K/s "),
            style: Style::default().fg(p.green),
            preserve_bg: false,
        });
    }

    if let (Some(used), Some(total)) = (metrics.mem_used_gb, metrics.mem_total_gb) {
        out.push(Segment {
            text: format!("  {used:.1}/{total:.0} GB "),
            style: Style::default().fg(p.yellow),
            preserve_bg: false,
        });
    }

    if let Some(cpu) = metrics.cpu_percent {
        out.push(Segment {
            text: format!("  {cpu}% "),
            style: Style::default().fg(p.red),
            preserve_bg: false,
        });
    }

    if let Some(pct) = metrics.battery_percent {
        let icon = match metrics.battery_charging {
            Some(true) => "󰂄",
            _ => battery_icon(pct),
        };
        out.push(Segment {
            text: format!(" {icon} {pct}% "),
            style: Style::default().fg(p.blue),
            preserve_bg: false,
        });
    }

    let (date, time) = local_date_time();
    out.push(Segment {
        text: format!("  {date} "),
        style: Style::default().fg(p.overlay0),
        preserve_bg: false,
    });
    out.push(Segment {
        text: format!(" {time} "),
        style: Style::default().fg(p.subtext0),
        preserve_bg: false,
    });
    out
}

/// Nerd Font battery glyph by charge bucket.
fn battery_icon(pct: u8) -> &'static str {
    match pct {
        0..=10 => "󰁺",
        11..=20 => "󰁻",
        21..=30 => "󰁼",
        31..=40 => "󰁽",
        41..=50 => "󰁾",
        51..=60 => "󰁿",
        61..=70 => "󰂀",
        71..=80 => "󰂁",
        81..=90 => "󰂂",
        _ => "󰁹",
    }
}

fn focused_identity(app: &AppState) -> (String, String, String, Option<PathBuf>, Option<String>) {
    let Some(ws_idx) = app.active else {
        return ("1".into(), "1".into(), "1".into(), None, None);
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return ("1".into(), "1".into(), "1".into(), None, None);
    };
    let ws_label = (ws_idx + 1).to_string();
    let tab_idx = ws.active_tab_index();
    let tab_label = (tab_idx + 1).to_string();
    let pane_id = ws.focused_pane_id();
    // Prefer stable public pane numbers (tmux #P parity), not internal PaneId.
    let pane_label = pane_id
        .and_then(|id| ws.public_pane_number(id))
        .map(|n| n.to_string())
        .unwrap_or_else(|| "1".into());
    // Prefer live focused-pane terminal cwd; fall back to workspace identity.
    let cwd = pane_id
        .and_then(|pane_id| {
            ws.tabs
                .get(tab_idx)
                .and_then(|tab| tab.panes.get(&pane_id))
                .and_then(|pane| app.terminals.get(&pane.attached_terminal_id))
                .map(|terminal| terminal.cwd.clone())
        })
        .or_else(|| ws.resolved_identity_cwd())
        .or_else(|| Some(ws.identity_cwd.clone()));
    let branch = ws.cached_git_branch.clone();
    (ws_label, tab_label, pane_label, cwd, branch)
}

fn shorten_path(path: &Path, max_width: usize) -> String {
    let raw = path.to_string_lossy();
    let display = if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = raw.strip_prefix(&home) {
            // Powerline `pwd` collapses $HOME to `~` (keeping the slash as `~/…`).
            format!("~{rest}")
        } else {
            raw.into_owned()
        }
    } else {
        raw.into_owned()
    };
    if display_width(&display) <= max_width {
        return display;
    }
    // Left-truncate like powerline: keep the trailing path, prefix with `…/`.
    left_truncate_path(&display, max_width)
}

fn left_truncate_path(display: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    // Never shorter than the final path component if possible.
    let file = display.rsplit('/').next().unwrap_or(display);
    let file_w = display_width(file);
    if file_w >= max_width {
        return truncate_end(file, max_width);
    }
    let ellipsis = "…/";
    let ellipsis_w = display_width(ellipsis);
    if ellipsis_w + file_w >= max_width {
        return truncate_end(file, max_width);
    }
    let budget = max_width.saturating_sub(ellipsis_w);
    // Take a suffix of `display` that fits in budget, then force a clean `…/rest`.
    let mut width = 0usize;
    let mut start = display.len();
    for (idx, ch) in display.char_indices().rev() {
        let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_w > budget {
            break;
        }
        width += ch_w;
        start = idx;
    }
    let mut suffix = &display[start..];
    // Drop a partial leading component so we always start on a path boundary when possible.
    if let Some(slash) = suffix.find('/') {
        suffix = &suffix[slash + 1..];
    }
    if suffix.is_empty() {
        suffix = file;
    }
    format!("{ellipsis}{suffix}")
}

fn shorten_branch(branch: &str, max_width: usize) -> String {
    if display_width(branch) <= max_width {
        branch.to_string()
    } else {
        truncate_end(branch, max_width)
    }
}

fn local_date_time() -> (String, String) {
    // Characterization tests hash the full frame; freeze wall-clock there.
    #[cfg(test)]
    {
        return ("2026-01-02".into(), "03:04".into());
    }

    // Format via libc localtime to avoid a chrono dependency.
    #[cfg(not(test))]
    {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        {
            // SAFETY: localtime_r writes into our stack tm.
            let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
            let t = secs as libc::time_t;
            let rc = unsafe { libc::localtime_r(&t, &mut tm) };
            if !rc.is_null() {
                let date =
                    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday);
                let time = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
                return (date, time);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = secs;
        }
        ("----/--/--".into(), "--:--".into())
    }
}

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
) -> Rect {
    let content_width = display_width_u16(&toast.title)
        .max(display_width_u16(&toast.context))
        .saturating_add(4);
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::BottomLeft => area.x,
        ToastHerdrPosition::TopRight | ToastHerdrPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let warning_offset = u16::from(offset_for_warning);
    let y = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::TopRight => {
            area.y + warning_offset.min(area.height)
        }
        ToastHerdrPosition::BottomLeft | ToastHerdrPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + warning_offset)
        }
    };
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning, position);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
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
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

pub(super) fn render_config_diagnostic(frame: &mut Frame, area: Rect, message: &str, p: &Palette) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_dot(state: AgentState, seen: bool, p: &Palette) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("●", Style::default().fg(p.red)),
        (AgentState::Working, _) => ("●", Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("○", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("·", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn agent_icon(
    state: AgentState,
    seen: bool,
    tick: u32,
    p: &Palette,
) -> (&'static str, Style) {
    match (state, seen) {
        (AgentState::Blocked, _) => ("◉", Style::default().fg(p.red)),
        (AgentState::Working, _) => (super::spinner_frame(tick), Style::default().fg(p.yellow)),
        (AgentState::Idle, false) => ("●", Style::default().fg(p.teal)),
        (AgentState::Idle, true) => ("✓", Style::default().fg(p.green)),
        (AgentState::Unknown, _) => ("○", Style::default().fg(p.overlay0)),
    }
}

pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.yellow,
        (AgentState::Idle, false) => p.teal,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastHerdrPosition};

    fn toast() -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: "done".to_string(),
            context: "workspace".to_string(),
            position: None,
            target: None,
        }
    }

    fn feedback() -> CopyFeedback {
        CopyFeedback {
            message: "copied to clipboard".to_string(),
        }
    }

    #[test]
    fn toast_rect_uses_configured_corner() {
        let area = Rect::new(10, 20, 100, 40);
        let toast = toast();

        let top_left = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopLeft);
        assert_eq!(top_left.x, area.x);
        assert_eq!(top_left.y, area.y);

        let top_right = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);
        assert_eq!(top_right.x + top_right.width, area.x + area.width);
        assert_eq!(top_right.y, area.y);

        let bottom_left =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomLeft);
        assert_eq!(bottom_left.x, area.x);
        assert_eq!(bottom_left.y + bottom_left.height, area.y + area.height);

        let bottom_right =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomRight);
        assert_eq!(bottom_right.x + bottom_right.width, area.x + area.width);
        assert_eq!(bottom_right.y + bottom_right.height, area.y + area.height);
    }

    #[test]
    fn toast_rect_uses_display_width_for_cjk_labels() {
        let area = Rect::new(0, 0, 100, 20);
        let toast = ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "重构用户认证模块".to_string(),
            context: "提交 herdr 的反馈".to_string(),
            position: None,
            target: None,
        };

        let rect = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);

        let expected_content_width =
            display_width_u16(&toast.title).max(display_width_u16(&toast.context)) + 6;
        assert_eq!(rect.width, expected_content_width);
        assert_eq!(rect.x + rect.width, area.x + area.width);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = feedback();

        let top_center = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top_center.y, area.y);
        assert_eq!(
            top_center.x,
            area.x + area.width.saturating_sub(top_center.width) / 2
        );

        let bottom_center =
            copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom_center.y + bottom_center.height, area.y + area.height);
        assert_eq!(
            bottom_center.x,
            area.x + area.width.saturating_sub(bottom_center.width) / 2
        );
    }

    #[test]
    fn shorten_path_uses_tilde_for_home() {
        // Other tests mutate HOME; pin a private value for this assertion.
        let home = "/tmp/herdr-status-home-fixture";
        // SAFETY: tests run single-threaded in CI for env-sensitive cases; we restore below.
        let previous = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        let path = PathBuf::from(format!("{home}/Code/personal/home"));
        let display = shorten_path(&path, 80);
        match previous {
            Some(value) => unsafe {
                std::env::set_var("HOME", value);
            },
            None => unsafe {
                std::env::remove_var("HOME");
            },
        }
        assert!(
            display.starts_with("~/") || display.starts_with("~\\"),
            "expected tilde-shortened path, got {display}"
        );
        assert!(display.contains("Code"));
    }

    #[test]
    fn local_date_time_returns_plausible_shapes() {
        let (date, time) = local_date_time();
        assert_eq!(date.len(), 10, "YYYY-MM-DD");
        assert_eq!(time.len(), 5, "HH:MM");
        assert_eq!(&date[4..5], "-");
        assert_eq!(&date[7..8], "-");
        assert_eq!(&time[2..3], ":");
    }
}
