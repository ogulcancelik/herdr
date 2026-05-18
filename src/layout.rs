//! BSP tree layout for tiling panes within a workspace.

use ratatui::layout::{Direction, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PaneId(u32);

/// Global atomic counter for unique PaneId generation across all workspaces.
static NEXT_PANE_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl PaneId {
    /// Allocate a globally unique PaneId.
    pub fn alloc() -> Self {
        Self(NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    pub fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct from a saved u32 (persistence only).
    pub fn from_raw(id: u32) -> Self {
        Self(id)
    }
}

/// Snapshot of a pane's position and focus state after layout.
#[derive(Clone)]
pub struct PaneInfo {
    pub id: PaneId,
    /// Outer rect (including borders if present).
    pub rect: Rect,
    /// Inner rect (content area, excluding borders). Used for selection.
    pub inner_rect: Rect,
    /// Visible scrollbar lane, when scrollback is present. `inner_rect` may still
    /// exclude a stable hidden gutter when this is `None`.
    pub scrollbar_rect: Option<Rect>,
    pub is_focused: bool,
}

/// Info about a split boundary, used for mouse drag resize.
#[derive(Clone)]
pub struct SplitBorder {
    /// Position of the divider line (x for horizontal split, y for vertical).
    pub pos: u16,
    /// Direction of the split that created this border.
    pub direction: Direction,
    /// Total area of the split node.
    pub area: Rect,
    /// Path from root to this split node (false=first, true=second).
    pub path: Vec<bool>,
}

/// Cardinal direction for pane navigation.
#[derive(Debug, Clone, Copy)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Preset layout shapes for auto-layout cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    EvenHorizontal,
    EvenVertical,
    MainVertical,
    MainHorizontal,
    Tiled,
}

impl LayoutKind {
    /// The next layout in the cycle (wraps around).
    pub fn next(self) -> Self {
        match self {
            Self::EvenHorizontal => Self::EvenVertical,
            Self::EvenVertical => Self::MainVertical,
            Self::MainVertical => Self::MainHorizontal,
            Self::MainHorizontal => Self::Tiled,
            Self::Tiled => Self::EvenHorizontal,
        }
    }
}

/// A node in the BSP tree. Public for serialization.
pub enum Node {
    Pane(PaneId),
    Split {
        direction: Direction,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// BSP tiling layout. Tracks a tree of splits and a focused pane.
pub struct TileLayout {
    root: Node,
    focus: PaneId,
    /// Last preset layout applied via [`apply_layout`] / [`cycle_layout`].
    /// Not persisted — resets to `None` on restore.
    last_layout: Option<LayoutKind>,
}

impl TileLayout {
    /// Create a new layout with a single pane (globally unique ID).
    /// Returns (layout, root_pane_id) so the caller can create the pane.
    pub fn new() -> (Self, PaneId) {
        let root_id = PaneId::alloc();
        (
            Self {
                root: Node::Pane(root_id),
                focus: root_id,
                last_layout: None,
            },
            root_id,
        )
    }

    pub fn focused(&self) -> PaneId {
        self.focus
    }

    pub fn pane_count(&self) -> usize {
        count_panes(&self.root)
    }

    /// Compute rects for all panes given the available area.
    pub fn panes(&self, area: Rect) -> Vec<PaneInfo> {
        let mut result = Vec::new();
        collect_panes(&self.root, area, self.focus, &mut result);
        result
    }

    /// Collect all split boundaries for mouse drag resize.
    pub fn splits(&self, area: Rect) -> Vec<SplitBorder> {
        let mut result = Vec::new();
        collect_splits(&self.root, area, vec![], &mut result);
        result
    }

    /// Split the focused pane. Returns the new pane's id.
    pub fn split_focused(&mut self, direction: Direction) -> PaneId {
        let new_id = PaneId::alloc();
        let placeholder = PaneId::from_raw(0);
        let old = std::mem::replace(&mut self.root, Node::Pane(placeholder));
        self.root = split_at(old, self.focus, direction, new_id);
        self.focus = new_id;
        new_id
    }

    /// Close the focused pane. Returns false if it's the last pane.
    pub fn close_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let target = self.focus;
        let ids = self.pane_ids();
        let pos = ids.iter().position(|id| *id == target).unwrap();
        let new_focus = if pos + 1 < ids.len() {
            ids[pos + 1]
        } else {
            ids[pos - 1]
        };
        let placeholder = PaneId::from_raw(0);
        let old = std::mem::replace(&mut self.root, Node::Pane(placeholder));
        if let Some(new_root) = remove_pane(old, target) {
            self.root = new_root;
            self.focus = new_focus;
            true
        } else {
            false
        }
    }

    pub fn focus_next(&mut self) {
        let ids = self.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == self.focus) {
            self.focus = ids[(pos + 1) % ids.len()];
        }
    }

    pub fn focus_prev(&mut self) {
        let ids = self.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == self.focus) {
            self.focus = ids[(pos + ids.len() - 1) % ids.len()];
        }
    }

    pub fn focus_pane(&mut self, id: PaneId) {
        if self.pane_ids().contains(&id) {
            self.focus = id;
        }
    }

    /// Set the ratio of a split node at the given path.
    pub fn set_ratio_at(&mut self, path: &[bool], ratio: f32) {
        set_ratio_at(&mut self.root, path, ratio.clamp(0.1, 0.9));
    }

    /// Adjust the nearest split in the given direction for the focused pane.
    /// `delta` is positive to grow, negative to shrink.
    pub fn resize_focused(&mut self, nav: NavDirection, delta: f32, area: Rect) {
        let panes = self.panes(area);
        let Some(focused) = panes.iter().find(|p| p.is_focused) else {
            return;
        };
        let focused_rect = focused.rect;
        let splits = self.splits(area);

        // Find the split whose border is adjacent to the focused pane in the given direction
        let target_dir = match nav {
            NavDirection::Left | NavDirection::Right => Direction::Horizontal,
            NavDirection::Up | NavDirection::Down => Direction::Vertical,
        };
        let grows = matches!(nav, NavDirection::Right | NavDirection::Down);

        // Find the closest matching split border
        let best = splits
            .iter()
            .filter(|s| s.direction == target_dir)
            .filter(|s| match target_dir {
                Direction::Horizontal => {
                    // Border must be near the focused pane's left or right edge
                    let near_right = (s.pos as i32 - (focused_rect.x + focused_rect.width) as i32)
                        .unsigned_abs()
                        <= 1;
                    let near_left = (s.pos as i32 - focused_rect.x as i32).unsigned_abs() <= 1;
                    near_right || near_left
                }
                Direction::Vertical => {
                    let near_bottom = (s.pos as i32
                        - (focused_rect.y + focused_rect.height) as i32)
                        .unsigned_abs()
                        <= 1;
                    let near_top = (s.pos as i32 - focused_rect.y as i32).unsigned_abs() <= 1;
                    near_bottom || near_top
                }
            })
            .min_by_key(|s| {
                // Prefer the border in the direction we're resizing toward
                match (target_dir, grows) {
                    (Direction::Horizontal, true) => {
                        ((focused_rect.x + focused_rect.width) as i32 - s.pos as i32).unsigned_abs()
                    }
                    (Direction::Horizontal, false) => {
                        (focused_rect.x as i32 - s.pos as i32).unsigned_abs()
                    }
                    (Direction::Vertical, true) => ((focused_rect.y + focused_rect.height) as i32
                        - s.pos as i32)
                        .unsigned_abs(),
                    (Direction::Vertical, false) => {
                        (focused_rect.y as i32 - s.pos as i32).unsigned_abs()
                    }
                }
            });

        if let Some(split) = best {
            let path = split.path.clone();
            let current_ratio = get_ratio_at(&self.root, &path).unwrap_or(0.5);
            let adj = if grows { delta } else { -delta };
            self.set_ratio_at(&path, current_ratio + adj);
        }
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        collect_ids(&self.root, &mut ids);
        ids
    }

    /// Access the tree root for serialization.
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Reconstruct a layout from a saved tree.
    pub fn from_saved(root: Node, focus: PaneId) -> Self {
        Self {
            root,
            focus,
            last_layout: None,
        }
    }

    #[cfg(test)]
    pub fn current_layout(&self) -> Option<LayoutKind> {
        self.last_layout
    }

    /// Rebuild the BSP tree into the given preset. Preserves the set of pane
    /// IDs and the focused pane; only the tree structure and ratios change.
    ///
    /// For `MainVertical` / `MainHorizontal`, `main` becomes the large pane.
    /// For other kinds, `main` is unused but kept for a uniform call site.
    pub fn apply_layout(&mut self, kind: LayoutKind, main: PaneId) {
        let ids = self.pane_ids();
        if ids.len() <= 1 {
            // Nothing to restructure, but remember the kind so cycling progresses.
            self.last_layout = Some(kind);
            return;
        }
        let main = if ids.contains(&main) {
            main
        } else {
            self.focus
        };
        self.root = build_layout(&ids, kind, main);
        self.last_layout = Some(kind);
    }

    /// Apply the next preset in the cycle. Returns the applied kind.
    pub fn cycle_layout(&mut self, main: PaneId) -> LayoutKind {
        let next = self
            .last_layout
            .map_or(LayoutKind::EvenHorizontal, LayoutKind::next);
        self.apply_layout(next, main);
        next
    }
}

// --- Directional pane navigation ---

/// Find the nearest pane in the given direction from `focused`.
pub fn find_in_direction(
    focused: &PaneInfo,
    direction: NavDirection,
    panes: &[PaneInfo],
) -> Option<PaneId> {
    let fr = focused.rect;

    panes
        .iter()
        .filter(|p| p.id != focused.id)
        .filter(|p| {
            let r = p.rect;
            match direction {
                NavDirection::Left => {
                    r.x + r.width <= fr.x && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Right => {
                    r.x >= fr.x + fr.width && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Up => {
                    r.y + r.height <= fr.y && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
                NavDirection::Down => {
                    r.y >= fr.y + fr.height && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
            }
        })
        .min_by_key(|p| {
            let r = p.rect;
            match direction {
                NavDirection::Left => fr.x.saturating_sub(r.x + r.width),
                NavDirection::Right => r.x.saturating_sub(fr.x + fr.width),
                NavDirection::Up => fr.y.saturating_sub(r.y + r.height),
                NavDirection::Down => r.y.saturating_sub(fr.y + fr.height),
            }
        })
        .map(|p| p.id)
}

fn ranges_overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> bool {
    a_start < b_start + b_len && a_start + a_len > b_start
}

// --- Tree operations ---

fn count_panes(node: &Node) -> usize {
    match node {
        Node::Pane(_) => 1,
        Node::Split { first, second, .. } => count_panes(first) + count_panes(second),
    }
}

fn collect_panes(node: &Node, area: Rect, focus: PaneId, result: &mut Vec<PaneInfo>) {
    match node {
        Node::Pane(id) => {
            result.push(PaneInfo {
                id: *id,
                rect: area,
                // inner_rect is set during render when we know if borders are shown
                inner_rect: area,
                scrollbar_rect: None,
                is_focused: *id == focus,
            });
        }
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a, b) = split_rect(area, *direction, *ratio);
            collect_panes(first, a, focus, result);
            collect_panes(second, b, focus, result);
        }
    }
}

fn collect_splits(node: &Node, area: Rect, path: Vec<bool>, result: &mut Vec<SplitBorder>) {
    if let Node::Split {
        direction,
        ratio,
        first,
        second,
    } = node
    {
        let (a, b) = split_rect(area, *direction, *ratio);
        let pos = match direction {
            Direction::Horizontal => a.x + a.width,
            Direction::Vertical => a.y + a.height,
        };
        result.push(SplitBorder {
            pos,
            direction: *direction,
            area,
            path: path.clone(),
        });
        let mut lp = path.clone();
        lp.push(false);
        collect_splits(first, a, lp, result);
        let mut rp = path;
        rp.push(true);
        collect_splits(second, b, rp, result);
    }
}

fn collect_ids(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids(first, ids);
            collect_ids(second, ids);
        }
    }
}

fn split_at(node: Node, target: PaneId, direction: Direction, new_id: PaneId) -> Node {
    match node {
        Node::Pane(id) if id == target => Node::Split {
            direction,
            ratio: 0.5,
            first: Box::new(Node::Pane(id)),
            second: Box::new(Node::Pane(new_id)),
        },
        Node::Pane(_) => node,
        Node::Split {
            direction: d,
            ratio,
            first,
            second,
        } => Node::Split {
            direction: d,
            ratio,
            first: Box::new(split_at(*first, target, direction, new_id)),
            second: Box::new(split_at(*second, target, direction, new_id)),
        },
    }
}

fn remove_pane(node: Node, target: PaneId) -> Option<Node> {
    match node {
        Node::Pane(id) if id == target => None,
        Node::Pane(_) => Some(node),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => match (remove_pane(*first, target), remove_pane(*second, target)) {
            (None, Some(s)) => Some(s),
            (Some(f), None) => Some(f),
            (Some(f), Some(s)) => Some(Node::Split {
                direction,
                ratio,
                first: Box::new(f),
                second: Box::new(s),
            }),
            (None, None) => None,
        },
    }
}

fn set_ratio_at(node: &mut Node, path: &[bool], new_ratio: f32) {
    if let Node::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        if path.is_empty() {
            *ratio = new_ratio;
        } else if path[0] {
            set_ratio_at(second, &path[1..], new_ratio);
        } else {
            set_ratio_at(first, &path[1..], new_ratio);
        }
    }
}

fn get_ratio_at(node: &Node, path: &[bool]) -> Option<f32> {
    if let Node::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        if path.is_empty() {
            Some(*ratio)
        } else if path[0] {
            get_ratio_at(second, &path[1..])
        } else {
            get_ratio_at(first, &path[1..])
        }
    } else {
        None
    }
}

fn split_rect(area: Rect, direction: Direction, ratio: f32) -> (Rect, Rect) {
    match direction {
        Direction::Horizontal => {
            let first_w = ((area.width as f32) * ratio).round() as u16;
            let second_w = area.width.saturating_sub(first_w);
            (
                Rect::new(area.x, area.y, first_w, area.height),
                Rect::new(area.x + first_w, area.y, second_w, area.height),
            )
        }
        Direction::Vertical => {
            let first_h = ((area.height as f32) * ratio).round() as u16;
            let second_h = area.height.saturating_sub(first_h);
            (
                Rect::new(area.x, area.y, area.width, first_h),
                Rect::new(area.x, area.y + first_h, area.width, second_h),
            )
        }
    }
}

// --- Preset layout builders ---

fn build_layout(ids: &[PaneId], kind: LayoutKind, main: PaneId) -> Node {
    debug_assert!(!ids.is_empty(), "build_layout called with empty pane list");
    match kind {
        LayoutKind::EvenHorizontal => build_even_chain(ids, Direction::Horizontal),
        LayoutKind::EvenVertical => build_even_chain(ids, Direction::Vertical),
        LayoutKind::MainVertical => build_main(ids, main, Direction::Horizontal),
        LayoutKind::MainHorizontal => build_main(ids, main, Direction::Vertical),
        LayoutKind::Tiled => build_tiled(ids),
    }
}

/// Build a left-leaning chain of equal-sized panes along `direction`. The
/// split at depth `k` uses ratio `1 / (n - k)`, so each cell ends up the same
/// fraction of the total.
fn build_even_chain(ids: &[PaneId], direction: Direction) -> Node {
    let n = ids.len();
    debug_assert!(n >= 1);
    if n == 1 {
        return Node::Pane(ids[0]);
    }
    let mut node = Node::Pane(ids[n - 1]);
    for (i, id) in ids[..n - 1].iter().enumerate().rev() {
        let remaining = n - i;
        node = Node::Split {
            direction,
            ratio: 1.0 / remaining as f32,
            first: Box::new(Node::Pane(*id)),
            second: Box::new(node),
        };
    }
    node
}

/// Main pane on one side, even chain of the rest on the other side.
/// `outer` is the direction of the top-level split:
///   - `Horizontal` → main on the left, rest stacked vertically on the right
///     (i.e. `MainVertical`)
///   - `Vertical` → main on top, rest side-by-side horizontally below
///     (i.e. `MainHorizontal`)
fn build_main(ids: &[PaneId], main: PaneId, outer: Direction) -> Node {
    let rest: Vec<PaneId> = ids.iter().copied().filter(|id| *id != main).collect();
    if rest.is_empty() {
        return Node::Pane(main);
    }
    let chain_direction = match outer {
        Direction::Horizontal => Direction::Vertical,
        Direction::Vertical => Direction::Horizontal,
    };
    let rest_node = build_even_chain(&rest, chain_direction);
    Node::Split {
        direction: outer,
        ratio: 0.5,
        first: Box::new(Node::Pane(main)),
        second: Box::new(rest_node),
    }
}

/// Grid layout: roughly `ceil(sqrt(n))` columns. The first `n % cols` columns
/// hold one extra pane than the rest, so panes are distributed as evenly as
/// possible.
fn build_tiled(ids: &[PaneId]) -> Node {
    let n = ids.len();
    debug_assert!(n >= 1);
    if n == 1 {
        return Node::Pane(ids[0]);
    }
    let cols = (n as f32).sqrt().ceil() as usize;
    let base = n / cols;
    let extras = n % cols;

    // Partition ids into columns, each column gets `base + 1` if its index < extras.
    let mut columns: Vec<Vec<PaneId>> = Vec::with_capacity(cols);
    let mut cursor = 0;
    for col in 0..cols {
        let take = base + if col < extras { 1 } else { 0 };
        columns.push(ids[cursor..cursor + take].to_vec());
        cursor += take;
    }

    // Build a left-leaning chain of columns. Each column's "width" is
    // proportional to the number of panes it holds, so cells stay roughly
    // square — but for predictability we just give each column equal width
    // (matches tmux behavior).
    let mut node = build_even_chain(&columns[cols - 1], Direction::Vertical);
    for (i, col_ids) in columns[..cols - 1].iter().enumerate().rev() {
        let remaining = cols - i;
        node = Node::Split {
            direction: Direction::Horizontal,
            ratio: 1.0 / remaining as f32,
            first: Box::new(build_even_chain(col_ids, Direction::Vertical)),
            second: Box::new(node),
        };
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_panes(n: usize) -> Vec<PaneId> {
        (0..n).map(|_| PaneId::alloc()).collect()
    }

    fn layout_with_panes(n: usize) -> (TileLayout, Vec<PaneId>) {
        assert!(n >= 1);
        let ids = make_panes(n);
        // Build a degenerate left-leaning vertical chain to get the layout
        // into a non-canonical state, so apply_layout has work to do.
        let mut node = Node::Pane(ids[n - 1]);
        for id in ids[..n - 1].iter().rev() {
            node = Node::Split {
                direction: Direction::Vertical,
                ratio: 0.3,
                first: Box::new(Node::Pane(*id)),
                second: Box::new(node),
            };
        }
        let layout = TileLayout::from_saved(node, ids[0]);
        (layout, ids)
    }

    fn pane_id_set(layout: &TileLayout) -> HashSet<PaneId> {
        layout.pane_ids().into_iter().collect()
    }

    fn assert_root_split(node: &Node, expected: Direction) {
        match node {
            Node::Split { direction, .. } => assert_eq!(*direction, expected),
            Node::Pane(_) => panic!("expected split at root, got pane"),
        }
    }

    #[test]
    fn cycle_layout_advances_through_all_five_then_wraps() {
        let (mut layout, _ids) = layout_with_panes(4);
        let main = layout.focused();
        let kinds = [
            LayoutKind::EvenHorizontal,
            LayoutKind::EvenVertical,
            LayoutKind::MainVertical,
            LayoutKind::MainHorizontal,
            LayoutKind::Tiled,
            LayoutKind::EvenHorizontal, // wraps
        ];
        for expected in kinds {
            let applied = layout.cycle_layout(main);
            assert_eq!(applied, expected);
            assert_eq!(layout.current_layout(), Some(expected));
        }
    }

    #[test]
    fn apply_layout_preserves_pane_ids_and_focus() {
        for kind in [
            LayoutKind::EvenHorizontal,
            LayoutKind::EvenVertical,
            LayoutKind::MainVertical,
            LayoutKind::MainHorizontal,
            LayoutKind::Tiled,
        ] {
            for n in [2usize, 3, 4, 5, 9] {
                let (mut layout, ids) = layout_with_panes(n);
                let before = pane_id_set(&layout);
                let focus = layout.focused();
                layout.apply_layout(kind, focus);
                assert_eq!(
                    pane_id_set(&layout),
                    before,
                    "{kind:?} n={n} must preserve pane id set"
                );
                assert_eq!(
                    layout.focused(),
                    focus,
                    "{kind:?} n={n} must preserve focus"
                );
                assert_eq!(layout.pane_count(), n);
                assert!(ids.iter().all(|id| layout.pane_ids().contains(id)));
            }
        }
    }

    #[test]
    fn even_horizontal_root_is_horizontal() {
        let (mut layout, _) = layout_with_panes(4);
        layout.apply_layout(LayoutKind::EvenHorizontal, layout.focused());
        assert_root_split(layout.root(), Direction::Horizontal);
    }

    #[test]
    fn even_vertical_root_is_vertical() {
        let (mut layout, _) = layout_with_panes(4);
        layout.apply_layout(LayoutKind::EvenVertical, layout.focused());
        assert_root_split(layout.root(), Direction::Vertical);
    }

    #[test]
    fn main_vertical_puts_main_on_left() {
        let (mut layout, ids) = layout_with_panes(4);
        let main = ids[2];
        layout.apply_layout(LayoutKind::MainVertical, main);
        match layout.root() {
            Node::Split {
                direction,
                ratio,
                first,
                ..
            } => {
                assert_eq!(*direction, Direction::Horizontal);
                assert!((*ratio - 0.5).abs() < 1e-6);
                match first.as_ref() {
                    Node::Pane(id) => assert_eq!(*id, main),
                    _ => panic!("main pane should be a leaf at first child"),
                }
            }
            _ => panic!("MainVertical root must be a split"),
        }
    }

    #[test]
    fn main_horizontal_puts_main_on_top() {
        let (mut layout, ids) = layout_with_panes(4);
        let main = ids[1];
        layout.apply_layout(LayoutKind::MainHorizontal, main);
        match layout.root() {
            Node::Split {
                direction, first, ..
            } => {
                assert_eq!(*direction, Direction::Vertical);
                match first.as_ref() {
                    Node::Pane(id) => assert_eq!(*id, main),
                    _ => panic!("main pane should be a leaf at first child"),
                }
            }
            _ => panic!("MainHorizontal root must be a split"),
        }
    }

    #[test]
    fn even_horizontal_produces_equal_widths() {
        let (mut layout, _) = layout_with_panes(5);
        layout.apply_layout(LayoutKind::EvenHorizontal, layout.focused());
        let area = Rect::new(0, 0, 100, 20);
        let panes = layout.panes(area);
        assert_eq!(panes.len(), 5);
        // Each pane should be ~20 cols wide; allow ±1 for rounding.
        for p in &panes {
            assert!(
                (p.rect.width as i32 - 20).abs() <= 1,
                "expected ~20 cols, got {} (rect={:?})",
                p.rect.width,
                p.rect
            );
            assert_eq!(p.rect.height, 20);
        }
    }

    #[test]
    fn tiled_distributes_panes_into_grid() {
        let (mut layout, _) = layout_with_panes(4);
        layout.apply_layout(LayoutKind::Tiled, layout.focused());
        let panes = layout.panes(Rect::new(0, 0, 100, 100));
        assert_eq!(panes.len(), 4);
        // For n=4, cols=2, each column has 2 rows: a 2×2 grid.
        // Each cell should be ~50×50.
        for p in &panes {
            assert!((p.rect.width as i32 - 50).abs() <= 1);
            assert!((p.rect.height as i32 - 50).abs() <= 1);
        }
    }

    #[test]
    fn tiled_handles_uneven_pane_count() {
        // n=5 → cols=ceil(sqrt(5))=3, base=1, extras=2.
        // Columns hold [2, 2, 1] panes.
        let (mut layout, _) = layout_with_panes(5);
        layout.apply_layout(LayoutKind::Tiled, layout.focused());
        let panes = layout.panes(Rect::new(0, 0, 99, 100));
        assert_eq!(panes.len(), 5);
        // All panes should be non-empty.
        for p in &panes {
            assert!(p.rect.width > 0 && p.rect.height > 0);
        }
    }

    #[test]
    fn single_pane_layout_is_noop_but_advances_cycle_state() {
        let ids = make_panes(1);
        let mut layout = TileLayout::from_saved(Node::Pane(ids[0]), ids[0]);
        layout.apply_layout(LayoutKind::Tiled, ids[0]);
        assert_eq!(layout.pane_count(), 1);
        assert_eq!(layout.current_layout(), Some(LayoutKind::Tiled));
        // Cycling from Tiled should give EvenHorizontal.
        let next = layout.cycle_layout(ids[0]);
        assert_eq!(next, LayoutKind::EvenHorizontal);
    }

    #[test]
    fn from_saved_resets_layout_state() {
        let (mut layout, _) = layout_with_panes(3);
        layout.apply_layout(LayoutKind::Tiled, layout.focused());
        assert_eq!(layout.current_layout(), Some(LayoutKind::Tiled));

        // Round-trip through from_saved (simulating persistence reload).
        let focus = layout.focused();
        let root = std::mem::replace(&mut layout.root, Node::Pane(PaneId::from_raw(0)));
        let restored = TileLayout::from_saved(root, focus);
        assert_eq!(restored.current_layout(), None);
    }
}
