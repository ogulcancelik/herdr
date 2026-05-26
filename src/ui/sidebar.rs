use std::time::Duration;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{agent_icon, state_dot, state_label, state_label_color};
use crate::app::state::{
    ordered_sidebar_space_items, AgentPanelScope, Palette, SidebarAgentItem, SidebarLine,
    SidebarSpaceItem,
};
use crate::app::{AppState, Mode};
use crate::config::{SidebarColorPreset, SidebarItem};
use crate::detect::AgentState;
use crate::terminal::{TerminalRuntimeRegistry, WorkingDuration};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

pub(crate) struct AgentPanelEntry {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub pane_label: Option<String>,
    pub primary_label: String,
    pub primary_tab_label: Option<String>,
    pub agent_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    pub custom_status: Option<String>,
    pub working_duration: Option<WorkingDuration>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_current_workspace_idx(app: &AppState) -> Option<usize> {
    if matches!(
        app.mode,
        Mode::Navigate
            | Mode::RenameWorkspace
            | Mode::RenamePane
            | Mode::Resize
            | Mode::ConfirmClose
            | Mode::ContextMenu
            | Mode::Settings
            | Mode::GlobalMenu
            | Mode::KeybindHelp
            | Mode::ProductAnnouncement
    ) {
        Some(app.selected)
    } else {
        app.active
    }
}

fn agent_panel_toggle_label(scope: AgentPanelScope) -> &'static str {
    match scope {
        AgentPanelScope::CurrentWorkspace => "current",
        AgentPanelScope::AllWorkspaces => "all",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, scope: AgentPanelScope) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_toggle_label(scope);
    let width = label.chars().count() as u16;
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    match app.agent_panel_scope {
        AgentPanelScope::CurrentWorkspace => {
            let Some(ws_idx) = agent_panel_current_workspace_idx(app) else {
                return Vec::new();
            };
            let Some(ws) = app.workspaces.get(ws_idx) else {
                return Vec::new();
            };
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(|detail| {
                    let has_pane_label = detail.pane_label.is_some();
                    AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        pane_label: detail.pane_label,
                        primary_label: if has_pane_label {
                            workspace_label.clone()
                        } else {
                            detail.label
                        },
                        primary_tab_label: (has_pane_label && multi_tab)
                            .then_some(detail.tab_label),
                        agent_label: Some(detail.agent_label),
                        state: detail.state,
                        seen: detail.seen,
                        custom_status: detail.custom_status,
                        working_duration: detail.working_duration,
                    }
                })
                .collect()
        }
        AgentPanelScope::AllWorkspaces => app
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                let multi_tab = ws.tabs.len() > 1;
                let workspace_label = ws.display_name_from(&app.terminals, terminal_runtimes);
                ws.pane_details(&app.terminals)
                    .into_iter()
                    .map(move |detail| AgentPanelEntry {
                        ws_idx,
                        tab_idx: detail.tab_idx,
                        pane_id: detail.pane_id,
                        pane_label: detail.pane_label,
                        primary_label: workspace_label.clone(),
                        primary_tab_label: multi_tab.then_some(detail.tab_label),
                        agent_label: Some(detail.agent_label),
                        state: detail.state,
                        seen: detail.seen,
                        custom_status: detail.custom_status,
                        working_duration: detail.working_duration,
                    })
            })
            .collect(),
    }
}

fn truncate_text(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }
    let prefix_width = max_width.saturating_sub(1);
    let mut width = 0;
    let mut prefix = String::new();
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > prefix_width {
            break;
        }
        prefix.push(ch);
        width += ch_width;
    }
    format!("{prefix}…")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentPanelPrimarySegmentKind {
    Pane,
    Tab,
    Workspace,
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentPanelPrimarySegment {
    text: String,
    kind: AgentPanelPrimarySegmentKind,
}

impl AgentPanelPrimarySegment {
    fn new(text: impl Into<String>, kind: AgentPanelPrimarySegmentKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

fn push_full_primary_segments(
    segments: &mut Vec<AgentPanelPrimarySegment>,
    pane_label: &str,
    tab_label: Option<&str>,
    workspace_label: &str,
) {
    let separator = " · ";
    segments.push(AgentPanelPrimarySegment::new(
        pane_label,
        AgentPanelPrimarySegmentKind::Pane,
    ));
    if let Some(tab_label) = tab_label {
        segments.push(AgentPanelPrimarySegment::new(
            separator,
            AgentPanelPrimarySegmentKind::Separator,
        ));
        segments.push(AgentPanelPrimarySegment::new(
            tab_label,
            AgentPanelPrimarySegmentKind::Tab,
        ));
    }
    segments.push(AgentPanelPrimarySegment::new(
        separator,
        AgentPanelPrimarySegmentKind::Separator,
    ));
    segments.push(AgentPanelPrimarySegment::new(
        workspace_label,
        AgentPanelPrimarySegmentKind::Workspace,
    ));
}

fn truncated_pane_side_segments(
    pane_label: &str,
    tab_label: Option<&str>,
    max_width: usize,
) -> Vec<AgentPanelPrimarySegment> {
    let separator = " · ";
    let separator_width = UnicodeWidthStr::width(separator);
    let Some(tab_label) = tab_label else {
        return vec![AgentPanelPrimarySegment::new(
            truncate_text(pane_label, max_width),
            AgentPanelPrimarySegmentKind::Pane,
        )];
    };

    let tab_width = UnicodeWidthStr::width(tab_label);
    let full_side = format!("{pane_label}{separator}{tab_label}");
    if UnicodeWidthStr::width(full_side.as_str()) <= max_width {
        return vec![
            AgentPanelPrimarySegment::new(pane_label, AgentPanelPrimarySegmentKind::Pane),
            AgentPanelPrimarySegment::new(separator, AgentPanelPrimarySegmentKind::Separator),
            AgentPanelPrimarySegment::new(tab_label, AgentPanelPrimarySegmentKind::Tab),
        ];
    }

    if max_width > separator_width + tab_width {
        let pane_width = max_width - separator_width - tab_width;
        return vec![
            AgentPanelPrimarySegment::new(
                truncate_text(pane_label, pane_width),
                AgentPanelPrimarySegmentKind::Pane,
            ),
            AgentPanelPrimarySegment::new(separator, AgentPanelPrimarySegmentKind::Separator),
            AgentPanelPrimarySegment::new(tab_label, AgentPanelPrimarySegmentKind::Tab),
        ];
    }

    vec![AgentPanelPrimarySegment::new(
        truncate_text(&full_side, max_width),
        AgentPanelPrimarySegmentKind::Pane,
    )]
}

fn agent_panel_primary_label_segments_with_options(
    entry: &AgentPanelEntry,
    max_width: usize,
    show_pane_name: bool,
    show_tab_name: bool,
) -> Vec<AgentPanelPrimarySegment> {
    let separator = " · ";
    let separator_width = UnicodeWidthStr::width(separator);
    let pane_label = show_pane_name
        .then_some(entry.pane_label.as_deref())
        .flatten();
    let tab_label = show_tab_name
        .then_some(entry.primary_tab_label.as_deref())
        .flatten();
    let Some(pane_label) = pane_label else {
        let Some(tab_label) = tab_label else {
            return vec![AgentPanelPrimarySegment::new(
                truncate_text(&entry.primary_label, max_width),
                AgentPanelPrimarySegmentKind::Workspace,
            )];
        };

        let full = format!("{tab_label}{separator}{}", entry.primary_label);
        if UnicodeWidthStr::width(full.as_str()) <= max_width {
            return vec![
                AgentPanelPrimarySegment::new(tab_label, AgentPanelPrimarySegmentKind::Tab),
                AgentPanelPrimarySegment::new(separator, AgentPanelPrimarySegmentKind::Separator),
                AgentPanelPrimarySegment::new(
                    entry.primary_label.clone(),
                    AgentPanelPrimarySegmentKind::Workspace,
                ),
            ];
        }

        let workspace_width = UnicodeWidthStr::width(entry.primary_label.as_str());
        if max_width > workspace_width + separator_width {
            let tab_width = max_width - workspace_width - separator_width;
            return vec![
                AgentPanelPrimarySegment::new(
                    truncate_text(tab_label, tab_width),
                    AgentPanelPrimarySegmentKind::Tab,
                ),
                AgentPanelPrimarySegment::new(separator, AgentPanelPrimarySegmentKind::Separator),
                AgentPanelPrimarySegment::new(
                    entry.primary_label.clone(),
                    AgentPanelPrimarySegmentKind::Workspace,
                ),
            ];
        }

        return vec![AgentPanelPrimarySegment::new(
            truncate_text(&full, max_width),
            AgentPanelPrimarySegmentKind::Workspace,
        )];
    };

    let side = match tab_label {
        Some(tab_label) => format!("{pane_label}{separator}{tab_label}"),
        None => pane_label.to_string(),
    };
    let full = format!("{side}{separator}{}", entry.primary_label);
    if UnicodeWidthStr::width(full.as_str()) <= max_width {
        let mut segments = Vec::new();
        push_full_primary_segments(&mut segments, pane_label, tab_label, &entry.primary_label);
        return segments;
    }

    let workspace_width = UnicodeWidthStr::width(entry.primary_label.as_str());
    let min_named_side_width = 1;
    if max_width >= workspace_width + separator_width + min_named_side_width {
        let side_width = max_width - workspace_width - separator_width;
        let mut segments = truncated_pane_side_segments(pane_label, tab_label, side_width);
        segments.push(AgentPanelPrimarySegment::new(
            separator,
            AgentPanelPrimarySegmentKind::Separator,
        ));
        segments.push(AgentPanelPrimarySegment::new(
            entry.primary_label.clone(),
            AgentPanelPrimarySegmentKind::Workspace,
        ));
        return segments;
    }

    vec![AgentPanelPrimarySegment::new(
        truncate_text(&full, max_width),
        AgentPanelPrimarySegmentKind::Workspace,
    )]
}

#[cfg(test)]
pub(super) fn format_agent_panel_primary_label(
    entry: &AgentPanelEntry,
    max_width: usize,
) -> String {
    format_agent_panel_primary_label_with_options(entry, max_width, true, true)
}

#[cfg(test)]
fn format_agent_panel_primary_label_with_options(
    entry: &AgentPanelEntry,
    max_width: usize,
    show_pane_name: bool,
    show_tab_name: bool,
) -> String {
    agent_panel_primary_label_segments_with_options(entry, max_width, show_pane_name, show_tab_name)
        .into_iter()
        .map(|segment| segment.text)
        .collect()
}

fn agent_panel_primary_label_spans(
    entry: &AgentPanelEntry,
    max_width: usize,
    show_pane_name: bool,
    show_tab_name: bool,
    pane_style: Style,
    tab_style: Style,
    workspace_style: Style,
    p: &Palette,
) -> Vec<Span<'static>> {
    agent_panel_primary_label_segments_with_options(entry, max_width, show_pane_name, show_tab_name)
        .into_iter()
        .map(|segment| {
            let style = match segment.kind {
                AgentPanelPrimarySegmentKind::Pane => pane_style,
                AgentPanelPrimarySegmentKind::Tab => tab_style,
                AgentPanelPrimarySegmentKind::Workspace => workspace_style,
                AgentPanelPrimarySegmentKind::Separator => Style::default().fg(p.overlay0),
            };
            Span::styled(segment.text, style)
        })
        .collect()
}

fn compact_duration_parts(duration: Duration) -> Vec<(String, &'static str)> {
    let total_seconds = duration.as_secs().max(1);
    if total_seconds < 60 {
        return vec![(total_seconds.to_string(), "s")];
    }

    let total_minutes = total_seconds / 60;
    if total_minutes < 60 {
        return vec![
            (total_minutes.to_string(), "m"),
            ((total_seconds % 60).to_string(), "s"),
        ];
    }

    vec![
        ((total_minutes / 60).to_string(), "h"),
        ((total_minutes % 60).to_string(), "m"),
    ]
}

fn duration_number_style(duration: WorkingDuration, p: &Palette) -> Style {
    if duration.is_live {
        Style::default().fg(p.subtext0)
    } else {
        Style::default().fg(p.overlay0).add_modifier(Modifier::DIM)
    }
}

fn duration_unit_style(unit: &str, duration: WorkingDuration, p: &Palette) -> Style {
    if !duration.is_live {
        return duration_number_style(duration, p);
    }
    if unit == "s" {
        return duration_number_style(duration, p);
    }

    let color = match unit {
        "h" => p.subtext0,
        "m" => p.overlay1,
        _ => p.overlay0,
    };
    Style::default().fg(color)
}

fn duration_value_spans(duration: WorkingDuration, p: &Palette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (value, unit) in compact_duration_parts(duration.elapsed) {
        spans.push(Span::styled(value, duration_number_style(duration, p)));
        spans.push(Span::styled(unit, duration_unit_style(unit, duration, p)));
    }
    spans
}

fn style_with_sidebar_color(
    style: Style,
    preset: SidebarColorPreset,
    default: Color,
    p: &Palette,
) -> Style {
    if preset == SidebarColorPreset::Default {
        style
    } else {
        style.fg(p.sidebar_color(preset, default))
    }
}

fn spans_with_sidebar_color(
    spans: Vec<Span<'static>>,
    preset: SidebarColorPreset,
    p: &Palette,
) -> Vec<Span<'static>> {
    if preset == SidebarColorPreset::Default {
        return spans;
    }

    spans
        .into_iter()
        .map(|span| {
            let Span { content, style } = span;
            let default = style.fg.unwrap_or(p.text);
            Span::styled(content, style_with_sidebar_color(style, preset, default, p))
        })
        .collect()
}

fn push_separator(spans: &mut Vec<Span<'static>>, style: Style) {
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", style));
    }
}

fn push_space_separator(
    spans: &mut Vec<Span<'static>>,
    previous_was_status: &mut bool,
    style: Style,
) {
    if !spans.is_empty() {
        let separator = if *previous_was_status { " " } else { " · " };
        spans.push(Span::styled(separator, style));
    }
    *previous_was_status = false;
}

fn branch_status_parts(
    ahead_behind: Option<(usize, usize)>,
    p: &Palette,
) -> Option<Vec<(String, ratatui::style::Color)>> {
    let (ahead, behind) = ahead_behind?;
    let mut parts = Vec::new();
    if ahead > 0 {
        parts.push((format!("↑{}", ahead), p.green));
    }
    if behind > 0 {
        parts.push((format!("↓{}", behind), p.red));
    }
    (!parts.is_empty()).then_some(parts)
}

fn sidebar_space_item_has_content(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    item: SidebarSpaceItem,
) -> bool {
    if !item.enabled(&app.sidebar_space) {
        return false;
    }
    match item {
        SidebarSpaceItem::Status | SidebarSpaceItem::Name => true,
        SidebarSpaceItem::Branch => ws.branch().is_some(),
        SidebarSpaceItem::BranchStatus => ws.branch().is_some() && ws.git_ahead_behind().is_some(),
    }
}

fn workspace_line_has_content(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    line: SidebarLine,
) -> bool {
    ordered_sidebar_space_items(&app.sidebar_space)
        .into_iter()
        .filter(|item| item.line(&app.sidebar_space) == line)
        .any(|item| sidebar_space_item_has_content(app, ws, item))
}

fn workspace_render_lines(app: &AppState, ws: &crate::workspace::Workspace) -> Vec<SidebarLine> {
    let mut lines = Vec::new();
    for line in (0..app.sidebar_space.lines.len().max(1)).map(SidebarLine::from_index) {
        if workspace_line_has_content(app, ws, line) {
            lines.push(line);
        }
    }
    if !lines.is_empty() && !lines.contains(&SidebarLine::First) {
        lines.insert(0, SidebarLine::First);
    }
    if lines.is_empty() {
        lines.push(SidebarLine::First);
    }
    lines
}

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace) -> u16 {
    workspace_render_lines(app, ws).len() as u16
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    app.workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals))
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

fn grouped_child_display_label(label: &str, branch: Option<&str>, has_custom_name: bool) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace { ws_idx: usize, indented: bool },
}

fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(WorkspaceListEntry::Workspace { indented: true, .. })
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    let entry_count = workspace_list_entries(app).len();
    if entry_count == 0 {
        0
    } else {
        requested.min(entry_count.saturating_sub(1))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        if let Some(space) = ws.worktree_space() {
            members_by_key
                .entry(space.key.clone())
                .or_default()
                .push(ws_idx);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members.iter().any(|idx| {
                    app.workspaces
                        .get(*idx)
                        .and_then(|ws| ws.worktree_space())
                        .is_some_and(|space| !space.is_linked_worktree)
                })
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_group_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_group = visible_group_idx.and_then(|idx| {
        app.workspaces
            .get(idx)
            .and_then(|ws| ws.worktree_space())
            .map(|space| space.key.clone())
    });

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (ws_idx, ws) in app.workspaces.iter().enumerate() {
        let Some(space) = ws
            .worktree_space()
            .filter(|space| grouped_keys.contains(&space.key))
        else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };

        if !emitted_groups.insert(space.key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(&space.key) else {
            continue;
        };
        let Some(parent_idx) = members.iter().copied().find(|idx| {
            app.workspaces
                .get(*idx)
                .and_then(|member| member.worktree_space())
                .is_some_and(|member_space| !member_space.is_linked_worktree)
        }) else {
            entries.push(WorkspaceListEntry::Workspace {
                ws_idx,
                indented: false,
            });
            continue;
        };
        let collapsed = app.collapsed_space_keys.contains(&space.key);
        entries.push(WorkspaceListEntry::Workspace {
            ws_idx: parent_idx,
            indented: false,
        });

        if collapsed {
            if let Some(active_idx) = visible_group_idx
                .filter(|idx| *idx != parent_idx)
                .filter(|_| active_group.as_deref() == Some(space.key.as_str()))
            {
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: active_idx,
                    indented: true,
                });
            }
        } else {
            for member_idx in members {
                if *member_idx == parent_idx {
                    continue;
                }
                entries.push(WorkspaceListEntry::Workspace {
                    ws_idx: *member_idx,
                    indented: true,
                });
            }
        }
    }
    entries
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let needed = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(app, ws)
                };
                let gap = u16::from(
                    !(*indented && next_entry_is_indented_workspace(&entries, entry_idx)),
                );
                row_height.saturating_add(gap)
            }
        };
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        visible += 1;
    }
    visible
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let entries = workspace_list_entries(app);
    let total_rows = entries.len();
    let scroll = app.workspace_scroll.min(total_rows.saturating_sub(1));
    let viewport_rows = workspace_list_visible_count(app, area, scroll);
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn sidebar_agent_render_lines(app: &AppState) -> Vec<SidebarLine> {
    let mut lines: Vec<_> = app
        .sidebar_agent
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.iter().any(|item| item.show))
        .map(|(idx, _)| SidebarLine::from_index(idx))
        .collect();
    if lines.is_empty() {
        lines.push(SidebarLine::First);
    }
    lines
}

pub(crate) fn agent_panel_entry_row_count(app: &AppState) -> u16 {
    sidebar_agent_render_lines(app).len() as u16
}

fn agent_panel_visible_count(app: &AppState, area: Rect) -> usize {
    let body = agent_panel_body_rect(area, false);
    let entry_rows = agent_panel_entry_row_count(app);
    if body.width == 0 || entry_rows == 0 || body.height < entry_rows {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    while used_rows.saturating_add(entry_rows) <= body.height {
        used_rows = used_rows.saturating_add(entry_rows);
        visible += 1;
        if used_rows < body.height {
            used_rows = used_rows.saturating_add(1);
        }
    }
    visible
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let viewport_rows = agent_panel_visible_count(app, area);
    let total_rows = agent_panel_entries(app).len();
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.agent_panel_scroll)
        .saturating_sub(viewport_rows);

    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = if *indented {
                    1
                } else {
                    workspace_row_height(app, ws)
                };
                let gap = u16::from(
                    !(*indented && next_entry_is_indented_workspace(&entries, entry_idx)),
                );
                if row_y.saturating_add(row_height).saturating_add(gap) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                });
                row_y = row_y.saturating_add(row_height + gap);
            }
        }
    }

    (cards, headers)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, ws) in app.workspaces.iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = visible_idx == app.selected && is_navigating;
        let is_active = Some(visible_idx) == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_ws_idx = if is_navigating {
        Some(app.selected)
    } else {
        app.active
    };
    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        if let Some(ws_idx) = detail_ws_idx {
            if let Some(ws) = app.workspaces.get(ws_idx) {
                for (detail_idx, detail) in ws.pane_details(&app.terminals).iter().enumerate() {
                    let y = detail_content_area.y + detail_idx as u16;
                    if y >= detail_content_area.y + detail_content_area.height {
                        break;
                    }
                    let pane_num = ws
                        .public_pane_number(detail.pane_id)
                        .unwrap_or(detail_idx + 1);
                    let pane_style = Style::default().fg(p.overlay0);
                    let (icon, icon_style) =
                        agent_icon(detail.state, detail.seen, app.spinner_tick, p);
                    frame.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(format!("{pane_num}"), pane_style),
                            Span::styled(" ", pane_style),
                            Span::styled(icon, icon_style),
                        ])),
                        Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
                    );
                }
            }
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let first = cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, detail_area);
    render_sidebar_toggle(app, frame, area, false, p);
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(&app.view.workspace_card_areas, area, *insert_idx),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    for card in cards {
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let label = ws.display_name_from(&app.terminals, terminal_runtimes);
        let parent_group = (!card.indented)
            .then(|| workspace_parent_group_state(app, i))
            .flatten();
        let status_dot = if let Some((key, true)) = parent_group.as_ref() {
            let (state, seen) = space_aggregate_state(app, key);
            state_dot(state, seen, p)
        } else {
            (icon, icon_style)
        };
        let branch_color = if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        };
        let branch_style = Style::default().fg(branch_color);
        let separator_style = Style::default().fg(p.overlay0);
        let ordered_items = ordered_sidebar_space_items(&app.sidebar_space);
        let render_lines = workspace_render_lines(app, ws);

        for (render_idx, line) in render_lines.into_iter().enumerate() {
            let y = row_y + render_idx as u16;
            if y >= list_bottom || render_idx as u16 >= row_height {
                continue;
            }

            let mut prefix = Vec::new();
            if render_idx == 0 {
                if card.indented {
                    prefix.push(Span::styled("   ", Style::default()));
                } else if let Some((_, collapsed)) = parent_group.as_ref() {
                    let icon = if *collapsed { "▸" } else { "▾" };
                    prefix.push(Span::styled(icon, Style::default().fg(p.accent)));
                    prefix.push(Span::styled(" ", Style::default()));
                } else {
                    prefix.push(Span::styled(" ", Style::default()));
                }
            } else {
                prefix.push(Span::styled(
                    if card.indented { "     " } else { "   " },
                    Style::default(),
                ));
            }

            let mut content = Vec::new();
            let mut previous_was_status = false;
            for item in ordered_items
                .iter()
                .copied()
                .filter(|item| item.line(&app.sidebar_space) == line)
                .filter(|item| item.enabled(&app.sidebar_space))
            {
                match item {
                    SidebarSpaceItem::Status => {
                        push_separator(&mut content, separator_style);
                        let icon_style = style_with_sidebar_color(
                            status_dot.1,
                            item.color(&app.sidebar_space),
                            status_dot.1.fg.unwrap_or(p.accent),
                            p,
                        );
                        content.push(Span::styled(status_dot.0, icon_style));
                        previous_was_status = true;
                    }
                    SidebarSpaceItem::Name => {
                        push_space_separator(
                            &mut content,
                            &mut previous_was_status,
                            separator_style,
                        );
                        let display_label = if card.indented {
                            grouped_child_display_label(
                                &label,
                                ws.branch().as_deref(),
                                ws.custom_name.is_some(),
                            )
                        } else {
                            label.clone()
                        };
                        let item_style = style_with_sidebar_color(
                            name_style,
                            item.color(&app.sidebar_space),
                            name_style.fg.unwrap_or(p.subtext0),
                            p,
                        );
                        content.push(Span::styled(display_label, item_style));
                    }
                    SidebarSpaceItem::Branch => {
                        if let Some(branch) = ws.branch() {
                            push_space_separator(
                                &mut content,
                                &mut previous_was_status,
                                separator_style,
                            );
                            let item_style = style_with_sidebar_color(
                                branch_style,
                                item.color(&app.sidebar_space),
                                branch_style.fg.unwrap_or(p.overlay0),
                                p,
                            );
                            content.push(Span::styled(branch, item_style));
                        }
                    }
                    SidebarSpaceItem::BranchStatus => {
                        if ws.branch().is_some() {
                            if let Some(parts) = branch_status_parts(ws.git_ahead_behind(), p) {
                                push_space_separator(
                                    &mut content,
                                    &mut previous_was_status,
                                    separator_style,
                                );
                                for (idx, (label, color)) in parts.into_iter().enumerate() {
                                    if idx > 0 {
                                        content.push(Span::styled(" ", Style::default()));
                                    }
                                    content.push(Span::styled(
                                        label,
                                        Style::default().fg(
                                            p.sidebar_color(item.color(&app.sidebar_space), color)
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }

            prefix.extend(content);
            frame.render_widget(
                Paragraph::new(Line::from(prefix)),
                Rect::new(card.rect.x, y, card.rect.width, 1),
            );
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("menu", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

fn sidebar_agent_item_spans(
    entry: &AgentPanelEntry,
    app: &AppState,
    item: SidebarAgentItem,
    name_style: Style,
    status_style: Style,
    agent_style: Style,
    p: &Palette,
) -> Option<Vec<Span<'static>>> {
    if !item.enabled(&app.sidebar_agent) {
        return None;
    }
    let spans = match item {
        SidebarAgentItem::AgentStatus => {
            let (icon, icon_style) = agent_icon(entry.state, entry.seen, app.spinner_tick, p);
            Some(vec![Span::styled(icon, icon_style)])
        }
        SidebarAgentItem::PaneName => entry
            .pane_label
            .as_ref()
            .map(|label| vec![Span::styled(label.clone(), Style::default().fg(p.green))]),
        SidebarAgentItem::TabName => entry
            .primary_tab_label
            .as_ref()
            .map(|label| vec![Span::styled(label.clone(), Style::default().fg(p.mauve))]),
        SidebarAgentItem::SpaceName => {
            Some(vec![Span::styled(entry.primary_label.clone(), name_style)])
        }
        SidebarAgentItem::Status => Some(vec![Span::styled(
            state_label(entry.state, entry.seen),
            status_style,
        )]),
        SidebarAgentItem::Time => entry
            .working_duration
            .map(|duration| duration_value_spans(duration, p)),
        SidebarAgentItem::CustomStatus => entry
            .custom_status
            .as_ref()
            .map(|status| vec![Span::styled(status.clone(), agent_style)]),
        SidebarAgentItem::AgentName => entry
            .agent_label
            .as_ref()
            .map(|label| vec![Span::styled(label.clone(), agent_style)]),
        SidebarAgentItem::RightAlignment => None,
    }?;
    Some(spans_with_sidebar_color(
        spans,
        item.color(&app.sidebar_agent),
        p,
    ))
}

fn is_agent_identity_item(item: SidebarAgentItem) -> bool {
    matches!(
        item,
        SidebarAgentItem::PaneName | SidebarAgentItem::TabName | SidebarAgentItem::SpaceName
    )
}

fn identity_items_keep_default_order(items: &[SidebarAgentItem]) -> bool {
    let mut next_default_idx = 0;
    for item in items {
        let Some(default_idx) = [
            SidebarAgentItem::PaneName,
            SidebarAgentItem::TabName,
            SidebarAgentItem::SpaceName,
        ]
        .iter()
        .position(|candidate| candidate == item) else {
            return false;
        };
        if default_idx < next_default_idx {
            return false;
        }
        next_default_idx = default_idx + 1;
    }
    items.contains(&SidebarAgentItem::SpaceName)
}

#[derive(Default)]
struct SidebarAgentLineContent {
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
}

fn push_agent_line_spans(
    spans: &mut Vec<Span<'static>>,
    previous_was_agent_status: &mut bool,
    item_spans: Vec<Span<'static>>,
    item: SidebarAgentItem,
    separator_style: Style,
) {
    push_space_separator(spans, previous_was_agent_status, separator_style);
    spans.extend(item_spans);
    *previous_was_agent_status = item == SidebarAgentItem::AgentStatus;
}

fn sidebar_agent_line_content(
    entry: &AgentPanelEntry,
    app: &AppState,
    line: SidebarLine,
    max_width: usize,
    name_style: Style,
    status_style: Style,
    agent_style: Style,
    p: &Palette,
) -> SidebarAgentLineContent {
    let mut content = SidebarAgentLineContent::default();
    let line_items: Vec<SidebarItem<SidebarAgentItem>> = app
        .sidebar_agent
        .lines
        .get(line.index())
        .cloned()
        .unwrap_or_default();
    let mut idx = 0;
    let mut right_aligned = false;
    let mut previous_left_was_agent_status = false;
    let mut previous_right_was_agent_status = false;
    while idx < line_items.len() {
        let item = line_items[idx].field;
        if item == SidebarAgentItem::RightAlignment {
            if line_items[idx].show {
                right_aligned = true;
            }
            idx += 1;
            continue;
        }
        if is_agent_identity_item(item) {
            let end = line_items[idx..]
                .iter()
                .position(|candidate| !is_agent_identity_item(candidate.field))
                .map_or(line_items.len(), |offset| idx + offset);
            let sequence: Vec<_> = line_items[idx..end]
                .iter()
                .map(|entry| entry.field)
                .collect();
            if SidebarAgentItem::SpaceName.enabled(&app.sidebar_agent)
                && identity_items_keep_default_order(&sequence)
            {
                let show_pane_name = sequence.contains(&SidebarAgentItem::PaneName)
                    && SidebarAgentItem::PaneName.enabled(&app.sidebar_agent);
                let show_tab_name = sequence.contains(&SidebarAgentItem::TabName)
                    && SidebarAgentItem::TabName.enabled(&app.sidebar_agent);
                let pane_style = style_with_sidebar_color(
                    Style::default().fg(p.green),
                    SidebarAgentItem::PaneName.color(&app.sidebar_agent),
                    p.green,
                    p,
                );
                let tab_style = style_with_sidebar_color(
                    Style::default().fg(p.mauve),
                    SidebarAgentItem::TabName.color(&app.sidebar_agent),
                    p.mauve,
                    p,
                );
                let workspace_style = style_with_sidebar_color(
                    name_style,
                    SidebarAgentItem::SpaceName.color(&app.sidebar_agent),
                    name_style.fg.unwrap_or(p.subtext0),
                    p,
                );
                if right_aligned {
                    let separator_width = if content.right.is_empty() {
                        0
                    } else if previous_right_was_agent_status {
                        1
                    } else {
                        3
                    };
                    let available_width =
                        max_width.saturating_sub(spans_width(&content.right) + separator_width);
                    let item_spans = agent_panel_primary_label_spans(
                        entry,
                        available_width,
                        show_pane_name,
                        show_tab_name,
                        pane_style,
                        tab_style,
                        workspace_style,
                        p,
                    );
                    push_agent_line_spans(
                        &mut content.right,
                        &mut previous_right_was_agent_status,
                        item_spans,
                        SidebarAgentItem::SpaceName,
                        agent_style,
                    );
                } else {
                    let separator_width = if content.left.is_empty() {
                        0
                    } else if previous_left_was_agent_status {
                        1
                    } else {
                        3
                    };
                    let available_width =
                        max_width.saturating_sub(spans_width(&content.left) + separator_width);
                    let item_spans = agent_panel_primary_label_spans(
                        entry,
                        available_width,
                        show_pane_name,
                        show_tab_name,
                        pane_style,
                        tab_style,
                        workspace_style,
                        p,
                    );
                    push_agent_line_spans(
                        &mut content.left,
                        &mut previous_left_was_agent_status,
                        item_spans,
                        SidebarAgentItem::SpaceName,
                        agent_style,
                    );
                }
                idx = end;
                continue;
            }
        }
        if let Some(item_spans) =
            sidebar_agent_item_spans(entry, app, item, name_style, status_style, agent_style, p)
        {
            if right_aligned {
                push_agent_line_spans(
                    &mut content.right,
                    &mut previous_right_was_agent_status,
                    item_spans,
                    item,
                    agent_style,
                );
            } else {
                push_agent_line_spans(
                    &mut content.left,
                    &mut previous_left_was_agent_status,
                    item_spans,
                    item,
                    agent_style,
                );
            }
        }
        idx += 1;
    }
    content
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn truncate_spans_to_width(spans: &[Span<'static>], max_width: usize) -> Vec<Span<'static>> {
    let mut width = 0;
    let mut truncated = Vec::new();
    for span in spans {
        if width >= max_width {
            break;
        }
        let mut content = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if width + ch_width > max_width {
                break;
            }
            content.push(ch);
            width += ch_width;
        }
        if !content.is_empty() {
            truncated.push(Span::styled(content, span.style));
        }
    }
    truncated
}

fn fixed_width_agent_line_spans(
    mut content: SidebarAgentLineContent,
    width: u16,
    fill_style: Style,
) -> Vec<Span<'static>> {
    let width = width as usize;
    if width == 0 {
        return Vec::new();
    }
    if content.right.is_empty() {
        return truncate_spans_to_width(&content.left, width);
    }

    let mut right = content.right;
    let right_width = spans_width(&right).min(width);
    if right_width < spans_width(&right) {
        right = truncate_spans_to_width(&right, right_width);
    }
    let gap = usize::from(!content.left.is_empty() && right_width < width);
    let left_width = width.saturating_sub(right_width + gap);
    content.left = truncate_spans_to_width(&content.left, left_width);
    let left_width = spans_width(&content.left);
    let pad_width = width.saturating_sub(left_width + right_width);
    let mut spans = content.left;
    if pad_width > 0 {
        spans.push(Span::styled(" ".repeat(pad_width), fill_style));
    }
    spans.extend(right);
    spans
}

fn render_agent_line(
    frame: &mut Frame,
    rect: Rect,
    content: SidebarAgentLineContent,
    row_style: Style,
    right_style: Style,
) {
    if rect.width == 0 {
        return;
    }
    if content.right.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(content.left)).style(row_style),
            rect,
        );
        return;
    }

    {
        let buf = frame.buffer_mut();
        for x in rect.x..rect.x + rect.width {
            buf[(x, rect.y)].set_style(row_style);
        }
    }
    let right_width = spans_width(&content.right).min(rect.width as usize) as u16;
    let gap = u16::from(!content.left.is_empty() && right_width < rect.width);
    let left_width = rect.width.saturating_sub(right_width.saturating_add(gap));
    if left_width > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(content.left)).style(row_style),
            Rect::new(rect.x, rect.y, left_width, 1),
        );
    }
    if right_width > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(content.right))
                .style(row_style)
                .alignment(Alignment::Right),
            Rect::new(
                rect.x + rect.width.saturating_sub(right_width),
                rect.y,
                right_width,
                1,
            ),
        );
        if gap > 0 {
            let gap_x = rect.x + rect.width.saturating_sub(right_width + gap);
            frame.buffer_mut()[(gap_x, rect.y)].set_style(right_style);
        }
    }
}

pub(crate) fn settings_sidebar_space_demo_lines(app: &AppState) -> Vec<Line<'static>> {
    let p = &app.palette;
    let (status, status_style) = state_dot(AgentState::Working, true, p);
    let branch = "feature/sidebar";
    let branch_status = branch_status_parts(Some((2, 1)), p);
    let separator_style = Style::default().fg(p.overlay0);
    let name_style = Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD);
    let branch_style = Style::default().fg(p.mauve);
    let mut lines = Vec::new();

    for line in (0..app.sidebar_space.lines.len().max(1)).map(SidebarLine::from_index) {
        let mut content = Vec::new();
        let mut previous_was_status = false;
        for item in ordered_sidebar_space_items(&app.sidebar_space)
            .into_iter()
            .filter(|item| item.line(&app.sidebar_space) == line)
            .filter(|item| item.enabled(&app.sidebar_space))
        {
            match item {
                SidebarSpaceItem::Status => {
                    push_separator(&mut content, separator_style);
                    let item_style = style_with_sidebar_color(
                        status_style,
                        item.color(&app.sidebar_space),
                        status_style.fg.unwrap_or(p.accent),
                        p,
                    );
                    content.push(Span::styled(status, item_style));
                    previous_was_status = true;
                }
                SidebarSpaceItem::Name => {
                    push_space_separator(&mut content, &mut previous_was_status, separator_style);
                    let item_style = style_with_sidebar_color(
                        name_style,
                        item.color(&app.sidebar_space),
                        name_style.fg.unwrap_or(p.subtext0),
                        p,
                    );
                    content.push(Span::styled("demo-space", item_style));
                }
                SidebarSpaceItem::Branch => {
                    push_space_separator(&mut content, &mut previous_was_status, separator_style);
                    let item_style = style_with_sidebar_color(
                        branch_style,
                        item.color(&app.sidebar_space),
                        branch_style.fg.unwrap_or(p.mauve),
                        p,
                    );
                    content.push(Span::styled(branch, item_style));
                }
                SidebarSpaceItem::BranchStatus => {
                    if let Some(parts) = branch_status.clone() {
                        push_space_separator(
                            &mut content,
                            &mut previous_was_status,
                            separator_style,
                        );
                        for (idx, (label, color)) in parts.into_iter().enumerate() {
                            if idx > 0 {
                                content.push(Span::styled(" ", Style::default()));
                            }
                            content.push(Span::styled(
                                label,
                                Style::default()
                                    .fg(p.sidebar_color(item.color(&app.sidebar_space), color)),
                            ));
                        }
                    }
                }
            }
        }
        if !content.is_empty() {
            let mut spans = vec![Span::styled("  ", Style::default())];
            spans.extend(content);
            lines.push(Line::from(spans));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  all demo space fields hidden",
            Style::default().fg(p.overlay1),
        )));
    }
    lines
}

pub(crate) fn settings_sidebar_agent_demo_width(app: &AppState) -> u16 {
    app.view.sidebar_rect.width.max(app.sidebar_width).max(1)
}

pub(crate) fn settings_sidebar_agent_demo_lines(app: &AppState, width: u16) -> Vec<Line<'static>> {
    let p = &app.palette;
    let demos = [
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            pane_label: Some("pane-alpha".into()),
            primary_label: "workspace-app".into(),
            primary_tab_label: Some("main".into()),
            agent_label: Some("claude".into()),
            state: AgentState::Working,
            seen: true,
            custom_status: Some("planning".into()),
            working_duration: Some(WorkingDuration {
                elapsed: Duration::from_secs(92),
                is_live: true,
            }),
        },
        AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 1,
            pane_id: crate::layout::PaneId::from_raw(2),
            pane_label: Some("pane-beta".into()),
            primary_label: "workspace-tests".into(),
            primary_tab_label: Some("review".into()),
            agent_label: Some("codex".into()),
            state: AgentState::Idle,
            seen: true,
            custom_status: Some("ready".into()),
            working_duration: Some(WorkingDuration {
                elapsed: Duration::from_secs(8),
                is_live: false,
            }),
        },
    ];
    let render_lines = sidebar_agent_render_lines(app);
    let mut lines = Vec::new();

    for entry in demos {
        let label_color = state_label_color(entry.state, entry.seen, p);
        let name_style = Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD);
        let status_style = Style::default().fg(label_color);
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
        for (render_idx, line) in render_lines.iter().copied().enumerate() {
            let prefix = if render_idx == 0 { " " } else { "   " };
            let mut content = sidebar_agent_line_content(
                &entry,
                app,
                line,
                width.saturating_sub(prefix.len() as u16) as usize,
                name_style,
                status_style,
                agent_style,
                p,
            );
            content
                .left
                .insert(0, Span::styled(prefix, Style::default()));
            lines.push(Line::from(fixed_width_agent_line_spans(
                content,
                width,
                agent_style,
            )));
        }
    }

    lines
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_scope);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_toggle_label(app.agent_panel_scope),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let render_lines = sidebar_agent_render_lines(app);
    for detail in details.iter().skip(app.agent_panel_scroll) {
        let entry_rows = render_lines.len() as u16;
        if row_y.saturating_add(entry_rows) > body_bottom {
            break;
        }

        let is_active = app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id);

        let label_color = state_label_color(detail.state, detail.seen, p);

        let row_style = if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };

        let name_style = if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        let status_style = if is_active {
            Style::default().fg(label_color)
        } else {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        };
        let agent_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);

        for (render_idx, line) in render_lines.iter().copied().enumerate() {
            let prefix = if render_idx == 0 { " " } else { "   " };
            let mut content = sidebar_agent_line_content(
                detail,
                app,
                line,
                body.width.saturating_sub(prefix.len() as u16) as usize,
                name_style,
                status_style,
                agent_style,
                p,
            );
            content
                .left
                .insert(0, Span::styled(prefix, Style::default()));

            render_agent_line(
                frame,
                Rect::new(body.x, row_y, body.width, 1),
                content,
                row_style,
                agent_style,
            );
            row_y += 1;
        }

        if row_y < body_bottom {
            row_y += 1;
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    if !collapsed {
        return;
    }
    let toggle_area = collapsed_sidebar_toggle_rect(area);
    if toggle_area == Rect::default() {
        return;
    }
    let icon_style = if app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled("»", icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};

    fn agent_entry_row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn compact_duration_text(duration: Duration) -> String {
        compact_duration_parts(duration)
            .into_iter()
            .map(|(value, unit)| format!("{value}{unit}"))
            .collect()
    }

    fn cell_col_for_substr(row: &str, needle: &str) -> u16 {
        row.find(needle)
            .map(|byte_idx| row[..byte_idx].chars().count() as u16)
            .expect("substring should render")
    }

    fn root_terminal_id(app: &AppState) -> crate::terminal::TerminalId {
        let pane = app.workspaces[0].tabs[0].root_pane;
        app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone()
    }

    fn working_agent_app() -> AppState {
        working_agent_app_at(std::time::Instant::now() - Duration::from_secs(132))
    }

    fn working_agent_app_at(start: std::time::Instant) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("herdr")];
        app.ensure_test_terminals();
        let terminal_id = root_terminal_id(&app);
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            start,
        );
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;
        app
    }

    fn render_agent_detail_row(app: &AppState, width: u16, height: u16, row: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, width, height),
                )
            })
            .unwrap();
        agent_entry_row_text(terminal.backend().buffer(), row, width)
    }

    #[test]
    fn truncate_text_respects_display_width() {
        assert_eq!(truncate_text("workspace", 6), "works…");
        assert_eq!(truncate_text("작업중", 5), "작업…");
        assert!(UnicodeWidthStr::width(truncate_text("작업중", 4).as_str()) <= 4);
    }

    #[test]
    fn primary_label_segments_measure_display_width() {
        let entry = AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            pane_label: Some("작업중".into()),
            primary_label: "공간".into(),
            primary_tab_label: Some("검토".into()),
            agent_label: Some("codex".into()),
            state: AgentState::Idle,
            seen: true,
            custom_status: None,
            working_duration: None,
        };

        let label = format_agent_panel_primary_label_with_options(&entry, 10, true, true);

        assert!(UnicodeWidthStr::width(label.as_str()) <= 10);
        assert!(label.contains("…"));
    }

    #[test]
    fn all_workspaces_agent_panel_entries_use_workspace_and_optional_tab_labels() {
        let mut app = crate::app::state::AppState::test_new();
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_tab = second.test_add_tab(Some("logs"));
        let second_pane = second.tabs[second_tab].root_pane;

        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        let second_terminal_id = app.workspaces[1].tabs[second_tab].panes[&second_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "one");
        assert!(entries[0].primary_tab_label.is_none());
        assert_eq!(entries[0].agent_label.as_deref(), Some("pi"));
        assert_eq!(entries[1].primary_label, "two");
        assert_eq!(entries[1].primary_tab_label.as_deref(), Some("logs"));
        assert_eq!(entries[1].agent_label.as_deref(), Some("claude"));
    }

    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            "/bin/sh",
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn settings_agent_demo_does_not_render_custom_status_outside_configured_fields() {
        let mut app = crate::app::state::AppState::test_new();
        for item in crate::app::state::SIDEBAR_AGENT_ITEMS {
            item.set_enabled(&mut app.sidebar_agent, false);
        }
        crate::app::state::SidebarAgentItem::Status.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::AgentName.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, false);

        let rendered = settings_sidebar_agent_demo_lines(&app, 32)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("working · claude"), "{rendered:?}");
        assert!(rendered.contains("idle · codex"), "{rendered:?}");
        assert!(!rendered.contains("planning"), "{rendered:?}");
        assert!(!rendered.contains("ready"), "{rendered:?}");
    }

    #[test]
    fn sidebar_agent_custom_status_follows_right_alignment_marker() {
        let mut app = crate::app::state::AppState::test_new();
        let config: crate::config::Config = toml::from_str(
            r#"
[ui.sidebar.agents]
lines = [
  [
    { field = "agent_status", show = false },
    { field = "pane_name", show = false },
    { field = "tab_name", show = false },
    { field = "space_name", show = false },
    { field = "status", show = true },
    { field = "time", show = false },
    { field = "right_alignment", show = true },
    { field = "custom_status", show = true },
    { field = "agent_name", show = true },
  ],
]
"#,
        )
        .unwrap();
        app.sidebar_agent = config.ui.sidebar.agents;
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority_with_custom_status(
                "test".into(),
                "codex".into(),
                AgentState::Working,
                None,
                Some("planning".into()),
                None,
            );
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(42, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 42, 8),
                )
            })
            .unwrap();
        let row = agent_entry_row_text(terminal.backend().buffer(), 3, 42);

        assert!(row.contains(" working"), "row: {row:?}");
        assert!(row.ends_with("planning · codex"), "row: {row:?}");
    }

    #[test]
    fn sidebar_agent_color_preset_overrides_configured_item_color() {
        let mut app = crate::app::state::AppState::test_new();
        crate::app::state::SidebarAgentItem::AgentName.set_color(
            &mut app.sidebar_agent,
            crate::config::SidebarColorPreset::Cool,
        );
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority_with_custom_status(
                "test".into(),
                "codex".into(),
                AgentState::Idle,
                None,
                None,
                None,
            );
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(42, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 42, 8),
                )
            })
            .unwrap();
        let row = agent_entry_row_text(terminal.backend().buffer(), 4, 42);
        let codex_col = row
            .find("codex")
            .unwrap_or_else(|| panic!("codex should render in row: {row:?}"))
            as u16;

        assert_eq!(
            terminal.backend().buffer()[(codex_col, 4)].fg,
            app.palette.teal
        );
    }

    #[test]
    fn all_workspaces_primary_label_without_pane_label_uses_tab_and_workspace_when_tab_exists() {
        let entry = AgentPanelEntry {
            ws_idx: 0,
            tab_idx: 0,
            pane_id: crate::layout::PaneId::from_raw(1),
            pane_label: None,
            primary_label: "agent-browser".into(),
            primary_tab_label: Some("test-escalation".into()),
            agent_label: Some("claude".into()),
            state: AgentState::Idle,
            seen: true,
            custom_status: None,
            working_duration: None,
        };

        let label = format_agent_panel_primary_label(&entry, 23);

        assert_eq!(label, "test-e… · agent-browser");
    }

    #[test]
    fn named_pane_primary_label_uses_pane_and_workspace_for_single_tab_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        let label = format_agent_panel_primary_label(&entries[0], 30);

        assert_eq!(label, "Echo · herdr");
    }

    #[test]
    fn named_pane_primary_label_includes_tab_context_for_multi_tab_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        let label = format_agent_panel_primary_label(&entries[0], 30);

        assert_eq!(label, "Echo · main · herdr");
    }

    #[test]
    fn agent_primary_label_can_hide_pane_and_tab_segments_independently() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);

        assert_eq!(
            format_agent_panel_primary_label_with_options(&entries[0], 30, false, true),
            "main · herdr"
        );
        assert_eq!(
            format_agent_panel_primary_label_with_options(&entries[0], 30, true, false),
            "Echo · herdr"
        );
        assert_eq!(
            format_agent_panel_primary_label_with_options(&entries[0], 30, false, false),
            "herdr"
        );
    }

    #[test]
    fn primary_label_truncates_pane_side_before_workspace_label() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("EchoLongName".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        let label = format_agent_panel_primary_label(&entries[0], 16);

        assert_eq!(label, "EchoLon… · herdr");
    }

    #[test]
    fn missing_pane_label_keeps_workspace_only_primary_label() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Codex);
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        let label = format_agent_panel_primary_label(&entries[0], 30);

        assert_eq!(label, "herdr");
    }

    #[test]
    fn missing_pane_label_in_multi_tab_workspace_uses_tab_and_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Codex);
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let entries = agent_panel_entries(&app);
        let label = format_agent_panel_primary_label(&entries[0], 30);

        assert_eq!(label, "main · herdr");
    }

    #[test]
    fn named_pane_primary_label_segments_use_pane_tab_and_workspace_colors() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = agent_entry_row_text(buffer, 3, 40);

        assert!(row.contains("Echo · main · herdr"), "row: {row:?}");
        assert_eq!(buffer[(3, 3)].fg, app.palette.green);
        assert_eq!(buffer[(10, 3)].fg, app.palette.mauve);
        assert_eq!(buffer[(17, 3)].fg, app.palette.subtext0);
    }

    #[test]
    fn truncated_multi_tab_primary_label_keeps_tab_and_workspace_segment_colors() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("EchoLongName".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(24, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 24, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = agent_entry_row_text(buffer, 3, 24);
        let tab_col = cell_col_for_substr(&row, "main");
        let workspace_col = cell_col_for_substr(&row, "herdr");

        assert!(row.contains("EchoL… · main · herdr"), "row: {row:?}");
        assert_eq!(buffer[(tab_col, 3)].fg, app.palette.mauve);
        assert_eq!(buffer[(workspace_col, 3)].fg, app.palette.subtext0);
    }

    #[test]
    fn status_duration_formats_compact_seconds_minutes_and_hours() {
        assert_eq!(compact_duration_text(Duration::ZERO), "1s");
        assert_eq!(compact_duration_text(Duration::from_secs(10)), "10s");
        assert_eq!(compact_duration_text(Duration::from_secs(131)), "2m11s");
        assert_eq!(compact_duration_text(Duration::from_secs(4_800)), "1h20m");
    }

    #[test]
    fn duration_unit_styles_keep_seconds_unit_as_visible_as_number() {
        let palette = Palette::catppuccin();
        let live = WorkingDuration {
            elapsed: Duration::from_secs(4_800),
            is_live: true,
        };
        let stale = WorkingDuration {
            elapsed: Duration::from_secs(4_800),
            is_live: false,
        };

        assert_eq!(
            duration_unit_style("h", live, &palette).fg,
            Some(palette.subtext0)
        );
        assert_eq!(
            duration_unit_style("m", live, &palette).fg,
            Some(palette.overlay1)
        );
        assert_eq!(
            duration_unit_style("s", live, &palette).fg,
            duration_number_style(live, &palette).fg
        );
        assert_eq!(
            duration_unit_style("h", stale, &palette),
            duration_number_style(stale, &palette)
        );
        assert_eq!(
            duration_unit_style("m", stale, &palette),
            duration_number_style(stale, &palette)
        );
        assert_eq!(
            duration_unit_style("s", stale, &palette),
            duration_number_style(stale, &palette)
        );
        assert!(duration_unit_style("h", stale, &palette)
            .add_modifier
            .contains(Modifier::DIM));
    }

    #[test]
    fn status_row_keeps_agent_label_right_aligned_with_working_duration() {
        let app = working_agent_app();
        let row = render_agent_detail_row(&app, 40, 8, 4);

        assert!(row.contains("   working · 2m12s"), "status row: {row:?}");
        assert!(row.ends_with("codex"), "status row: {row:?}");
    }

    #[test]
    fn sidebar_agent_can_hide_time_without_disabling_right_alignment() {
        let mut app = working_agent_app();
        crate::app::state::SidebarAgentItem::Time.set_enabled(&mut app.sidebar_agent, false);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, true);
        let terminal_id = root_terminal_id(&app);
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_manual_label("Echo".into());
        let status_row = render_agent_detail_row(&app, 40, 8, 4);

        assert!(
            status_row.contains("   working"),
            "status row: {status_row:?}"
        );
        assert!(!status_row.contains("2m12s"), "status row: {status_row:?}");
        assert!(status_row.ends_with("codex"), "status row: {status_row:?}");
    }

    #[test]
    fn sidebar_agent_can_disable_right_alignment_without_hiding_time() {
        let mut app = working_agent_app();
        crate::app::state::SidebarAgentItem::Time.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, false);
        let status_row = render_agent_detail_row(&app, 40, 8, 4);

        assert!(
            status_row.contains("   working · 2m12s · codex"),
            "status row: {status_row:?}"
        );
    }

    #[test]
    fn sidebar_agent_right_alignment_marker_aligns_items_after_it() {
        let mut app = working_agent_app();
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::RightAlignment.set_order(&mut app.sidebar_agent, 1);
        crate::app::state::SidebarAgentItem::Time.set_order(&mut app.sidebar_agent, 2);
        crate::app::state::SidebarAgentItem::AgentName.set_order(&mut app.sidebar_agent, 3);
        let status_row = render_agent_detail_row(&app, 40, 8, 4);

        assert!(
            status_row.contains("   working"),
            "status row: {status_row:?}"
        );
        assert!(
            status_row.ends_with("2m12s · codex"),
            "status row: {status_row:?}"
        );
    }

    #[test]
    fn sidebar_agent_line_order_can_move_time_to_first_row() {
        let mut app = working_agent_app();
        crate::app::state::SidebarAgentItem::AgentStatus.set_enabled(&mut app.sidebar_agent, false);
        crate::app::state::SidebarAgentItem::Time.set_line(
            &mut app.sidebar_agent,
            crate::app::state::SidebarLine::First,
        );
        crate::app::state::SidebarAgentItem::Time.set_order(&mut app.sidebar_agent, 0);
        crate::app::state::SidebarAgentItem::PaneName.set_order(&mut app.sidebar_agent, 1);
        crate::app::state::SidebarAgentItem::TabName.set_order(&mut app.sidebar_agent, 2);
        crate::app::state::SidebarAgentItem::SpaceName.set_order(&mut app.sidebar_agent, 3);
        let first_row = render_agent_detail_row(&app, 40, 8, 3);

        assert!(first_row.contains("2m12s · herdr"), "row: {first_row:?}");
    }

    #[test]
    fn sidebar_agent_reorders_identity_items_in_agents_panel() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        crate::app::state::SidebarAgentItem::TabName.set_order(&mut app.sidebar_agent, 1);
        crate::app::state::SidebarAgentItem::PaneName.set_order(&mut app.sidebar_agent, 2);
        crate::app::state::SidebarAgentItem::SpaceName.set_order(&mut app.sidebar_agent, 3);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let first_row = agent_entry_row_text(terminal.backend().buffer(), 3, 40);

        assert!(
            first_row.contains("main · Echo · herdr"),
            "row: {first_row:?}"
        );
    }

    #[test]
    fn sidebar_agent_can_move_pane_name_to_second_row() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.tabs[0].custom_name = Some("main".into());
        let pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("2"));

        crate::app::state::SidebarAgentItem::PaneName.set_line(
            &mut app.sidebar_agent,
            crate::app::state::SidebarLine::Second,
        );
        crate::app::state::SidebarAgentItem::PaneName.set_order(&mut app.sidebar_agent, 3);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.sidebar_agent, false);
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_manual_label("Echo".into());
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = agent_entry_row_text(buffer, 3, 40);
        let second_row = agent_entry_row_text(buffer, 4, 40);

        assert!(!first_row.contains("Echo"), "row: {first_row:?}");
        assert!(second_row.contains("Echo"), "row: {second_row:?}");
    }

    #[test]
    fn sidebar_agent_can_hide_and_move_agent_status_indicator() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        app.agent_panel_scope = AgentPanelScope::AllWorkspaces;

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let first_row = agent_entry_row_text(terminal.backend().buffer(), 3, 40);
        assert!(first_row.contains("✓ herdr"), "row: {first_row:?}");

        crate::app::state::SidebarAgentItem::AgentStatus.set_enabled(&mut app.sidebar_agent, false);
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let first_row = agent_entry_row_text(terminal.backend().buffer(), 3, 40);
        assert!(!first_row.contains("✓"), "row: {first_row:?}");

        crate::app::state::SidebarAgentItem::AgentStatus.set_enabled(&mut app.sidebar_agent, true);
        crate::app::state::SidebarAgentItem::AgentStatus.set_line(
            &mut app.sidebar_agent,
            crate::app::state::SidebarLine::Second,
        );
        crate::app::state::SidebarAgentItem::AgentStatus.set_order(&mut app.sidebar_agent, 0);
        crate::app::state::SidebarAgentItem::Status.set_order(&mut app.sidebar_agent, 1);
        crate::app::state::SidebarAgentItem::Time.set_order(&mut app.sidebar_agent, 2);
        crate::app::state::SidebarAgentItem::AgentName.set_order(&mut app.sidebar_agent, 3);
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = agent_entry_row_text(buffer, 3, 40);
        let second_row = agent_entry_row_text(buffer, 4, 40);

        assert!(!first_row.contains("✓"), "row: {first_row:?}");
        assert!(second_row.contains("✓ idle"), "row: {second_row:?}");
    }

    #[test]
    fn sidebar_agent_renders_one_configured_line_from_config_model() {
        let mut app = working_agent_app();
        app.sidebar_agent.lines = vec![vec![
            crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::AgentStatus),
            crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::SpaceName),
            crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::Status),
            crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::Time),
            crate::config::SidebarItem::visible(
                crate::app::state::SidebarAgentItem::RightAlignment,
            ),
            crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::AgentName),
        ]];
        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = agent_entry_row_text(buffer, 3, 40);
        let second_row = agent_entry_row_text(buffer, 4, 40);

        assert!(first_row.contains("herdr"), "row: {first_row:?}");
        assert!(first_row.contains("working"), "row: {first_row:?}");
        assert!(first_row.ends_with("codex"), "row: {first_row:?}");
        assert!(!second_row.contains("herdr"), "row: {second_row:?}");
        assert!(!second_row.contains("working"), "row: {second_row:?}");
        assert!(!second_row.contains("codex"), "row: {second_row:?}");
    }

    #[test]
    fn sidebar_agent_renders_three_configured_lines_from_config_model() {
        let mut app = working_agent_app();
        app.sidebar_agent.lines = vec![
            vec![
                crate::config::SidebarItem::visible(
                    crate::app::state::SidebarAgentItem::AgentStatus,
                ),
                crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::SpaceName),
            ],
            vec![
                crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::Status),
                crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::Time),
            ],
            vec![
                crate::config::SidebarItem::visible(
                    crate::app::state::SidebarAgentItem::RightAlignment,
                ),
                crate::config::SidebarItem::visible(crate::app::state::SidebarAgentItem::AgentName),
            ],
        ];
        let backend = ratatui::backend::TestBackend::new(40, 9);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 9),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = agent_entry_row_text(buffer, 3, 40);
        let second_row = agent_entry_row_text(buffer, 4, 40);
        let third_row = agent_entry_row_text(buffer, 5, 40);

        assert!(first_row.contains("herdr"), "row: {first_row:?}");
        assert!(
            second_row.contains("working · 2m12s"),
            "row: {second_row:?}"
        );
        assert!(third_row.ends_with("codex"), "row: {third_row:?}");
    }

    #[test]
    fn sidebar_space_can_hide_branch_status_without_hiding_branch() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.cached_git_branch = Some("main".into());
        workspace.cached_git_ahead_behind = Some((2, 1));
        app.workspaces = vec![workspace];
        crate::app::state::SidebarSpaceItem::Branch.set_enabled(&mut app.sidebar_space, true);
        crate::app::state::SidebarSpaceItem::BranchStatus
            .set_enabled(&mut app.sidebar_space, false);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 2),
            indented: false,
        }];

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let branch_row = agent_entry_row_text(terminal.backend().buffer(), 3, 40);

        assert!(branch_row.contains("main"), "branch row: {branch_row:?}");
        assert!(!branch_row.contains("↑2"), "branch row: {branch_row:?}");
        assert!(!branch_row.contains("↓1"), "branch row: {branch_row:?}");
    }

    #[test]
    fn sidebar_space_renders_one_row_when_config_places_all_items_on_one_line() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.cached_git_branch = Some("main".into());
        workspace.cached_git_ahead_behind = Some((2, 1));
        app.sidebar_space.lines = vec![vec![
            crate::config::SidebarItem::visible(crate::app::state::SidebarSpaceItem::Status),
            crate::config::SidebarItem::visible(crate::app::state::SidebarSpaceItem::Name),
            crate::config::SidebarItem::visible(crate::app::state::SidebarSpaceItem::Branch),
            crate::config::SidebarItem::visible(crate::app::state::SidebarSpaceItem::BranchStatus),
        ]];
        app.workspaces = vec![workspace];
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 1),
            indented: false,
        }];

        assert_eq!(workspace_row_height(&app, &app.workspaces[0]), 1);

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let row = agent_entry_row_text(terminal.backend().buffer(), 2, 40);

        assert!(row.contains("herdr"), "row: {row:?}");
        assert!(row.contains("main"), "row: {row:?}");
        assert!(row.contains("↑2"), "row: {row:?}");
    }

    #[test]
    fn sidebar_space_line_order_can_move_branch_to_first_row() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.cached_git_branch = Some("main".into());
        app.workspaces = vec![workspace];
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Branch.set_line(
            &mut app.sidebar_space,
            crate::app::state::SidebarLine::First,
        );
        crate::app::state::SidebarSpaceItem::Branch.set_order(&mut app.sidebar_space, 0);
        crate::app::state::SidebarSpaceItem::Name.set_order(&mut app.sidebar_space, 1);
        crate::app::state::SidebarSpaceItem::BranchStatus
            .set_enabled(&mut app.sidebar_space, false);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 1),
            indented: false,
        }];

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let row = agent_entry_row_text(terminal.backend().buffer(), 2, 40);

        assert!(row.contains("main · herdr"), "row: {row:?}");
    }

    #[test]
    fn sidebar_space_can_hide_status_without_hiding_name() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("herdr");
        let pane = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Codex);
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Name.set_enabled(&mut app.sidebar_space, true);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 1),
            indented: false,
        }];

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let name_row = agent_entry_row_text(terminal.backend().buffer(), 2, 40);

        assert!(name_row.contains("herdr"), "name row: {name_row:?}");
        assert!(!name_row.contains("○"), "name row: {name_row:?}");
    }

    #[test]
    fn sidebar_space_preserves_blank_first_row_when_first_line_items_hidden() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.cached_git_branch = Some("main".into());
        app.workspaces = vec![workspace];
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Name.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Branch.set_enabled(&mut app.sidebar_space, true);
        crate::app::state::SidebarSpaceItem::BranchStatus
            .set_enabled(&mut app.sidebar_space, false);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 2),
            indented: false,
        }];

        assert_eq!(workspace_row_height(&app, &app.workspaces[0]), 2);

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = agent_entry_row_text(buffer, 2, 40);
        let second_row = agent_entry_row_text(buffer, 3, 40);

        assert!(!first_row.contains("main"), "first row: {first_row:?}");
        assert!(second_row.contains("main"), "second row: {second_row:?}");
    }

    #[test]
    fn sidebar_space_can_hide_space_name_and_branch() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("herdr");
        workspace.cached_git_branch = Some("main".into());
        app.workspaces = vec![workspace];
        crate::app::state::SidebarSpaceItem::Name.set_enabled(&mut app.sidebar_space, false);
        crate::app::state::SidebarSpaceItem::Branch.set_enabled(&mut app.sidebar_space, false);
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            rect: Rect::new(0, 2, 40, 1),
            indented: false,
        }];

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_workspace_list(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                    false,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let name_row = agent_entry_row_text(buffer, 2, 40);
        let branch_row = agent_entry_row_text(buffer, 3, 40);

        assert!(!name_row.contains("herdr"), "name row: {name_row:?}");
        assert!(!branch_row.contains("main"), "branch row: {branch_row:?}");
    }

    #[test]
    fn stale_status_duration_is_dimmed_after_working_finishes() {
        let start = std::time::Instant::now() - Duration::from_secs(132);
        let mut app = working_agent_app_at(start);
        let terminal_id = root_terminal_id(&app);
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            start + Duration::from_secs(132),
        );

        let backend = ratatui::backend::TestBackend::new(40, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_agent_detail(
                    &app,
                    &TerminalRuntimeRegistry::new(),
                    frame,
                    Rect::new(0, 0, 40, 8),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = agent_entry_row_text(buffer, 4, 40);
        let duration_col = row.find("2m12s").expect("duration should render") as u16;

        assert!(
            buffer[(duration_col, 4)].modifier.contains(Modifier::DIM),
            "duration cell should be dimmed in row: {row:?}"
        );
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_offset_can_start_inside_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }
}
