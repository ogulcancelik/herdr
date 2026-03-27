use crossterm::event::{KeyCode, KeyModifiers};
use crate::config::Keybinds;
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;

use crate::layout::{PaneInfo, SplitBorder};
use crate::selection::Selection;
use crate::workspace::Workspace;

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
pub struct ViewState {
    pub sidebar_rect: Rect,
    pub terminal_area: Rect,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Navigate,
    Terminal,
    CreateSession,
    RenameSession,
    Resize,
    ConfirmClose,
    ContextMenu,
}

/// Active mouse drag on a split border.
pub(crate) struct DragState {
    pub path: Vec<bool>,
    pub direction: Direction,
    pub area: Rect,
}

/// Right-click context menu state.
pub struct ContextMenuState {
    pub ws_idx: usize,
    pub x: u16,
    pub y: u16,
    pub selected: usize,
}

pub const CONTEXT_MENU_ITEMS: &[&str] = &["Rename", "Close"];

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
pub struct AppState {
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    pub name_input: String,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    pub selection: Option<Selection>,
    pub context_menu: Option<ContextMenuState>,
    // Update notification
    pub update_available: Option<String>,
    pub update_dismissed: bool,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    pub prefix_label: String,
    pub sidebar_width: u16,
    pub sidebar_collapsed: bool,
    pub confirm_close: bool,
    pub accent: Color,
    pub sound: bool,
    pub keybinds: Keybinds,
}

impl AppState {
    pub fn is_prefix(&self, key: &crossterm::event::KeyEvent) -> bool {
        key.code == self.prefix_code && key.modifiers.contains(self.prefix_mods)
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (24, 80)
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl AppState {
    /// Create an AppState for testing — no channels, no PTYs.
    pub fn test_new() -> Self {
        Self {
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            mode: Mode::Navigate,
            should_quit: false,
            name_input: String::new(),
            view: ViewState {
                sidebar_rect: Rect::default(),
                terminal_area: Rect::default(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
            },
            drag: None,
            selection: None,
            context_menu: None,
            update_available: None,
            update_dismissed: false,
            prefix_code: KeyCode::Char('s'),
            prefix_mods: KeyModifiers::CONTROL,
            prefix_label: "ctrl+s".into(),
            sidebar_width: 26,
            sidebar_collapsed: false,
            confirm_close: true,
            accent: Color::Cyan,
            sound: true,
            keybinds: Keybinds {
                split_vertical: (KeyCode::Char('v'), KeyModifiers::empty()),
                split_horizontal: (KeyCode::Char('-'), KeyModifiers::empty()),
                close_pane: (KeyCode::Char('x'), KeyModifiers::empty()),
                fullscreen: (KeyCode::Char('f'), KeyModifiers::empty()),
            },
        }
    }
}
