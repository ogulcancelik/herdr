//! Filesystem panel — an independent file tree rendered at the bottom of the
//! sidebar. Clicking a folder toggles it; clicking a file opens it in a pane.

use std::path::{Path, PathBuf};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use crate::app::state::{AppState, FilesRowArea, Mode};

/// Header rows reserved at the top of the files panel (separator + title).
pub(crate) const FILES_PANEL_HEADER_ROWS: u16 = 2;

/// A directory entry returned by a [`DirReader`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Reads a single directory's immediate children. Abstracted so the tree
/// flattening logic can be unit-tested without touching the filesystem.
pub(crate) trait DirReader {
    fn read_dir(&self, path: &Path) -> Vec<FileEntry>;
}

/// Real filesystem reader: hides dotfiles and noisy build dirs, and sorts
/// directories first, then alphabetically (case-insensitive).
pub(crate) struct FsDirReader;

fn is_noise(name: &str) -> bool {
    name.starts_with('.') || matches!(name, "node_modules" | "target")
}

impl DirReader for FsDirReader {
    fn read_dir(&self, path: &Path) -> Vec<FileEntry> {
        let Ok(read) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut entries: Vec<FileEntry> = read
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if is_noise(&name) {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(FileEntry {
                    name,
                    path: e.path(),
                    is_dir,
                })
            })
            .collect();
        sort_entries(&mut entries);
        entries
    }
}

pub(crate) fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// One flattened, visible node in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: u16,
}

/// Flatten the tree under `root` into the visible rows, descending only into
/// directories present in `expanded`. Pure: filesystem access is via `reader`.
pub(crate) fn flatten_visible<R: DirReader>(
    root: &Path,
    reader: &R,
    expanded: &std::collections::HashSet<PathBuf>,
) -> Vec<FlatNode> {
    let mut out = Vec::new();
    flatten_into(root, 0, reader, expanded, &mut out);
    out
}

fn flatten_into<R: DirReader>(
    dir: &Path,
    depth: u16,
    reader: &R,
    expanded: &std::collections::HashSet<PathBuf>,
    out: &mut Vec<FlatNode>,
) {
    // Guard against pathologically deep trees from a runaway expanded set.
    if depth > 32 {
        return;
    }
    for entry in reader.read_dir(dir) {
        let is_dir = entry.is_dir;
        let path = entry.path.clone();
        out.push(FlatNode {
            path: path.clone(),
            name: entry.name,
            is_dir,
            depth,
        });
        if is_dir && expanded.contains(&path) {
            flatten_into(&path, depth + 1, reader, expanded, out);
        }
    }
}

/// Directory the panel shows: the focused pane's current working directory in
/// the relevant workspace (selected while navigating, otherwise active), so it
/// follows the user as they switch panes or `cd`. Falls back to the workspace
/// identity when the focused pane hasn't reported a cwd yet.
fn files_root(app: &AppState) -> Option<PathBuf> {
    let idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    }?;
    let ws = app.workspaces.get(idx)?;
    let tab = ws.active_tab()?;
    tab.cwd_for_pane(tab.layout.focused(), &app.terminals, &app.terminal_runtimes)
        .or_else(|| ws.resolved_identity_cwd_from(&app.terminals, &app.terminal_runtimes))
}

pub(crate) fn files_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= FILES_PANEL_HEADER_ROWS {
        return Rect::default();
    }
    let body_y = area.y.saturating_add(FILES_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

/// Compute the visible, hit-testable rows for the files panel given the full
/// sidebar rect. Called once per render and cached on `ViewState`.
pub(crate) fn compute_files_rows(app: &AppState, sidebar_area: Rect) -> Vec<FilesRowArea> {
    let (_, _, files_area) = super::sidebar::expanded_sidebar_sections(
        sidebar_area,
        app.sidebar_section_split,
        app.files_section_split,
    );
    files_rows_for_area(app, files_area, &FsDirReader)
}

pub(crate) fn files_rows_for_area<R: DirReader>(
    app: &AppState,
    files_area: Rect,
    reader: &R,
) -> Vec<FilesRowArea> {
    if files_area.height <= FILES_PANEL_HEADER_ROWS || files_area.width == 0 {
        return Vec::new();
    }
    let Some(root) = files_root(app) else {
        return Vec::new();
    };
    let nodes = flatten_visible(&root, reader, &app.files_expanded);
    let body = files_panel_body_rect(files_area, false);
    if body.height == 0 {
        return Vec::new();
    }

    let body_bottom = body.y + body.height;
    (body.y..body_bottom)
        .zip(nodes.into_iter().skip(app.files_scroll))
        .map(|(y, node)| FilesRowArea {
            path: node.path,
            is_dir: node.is_dir,
            depth: node.depth,
            rect: Rect::new(body.x, y, body.width, 1),
        })
        .collect()
}

pub(crate) fn files_panel_scroll_metrics(
    app: &AppState,
    files_area: Rect,
) -> crate::pane::ScrollMetrics {
    let body = files_panel_body_rect(files_area, false);
    let viewport_rows = body.height as usize;
    let total_rows = match files_root(app) {
        Some(root) => flatten_visible(&root, &FsDirReader, &app.files_expanded).len(),
        None => 0,
    };
    let max_offset_from_bottom = total_rows.saturating_sub(viewport_rows);
    let offset_from_bottom = total_rows
        .saturating_sub(app.files_scroll)
        .saturating_sub(viewport_rows);
    crate::pane::ScrollMetrics {
        offset_from_bottom,
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn files_panel_scrollbar_rect(app: &AppState, files_area: Rect) -> Option<Rect> {
    let metrics = files_panel_scroll_metrics(app, files_area);
    let body = files_panel_body_rect(files_area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        files_area.x + files_area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(super) fn render_files_panel(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.height <= FILES_PANEL_HEADER_ROWS || area.width == 0 {
        return;
    }
    let p = &app.palette;

    let dragging = matches!(
        app.drag.as_ref().map(|d| &d.target),
        Some(crate::app::state::DragTarget::FilesSectionDivider)
    );
    super::sidebar::render_section_divider(
        frame,
        Rect::new(area.x, area.y, area.width, 1),
        dragging,
        p,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " files",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );

    let metrics = files_panel_scroll_metrics(app, area);
    let scrollbar_rect = files_panel_scrollbar_rect(app, area);

    for row in &app.view.files_rows {
        let indent = " ".repeat(row.depth as usize * 2);
        let glyph = if row.is_dir {
            if app.files_expanded.contains(&row.path) {
                "▾ "
            } else {
                "▸ "
            }
        } else {
            "  "
        };
        let name = row
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name_color = if row.is_dir { p.subtext0 } else { p.overlay0 };
        let avail = (row.rect.width as usize).saturating_sub(indent.len() + 2);
        let shown = if name.chars().count() > avail && avail > 1 {
            format!("{}…", name.chars().take(avail - 1).collect::<String>())
        } else {
            name
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(indent, Style::default()),
                Span::styled(
                    glyph,
                    Style::default().fg(if row.is_dir { p.accent } else { p.overlay0 }),
                ),
                Span::styled(shown, Style::default().fg(name_color)),
            ])),
            row.rect,
        );
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct FakeFs(std::collections::HashMap<PathBuf, Vec<FileEntry>>);

    impl DirReader for FakeFs {
        fn read_dir(&self, path: &Path) -> Vec<FileEntry> {
            self.0.get(path).cloned().unwrap_or_default()
        }
    }

    fn entry(parent: &str, name: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            path: PathBuf::from(parent).join(name),
            is_dir,
        }
    }

    #[test]
    fn collapsed_root_lists_only_top_level() {
        let mut map = std::collections::HashMap::new();
        map.insert(
            PathBuf::from("/r"),
            vec![entry("/r", "src", true), entry("/r", "a.txt", false)],
        );
        map.insert(
            PathBuf::from("/r/src"),
            vec![entry("/r/src", "lib.rs", false)],
        );
        let fs = FakeFs(map);

        let nodes = flatten_visible(Path::new("/r"), &fs, &HashSet::new());

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "src");
        assert_eq!(nodes[0].depth, 0);
        assert_eq!(nodes[1].name, "a.txt");
    }

    #[test]
    fn expanded_dir_descends_with_depth() {
        let mut map = std::collections::HashMap::new();
        map.insert(PathBuf::from("/r"), vec![entry("/r", "src", true)]);
        map.insert(
            PathBuf::from("/r/src"),
            vec![entry("/r/src", "lib.rs", false)],
        );
        let fs = FakeFs(map);
        let mut expanded = HashSet::new();
        expanded.insert(PathBuf::from("/r/src"));

        let nodes = flatten_visible(Path::new("/r"), &fs, &expanded);

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].name, "lib.rs");
        assert_eq!(nodes[1].depth, 1);
    }

    #[test]
    fn sort_puts_dirs_first_then_alpha() {
        let mut v = vec![
            entry("/r", "Zed", false),
            entry("/r", "alpha", true),
            entry("/r", "beta", false),
        ];
        sort_entries(&mut v);
        assert_eq!(v[0].name, "alpha");
        assert_eq!(v[1].name, "beta");
        assert_eq!(v[2].name, "Zed");
    }

    #[test]
    fn body_rect_excludes_header_and_scrollbar() {
        let area = Rect::new(0, 10, 20, 8);
        let body = files_panel_body_rect(area, true);
        assert_eq!(body, Rect::new(0, 12, 19, 6));
    }
}
